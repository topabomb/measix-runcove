#!/usr/bin/env python3
"""Verify large reads through the public DAV route and a real guest.

An image uses a disposable container/volume. --container uses a temporary user
on an existing deployment; existing users, tokens and files are never modified.
Credentials are held in memory and omitted from evidence.
"""
import argparse
import concurrent.futures
import http.client
import json
import os
from pathlib import Path
import shlex
import socket
import sys
import time
import tomllib
import uuid
from urllib.parse import urlsplit

from verify import Client, docker

LENGTH = 1 << 32


def incomplete(response, already_read=0):
    received = already_read
    failed = False
    try:
        while block := response.read(1024*1024):
            received += len(block)
    except http.client.IncompleteRead as error:
        received += len(error.partial)
        failed = True
    except ConnectionResetError:
        failed = True
    assert received < LENGTH and (failed or response.length > 0), 'incomplete transfer reported success'
    return received


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument('--image')
    target.add_argument('--container')
    parser.add_argument('--full-read', action='store_true')
    parser.add_argument('--stress', action='store_true', help='Exercise cancelled uploads mixed with normal DAV/MCP traffic')
    parser.add_argument('--identity-only', action='store_true', help='Only boot a real VM and verify image, cached and running executables')
    parser.add_argument('--evidence', type=Path, required=True)
    args = parser.parse_args()
    run = 'davlarge' + uuid.uuid4().hex[:10]
    args.evidence.parent.mkdir(parents=True, exist_ok=True)
    name = args.container or run
    volume = run + '-data'
    created = None
    evidence = {'passed': False, 'checks': []}
    args.evidence.write_text(json.dumps(evidence, indent=2)+'\n')
    volume_created = False
    container_started = False
    previous_token = os.environ.get('AGENT_SPACE_ADMIN_TOKEN')

    def passed(label):
        evidence['checks'].append(label)
        print('PASS: ' + label, flush=True)

    try:
        if args.image:
            config_path = args.evidence.parent.resolve() / (run + '.toml')
            config_path.write_text('''resource_prefix = "davlarge"
[server]
listen = "0.0.0.0:8932"
public_base_url = "http://localhost:8932"
max_sessions = 2
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
io_idle_timeout_secs = 5
''')
            # Random fixture credential goes through an inherited environment,
            # never command arguments or an evidence file.
            os.environ['AGENT_SPACE_ADMIN_TOKEN'] = uuid.uuid4().hex + uuid.uuid4().hex
            docker('volume', 'create', volume)
            volume_created = True
            args.image = docker('image', 'inspect', args.image, '--format', '{{.Id}}').strip()
            common = ['-e', 'AGENT_SPACE_ADMIN_TOKEN', '-v', volume + ':/var/lib/microsandbox',
                      '-v', str(config_path) + ':/etc/agent-space/config.toml:ro']
            docker('run', '--rm', *common, args.image, 'init-state')
            docker('run', '-d', '--name', name, '--device', '/dev/kvm', '--stop-timeout', '60',
                   '-p', '127.0.0.1::8932', '-p', '127.0.0.1::8933', *common, args.image)
            container_started = True
        info = json.loads(docker('inspect', name))[0]
        env = dict(item.split('=', 1) for item in info['Config']['Env'])
        standalone=info['Config']['Labels'].get('io.runcove.target')=='standalone'
        if standalone:
            assert args.container and name.startswith('runcove-qualify-') and env.get('RUNCOVE_PRIVATE_MARKER')=='must-not-reach-payload', 'Standalone file mutation requires this repository disposable qualification fixture'
            assert not args.stress and not args.identity_only, 'MSB-only qualification options do not apply to Standalone'
            config=tomllib.loads(docker('exec',name,'cat','/etc/runcove/config.toml'))
            authority=dav_authority=''
        else:
            config = tomllib.loads(docker('exec', name, 'cat', '/etc/agent-space/config.toml'))
            admin_token = env[config['admin']['token_env']]
            authority = urlsplit(config['server'].get('public_base_url', '')).netloc
            dav_authority = urlsplit(config.get('webdav', {}).get('public_base_url', '')).netloc
        assert config.get('webdav', {}).get('enabled', True)
        port = int(next(p['HostPort'] for p in info['NetworkSettings']['Ports']['8932/tcp'] if p['HostIp'] in ('', '0.0.0.0', '127.0.0.1')))
        dav_port = int(next(p['HostPort'] for p in info['NetworkSettings']['Ports']['8933/tcp'] if p['HostIp'] in ('', '0.0.0.0', '127.0.0.1')))
        authority = authority or f'127.0.0.1:{port}'
        dav_authority = dav_authority or f'127.0.0.1:{dav_port}'
        assert authority != dav_authority
        evidence.update(container=name, image_id=info['Image'],
                        source_sha256=info['Config']['Labels'].get('io.runcove.source-sha256',info['Config']['Labels'].get('io.agent-space.source-sha256')),
                        source_revision=info['Config']['Labels']['org.opencontainers.image.revision'],
                        helper_build_id=docker('exec', name, 'agent-space-webdav', '--build-id').strip(),
                        runtime={'target':'standalone','engine':'process'} if standalone else json.loads(docker('exec', name, 'cat', '/opt/agent-space/runtime.json')))

        def admin(method, path, body=None):
            connection = http.client.HTTPConnection('127.0.0.1', port, timeout=90)
            try:
                connection.request(method, path, None if body is None else json.dumps(body),
                                   {'Host': authority, 'Authorization': 'Bearer ' + admin_token, 'Content-Type': 'application/json'})
                response = connection.getresponse()
                raw = response.read()
                return response.status, json.loads(raw) if raw else None
            finally:
                connection.close()

        dav_root='/dav/' if standalone else f'/u/{run}/dav/'
        if standalone:
            client=Client(port,env[config.get('auth',{}).get('token_env','RUNCOVE_TOKEN')],endpoint='/mcp',authority=authority)
            dav={'token':env[config.get('auth',{}).get('dav_token_env','RUNCOVE_DAV_TOKEN')]}
        else:
            deadline = time.monotonic() + 120
            while True:
                try:
                    if admin('GET', '/admin/v1/status')[0] == 200:
                        break
                except (OSError, http.client.HTTPException):
                    pass
                assert time.monotonic() < deadline, 'service startup timeout'
                time.sleep(1)
            status, user = admin('POST', '/admin/v1/users', {'username': run})
            assert status == 201
            created = user
            space = created['agentSpaceId']
            status, dav = admin('POST', f'/admin/v1/users/{run}/dav-token', {'agentSpaceId': space})
            assert status == 200
            client = Client(port, created['token'], username=run, authority=authority)

        def guest(code):
            return client.tool('bash', cmd='python3 -c ' + shlex.quote(code))

        guest('''import os
with open('/workspace/large.bin', 'wb') as f:
    f.truncate(4294967296)
    f.seek(2147483640); f.write(bytes(range(1,17)))
    f.seek(4294967232); f.write(bytes(range(128,192)))
assert os.stat('/workspace/large.bin').st_blocks * 512 < 1024*1024
open('/workspace/empty.bin', 'wb').close()
with open('/workspace/small.bin', 'wb') as f: f.write(b'new')
''')

        if not standalone:
            hashes = docker('exec', name, 'sh', '-c', '''set -e
    sha256sum /usr/local/bin/msb /var/lib/microsandbox/.microsandbox/bin/msb
    for executable in /proc/[0-9]*/exe; do
        target=$(readlink "$executable" 2>/dev/null) || continue
        case "$target" in */msb) sha256sum "$executable";; esac
    done''')
            actual = [line.split()[0] for line in hashes.splitlines()]
            assert len(actual) >= 3 and all(value == evidence['runtime']['msb_sha256'] for value in actual)
            evidence['runtime_executables_verified'] = len(actual)
            passed('image, cached and running real VM executables match the runtime manifest')
            if args.identity_only:
                evidence['identity_only'] = True
                evidence['passed'] = True
                return

        def request(method='GET', headers=None, path='large.bin'):
            connection = http.client.HTTPConnection('127.0.0.1', dav_port, timeout=30)
            connection.connect()
            connection.sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 64 * 1024)
            connection.request(method, dav_root+path, headers={
                'Host': dav_authority, 'Authorization': 'Bearer ' + dav['token'], **(headers or {})})
            return connection, connection.getresponse()

        conn, response = request('HEAD')
        assert response.status == 200 and response.getheader('Content-Length') == str(LENGTH)
        etag = response.getheader('ETag')
        assert etag and response.read() == b''
        conn.close()
        passed('4 GiB sparse file HEAD, length and ETag through public DAV')
        for value, start, expected in [
            ('2147483640-2147483655', 2147483640, bytes(range(1, 17))),
            ('4294967295-4294967295', LENGTH-1, bytes([191])),
            ('4294967232-4294967295', LENGTH-64, bytes(range(128, 192))),
            ('4294967232-', LENGTH-64, bytes(range(128, 192))),
            ('-64', LENGTH-64, bytes(range(128, 192))),
            ('4294967232-18446744073709551615', LENGTH-64, bytes(range(128, 192))),
        ]:
            conn, response = request(headers={'Range': 'bytes='+value, 'If-Match': etag})
            assert response.status == 206
            assert response.getheader('ETag') == etag
            assert response.getheader('Content-Length') == str(len(expected))
            assert response.getheader('Content-Range') == f'bytes {start}-{start+len(expected)-1}/{LENGTH}'
            assert response.read() == expected
            conn.close()
        passed('cross-2-GiB, final byte/tail, open/suffix/huge-end ranges and current If-Match')
        conn, response = request(headers={'Range': f'bytes={LENGTH}-'})
        assert response.status == 416 and response.getheader('Content-Range') == f'bytes */{LENGTH}'
        response.read(); conn.close()
        passed('unsatisfiable range returns 416 and complete representation length')

        for path, value, expected in [
            ('empty.bin', 'bytes=-1', b''),
            ('empty.bin', 'bytes=0-', b''),
            ('small.bin', 'bytes=+1-2', b'new'),
            ('small.bin', 'bytes=0-+2', b'new'),
            ('small.bin', 'bytes=-+2', b'new'),
        ]:
            conn, response = request(path=path, headers={'Range': value})
            assert response.status == 200 and response.getheader('Content-Range') is None
            assert response.getheader('Content-Length') == str(len(expected))
            assert response.read() == expected
            conn.close()
        passed('empty file and malformed decimal ranges use ordinary GET semantics')

        if args.full_read:
            conn, response = request(headers={'If-Match': etag})
            assert response.status == 200
            total = 0
            markers = [(2147483640, bytes(range(1, 17))), (LENGTH-64, bytes(range(128, 192)))]
            begun = time.monotonic()
            while block := response.read(1024*1024):
                expected = bytearray(len(block))
                for offset, data in markers:
                    lo, hi = max(total, offset), min(total+len(block), offset+len(data))
                    if hi > lo: expected[lo-total:hi-total] = data[lo-offset:hi-offset]
                assert block == expected, f'content mismatch at {total}'
                total += len(block)
            conn.close()
            assert total == LENGTH
            evidence['full_read'] = {'bytes': total, 'seconds': time.monotonic()-begun}
            passed('full 4 GiB HTTP read checked byte-for-byte with bounded client memory')

        if args.stress:
            identity_code = "import glob,json;from pathlib import Path;print('IDENTITY='+json.dumps({'boot_id':Path('/proc/sys/kernel/random/boot_id').read_text().strip(),'instances':{p:json.load(open(p))['instance'] for p in glob.glob('/tmp/agent-space/*/instance.json')}},sort_keys=True))"

            def identity():
                # Bash also appends timing metadata; compare only process identity.
                lines = [line.removeprefix('IDENTITY=') for line in guest(identity_code).splitlines() if line.startswith('IDENTITY=')]
                assert len(lines) == 1, 'missing or ambiguous identity output'
                value = json.loads(lines[0])
                assert value['boot_id'] and value['instances'], 'empty runtime identity'
                return value

            identity_before = identity()
            evidence['cancellation_identity'] = {'before': identity_before}
            guest("open('/workspace/upload-cancel.bin','wb').write(b'original')")
            retries = 0
            recovery_delays = []

            def recovered_request(method='GET', path='large.bin'):
                nonlocal retries
                begun = time.monotonic()
                deadline = begun + 10
                while True:
                    conn, response = request(method, path=path)
                    if response.status != 503:
                        elapsed = time.monotonic() - begun
                        if elapsed > 10:
                            response.close(); conn.close()
                            raise AssertionError('DAV recovery exceeded ten seconds')
                        recovery_delays.append(elapsed)
                        return conn, response
                    response.read(); conn.close()
                    assert time.monotonic() < deadline, 'DAV stayed unavailable after cancellation'
                    retries += 1
                    time.sleep(.1)

            def rejected_upload(_):
                # The guest rejects replacing the root directory while H2G
                # body records are still queued. In 0.7.2 this can disconnect
                # the cached agent client without stopping its VM.
                conn = http.client.HTTPConnection('127.0.0.1', dav_port, timeout=30)
                try:
                    conn.request('PUT', dav_root, body=b'x' * (8*1024*1024), headers={
                        'Host': dav_authority, 'Authorization': 'Bearer ' + dav['token']})
                    response = conn.getresponse()
                    status = response.status
                    response.read()
                    assert status in (403, 405, 409, 502, 503), ('root overwrite unexpectedly succeeded', status)
                    return status
                except (OSError, http.client.HTTPException):
                    return 'connection-ended'
                finally:
                    conn.close()

            rejected = []
            for _ in range(3):
                with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
                    rejected.extend(pool.map(rejected_upload, range(8)))
                conn, response = recovered_request('HEAD')
                assert response.status == 200
                response.read(); conn.close()

            for _ in range(60):
                # Leave a request body incomplete after giving the guest time
                # to consume real H2G records, then cancel a G2H download too.
                upload = http.client.HTTPConnection('127.0.0.1', dav_port, timeout=30)
                upload.connect()
                upload.putrequest('PUT', dav_root+'upload-cancel.bin', skip_host=True)
                upload.putheader('Host', dav_authority)
                upload.putheader('Authorization', 'Bearer ' + dav['token'])
                upload.putheader('Content-Length', str(8*1024*1024))
                upload.endheaders()
                upload.send(b'x' * 65536)
                time.sleep(.05)
                upload.close()
                conn, response = recovered_request()
                assert response.status == 200
                response.read(1024)
                response.close(); conn.close()
                # A closed cached client can return one retryable 503. The
                # following request must recover promptly without restarting VM.
                conn, response = recovered_request('HEAD', path='upload-cancel.bin')
                assert response.status == 200
                response.read(); conn.close()
            conn, response = recovered_request(path='upload-cancel.bin')
            assert response.status == 200 and response.read() == b'original'
            conn.close()
            identity_after = identity()
            evidence['cancellation_identity']['after'] = identity_after
            assert identity_after == identity_before, 'VM or owned helper restarted during cancellation stress'
            assert 'MCP_HEALTHY' in guest("print('MCP_HEALTHY')")
            evidence['mixed_cancellation'] = {'upload_cancellations': 60, 'download_cancellations': 60,
                                            'early_rejected_uploads': rejected,
                                            'max_probe_seconds': max(recovery_delays),
                                            'retryable_responses': retries, 'same_helper_identity': True}
            status, detail = admin('GET', f'/admin/v1/users/{run}')
            assert status == 200
            runtime_log = docker('exec', name, 'cat',
                f'/var/lib/microsandbox/.microsandbox/sandboxes/{detail["sandboxName"]}/logs/runtime.log')
            evidence['mixed_cancellation']['relay_client_disconnects'] = runtime_log.count('client bulk record has no matching active operation')
            evidence['mixed_cancellation']['merge_failures'] = runtime_log.count('cross-lane merge failed')
            assert evidence['mixed_cancellation']['merge_failures'] == 0
            passed('60 cancelled uploads mixed with downloads retain original file, VM/helper identity and healthy MCP/DAV')

        if args.image:
            conn, response = request(headers={'Range': 'bytes=0-41943039'})
            assert response.status == 206
            begun, total = time.monotonic(), 0
            while block := response.read(65536):
                assert not any(block)
                total += len(block)
                time.sleep(.05)
            conn.close()
            assert total == 40*1024*1024 and time.monotonic()-begun > 30
            passed('paced real download lasts over 30 seconds and multiple 5-second idle intervals')

        # Find the exact helper that owns this test file. Stop all its threads,
        # confirm T state, truncate the same inode, then resume in finally. This
        # leaves already-buffered real bytes valid but guarantees future EOF.
        conn, response = request(headers={'Range': f'bytes=0-{LENGTH-1}'})
        assert response.status == 206 and response.read(1024)
        guest('''import os,signal,time
pids=[]
for p in os.listdir('/proc'):
    if not p.isdigit() or int(p)==os.getpid(): continue
    try:
        if any(os.readlink(f'/proc/{p}/fd/{f}')=='/workspace/large.bin' for f in os.listdir(f'/proc/{p}/fd')): pids.append(int(p))
    except (FileNotFoundError,PermissionError): pass
assert len(pids)==1, pids
p=pids[0]
assert 'webdav' in open(f'/proc/{p}/cmdline','rb').read().decode()
os.kill(p,signal.SIGSTOP)
try:
    deadline=time.monotonic()+3
    while not all(open(f'/proc/{p}/task/{t}/stat').read().split(') ',1)[1].startswith('T') for t in os.listdir(f'/proc/{p}/task')):
        assert time.monotonic()<deadline
        time.sleep(.01)
    os.truncate('/workspace/large.bin',0)
finally: os.kill(p,signal.SIGCONT)
''')
        received = incomplete(response, 1024)
        conn.close()
        evidence['truncated_bytes_received'] = received
        passed('synchronized real guest truncate yields incomplete HTTP body, not padded success')
        diagnostic = guest("import glob,os; print('\\n'.join(open(p).read() for p in glob.glob('/tmp/agent-space/*/log') if os.access(p,os.R_OK)))" )
        assert 'DAV premature EOF' in diagnostic and 'remaining=' in diagnostic
        passed('premature EOF diagnostic is present in bounded guest log')
        conn, response = request(headers={'Range': 'bytes=0-0', 'If-Match': etag})
        assert response.status == 412 and response.read() == b''
        conn.close()
        passed('old If-Match rejects modified file with 412 without replacement content')
        guest("with open('/workspace/large.bin','wb') as f: f.truncate(4294967296)")

        # Allow socket queues to fill; no client consumption after headers.
        # Existing production idle may be long: exercise this in the candidate
        # fixture (5 seconds), not by changing deployed configuration.
        if args.image:
            conn, response = request()
            time.sleep(8)
            incomplete(response)
            conn.close()
            passed('unconsumed real download expires at configured idle timeout')

        for _ in range(80):
            conn, response = request()
            assert response.status == 200
            response.read(1024)
            response.close(); conn.close()
        # Actual guest descriptor count provides evidence beyond lease release.
        counts = guest('''import os,time
deadline=time.monotonic()+3
while True:
    count=0
    for p in os.listdir('/proc'):
        if not p.isdigit(): continue
        try:
            for f in os.listdir(f'/proc/{p}/fd'):
                try: count += os.readlink(f'/proc/{p}/fd/{f}')=='/workspace/large.bin'
                except FileNotFoundError: pass
        except (FileNotFoundError,PermissionError): pass
    if count==0 or time.monotonic()>=deadline: break
    time.sleep(.05)
print('OPEN_FILES='+str(count))
''')
        assert 'OPEN_FILES=0' in counts
        passed('80 cancelled real downloads release guest file descriptors and connection slots')
        if not standalone:
            conn, response = request()
            response.read(1024)
            status, _ = admin('DELETE', f'/admin/v1/users/{run}/dav-token?agentSpaceId={space}')
            assert status == 204
            incomplete(response, 1024)
            conn.close()
            passed('DAV revocation interrupts real in-flight download')
            status, dav = admin('POST', f'/admin/v1/users/{run}/dav-token', {'agentSpaceId': space})
            assert status == 200
            conn, response = request()
            response.read(1024)
            status, _ = admin('PATCH', f'/admin/v1/users/{run}', {'agentSpaceId': space, 'enabled': False})
            assert status == 200
            incomplete(response, 1024)
            conn.close()
            passed('space disable interrupts real in-flight download')
        evidence['passed'] = True
    finally:
        cleanup_errors = []

        def cleanup(label, action):
            try:
                action()
            except Exception as error:
                # Do not serialize exception text: subprocess errors can include credentials.
                cleanup_errors.append(label + ': ' + type(error).__name__)

        def delete_user():
            status, _ = admin('DELETE', f'/admin/v1/users/{run}?agentSpaceId={created["agentSpaceId"]}')
            assert status in (200, 202, 204)
            deadline = time.monotonic()+90
            while admin('GET', f'/admin/v1/users/{run}')[0] != 404:
                assert time.monotonic()<deadline, 'temporary user cleanup timeout'
                time.sleep(.5)

        if created:
            cleanup('temporary user', delete_user)
        if container_started:
            cleanup('fixture container', lambda: docker('rm', '-f', '-v', name))
        if volume_created:
            cleanup('fixture volume', lambda: docker('volume', 'rm', volume))
        if args.image:
            if previous_token is None:
                os.environ.pop('AGENT_SPACE_ADMIN_TOKEN', None)
            else:
                os.environ['AGENT_SPACE_ADMIN_TOKEN'] = previous_token
        evidence['cleanup_passed']=not cleanup_errors
        if cleanup_errors:
            evidence['passed'] = False
            evidence['cleanup_errors'] = cleanup_errors
        args.evidence.write_text(json.dumps(evidence, indent=2)+'\n')
        if cleanup_errors and sys.exception() is None:
            raise RuntimeError('verification cleanup failed; see evidence')


if __name__ == '__main__':
    main()
