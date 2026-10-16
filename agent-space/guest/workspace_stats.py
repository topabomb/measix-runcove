"""One-shot, read-only /workspace statistics. Invoked only on a running VM."""
import hashlib
import json
import os

# microsandbox 0.7.2 derives the virtio-blk serial from the guest mount path.
expected_serial = "workspace_" + hashlib.sha256(b"/workspace").hexdigest()[:8]
fd = os.open("/workspace", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
try:
    device = os.fstat(fd).st_dev
    device_id = f"{os.major(device)}:{os.minor(device)}"
    with open(f"/sys/dev/block/{device_id}/serial", encoding="ascii") as stream:
        if stream.read().strip() != expected_serial:
            raise RuntimeError("workspace device mismatch")
    with open("/proc/self/mountinfo", encoding="utf-8") as stream:
        mounts = [line.split() for line in stream]
    # Require the actual root of this block filesystem, not a bind of a subdir.
    if not any(row[2] == device_id and row[3] == "/" and row[4] == "/workspace" for row in mounts):
        raise RuntimeError("workspace mount mismatch")
    stat = os.fstatvfs(fd)
    if not (0 <= stat.f_bavail <= stat.f_bfree <= stat.f_blocks) or stat.f_frsize <= 0:
        raise RuntimeError("invalid workspace statistics")
    print(json.dumps({
        "usedBytes": (stat.f_blocks - stat.f_bfree) * stat.f_frsize,
        "availableBytes": stat.f_bavail * stat.f_frsize,
    }))
finally:
    os.close(fd)
