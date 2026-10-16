#!/bin/bash
set -euo pipefail
# Shared helper executables must remain traversable by the data-plane UID.
# Secret/identity files use explicit 0600 modes in their owning code.
umask 022
# Initialize only an empty mount root. Existing user data ownership is never
# recursively rewritten; imports must use an explicit, verified mapping.
python3 -I -c '
import os,pathlib,stat,subprocess
for name in ("/opt/agent-space", "/opt/agent-space/bin", "/opt/agent-space/etc", "/tmp/agent-space"):
    reserved=pathlib.Path(name)
    reserved.mkdir(mode=0o755,exist_ok=True)
    if not stat.S_ISDIR(reserved.lstat().st_mode):
        raise SystemExit("reserved runtime path must be a directory: "+name)
    os.chown(reserved,0,0)
    reserved.chmod(0o755)
lock=os.open("/tmp/agent-space/file-operations.lock",os.O_CREAT|os.O_RDWR|os.O_NOFOLLOW,0o600)
if not stat.S_ISREG(os.fstat(lock).st_mode):
    raise SystemExit("invalid file operation lock")
os.fchown(lock,1001,1001)
os.fchmod(lock,0o600)
os.close(lock)
p=pathlib.Path("/workspace")
if not stat.S_ISDIR(p.lstat().st_mode):
    raise SystemExit("/workspace must be a directory mount")
if not any(p.iterdir()) and p.stat().st_uid==0:
    os.chown(p,1001,1001)
internal=p/".agent-space"
if not internal.exists():
    internal.mkdir(mode=0o700)
    os.chown(internal,1001,1001)
test=subprocess.run(["python3","-I","-c","import os;raise SystemExit(not os.access(\"/workspace\",os.W_OK|os.X_OK))"],user=1001,group=1001,extra_groups=[])
if test.returncode:
    raise SystemExit("/workspace is not writable by UID/GID1001; preserve source data and prepare target ownership explicitly")
'
exec /usr/local/bin/runcove-standalone "$@"
