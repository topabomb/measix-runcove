#!/usr/bin/env python3
"""Verify the deployed static-user upgrade and isolated full-volume rollback.

Uses only disposable Docker volumes, accounts and data. Keeps evidence/configs
(mode 0700 directory) but removes owned containers and volumes after success.
On failure retains volumes for diagnosis; never operates a production volume.
"""
import argparse
import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import time
import uuid
import xml.etree.ElementTree as ET
from urllib.parse import urlsplit

from verify import Client, docker

PROJECT = Path(__file__).resolve().parents[1]
STATE = '/var/lib/microsandbox/agent-space-state/state.json'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    parser.add_argument('--static-image', required=True)
    parser.add_argument('--rclone', help='Also run the real rclone/FUSE client suite against the migrated image')
    args = parser.parse_args()
    run = 'as-upgrade-' + uuid.uuid4().hex[:10]
    root = PROJECT / 'target/docker-verification' / run
    root.mkdir(parents=True, mode=0o700)
    os.environ['AGENT_SPACE_ADMIN_TOKEN'] = secrets.token_hex(32)
    containers, volumes, evidence = [], [], []
    config_ports = {}
    dav_ports = {}
    client_evidence = None
    success = False
    new_id = docker('image', 'inspect', args.image, '--format', '{{.Id}}').strip()

    def config(name, dynamic, users=False, dav=False):
        path = root / (name + '.toml')
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1', 0))
            dav_port = reservation.getsockname()[1]
        if dav:
            config_ports[str(path)] = dav_port
        primary = 'public_base_url = "http://localhost:8932"\n' if dynamic else ''

        path.write_text(f'''resource_prefix = "migration"
[server]
listen = "0.0.0.0:8932"
{primary}max_sessions = 2
[engine]
type = "msb"
[workspace]
image = "nikolaik/python-nodejs:python3.12-nodejs22-slim"
cpus = 1
memory_mib = 256
workspace_size_mib = 256
tmpfs_size_mib = 64
shell = "/bin/bash"
exec_timeout_secs = 30
max_output_lines = 2000
max_output_bytes = 51200
[lifecycle]
session_timeout_secs = 900
idle_timeout_secs = 3600
check_interval_secs = 0
[logging]
level = "info"
''' + ('''[state]
dir = "/var/lib/microsandbox/agent-space-state"
[admin]
token_env = "AGENT_SPACE_ADMIN_TOKEN"
''' if dynamic else '') + ('''[users.verify]
token = "migration-fixture-legacy-token"
workspace_name = "migration-workspace"
''' if users else '') + (f'''[web_publish]
enabled = true
[webdav]
enabled = true
public_base_url = "http://127.0.0.1:{dav_port}"
''' if dav else ('[webdav]\nenabled = false\n' if dynamic else '')))
        path.chmod(0o600)
        return path

    def common(volume, cfg):
        return ['-e', 'AGENT_SPACE_ADMIN_TOKEN', '-v', volume + ':/var/lib/microsandbox',
                '-v', str(cfg) + ':/etc/agent-space/config.toml:ro']

    def command(image, volume, cfg, *cmd):
        return docker('run', '--rm', *common(volume, cfg), image, *cmd)

    def launch(image, volume, cfg, name):
        name = run + '-' + name
        ports = ['-p', '127.0.0.1::8932']
        if str(cfg) in config_ports:
            ports += ['-p', f"127.0.0.1:{config_ports[str(cfg)]}:8933"]
        docker('run', '-d', '--name', name, '--device', '/dev/kvm', '--stop-timeout', '60',
               *ports, *common(volume, cfg), image)
        containers.append(name)
        port = ready(name)
        if str(cfg) in config_ports:
            dav_ports[port] = config_ports[str(cfg)]
        return name, port

    def ready(name):
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            info = json.loads(docker('inspect', name))[0]
            if not info['State']['Running']:
                raise AssertionError('Container stopped: ' + name + ' (inspect retained logs)')
            if 'listening on' in docker('logs', '--since', info['State']['StartedAt'], name):
                return int(info['NetworkSettings']['Ports']['8932/tcp'][0]['HostPort'])
            time.sleep(.5)
        raise AssertionError('Container startup timed out: ' + name)

    def stop(name):
        docker('stop', '--time', '60', name, timeout=90)
        state = json.loads(docker('inspect', name))[0]['State']
        assert state['ExitCode'] == 0 and not state['OOMKilled'], state

    def request(port, method, path, body=None, headers=None):
        url = urlsplit(path)
        target = dav_ports.get(port, port) if url.path.startswith('/u/') and '/dav' in url.path else port
        connection = http.client.HTTPConnection('127.0.0.1', target, timeout=90)
        try:
            outgoing = dict(headers or {})
            outgoing.setdefault('Host', url.netloc or 'localhost:8932')
            connection.request(method, (url.path or '/') + ('?' + url.query if url.query else ''),
                               body=body, headers=outgoing)
            response = connection.getresponse()
            return response.status, response.read(), dict(response.getheaders())
        finally:
            connection.close()

    def verify_dav_contract(port, path, auth, client):
        def dav(method, name, body=None, **headers):
            return request(port, method, path + name, body, dict(auth, **headers))
        status, _, headers = dav('PUT', 'dated-v2', b'new-version', **{'X-OC-Mtime': '946684800'})
        assert status == 201 and {k.lower(): v for k, v in headers.items()}['x-oc-mtime'] == 'accepted'
        assert '946684800' in client.tool('bash', cmd='stat -c %Y /workspace/dated-v2')
        # Root spellings must be rejected before recursive DAV work touches children.
        for suffix in ('', '/', '//'):
            for method in ('DELETE', 'MOVE'):
                assert dav(method, suffix, Destination=path + 'root-moved')[0] == 403
                assert dav('GET', 'dated-v2')[1] == b'new-version'
            for method in ('COPY', 'MOVE'):
                assert dav(method, 'report/', Destination=path + suffix)[0] == 403
                assert dav('GET', 'dated-v2')[1] == b'new-version'
                assert dav('GET', 'report/index.html')[1] == b'migration-preview'
        xml = b'<D:propertyupdate xmlns:D="DAV:"><D:set><D:prop><D:getlastmodified>Sun, 01 Jan 2023 00:00:00 GMT</D:getlastmodified></D:prop></D:set></D:propertyupdate>'
        status, body, _ = dav('PROPPATCH', 'dated-v2', xml)
        assert status == 207 and b'200 OK' in body
        status, _, headers = dav('HEAD', 'dated-v2')
        assert status == 200 and {k.lower(): v for k, v in headers.items()}['last-modified'] == 'Sun, 01 Jan 2023 00:00:00 GMT'
        xml = b'<D:propfind xmlns:D="DAV:"><D:prop><D:quota-used-bytes/><D:quota-available-bytes/></D:prop></D:propfind>'
        status, body, _ = dav('PROPFIND', '', xml, Depth='1')
        assert status == 207
        values = [(int(item.find('.//{DAV:}quota-used-bytes').text), int(item.find('.//{DAV:}quota-available-bytes').text)) for item in ET.fromstring(body)]
        assert values and values[0][1] > 0 and all(v == values[0] for v in values)
        assert dav('PUT', 'overwritten-v2', b'old')[0] == 201
        assert dav('COPY', 'dated-v2', Destination=path + 'overwritten-v2', Overwrite='F')[0] == 412
        assert dav('GET', 'overwritten-v2')[1] == b'old'
        assert dav('COPY', 'dated-v2', Destination=path + 'overwritten-v2')[0] == 204
        assert dav('GET', 'overwritten-v2')[1] == b'new-version'
        assert dav('MOVE', 'overwritten-v2', Destination=path + 'dated-v2')[0] == 204
        assert dav('GET', 'overwritten-v2')[0] == 404
        # Confirm the first PUT has entered guest staging before exercising a sibling.
        slow = http.client.HTTPConnection('127.0.0.1', dav_ports.get(port, port), timeout=15)
        try:
            slow.putrequest('PUT', path + 'slow-v2', skip_host=True)
            for key, value in dict(auth, **{'Content-Length': '6'}).items(): slow.putheader(key, value)
            slow.endheaders(); slow.send(b'abc')
            deadline = time.monotonic() + 10
            while True:
                count = client.tool('bash', cmd="find /workspace/.agent-space-upload -type f ! -name owner ! -name boot-id | wc -l")
                if count.splitlines() and count.splitlines()[0].strip() == '1': break
                assert time.monotonic() < deadline, 'Upload did not enter guest staging'
                time.sleep(.05)
            assert dav('PUT', 'unrelated-v2', b'independent')[0] == 201
            slow.send(b'def'); result = slow.getresponse()
            assert result.status == 201; result.read()
            assert dav('GET', 'slow-v2')[1] == b'abcdef'
        finally:
            slow.close()
        for name in ('dated-v2', 'unrelated-v2', 'slow-v2'):
            assert dav('DELETE', name)[0] == 204

    def admin(port, method, path, value=None):
        status, body, _ = request(port, method, '/admin/v1/users' + path,
                                  None if value is None else json.dumps(value),
                                  {'Host': 'localhost:8932', 'Content-Type': 'application/json',
                                   'Authorization': 'Bearer ' + os.environ['AGENT_SPACE_ADMIN_TOKEN']})
        assert status in (200, 201, 202), (method, path, status)
        return json.loads(body) if body else None

    def snapshot(volume):
        return json.loads(docker('run', '--rm', '--entrypoint', '/bin/cat',
                                '-v', volume + ':/var/lib/microsandbox:ro', args.image, STATE))

    def new_volume(suffix):
        name = run + '-' + suffix
        docker('volume', 'create', name)
        volumes.append(name)
        return name

    try:
        mode, old_image = 'static', args.static_image
        print('START: static-user upgrade', flush=True)
        volume = new_volume(mode)
        old_cfg = config(mode + '-old', dynamic=False, users=True)
        old, port = launch(old_image, volume, old_cfg, mode + '-old')
        old_token = 'migration-fixture-legacy-token'
        old_endpoint = '/mcp'
        client = Client(port, old_token, username='verify', endpoint=old_endpoint,
                        protocol_version='2025-03-26', expected_version='2024-11-05')
        assert {tool['name'] for tool in client.request('tools/list')['tools']} == {'bash', 'read', 'write', 'edit'}
        assert request(port, 'GET', '/admin/v1/users')[0] == 404
        assert request(port, 'GET', '/u/verify/dav/')[0] == 404
        client.tool('bash', command='printf root-before > /root/migration-marker; '
                    'printf workspace-before > /workspace/migration-marker; '
                    'mkdir -p /workspace/report; printf migration-preview > /workspace/report/index.html')
        stop(old)
        backup = mode + '-pre-upgrade.tar.gz'
        docker('run', '--rm', '--entrypoint', '/bin/tar', '-v', volume + ':/data:ro',
               '-v', str(root) + ':/backup', args.image, '--sparse', '-czf', '/backup/' + backup, '-C', '/data', '.', timeout=300)
        subprocess.run(['tar', 'tzf', str(root / backup)], check=True, stdout=subprocess.DEVNULL)
        upgraded_cfg = config(mode + '-new', dynamic=True, dav=True)
        importing = config(mode + '-import', dynamic=True, users=True)
        command(args.image, volume, importing, 'init-state')
        command(args.image, volume, importing, 'import-users', '--dry-run')
        command(args.image, volume, importing, 'import-users', '--apply')
        assert snapshot(volume)['schemaVersion'] == 3
        upgraded, port = launch(args.image, volume, upgraded_cfg, mode + '-upgraded')
        assert docker('exec', upgraded, 'cat', '/var/lib/microsandbox/.microsandbox/lib/.libkrunfw.version').strip() == '0.7.2'
        user = admin(port, 'GET', '/verify')
        space = user['agentSpaceId']
        assert not user['hasCredential']
        token = admin(port, 'POST', '/verify/token', {'agentSpaceId': space})['token']
        status, _, _ = request(port, 'POST', '/u/verify/mcp', b'{}',
            {'Host': 'localhost:8932', 'Authorization': 'Bearer ' + old_token, 'Content-Type': 'application/json'})
        assert status == 401
        client = Client(port, token, username='verify')
        text = client.tool('bash', cmd='cat /root/migration-marker /workspace/migration-marker')
        assert 'root-before' in text and 'workspace-before' in text
        published = json.loads(client.tool('web_publish', action='publish', name='report', directory='/workspace/report'))
        status, body, _ = request(port, 'GET', published['url'], headers={'X-Publish-Key':published['password']})
        assert status == 200 and body == b'migration-preview', (status, body)
        # Exercise caller-supplied first issuance, retry identity, replacement,
        # and generated credentials on the deployed static upgrade path.
        custom = secrets.token_urlsafe(32)
        payload = {'agentSpaceId': space, 'token': custom}
        key = admin(port, 'POST', '/verify/dav-token', payload)
        assert key['token'] == custom
        assert admin(port, 'POST', '/verify/dav-token', payload) == key
        generated = admin(port, 'POST', '/verify/dav-token', {'agentSpaceId': space})
        assert generated['keyId'] != key['keyId']
        assert request(port, 'GET', '/u/verify/dav/migration-marker', headers={
            'Host': f'127.0.0.1:{port}', 'Authorization': 'Bearer ' + custom})[0] == 401
        assert request(port, 'GET', '/u/verify/dav/migration-marker', headers={
            'Host': f'127.0.0.1:{port}', 'Authorization': 'Bearer ' + generated['token']})[0] == 200
        key = admin(port, 'POST', '/verify/dav-token', payload)
        assert request(port, 'GET', '/u/verify/dav/migration-marker', headers={
            'Host': f'127.0.0.1:{port}', 'Authorization': 'Bearer ' + generated['token']})[0] == 401
        auth = {'Host': f'127.0.0.1:{port}', 'Authorization': 'Basic ' +
                base64.b64encode(('verify:' + key['token']).encode()).decode()}
        dav_path = '/u/verify/dav/'
        status, body, _ = request(port, 'GET', dav_path + 'migration-marker', headers=auth)
        assert status == 200 and body == b'workspace-before'
        status, _, _ = request(port, 'PUT', dav_path + 'after-dav.txt', b'dav-after', auth)
        assert status == 201
        text = client.tool('bash', cmd='cat /workspace/after-dav.txt; '
            'test -x /opt/agent-space/bin/agent-space-static-http && '
            'test -x /opt/agent-space/bin/agent-space-webdav && '
            'test -f /opt/agent-space/README.md && echo managed-layout-ok')
        assert 'dav-after' in text and 'managed-layout-ok' in text
        verify_dav_contract(port, dav_path, auth, client)
        stop(upgraded)
        docker('start', upgraded)
        port = ready(upgraded)
        status, body, _ = request(port, 'GET', dav_path + 'after-dav.txt', headers=auth)
        assert status == 200 and body == b'dav-after'
        client = Client(port, token, username='verify')
        text = client.tool('bash', cmd='cat /root/migration-marker /workspace/migration-marker')
        assert 'root-before' in text and 'workspace-before' in text
        if args.rclone:
            client_path = root / 'clients.json'
            subprocess.run(['python3', str(PROJECT / 'docker/verify-webdav-clients.py'),
                '--url', key['davUrl'], '--username', 'verify', '--rclone', args.rclone,
                '--evidence', str(client_path)],
                env=dict(os.environ, AGENT_SPACE_DAV_TOKEN=key['token']), check=True)
            client_evidence = json.loads(client_path.read_text())
            assert client_evidence['passed']
            client_evidence['image_id'] = new_id
            client_path.write_text(json.dumps(client_evidence, indent=2) + '\n')
        stop(upgraded)
        rollback_volume = new_volume(mode + '-rollback')
        docker('run', '--rm', '--entrypoint', '/bin/tar', '-v', rollback_volume + ':/data',
               '-v', str(root) + ':/backup:ro', args.image, 'xzf', '/backup/' + backup, '-C', '/data', timeout=300)
        rollback, port = launch(old_image, rollback_volume, old_cfg, mode + '-rollback')
        client = Client(port, old_token, username='verify', endpoint=old_endpoint,
                        protocol_version='2025-03-26', expected_version='2024-11-05')
        text = client.tool('bash', command='cat /root/migration-marker /workspace/migration-marker; '
                           'test ! -e /workspace/after-dav.txt && echo backup-boundary-ok')
        assert all(value in text for value in ('root-before', 'workspace-before', 'backup-boundary-ok'))
        stop(rollback)
        with (root / backup).open('rb') as stream:
            backup_sha = hashlib.file_digest(stream, 'sha256').hexdigest()
        evidence.append({'mode': mode, 'old_image': old_image,
            'old_image_id': docker('image', 'inspect', old_image, '--format', '{{.Id}}').strip(),
            'backup_sha256': backup_sha, 'passed': True,
            'checks': ['released_static_four_tools', 'both_disks_preserved', 'static_user_import', 'credential_contract',
                       'managed_layout', 'dav_read_write', 'mtime_and_quota', 'atomic_file_overwrite', 'unrelated_path_concurrency', 'root_alias_protection', 'custom_dav_credentials', 'restart_persistence', 'isolated_backup_rollback']})
        print('PASS: ' + mode + ' migration, DAV, restart and isolated rollback', flush=True)
        success = True
    finally:
        cleanup_errors = []
        for name in reversed(containers):
            try:
                (root / (name + '.log')).write_text(docker('logs', name))
                docker('rm', '-f', '-v', name)
            except subprocess.CalledProcessError:
                cleanup_errors.append(name)
        if success:
            for volume in volumes:
                try:
                    docker('volume', 'rm', volume)
                except subprocess.CalledProcessError:
                    cleanup_errors.append(volume)
        result = {'passed': success and not cleanup_errors, 'image': args.image, 'image_id': new_id,
                  'upgrades': evidence, 'client_evidence': client_evidence, 'cleanup_errors': cleanup_errors,
                  'retained_volumes': [] if success else volumes}
        (root / 'upgrade.json').write_text(json.dumps(result, indent=2) + '\n')
        print('Upgrade evidence: ' + str(root), flush=True)
        if cleanup_errors:
            raise RuntimeError('Cleanup incomplete; see upgrade.json')


if __name__ == '__main__':
    main()
