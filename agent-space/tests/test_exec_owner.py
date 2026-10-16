import json, os, subprocess, time, unittest
from pathlib import Path
SCRIPT = Path(__file__).parents[1] / "guest/exec.py"

def run(command, timeout=2, max_bytes=4096, max_lines=20):
    spec = dict(program="bash", args=["-c", command], timeout_secs=timeout, max_bytes=max_bytes, max_lines=max_lines)
    p = subprocess.run(["python3", "-I", str(SCRIPT), json.dumps(spec)], capture_output=True, timeout=timeout+6)
    assert p.returncode == 0, p.stderr
    return json.loads(p.stdout)

class OwnerTests(unittest.TestCase):
    def test_output_is_drained_and_bounded(self):
        r = run("head -c 20000000 /dev/zero; printf final")
        self.assertTrue(r["truncated"])
        self.assertLessEqual(len(r["stdout"].encode()), 4096)
        self.assertTrue(r["stdout"].endswith("final"))
    def test_utf8_and_invalid_bytes(self):
        r = run("python3 -c 'import sys; sys.stdout.buffer.write(bytes([255])*5000)'", max_bytes=1024)
        self.assertLessEqual(len(r["stdout"].encode()), 1024)
        self.assertIn("�", r["stdout"])
    def test_timeout_preserves_partial_and_cleans_tree(self):
        r = run("echo partial; sleep 100 & echo $!; wait", timeout=.2)
        self.assertTrue(r["timed_out"])
        self.assertTrue(r["cleanup_confirmed"])
        pid = int(r["stdout"].splitlines()[1])
        self.assertFalse(Path(f"/proc/{pid}").exists() and Path(f"/proc/{pid}/stat").read_text().rsplit(")",1)[1].split()[0] != "Z")
        self.assertIn("partial", r["stdout"])
    def test_background_fd_does_not_hold_response(self):
        started = time.monotonic()
        r = run("sleep 100 & echo $!")
        pid = int(r["stdout"])
        try:
            self.assertLess(time.monotonic()-started, 1.5)
            self.assertEqual(r["exit_code"], 0)
            self.assertTrue(Path(f"/proc/{pid}").exists())
        finally:
            try: os.kill(pid, 9)
            except ProcessLookupError: pass
    def test_lines_keep_tail(self):
        r=run("seq 1 100",max_lines=3)
        self.assertEqual(r["stdout"], "98\n99\n100\n")

if __name__ == "__main__": unittest.main()
