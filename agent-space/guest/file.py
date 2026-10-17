"""Fixed MCP file operations; one short-lived process per request, no service.

Byte-range locks on a single stable inode coordinate canonical paths without a
persistent per-path registry. A 63-bit hash collision only serializes two paths.
Locks are automatically released when a process exits, including cancellation.
External editors, bash and DAV do not participate in this advisory protocol.
"""
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import signal
import stat
import sys
import tempfile
import io

SOURCE_LIMIT = 10 * 1024 * 1024


def read_file(path, spec):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC)
    except FileNotFoundError:
        print("FILE_NOT_FOUND")
        return 1
    with os.fdopen(fd, "rb") as stream:
        meta = os.fstat(stream.fileno())
        if not stat.S_ISREG(meta.st_mode):
            print("NOT_A_FILE")
            return 1
        if not meta.st_size:
            print("EMPTY")
            return 0
        head = stream.read(512)
        supported = (head.startswith((b"\xff\xd8", b"\x89PNG", b"GIF87a", b"GIF89a"))
                     or (head.startswith(b"RIFF") and head[8:12] == b"WEBP"))
        text_bom = head.startswith((b"\xff\xfe", b"\xfe\xff", b"\0\0\xfe\xff"))
        xml = head.lstrip().lower()
        svg = xml.startswith(b"<svg") or (xml.startswith(b"<?xml") and b"<svg" in xml)
        media = supported or (b"\0" in head and not text_bom) or head.startswith((b"BM", b"II*", b"MM\0*", b"%PDF")) or svg
        stream.seek(0)
        if media:
            # This separate marker survives bounded stdout collection. Media
            # reports must be parsed whole; they never become truncated text.
            print("RUNCOVE_READ_MEDIA", file=sys.stderr, flush=True)
            error = None
            if spec.get("line_range"):
                error = {"code": "INVALID_ARGUMENT", "message": "offset and limit apply only to text files; omit both for images"}
            elif not supported:
                error = {"code": "UNSUPPORTED_MEDIA", "message": "Supported images are JPEG, PNG, WebP, and GIF"}
            elif not spec.get("multimodal", False):
                error = {"code": "MULTIMODAL_REQUIRED", "message": "Set multimodal=true to read image content"}
            elif meta.st_size > SOURCE_LIMIT:
                error = {"code": "IMAGE_TOO_LARGE", "message": "Source image exceeds 10 MiB"}
            if error:
                print(json.dumps({"error": error}))
                return 0
            data = stream.read(SOURCE_LIMIT + 1)
            if len(data) > SOURCE_LIMIT:
                print(json.dumps({"error": {"code": "IMAGE_TOO_LARGE", "message": "Source image exceeds 10 MiB"}}))
            else:
                print(json.dumps({"path": path, "data": base64.b64encode(data).decode("ascii")}))
            return 0
        print("RUNCOVE_READ_TEXT", file=sys.stderr, flush=True)
        start, count = spec["offset"], spec["limit"]
        found = False
        with io.TextIOWrapper(stream, encoding="utf-8", errors="replace", newline="") as text:
            for number, line in enumerate(text, 1):
                if number >= start + count:
                    break
                if number >= start:
                    print(f"{number:6}\t{line.rstrip(chr(10))}")
                    found = True
        if not found:
            print("NO_CONTENT")
        return 0


def interrupted(_signum, _frame):
    raise InterruptedError("file operation cancelled; inspect the file before retrying")


def atomic_write(path, content):
    previous = None
    try:
        previous = os.stat(path)
        if not stat.S_ISREG(previous.st_mode):
            raise ValueError("target must be a regular file")
    except FileNotFoundError:
        pass
    fd, temporary = tempfile.mkstemp(prefix=".agent-space-write-", dir=os.path.dirname(path))
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(content.encode("utf-8"))
            if previous is not None:
                os.fchown(stream.fileno(), previous.st_uid, previous.st_gid)
                os.fchmod(stream.fileno(), stat.S_IMODE(previous.st_mode))
            else:
                mask = os.umask(0)
                os.umask(mask)
                os.fchmod(stream.fileno(), 0o666 & ~mask)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def run(spec):
    path = os.path.realpath(spec["path"])
    mode = spec["operation"]
    # Never unlink this inode while the VM is alive: doing so would split locks.
    directory = Path("/tmp/agent-space")
    directory.mkdir(mode=0o755, parents=True, exist_ok=True)
    fd = os.open(directory / "file-operations.lock", os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
    try:
        offset = int.from_bytes(hashlib.sha256(os.fsencode(path)).digest()[:8], "big") & ((1 << 63) - 2)
        fcntl.lockf(fd, fcntl.LOCK_SH if mode == "read" else fcntl.LOCK_EX, 1, offset)
        if mode == "read":
            return read_file(path, spec)
        elif mode == "write":
            os.makedirs(os.path.dirname(path), exist_ok=True)
            atomic_write(path, spec["content"])
            print(len(spec["content"].encode("utf-8")))
        elif mode == "edit":
            with open(path, "r", encoding="utf-8", newline="") as stream:
                content = stream.read()
            count = content.count(spec["old_text"])
            if count == 0:
                print("NOT_FOUND")
                return 1
            if count > 1:
                print("MULTIPLE:" + str(count))
                return 2
            atomic_write(path, content.replace(spec["old_text"], spec["new_text"], 1))
            print("OK")
        else:
            raise ValueError("unknown file operation")
        return 0
    finally:
        os.close(fd)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupted)
    try:
        sys.exit(run(json.loads(sys.argv[1])))
    except IsADirectoryError:
        print("The destination is a directory. Provide a file path.", file=sys.stderr)
        sys.exit(1)
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
