#!/usr/bin/env python3
"""Verify native LAN uploads with concurrent queries using only a temporary user."""
import argparse
import base64
from collections import Counter
from datetime import datetime, timezone
import hashlib
import http.client
import json
import os
from pathlib import Path
import subprocess
import threading
import time
import tomllib
from urllib.parse import urlsplit
import uuid

from verify import Client, docker

LENGTH = 540_079_753
BLOCK = bytes(range(256)) * 4096
FIXTURE_CONFIG = '''resource_prefix = "davupload"
[server]
listen = "0.0.0.0:8932"
public_base_url = "http://localhost:8932"
max_sessions = 2
[engine]
type = "msb"
[workspace]
image = "nikolaik/python-nodejs:python3.12-nodejs22-slim"
cpus = 1
memory_mib = 512
workspace_size_mib = 1024
tmpfs_size_mib = 64
shell = "/bin/bash"
[lifecycle]
session_timeout_secs = 900
idle_timeout_secs = 3600
check_interval_secs = 1
[state]
dir = "/var/lib/microsandbox/agent-space-state"
[admin]
token_env = "AGENT_SPACE_ADMIN_TOKEN"
[webdav]
enabled = true
public_base_url = "http://localhost:8933"
startup_timeout_secs = 60
io_idle_timeout_secs = 30
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument('--image')
    target.add_argument('--container')
    parser.add_argument('--lan-host', required=True)
    parser.add_argument('--native-curl', default='/mnt/c/Windows/System32/curl.exe')
    parser.add_argument('--attempts', type=int, default=6)
    parser.add_argument('--evidence', type=Path, required=True)
    args = parser.parse_args()
    assert args.attempts > 0
    args.evidence.parent.mkdir(parents=True, exist_ok=True)
    name = args.container or ('davupload' + uuid.uuid4().hex[:10])
    volume = name + '-data'
    username = 'davupload' + uuid.uuid4().hex[:10]
    config_path = args.evidence.parent.resolve() / (username + '.toml')
    payload = args.evidence.parent.resolve() / (username + '.bin')
    report = {'passed': False, 'started_utc': datetime.now(timezone.utc).isoformat(),
              'bytes_per_upload': LENGTH, 'attempts': [], 'checks': []}
    created = None
    volume_created = container_created = False
    previous_token = os.environ.get('AGENT_SPACE_ADMIN_TOKEN')
    try:
        if args.image:
            config_path.write_text(FIXTURE_CONFIG)
            os.environ['AGENT_SPACE_ADMIN_TOKEN'] = uuid.uuid4().hex + uuid.uuid4().hex
            docker('volume', 'create', volume)
            volume_created = True
            mounts = ['-e', 'AGENT_SPACE_ADMIN_TOKEN', '-v', volume + ':/var/lib/microsandbox',
                      '-v', str(config_path) + ':/etc/agent-space/config.toml:ro']
            docker('run', '--rm', *mounts, args.image, 'init-state')
            docker('run', '-d', '--name', name, '--device', '/dev/kvm', '--stop-timeout', '60',
                   '-p', '0.0.0.0::8932', '-p', '0.0.0.0::8933', *mounts, args.image)
            container_created = True
        info = json.loads(docker('inspect', name))[0]
        env = dict(item.split('=', 1) for item in info['Config']['Env'])
        config = tomllib.loads(docker('exec', name, 'cat', '/etc/agent-space/config.toml'))
        admin_token = env[config['admin']['token_env']]
        authority = urlsplit(config['server'].get('public_base_url', '')).netloc
        dav_authority = urlsplit(config.get('webdav', {}).get('public_base_url', '')).netloc
        bindings = info['NetworkSettings']['Ports']['8932/tcp']
        ports = [int(p['HostPort']) for p in bindings if p['HostIp'] in ('', '0.0.0.0', '127.0.0.1')]
        assert ports
        core_port = urlsplit(config['server'].get('public_base_url', '')).port
        core_port = core_port if core_port in ports else ports[0]
        dav_bindings = info['NetworkSettings']['Ports']['8933/tcp']
        dav_port = int(next(p['HostPort'] for p in dav_bindings if p['HostIp'] in ('', '0.0.0.0', '127.0.0.1')))
        authority = authority or f'127.0.0.1:{core_port}'
        dav_authority = dav_authority or f'127.0.0.1:{dav_port}'
        assert authority != dav_authority
        report.update(container=name, image_id=info['Image'],
                      source_sha256=info['Config']['Labels']['io.agent-space.source-sha256'],
                      runtime=json.loads(docker('exec', name, 'cat', '/opt/agent-space/runtime.json')))

        def request(method, path, body=None, headers=None, port=core_port, host=authority, timeout=30):
            conn = http.client.HTTPConnection('127.0.0.1', port, timeout=timeout)
            try:
                conn.request(method, path, body, {'Host': host, **(headers or {})})
                response = conn.getresponse()
                return response.status, response.read(), {k.lower(): v for k, v in response.getheaders()}
            finally:
                conn.close()

        def admin(method, path, body=None):
            status, raw, _ = request(method, path, None if body is None else json.dumps(body),
                {'Authorization': 'Bearer ' + admin_token, 'Content-Type': 'application/json'})
            return status, json.loads(raw) if raw else None

        deadline = time.monotonic() + 120
        while True:
            try:
                if admin('GET', '/admin/v1/status')[0] == 200:
                    break
            except (OSError, http.client.HTTPException):
                pass
            assert time.monotonic() < deadline, 'service startup timeout'
            time.sleep(1)
        status, created = admin('POST', '/admin/v1/users', {'username': username})
        assert status == 201
        space = created['agentSpaceId']
        status, dav = admin('POST', f'/admin/v1/users/{username}/dav-token', {'agentSpaceId': space})
        assert status == 200
        auth = 'Basic ' + base64.b64encode((username + ':' + dav['token']).encode()).decode()
        dav_root = f'/u/{username}/dav/'
        path = dav_root + 'upload.bin'

        def dav_request(method, target=dav_root, headers=None, timeout=30):
            return request(method, target, headers={'Authorization': auth, **(headers or {})},
                           port=dav_port, host=dav_authority, timeout=timeout)

        assert dav_request('PROPFIND', headers={'Depth': '0'}, timeout=90)[0] == 207
        client = Client(core_port, created['token'], username=username, authority=authority)
        identity_command = "cat /proc/sys/kernel/random/boot_id; python3 -c \"import glob,json; print([json.load(open(p))['instance'] for p in sorted(glob.glob('/tmp/agent-space/*/instance.json'))])\""
        identity = client.tool('bash', cmd=identity_command)
        expected = hashlib.sha256()
        with payload.open('wb') as file:
            remaining = LENGTH
            while remaining:
                block = BLOCK[:min(remaining, len(BLOCK))]
                file.write(block)
                expected.update(block)
                remaining -= len(block)
        windows_payload = subprocess.check_output(['wslpath', '-w', str(payload)], text=True).strip()
        report['expected_sha256'] = expected.hexdigest()
        for attempt in range(args.attempts):
            stop = threading.Event()
            statuses = Counter()
            status_lock = threading.Lock()

            def probe():
                while not stop.is_set():
                    try:
                        key = str(dav_request('PROPFIND', headers={'Depth': '0'}, timeout=10)[0])
                    except (OSError, http.client.HTTPException) as error:
                        key = type(error).__name__
                    with status_lock:
                        statuses[key] += 1
                    stop.wait(.01)

            threads = [threading.Thread(target=probe) for _ in range(3)]
            for thread in threads:
                thread.start()
            try:
                # Credentials travel over stdin, never process arguments or evidence.
                curl_config = f'url = "http://{args.lan_host}:{dav_port}{path}"\nheader = "Host: {dav_authority}"\nheader = "Authorization: {auth}"\n'
                result = subprocess.run([args.native_curl, '--noproxy', '*', '--silent', '--show-error',
                    '--max-time', '90', '--config', '-', '--upload-file', windows_payload,
                    '--output', 'NUL', '--write-out',
                    '{"status":%{http_code},"bytes_sent":%{size_upload},"seconds":%{time_total}}'],
                    input=curl_config, capture_output=True, text=True, timeout=95)
            finally:
                stop.set()
                for thread in threads:
                    thread.join(12)
                assert not any(thread.is_alive() for thread in threads), 'directory probes did not stop'
            detail = json.loads(result.stdout)
            detail.update(attempt=attempt + 1, directory_statuses=dict(statuses),
                          curl_exit_code=result.returncode)
            report['attempts'].append(detail)
            assert result.returncode == 0 and detail['status'] in (201, 204), detail
            assert detail['bytes_sent'] == LENGTH, detail
            assert statuses['207'] > 0 and set(statuses) == {'207'}, detail
            status, _, headers = dav_request('HEAD', path)
            assert status == 200 and int(headers['content-length']) == LENGTH
            conn = http.client.HTTPConnection('127.0.0.1', dav_port, timeout=30)
            try:
                conn.request('GET', path, headers={'Host': dav_authority, 'Authorization': auth})
                response = conn.getresponse()
                assert response.status == 200
                digest = hashlib.sha256()
                received = 0
                while block := response.read(1024 * 1024):
                    digest.update(block)
                    received += len(block)
                assert received == LENGTH and digest.hexdigest() == expected.hexdigest()
            finally:
                conn.close()
            detail.update(full_get_bytes=received, sha256_match=True)
            for start in (len(BLOCK) - 5, LENGTH - 64):
                status, raw, _ = dav_request('GET', path, {'Range': f'bytes={start}-{start+63}'})
                assert status == 206 and raw == bytes((start+i) % 256 for i in range(64))
            # Atomic overwrite temporarily needs both old and new full files.
            # A 1 GiB workspace cannot hold two 515 MiB copies. Reclaim this
            # verified fixture before the next capacity-contained upload.
            assert dav_request('DELETE', path)[0] == 204
            print(f'PASS upload {attempt+1}: {LENGTH} bytes, SHA-256, ranges, {statuses["207"]} concurrent directory requests', flush=True)
        assert client.tool('bash', cmd=identity_command) == identity, 'VM/helper identity changed'
        report['checks'].append('same VM/helper and healthy MCP after native LAN uploads')
        status, user = admin('GET', f'/admin/v1/users/{username}')
        assert status == 200
        runtime_log = docker('exec', name, 'cat',
            f'/var/lib/microsandbox/.microsandbox/sandboxes/{user["sandboxName"]}/logs/runtime.log')
        errors = [line for line in runtime_log.splitlines() if any(t in line for t in
            ('frame too short', 'frame too large', 'cross-lane merge failed', 'agent relay error'))]
        report['relay_errors'] = errors
        assert not errors, 'relay errors during valid uploads'
        hashes = docker('exec', name, 'sh', '-c', '''sha256sum /usr/local/bin/msb /var/lib/microsandbox/.microsandbox/bin/msb
for executable in /proc/[0-9]*/exe; do
 target=$(readlink "$executable" 2>/dev/null) || continue
 case "$target" in */msb) sha256sum "$executable";; esac
done''')
        actual = [line.split()[0] for line in hashes.splitlines()]
        assert len(actual) >= 3 and set(actual) == {report['runtime']['msb_sha256']}
        report['checks'].append('image, cached and running VM runtime hashes match manifest')
        report['passed'] = True
    finally:
        cleanup_errors = []
        if created:
            try:
                status, user = admin('GET', f'/admin/v1/users/{username}')
                if status == 200:
                    log = docker('exec', name, 'cat',
                        f'/var/lib/microsandbox/.microsandbox/sandboxes/{user["sandboxName"]}/logs/runtime.log')
                    report['relay_errors'] = [line for line in log.splitlines() if any(t in line for t in
                        ('frame too short', 'frame too large', 'cross-lane merge failed', 'agent relay error'))]
                    report['client_disconnect_count'] = sum('client disconnected' in line
                        for line in log.splitlines())
            except Exception:
                report['runtime_log_available'] = False
            try:
                status, _ = admin('DELETE', f'/admin/v1/users/{username}?agentSpaceId={created["agentSpaceId"]}')
                assert status in (200, 202, 204)
                deadline = time.monotonic() + 90
                while admin('GET', f'/admin/v1/users/{username}')[0] != 404:
                    assert time.monotonic() < deadline
                    time.sleep(.2)
            except Exception as error:
                cleanup_errors.append('temporary user: ' + type(error).__name__)
        for label, action in [('fixture container', lambda: docker('rm', '-f', '-v', name) if container_created else None),
                              ('fixture volume', lambda: docker('volume', 'rm', volume) if volume_created else None),
                              ('payload', lambda: payload.unlink(missing_ok=True)),
                              ('fixture config', lambda: config_path.unlink(missing_ok=True))]:
            try:
                action()
            except Exception as error:
                cleanup_errors.append(label + ': ' + type(error).__name__)
        if args.image:
            if previous_token is None:
                os.environ.pop('AGENT_SPACE_ADMIN_TOKEN', None)
            else:
                os.environ['AGENT_SPACE_ADMIN_TOKEN'] = previous_token
        report['cleanup_errors'] = cleanup_errors
        report['passed'] = report['passed'] and not cleanup_errors
        report['finished_utc'] = datetime.now(timezone.utc).isoformat()
        args.evidence.write_text(json.dumps(report, indent=2) + '\n')
        if cleanup_errors:
            raise RuntimeError('verification cleanup failed; see evidence')


if __name__ == '__main__':
    main()
