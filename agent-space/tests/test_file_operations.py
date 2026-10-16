"""Real processes exercise fixed MCP file commands and kernel path locks."""
import concurrent.futures
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

SOURCE = Path(__file__).resolve().parents[1] / "guest/file.py"
spec = importlib.util.spec_from_file_location("file_ops", SOURCE)
ops = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ops)


def command(operation, path, **kw):
    return [sys.executable, "-I", str(SOURCE), json.dumps(dict(operation=operation, path=str(path), **kw))]


class Files(unittest.TestCase):
    def test_directory_and_missing_file_are_distinct(self):
        with tempfile.TemporaryDirectory() as d:
            result = subprocess.run(command("read", d, offset=1, limit=5), capture_output=True, text=True)
            self.assertEqual(result.stdout.strip(), "NOT_A_FILE")
            self.assertNotEqual(result.returncode, 0)
            result = subprocess.run(command("read", Path(d)/"missing", offset=1, limit=5), capture_output=True, text=True)
            self.assertEqual(result.stdout.strip(), "FILE_NOT_FOUND")
            result = subprocess.run(command("edit", d, old_text="a", new_text="b"), capture_output=True, text=True)
            self.assertIn("directory", result.stderr)
            self.assertNotIn("Traceback", result.stderr)

    def test_parallel_edits_and_symlink_aliases_do_not_lose_updates(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "file.txt"
            link = Path(d) / "alias.txt"
            path.write_text("".join(f"<old{i}>" for i in range(20)))
            path.chmod(0o640)
            link.symlink_to(path)
            def edit(i):
                result = subprocess.run(command("edit", link if i % 2 else path, old_text=f"<old{i}>", new_text=f"<new{i}>"), capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
            with concurrent.futures.ThreadPoolExecutor(12) as pool:
                list(pool.map(edit, range(20)))
            self.assertEqual(path.read_text(), "".join(f"<new{i}>" for i in range(20)))
            self.assertTrue(link.is_symlink())
            self.assertEqual(path.stat().st_mode & 0o777, 0o640)
            self.assertEqual(list(Path(d).glob(".agent-space-write-*")), [])

    def test_blocked_path_does_not_block_other_path_and_cancel_releases_lock(self):
        with tempfile.TemporaryDirectory() as d:
            a, b = Path(d) / "a", Path(d) / "b"
            a.write_text("original")
            Path("/tmp/agent-space").mkdir(exist_ok=True)
            with open("/tmp/agent-space/file-operations.lock", "a+b") as lock:
                offset = int.from_bytes(hashlib.sha256(os.fsencode(a.resolve())).digest()[:8], "big") & ((1 << 63) - 2)
                fcntl.lockf(lock, fcntl.LOCK_EX, 1, offset)
                blocked = subprocess.Popen(command("write", a, content="wrong"), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                try:
                    time.sleep(.15)
                    self.assertIsNone(blocked.poll())
                    other = subprocess.run(command("write", b, content="parallel"), capture_output=True, timeout=2)
                    self.assertEqual(other.returncode, 0, other.stderr)
                    blocked.terminate()
                    blocked.communicate(timeout=2)
                    self.assertEqual(a.read_text(), "original")
                finally:
                    if blocked.poll() is None:
                        blocked.kill()
                        blocked.communicate()
            done = subprocess.run(command("write", a, content="after"), capture_output=True, timeout=2)
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual(a.read_text(), "after")

    def test_replace_failure_preserves_old_file_and_removes_temporary(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "file"
            p.write_text("original")
            with mock.patch.object(ops.os, "replace", side_effect=OSError("injected failure")):
                with self.assertRaises(OSError):
                    ops.atomic_write(str(p), "replacement")
            self.assertEqual(p.read_text(), "original")
            self.assertEqual(sorted(x.name for x in Path(d).iterdir()), ["file"])

    def test_unicode_pagination_crlf_and_failed_edit_preserve_content(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "file"
            result = subprocess.run(command("write", p, content="first\r\n中文\r\nlast\r\n"), capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            result = subprocess.run(command("read", p, offset=2, limit=1), capture_output=True, text=True)
            self.assertEqual(result.stdout.strip(), "2\t中文")
            result = subprocess.run(command("edit", p, old_text="中文", new_text="你好"), capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(p.read_bytes(), "first\r\n你好\r\nlast\r\n".encode())
            before = p.read_bytes()
            result = subprocess.run(command("edit", p, old_text="missing", new_text="bad"), capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(p.read_bytes(), before)


if __name__ == "__main__":
    unittest.main(verbosity=2)
