"""Ephemeral guest launcher invoked by the Host; never a background supervisor.
Only fixed project executables are supported. JSON records are recovery hints;
PID, boot/start identity, executable inode and private markers must all agree.
"""
import hashlib
import json
import os
import select
import signal
import socket
import stat
import subprocess
import sys
import time
import uuid

ROOT = "/opt/agent-space"
RUN = "/tmp/agent-space"
EXECUTABLES = {"static-http": ROOT + "/bin/agent-space-static-http", "webdav": ROOT + "/bin/agent-space-webdav", "desktop": ROOT + "/bin/agent-space-desktop"}


def instance_dir(instance):
    uuid.UUID(instance)
    return RUN + "/" + instance


def atomic(path, data, mode=0o600):
    temporary = path + ".tmp"
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, mode)
    with os.fdopen(fd, "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    os.chmod(temporary, mode)
    os.replace(temporary, path)
    fd = os.open(os.path.dirname(path), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def read_json(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise RuntimeError("invalid managed record")
        return json.loads(stream.read(32769))


def boot():
    with open("/proc/sys/kernel/random/boot_id") as stream:
        return stream.read().strip()


def process_start(pid):
    with open(f"/proc/{pid}/stat") as stream:
        fields = stream.read().rsplit(")", 1)[1].split()
    return int(fields[19]), fields[0]


def digest(path):
    with open(path, "rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def identity_fd(item):
    pid = item["pid"]
    if pid < 2 or item["bootId"] != boot():
        return None
    try:
        fd = os.pidfd_open(pid)
    except ProcessLookupError:
        return None
    try:
        ticks, status = process_start(pid)
        if ticks != item["startTicks"] or status == "Z":
            os.close(fd)
            return None
        executable = EXECUTABLES[item["service"]]
        with open(f"/proc/{pid}/cmdline", "rb") as stream:
            command = stream.read(8192).split(b"\0", 1)[0].decode()
        with open(f"/proc/{pid}/environ", "rb") as stream:
            environment = stream.read(65536).split(b"\0")
        inode = os.stat(f"/proc/{pid}/exe")
        expected = [b"AGENT_SPACE_HELPER=1", ("AGENT_SPACE_HELPER_INSTANCE=" + item["instanceId"]).encode()]
        expected.append(("AGENT_SPACE_HELPER_SERVICE=" + item["service"]).encode())
        if command != executable or not all(marker in environment for marker in expected) or os.getpgid(pid) != pid or inode.st_ino != item["executableInode"] or inode.st_dev != item["executableDevice"]:
            raise RuntimeError("managed process identity mismatch; retained for reconciliation")
        return fd
    except (FileNotFoundError, ProcessLookupError):
        os.close(fd)
        return None
    except BaseException:
        os.close(fd)
        raise


def clean_uploads(item, settings):
    if item["service"] != "webdav":
        return
    for stage in ("/workspace/.agent-space/uploads", "/workspace/.agent-space-upload"):
        clean_uploads_at(item, settings, stage)


def clean_uploads_at(item, settings, stage):
    try:
        fd = os.open(stage, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    except (FileNotFoundError, NotADirectoryError):
        return
    try:
        try:
            os.stat(item["instanceId"], dir_fd=fd, follow_symlinks=False)
        except FileNotFoundError:
            return
        marker = os.open("owner", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=fd)
        with os.fdopen(marker, "rb") as stream:
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                raise RuntimeError("invalid upload owner marker")
            if stream.read(256) != ("agent-space-upload-v1:" + settings["owner"] + "\n").encode():
                raise RuntimeError("upload owner mismatch; refusing cleanup")
        try:
            child = os.open(item["instanceId"], os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
        except FileNotFoundError:
            return
        try:
            names = os.listdir(child)
            try:
                marker = os.open("boot-id", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=child)
            except FileNotFoundError:
                # Startup can end after mkdir but before writing its marker.
                # Only the empty residue is safe to remove without boot proof.
                if names:
                    raise RuntimeError("upload boot marker missing; refusing cleanup")
            else:
                with os.fdopen(marker, "rb") as stream:
                    if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode) or stream.read(64) != (item["bootId"] + "\n").encode():
                        raise RuntimeError("upload boot marker mismatch; refusing cleanup")
            # Validate every entry before mutating anything. listdir order is
            # unspecified, including the position of ownership markers.
            uploads = [name for name in names if name != "boot-id"]
            for name in uploads:
                uuid.UUID(name)  # Only the unique request files this helper creates.
                if not stat.S_ISREG(os.stat(name, dir_fd=child, follow_symlinks=False).st_mode):
                    raise RuntimeError("unexpected upload entry; refusing cleanup")
            for name in uploads:
                os.unlink(name, dir_fd=child)
            try:
                os.unlink("boot-id", dir_fd=child)
            except FileNotFoundError:
                pass
        finally:
            os.close(child)
        os.rmdir(item["instanceId"], dir_fd=fd)
    finally:
        os.close(fd)


def clean_record(item):
    directory = instance_dir(item["instanceId"])
    try:
        record = read_json(directory + "/instance.json")
    except FileNotFoundError:
        return
    # A stale or forged caller must not erase the recovery record of a live
    # instance. Port can change during readiness; process identity cannot.
    keys = ("service", "instanceId", "pid", "startTicks", "bootId", "executableDevice", "executableInode", "binaryDigest", "launchDigest")
    if any(record["instance"][key] != item[key] for key in keys):
        raise RuntimeError("instance record mismatch; ownership retained")
    clean_uploads(item, record["settings"])
    temporary = EXECUTABLES[item["service"]] + "." + item["instanceId"] + ".tmp"
    try:
        if stat.S_ISREG(os.lstat(temporary).st_mode):
            os.unlink(temporary)
    except FileNotFoundError:
        pass
    allowed = {"instance.json", "instance.json.tmp", "ready", "log", "config.json", "config.json.tmp"}
    if item["service"] == "desktop":
        entries = os.listdir(directory)
        owned = []
        for name in entries:
            if name.startswith("desktop-") and name.endswith(".log"):
                if name not in {f"desktop-{p}-{s}.log" for p in ("vnc", "desktop", "terminal", "browser") for s in ("out", "err")}:
                    raise RuntimeError("unexpected desktop log")
                if not stat.S_ISREG(os.lstat(directory + "/" + name).st_mode):
                    raise RuntimeError("invalid desktop log")
                allowed.add(name)
            elif name.startswith("job-"):
                uuid.UUID(name[4:])
                child = directory + "/" + name
                if not stat.S_ISDIR(os.lstat(child).st_mode):
                    raise RuntimeError("invalid desktop job directory")
                files = os.listdir(child)
                if any(f not in {"result.json", "observation.json"} or not stat.S_ISREG(os.lstat(child + "/" + f).st_mode) for f in files):
                    raise RuntimeError("unexpected desktop job entry; retained")
                owned.append((child, files))
        for child, files in owned:
            for name in files: os.unlink(child + "/" + name)
            os.rmdir(child)
    for name in os.listdir(directory):
        if name not in allowed or not stat.S_ISREG(os.lstat(directory + "/" + name).st_mode):
            raise RuntimeError("unexpected instance entry; refusing cleanup")
    for name in os.listdir(directory):
        os.unlink(directory + "/" + name)
    os.rmdir(directory)


def stop(item):
    fd = identity_fd(item)
    if fd is not None:
        try:
            poll = select.poll()
            poll.register(fd, select.POLLIN)
            signal.pidfd_send_signal(fd, signal.SIGTERM)
            if not poll.poll(15000 if item["service"] == "desktop" else 2000):
                signal.pidfd_send_signal(fd, signal.SIGKILL)
                if not poll.poll(2000):
                    raise RuntimeError("managed process did not exit; ownership retained")
        except ProcessLookupError:
            pass
        finally:
            os.close(fd)
    clean_record(item)


def listing(service):
    result = []
    if os.path.isdir(RUN):
        for identifier in os.listdir(RUN):
            try:
                directory = instance_dir(identifier)
            except ValueError:
                continue
            if not stat.S_ISDIR(os.lstat(directory).st_mode):
                raise RuntimeError("unexpected instance directory")
            record = read_json(directory + "/instance.json")
            item = record["instance"]
            if item["instanceId"] != identifier or item["service"] not in EXECUTABLES:
                raise RuntimeError("invalid instance record")
            if item["service"] == service:
                result.append(item)
    return result


def start(data):
    item = data["instance"]
    directory = instance_dir(item["instanceId"])
    executable = EXECUTABLES[item["service"]]
    temporary = executable + "." + item["instanceId"] + ".tmp"
    if digest(temporary) != item["binaryDigest"]:
        os.unlink(temporary)
        raise RuntimeError("helper digest mismatch")
    os.chmod(temporary, 0o755)
    os.replace(temporary, executable)
    inode = os.stat(executable)
    item.update(bootId=boot(), executableDevice=inode.st_dev, executableInode=inode.st_ino)
    settings = data["settings"]
    if item["service"] == "static-http":
        args = [executable, "file" if settings["file"] else "directory", settings["root"]]
    elif item["service"] == "desktop":
        args = [executable]
    else:
        config = json.dumps(settings["config"], sort_keys=True).encode()
        atomic(ROOT + "/etc/webdav.json", config)
        atomic(directory + "/config.json", config)
        args = [executable, "--config", directory + "/config.json", "--owner", settings["owner"], "--instance", item["instanceId"]]
    env = dict(os.environ, AGENT_SPACE_HELPER="1", AGENT_SPACE_HELPER_INSTANCE=item["instanceId"], AGENT_SPACE_HELPER_SERVICE=item["service"])
    if "uid" in data:
        if data["uid"] != 1001 or item["service"] == "desktop":
            raise RuntimeError("invalid local helper identity")
        env["AGENT_SPACE_DATA_UID"] = str(data["uid"])

    def record_child():
        child = dict(item, pid=os.getpid(), startTicks=process_start(os.getpid())[0])
        atomic(directory + "/instance.json", json.dumps(dict(instance=child, settings=settings)).encode())

    with open(directory + "/ready", "wb") as ready, open(directory + "/log", "ab") as log:
        if "uid" in data:
            # Identity records stay root-owned. Only non-sensitive config and
            # this helper's bounded log are accessible to its data-plane UID.
            os.chmod(directory, 0o711)
            if item["service"] == "webdav":
                os.chmod(directory + "/config.json", 0o644)
            os.chown(directory + "/log", data["uid"], data["uid"])
        child = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=ready, stderr=log, env=env, close_fds=True, start_new_session=True, preexec_fn=record_child)
    # The child records ownership before exec. Cancellation of this launcher
    # therefore cannot leave an unrecorded detached helper.
    record = read_json(directory + "/instance.json")
    item = record["instance"]
    try:
        for _ in range(50):
            if child.poll() is not None:
                raise RuntimeError("helper exited before readiness")
            with open(directory + "/ready", "rb") as ready:
                fields = ready.read(128).split()
            if len(fields) == 2 and fields[0] == b"READY":
                port = int(fields[1])
                if not 0 < port <= 65535:
                    raise RuntimeError("invalid helper port")
                fd = identity_fd(item)
                if fd is None:
                    raise RuntimeError("helper exited before readiness")
                os.close(fd)
                with socket.create_connection(("127.0.0.1", port), timeout=1):
                    pass
                item["port"] = port
                atomic(directory + "/instance.json", json.dumps(dict(instance=item, settings=settings)).encode())
                return item
            time.sleep(.1)
        raise RuntimeError("helper readiness timeout")
    except BaseException as error:
        with open(directory + "/log", "rb") as log:
            detail = log.read(4096).decode(errors="replace")
        try:
            stop(item)
        except Exception as cleanup:
            raise RuntimeError(f"{error}: {detail}; cleanup retained: {cleanup}") from error
        raise RuntimeError(f"{error}: {detail}") from error


def project(data):
    for path in (ROOT, ROOT + "/bin", ROOT + "/etc", RUN):
        os.makedirs(path, mode=0o755, exist_ok=True)
        if not stat.S_ISDIR(os.lstat(path).st_mode):
            raise RuntimeError("managed directory is not a directory")
    atomic(ROOT + "/README.md", data["readme"].encode(), 0o644)
    if data["instance"]["service"] == "webdav":
        atomic(ROOT + "/etc/webdav.json", json.dumps(data["settings"]["config"], sort_keys=True).encode())


def main():
    action, data = sys.argv[1], json.loads(sys.argv[2])
    if action == "project":
        project(data)
        return None
    if action == "prepare":
        project(data)
        directory = instance_dir(data["instance"]["instanceId"])
        os.mkdir(directory, 0o700)
        try:
            atomic(directory + "/instance.json", json.dumps(dict(instance=data["instance"], settings=data["settings"])).encode())
        except BaseException:
            # No process has been launched and no binary copied yet.
            for name in ("instance.json.tmp", "instance.json"):
                try:
                    os.unlink(directory + "/" + name)
                except FileNotFoundError:
                    pass
            os.rmdir(directory)
            raise
        return None
    if action == "start":
        return start(data)
    if action == "list":
        return listing(data["service"])
    if action == "stop":
        stop(data)
        return None
    if action == "alive":
        fd = identity_fd(data)
        if fd is None:
            return False
        os.close(fd)
        return True
    raise RuntimeError("unknown managed action")


if __name__ == "__main__":
    try:
        print(json.dumps(main()))
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
