"""Restart only this qualification fixture; never stop a user web server."""
import os
import signal
import subprocess
import time
import urllib.request
from pathlib import Path

script = "/workspace/qualification/server.cjs"
for entry in Path("/proc").iterdir():
    if not entry.name.isdigit():
        continue
    try:
        fd = os.pidfd_open(int(entry.name))
    except (ProcessLookupError, PermissionError):
        continue
    try:
        args = entry.joinpath("cmdline").read_bytes().split(b"\0")
        if Path(os.fsdecode(args[0])).name != "node" or args[1:2] != [script.encode()]:
            continue
        signal.pidfd_send_signal(fd, signal.SIGTERM)
    except (FileNotFoundError, ProcessLookupError):
        pass
    finally:
        os.close(fd)

# The known fixture obeys TERM. A occupied port is a test failure, not a reason
# to kill another process or falsely reuse an old site.
time.sleep(0.1)
with open("/workspace/qualification/server.log", "ab") as log:
    process = subprocess.Popen(
        ["node", script], stdin=subprocess.DEVNULL, stdout=log, stderr=log,
        start_new_session=True,
    )
for _ in range(80):
    if process.poll() is not None:
        raise RuntimeError("Qualification fixture failed to bind; inspect its log")
    try:
        with urllib.request.urlopen("http://127.0.0.1:8124/", timeout=1) as response:
            if b'id="generate"' in response.read():
                break
    except OSError:
        pass
    time.sleep(0.1)
else:
    raise RuntimeError("Qualification fixture did not become ready")
print("Qualification fixture ready")
