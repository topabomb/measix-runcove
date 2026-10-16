"""One-shot, grow-only tmpfs preparation; no files are installed or deleted."""
import os
import re
import subprocess
import sys


def mount_identity(path):
    def unescape(value):
        return re.sub(r"\\([0-7]{3})", lambda m: chr(int(m[1], 8)), value)

    matches = []
    with open("/proc/self/mountinfo") as stream:
        for line in stream:
            fields, filesystem = line.rstrip().split(" - ", 1)
            fields = fields.split()
            if unescape(fields[4]) == path:
                if filesystem.split()[0] != "tmpfs":
                    raise RuntimeError("configured mount is not tmpfs")
                matches.append(fields[:6])
    if len(matches) != 1:
        raise RuntimeError("configured tmpfs must be one unambiguous mount point")
    return matches[0]


def capacity(path):
    value = os.statvfs(path)
    return value.f_blocks * value.f_frsize


def grow(path, size_mib):
    if not os.path.isabs(path) or os.path.realpath(path) != path or size_mib <= 0:
        raise RuntimeError("invalid tmpfs path or size")
    before_mount = mount_identity(path)
    before = os.stat(path)
    identity = (before.st_dev, before.st_ino, before.st_mode, before.st_uid, before.st_gid)
    target = size_mib * 1024 * 1024
    if capacity(path) >= target:
        return
    with open("/proc/meminfo") as stream:
        memory = next(int(line.split()[1]) * 1024 for line in stream if line.startswith("MemTotal:"))
    if target > memory:
        raise RuntimeError("tmpfs target exceeds actual guest memory")
    subprocess.run(["mount", "-o", f"remount,size={target}", path], check=True, timeout=5)
    after = os.stat(path)
    if mount_identity(path) != before_mount or identity != (
        after.st_dev, after.st_ino, after.st_mode, after.st_uid, after.st_gid
    ) or capacity(path) < target:
        raise RuntimeError("tmpfs identity, permissions or capacity verification failed")


if __name__ == "__main__":
    grow(sys.argv[1], int(sys.argv[2]))
