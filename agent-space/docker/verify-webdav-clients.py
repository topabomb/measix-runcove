#!/usr/bin/env python3
"""Exercise a real DAV endpoint with rclone, including an actual FUSE mount.

Uses a unique remote test directory and only deletes that directory. Supply a
DAV token through an environment variable; credentials never appear in argv or
the evidence. This runs separately from cargo tests and requires rclone/FUSE.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url', required=True)
    parser.add_argument('--username', required=True)
    parser.add_argument('--token-env', default='AGENT_SPACE_DAV_TOKEN')
    parser.add_argument('--rclone', default='rclone')
    parser.add_argument('--evidence', type=Path, required=True)
    args = parser.parse_args()
    token = os.environ[args.token_env]
    work = Path(tempfile.mkdtemp(prefix='agent-space-dav-client-'))
    remote_dir = 'client-verification-' + uuid.uuid4().hex
    remote = 'verify:' + remote_dir
    mountpoint = work / 'mount'
    mountpoint.mkdir()
    env = dict(os.environ)
    env.pop(args.token_env, None)
    password = subprocess.check_output([args.rclone, 'obscure', '-'], input=token+'\n', text=True).strip()
    env.update(RCLONE_CONFIG_VERIFY_TYPE='webdav', RCLONE_CONFIG_VERIFY_VENDOR='other',
               RCLONE_CONFIG_VERIFY_URL=args.url, RCLONE_CONFIG_VERIFY_USER=args.username,
               RCLONE_CONFIG_VERIFY_PASS=password)
    common = [args.rclone, '--config', '/dev/null', '--retries', '1', '--low-level-retries', '1', '--timeout', '30s']
    def run(*command, timeout=180):
        result = subprocess.run([*common, *map(str, command)], env=env, capture_output=True, text=True, timeout=timeout)
        if result.returncode:
            raise RuntimeError(f'rclone {command[0]} failed: {result.stderr[-3000:]}')
        return result.stdout
    evidence = {'url': args.url, 'client': run('version').splitlines()[0], 'checks': [], 'passed': False}
    mounted = None
    try:
        local = work / 'source'; local.mkdir()
        (local/'空 格 #%.txt').write_text('material\n', encoding='utf-8')
        (local/'empty').write_bytes(b'')
        (local/'nested').mkdir(); (local/'nested'/'result.txt').write_text('result\n')
        large = local/'large.bin'
        block = bytes(range(256))*4096
        with large.open('wb') as stream:
            for _ in range(64): stream.write(block)
        digest = hashlib.sha256(large.read_bytes()).hexdigest()
        run('copy', local, remote)
        listed = json.loads(run('lsjson', remote, '--recursive'))
        assert {v['Path'] for v in listed} >= {'空 格 #%.txt','empty','nested/result.txt','large.bin'}
        evidence['checks'].append('recursive browse, mkdir, Unicode/spaces/reserved characters, empty file upload')
        downloaded = work/'download.bin'
        run('copyto', remote+'/large.bin', downloaded)
        assert hashlib.sha256(downloaded.read_bytes()).hexdigest() == digest
        evidence.update(bytes=large.stat().st_size, sha256=digest)
        evidence['checks'].append('64 MiB upload/download SHA-256')
        run('copyto', remote+'/nested/result.txt', remote+'/copied.txt')
        run('moveto', remote+'/copied.txt', remote+'/renamed.txt')
        assert run('cat', remote+'/renamed.txt') == 'result\n'
        (local/'nested'/'result.txt').write_text('replacement\n')
        run('copyto', local/'nested'/'result.txt', remote+'/renamed.txt', '--ignore-times')
        assert run('cat', remote+'/renamed.txt') == 'replacement\n'
        run('deletefile', remote+'/renamed.txt')
        evidence['checks'].append('remote copy, rename, overwrite and delete')
        log = (work/'mount.log').open('w')
        mounted = subprocess.Popen([*common, 'mount', remote, str(mountpoint), '--vfs-cache-mode', 'writes',
                                    '--vfs-write-back', '0s', '--cache-dir', str(work/'cache'), '--dir-cache-time', '1s'],
                                   env=env, stdout=log, stderr=log)
        log.close()
        deadline = time.monotonic()+20
        while not os.path.ismount(mountpoint):
            if mounted.poll() is not None or time.monotonic()>deadline:
                raise RuntimeError('FUSE mount failed: '+(work/'mount.log').read_text()[-3000:])
            time.sleep(.1)
        assert (mountpoint/'空 格 #%.txt').read_text() == 'material\n'
        with (mountpoint/'large.bin').open('rb') as stream:
            stream.seek(32*1024*1024+250)
            assert stream.read(10) == block[250:260]
        (mountpoint/'mounted').mkdir()
        (mountpoint/'mounted'/'new.txt').write_text('written through mount\n')
        deadline = time.monotonic()+30
        while True:
            try:
                if run('cat', remote+'/mounted/new.txt') == 'written through mount\n': break
            except RuntimeError:
                pass
            if time.monotonic()>deadline: raise RuntimeError('mount writeback did not reach server')
            time.sleep(.1)
        (mountpoint/'mounted'/'new.txt').rename(mountpoint/'mounted'/'moved.txt')
        assert (mountpoint/'mounted'/'moved.txt').read_text() == 'written through mount\n'
        (mountpoint/'mounted'/'moved.txt').unlink()
        (mountpoint/'mounted').rmdir()
        evidence['checks'].append('real FUSE mount: browse, seek/range read, mkdir, writeback, rename, delete')
        evidence['passed'] = True
    finally:
        try:
            if os.path.ismount(mountpoint):
                subprocess.run(['fusermount3', '-u', str(mountpoint)], timeout=15, check=True)
            if mounted is not None:
                if mounted.poll() is None: mounted.terminate()
                mounted.wait(timeout=15)
            if os.path.ismount(mountpoint):
                raise RuntimeError(f'mount still active; retained local directory {work}')
            run('purge', remote)
        except BaseException:
            evidence['passed'] = False
            raise
        finally:
            args.evidence.write_text(json.dumps(evidence, ensure_ascii=False, indent=2)+'\n')
            if evidence['passed']: shutil.rmtree(work)
            else: print(f'Client diagnostics retained: {work}')
    print(json.dumps(evidence, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
