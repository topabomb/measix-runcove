"""Foreground execution owner. Embedded by Rust, never installed as a daemon.

Drain child pipes continuously into bounded tails. Only the final bounded JSON
crosses the SDK event queue. Normal completion preserves background services;
deadline/cancellation stops this owner's descendants with PID identity checks.
"""
import ctypes
import json
import os
import pathlib
import selectors
import signal
import subprocess
import sys
import time


def identity(pid):
    fields = pathlib.Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    if fields[0] == "Z":
        raise ProcessLookupError(pid)
    return pid, fields[19]


def descendants():
    parents = {}
    for p in pathlib.Path('/proc').glob('[0-9]*'):
        try:
            parents[int(p.name)] = int(p.joinpath('stat').read_text().rsplit(')', 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            pass
    owned = {os.getpid()}
    while True:
        additions = {p for p, parent in parents.items() if parent in owned} - owned
        if not additions:
            break
        owned.update(additions)
    result = []
    for pid in owned - {os.getpid()}:
        try:
            result.append(identity(pid))
        except (OSError, ValueError):
            pass
    return result


def cleanup():
    for action, seconds in [(signal.SIGTERM, 2), (signal.SIGKILL, 1)]:
        until = time.monotonic() + seconds
        while True:
            remaining = descendants()
            if not remaining:
                return True
            for record in remaining:
                try:
                    if identity(record[0]) == record:
                        os.kill(record[0], action)
                except (ProcessLookupError, FileNotFoundError):
                    pass
            if time.monotonic() >= until:
                break
            time.sleep(.02)
    return not descendants()


def run(spec):
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0):
        raise OSError('PR_SET_CHILD_SUBREAPER failed')
    interrupted = [False]
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_: interrupted.__setitem__(0, True))
    deadline = time.monotonic() + spec['timeout_secs'] if spec['timeout_secs'] else float('inf')
    process = subprocess.Popen([spec['program'], *spec['args']], stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
                               **({"user": spec["uid"], "group": spec["uid"], "extra_groups": []} if "uid" in spec else {}))
    selector = selectors.DefaultSelector()
    tails = {name: bytearray() for name in ('stdout', 'stderr')}
    truncated = False
    for name, pipe in [('stdout', process.stdout), ('stderr', process.stderr)]:
        os.set_blocking(pipe.fileno(), False)
        selector.register(pipe, selectors.EVENT_READ, name)
    exited_at = None
    timed_out = False
    clean = True
    try:
        while True:
            now = time.monotonic()
            if interrupted[0] or now >= deadline:
                timed_out = not interrupted[0]
                clean = cleanup()
                break
            if process.poll() is not None:
                exited_at = exited_at or now
                if not selector.get_map() or now - exited_at >= .25:
                    break
            for key, _ in selector.select(.02):
                chunk = os.read(key.fileobj.fileno(), 16384)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                tail = tails[key.data]
                tail.extend(chunk)
                if len(tail) > spec['max_bytes']:
                    del tail[:-spec['max_bytes']]
                    truncated = True
    finally:
        selector.close()
        process.stdout.close()
        process.stderr.close()
        if process.poll() is None or spec.get("cleanup_on_exit", False):
            clean = cleanup() and clean
        if process.poll() is not None:
            process.wait()
    output = {}
    for name, tail in tails.items():
        text = tail.decode('utf-8', errors='replace')
        lines = text.splitlines(keepends=True)
        if len(lines) > spec['max_lines']:
            text = ''.join(lines[-spec['max_lines']:])
            truncated = True
        encoded = text.encode('utf-8')
        if len(encoded) > spec['max_bytes']:
            text = encoded[-spec['max_bytes']:].decode('utf-8', errors='ignore')
            truncated = True
        output[name] = text
    return dict(**output, exit_code=process.returncode if process.returncode is not None else -1,
                truncated=truncated, timed_out=timed_out, cancelled=interrupted[0], cleanup_confirmed=clean)


if __name__ == '__main__':
    print(json.dumps(run(json.loads(sys.argv[1])), ensure_ascii=False))
