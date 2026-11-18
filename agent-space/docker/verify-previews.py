#!/usr/bin/env python3
"""Public media-preview qualification using a new container and disposable volume.

No existing deployment, user, credential or data volume is accepted as input.
The report binds the exact image and helper; credentials are kept in memory.
"""
import argparse
import concurrent.futures
import hashlib
import http.client
import io
import inspect
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import shutil
import socket
import subprocess
import struct
import sys
import threading
import time
import uuid
from urllib.parse import urlencode

from verify import Client
from release_identity import source_digest

ROOT = Path(__file__).resolve().parents[1]
MAX_OUTPUT = 1024 * 1024
OWNER_LABEL = 'io.runcove.preview-qualification'
VIDEO_FIXTURES = [('clip.mp4', 'libx264', 'mp4'), ('clip.m4v', 'libx264', 'mp4'),
                  ('clip.mov', 'libx264', 'mov'), ('clip.webm', 'libvpx', 'webm'),
                  ('clip.ogv', 'libtheora', 'ogg')]
VIDEO_PREVIEW_NAMES = [video[0] for video in VIDEO_FIXTURES] + ['cover-first.mp4', 'alpha-vp8.webm', 'alpha-vp9.webm']


def original_process_present(stat, starttime):
    # A zombie still owns its PID/starttime and must be waited for. cmdline is
    # empty for zombies, so it cannot establish that cleanup has completed.
    return stat.rsplit(')', 1)[1].split()[19] == starttime


class FixtureOwner:
    """Register intent before Docker calls and reconcile using ownership labels."""
    def __init__(self, command, identity):
        self.command, self.identity = command, identity
        self.containers, self.volumes = [], []

    def info(self, kind, name):
        try:
            return json.loads(self.command(kind, 'inspect', name))[0]
        except RuntimeError as error:
            if ('no such '+kind) in str(error).lower():
                return None
            raise

    def owned(self, kind, name):
        info = self.info(kind, name)
        if info is not None:
            labels = info['Config']['Labels'] if kind == 'container' else info['Labels']
            if not labels or labels.get(OWNER_LABEL) != self.identity:
                raise RuntimeError('fixture ownership does not match: '+kind)
        return info

    def volume(self, name):
        self.volumes.append(name)
        result = self.command('volume', 'create', '--label', OWNER_LABEL+'='+self.identity, name)
        assert self.owned('volume', name) is not None
        return result

    def container(self, name, arguments):
        self.containers.append(name)
        result = self.command('create', '--name', name, '--label', OWNER_LABEL+'='+self.identity, *arguments)
        assert self.owned('container', name) is not None
        return result

    def cleanup(self):
        errors = []
        for container in reversed(self.containers):
            try:
                info = self.owned('container', container)
                if info is None:
                    continue
                try:
                    if info['State']['Running']:
                        self.command('stop', '--time', '60', container, timeout=90)
                    state = self.owned('container', container)['State']
                    if state['ExitCode'] != 0 or state['OOMKilled']:
                        errors.append('fixture container unclean exit')
                finally:
                    # The label has already established ownership. Removing a
                    # stopped fixture must still happen if exit checks fail.
                    try:
                        self.command('rm', '-f', '-v', container)
                    except Exception:
                        if self.info('container', container) is not None:
                            raise
            except Exception as error:
                errors.append('fixture container: '+type(error).__name__)
        for volume in reversed(self.volumes):
            try:
                if self.owned('volume', volume) is None:
                    continue
                try:
                    self.command('volume', 'rm', volume)
                except Exception:
                    if self.info('volume', volume) is not None:
                        raise
            except Exception as error:
                errors.append('fixture volume: '+type(error).__name__)
        return errors


def inspect_preview(headers, data, source, size):
    """Validate the transferred representation, including its derived identity."""
    from PIL import Image
    assert headers.get('content-type') == 'image/webp', 'wrong preview content type'
    assert headers.get('preview-source-etag') == source, 'wrong source version'
    assert headers.get('etag') == '"' + hashlib.sha256(data).hexdigest() + '"', 'wrong output ETag'
    assert headers.get('content-length') == str(len(data)), 'wrong preview length'
    assert len(data) <= MAX_OUTPUT and data[:4] == b'RIFF' and data[8:12] == b'WEBP'
    assert headers.get('cache-control') == 'no-store'
    assert headers.get('x-content-type-options') == 'nosniff'
    assert headers.get('content-disposition') == 'inline; filename="thumbnail.webp"'
    image = Image.open(io.BytesIO(data))
    assert image.format == 'WEBP' and getattr(image, 'n_frames', 1) == 1
    assert min(image.size) >= 1 and max(image.size) <= size
    return image.convert('RGBA')


def inspect_problem(result, expected_status, expected_code):
    status, headers, body = result
    assert status == expected_status, (status, body)
    assert headers.get('content-type') == 'application/problem+json', (status, headers)
    assert headers.get('cache-control') == 'no-store'
    problem = json.loads(body)
    assert problem.get('status') == status and problem.get('code') == expected_code, problem
    return headers


def inspect_body_guard_wire(wire, method):
    header, separator, body = wire.partition(b'\r\n\r\n')
    assert separator and len(wire) <= 16 * 1024, 'invalid body-guard HTTP response'
    lines = header.split(b'\r\n')
    status = int(lines[0].split()[1])
    headers = {name.decode('ascii').lower(): value.strip().decode('ascii')
               for name, value in (line.split(b':', 1) for line in lines[1:])}
    if method == 'HEAD':
        # HTTP clients suppress HEAD bodies themselves. Inspect the raw wire so
        # an erroneous JSON body cannot be mistaken for compliant HEAD output.
        assert status == 400 and not body, (status, body)
        assert headers.get('content-type') == 'application/problem+json'
        assert headers.get('cache-control') == 'no-store'
    else:
        class BufferedSocket:
            def makefile(self, *_):
                return io.BytesIO(wire)
        response = http.client.HTTPResponse(BufferedSocket(), method=method)
        response.begin()
        inspect_problem((response.status, headers, response.read()), 400, 'invalid_preview_request')


def request_body_guard(port, authority, path, secret, method):
    request = (method+' '+path+' HTTP/1.1\r\nHost: '+authority+'\r\nAuthorization: Bearer '+secret+
               '\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx').encode('ascii')
    with socket.create_connection(('127.0.0.1', port), timeout=15) as connection:
        connection.sendall(request)
        wire = bytearray()
        while chunk := connection.recv(8192):
            wire.extend(chunk)
            assert len(wire) <= 16 * 1024, 'body-guard response exceeds bound'
    inspect_body_guard_wire(bytes(wire), method)


def fixtures(output):
    from PIL import Image
    Image.new('RGBA', (320, 80), (240, 10, 20, 90)).save(output / 'alpha.png')
    # Used only for deterministic stopped-worker cancellation, never for a
    # timing-dependent claim that a naturally slow request was interrupted.
    Image.frombytes('RGBA', (1024, 1024), os.urandom(1024 * 1024 * 4)).save(output / 'cancel.png')
    for name, codec, container in VIDEO_FIXTURES:
        subprocess.run(['/usr/bin/ffmpeg', '-v', 'error', '-f', 'lavfi', '-i',
                        'color=red:s=320x180:r=10:d=1', '-c:v', codec, '-threads', '1',
                        '-pix_fmt', 'yuv420p', '-f', container, str(output / name)], check=True, timeout=30)
    for name, codec in [('alpha-vp8.webm', 'libvpx'), ('alpha-vp9.webm', 'libvpx-vp9')]:
        subprocess.run(['/usr/bin/ffmpeg', '-v', 'error', '-loop', '1', '-i', str(output/'alpha.png'),
                        '-t', '1', '-c:v', codec, '-threads', '1', '-pix_fmt', 'yuva420p',
                        '-auto-alt-ref', '0', str(output/name)], check=True, timeout=30)
    Image.new('RGB', (320, 180), 'blue').save(output / 'cover.jpg')
    subprocess.run(['/usr/bin/ffmpeg', '-v', 'error', '-i', str(output/'clip.mp4'), '-i', str(output/'cover.jpg'),
                    '-map', '0:v', '-map', '1:v', '-c', 'copy', '-disposition:v:1', 'attached_pic',
                    str(output/'with-cover.mp4')], check=True, timeout=30)

    def boxes(data):
        at = 0
        while at < len(data):
            size = struct.unpack_from('>I', data, at)[0]
            if size == 1:
                size = struct.unpack_from('>Q', data, at + 8)[0]
            if size == 0:
                size = len(data) - at
            assert size >= 8 and at + size <= len(data), 'invalid generated MP4 atom'
            yield at, data[at:at + size]
            at += size

    # Put the standard covr metadata before trak without changing atom sizes or
    # chunk offsets. This makes the attached JPEG the actual first video stream,
    # so checking that the returned frame is red distinguishes V:0 from v:0.
    data = (output/'with-cover.mp4').read_bytes()
    for at, box in boxes(data):
        if box[4:8] == b'moov':
            children = [child for _, child in boxes(box[8:])]
            children.sort(key=lambda child: 0 if child[4:8] == b'mvhd' else 1 if child[4:8] == b'udta' else 2)
            data = data[:at] + box[:8] + b''.join(children) + data[at + len(box):]
            break
    (output/'cover-first.mp4').write_bytes(data)


def freeze_worker(output):
    frozen = output / 'worker-inputs'
    hashes = {}
    for relative in ['webdav/src/preview_worker.py', 'webdav/tests/test_preview_worker.py']:
        destination = frozen / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / relative, destination)
        hashes[relative] = hashlib.file_digest(destination.open('rb'), 'sha256').hexdigest()
    return frozen, hashes


def worker_test_count(log):
    result = re.search(r'(?m)^Ran ([1-9][0-9]*) tests? in ', log)
    assert result and re.search(r'(?m)^OK\s*$', log), 'worker regression suite did not complete without skips'
    return int(result[1])


def verify_worker(image_id, output, command, owner=None):
    """Run the real format/rotation suite with the image's system decoders."""
    frozen, hashes = freeze_worker(output)
    name = 'runcove-preview-worker-' + uuid.uuid4().hex[:10]
    cleanup_here = owner is None
    owner = owner or FixtureOwner(command, name)
    try:
        owner.container(name, ['--entrypoint', '/usr/bin/python3', '--memory', '3g',
                '--mount', f'type=bind,src={frozen},dst=/qualification,readonly', image_id,
                '/qualification/webdav/tests/test_preview_worker.py'])
        command('start', '-a', name, timeout=60)
        state = json.loads(command('inspect', name))[0]['State']
        assert state['ExitCode'] == 0 and not state['OOMKilled'], state
        log = command('logs', name)
        count = worker_test_count(log)
        (output / 'worker-image.log').write_text(log + '\n')
        return {'image_id': image_id, 'source_sha256': hashes,
                'log_sha256': hashlib.sha256((log+'\n').encode()).hexdigest(), 'tests': count, 'passed': True}
    finally:
        if cleanup_here and owner.cleanup():
            raise RuntimeError('worker fixture cleanup failed')


def preview_load(request, clients, stats, payloads):
    """Compare two-space cold and warm loads alongside an exact 1 MiB Range."""
    data = hashlib.shake_256(b'runcove ordinary range').digest(2 * 1024 * 1024)
    offset, length = 333, 1024 * 1024
    last = offset + length - 1
    root, _, first_key, _ = clients[0]
    assert request('PUT', root + 'preview-load-range.bin', data, secret=first_key)[0] == 201

    def ranged():
        status, headers, body = request(
            'GET', root + 'preview-load-range.bin', headers={'Range': f'bytes={offset}-{last}'},
            secret=first_key)
        assert status == 206 and headers['content-range'] == f'bytes {offset}-{last}/{len(data)}'
        assert body == data[offset:last+1], 'concurrent Range bytes differ'
        return status

    def asset(client, path, payload):
        dav, previews, secret, space = client
        assert request('PUT', dav + path, payload, secret=secret)[0] == 201
        status, headers, _ = request('HEAD', dav + path, secret=secret)
        assert status == 200
        source = headers['etag']
        url = previews + '?' + urlencode({'path': path, 'expectedEtag': source, 'size': 128})
        return url, secret, space, source

    def read(item, expected=None):
        url, secret, space, source = item
        status, headers, body = request('GET', url, secret=secret)
        assert 'x-agent-space-preview-busy-reason' not in headers
        if status == 429:
            assert inspect_problem((status, headers, body), 429, 'preview_busy')['retry-after'] == '1'
            return status, None
        assert status == 200 and headers['agent-space-id'] == space
        assert headers['preview-source-etag'] == source and headers['content-type'] == 'image/webp'
        assert body[:4] == b'RIFF' and body[8:12] == b'WEBP'
        digest = hashlib.sha256(body).hexdigest()
        assert headers['etag'] == f'"{digest}"'
        assert expected is None or digest == expected
        return status, digest

    def phase(waves, allow_busy):
        durations, ranges, statuses = [], [], []
        before = stats()
        start = time.perf_counter()
        with concurrent.futures.ThreadPoolExecutor(max_workers=9) as pool:
            for calls in waves:
                barrier = threading.Barrier(len(calls) + 1, timeout=15)

                def timed(call):
                    barrier.wait()
                    at = time.perf_counter()
                    return call(), (time.perf_counter() - at) * 1000

                previews = [pool.submit(timed, call) for call in calls]
                normal_range = pool.submit(timed, ranged)
                for result in previews:
                    (status, _), elapsed = result.result()
                    statuses.append(status)
                    if status == 200:
                        durations.append(elapsed)
                ranges.append(normal_range.result()[1])
        elapsed = time.perf_counter() - start
        after = stats()
        success, busy = statuses.count(200), statuses.count(429)
        assert success and (allow_busy or busy == 0)

        def p95(values):
            return round(sorted(values)[(95 * len(values) + 99) // 100 - 1], 2)

        return {'requests': len(statuses), 'success': success, 'busy': busy,
                'seconds': round(elapsed, 3), 'successful_per_second': round(success/elapsed, 2),
                'successful_preview_p95_ms': p95(durations), 'range_p95_ms': p95(ranges),
                'ranges_verified': len(ranges), 'cpu_us': after[0]-before[0],
                'container_peak_memory_bytes_after_phase': after[1]}

    warm = [asset(client, f'preview-load-warm-{path}', payload) for client in clients
            for path, payload in payloads]
    warmed = [read(item) for item in warm]
    assert all(status == 200 for status, _ in warmed)
    warm_calls = [lambda item=item, digest=digest: read(item, digest) for item, (_, digest) in zip(warm, warmed)]
    cold_waves = []
    for wave in range(12):
        path, payload = payloads[wave % len(payloads)]
        assets = [asset(client, f'preview-load-cold-{wave}-{path}', payload) for client in clients]
        cold_waves.append([lambda item=item: read(item) for item in assets])
    return {'warm': phase([warm_calls * 2] * 12, True), 'cold': phase(cold_waves, False)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    parser.add_argument('--target', required=True, choices=['server', 'standalone'])
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--development-assets', action='store_true',
                        help='Bind frozen debug binaries into an existing dependency/runtime image; never final-image evidence')
    parser.add_argument('--workspace-assets', type=Path,
                        help='Server development assets only: verified offline Workspace cache with desktop.tar.gz and desktop.reference')
    parser.add_argument('--expected-source', help='Independently computed source digest of the fixed final build checkout')
    parser.add_argument('--expected-helper-build-id', help='Current helper source fingerprint, computed before final-image qualification')
    parser.add_argument('--compare-preview-load', type=int, choices=[4, 8],
                        help='Server-only load lane. 4 writes an override and 8 exercises the default')
    args = parser.parse_args()
    if args.compare_preview_load and args.target != 'server':
        parser.error('--compare-preview-load requires Server')
    if args.development_assets and args.target == 'server' and not args.workspace_assets:
        parser.error('Server development assets require --workspace-assets')
    if args.workspace_assets and not (args.development_assets and args.target == 'server'):
        parser.error('--workspace-assets applies only to Server development assets')
    if not args.development_assets:
        if not (args.expected_source and args.expected_helper_build_id):
            parser.error('final-image qualification requires --expected-source and --expected-helper-build-id')
        assert all(re.fullmatch(r'[0-9a-f]{64}', value) for value in (args.expected_source, args.expected_helper_build_id))
    args.evidence = args.evidence.resolve()
    args.evidence.parent.mkdir(parents=True, exist_ok=True)
    evidence_stream = args.evidence.open('x', encoding='utf-8')
    name = 'runcove-preview-' + uuid.uuid4().hex[:10]
    output = args.evidence.parent / name
    output.mkdir(mode=0o700)
    verifier_copy = output / 'verify-previews.py'
    shutil.copy2(Path(__file__), verifier_copy)
    tokens = [secrets.token_urlsafe(40) for _ in range(3)]
    admin_token, token, dav_token = tokens
    environment = dict(os.environ, AGENT_SPACE_ADMIN_TOKEN=admin_token,
                       RUNCOVE_TOKEN=token, RUNCOVE_DAV_TOKEN=dav_token)
    report = {'target': args.target, 'scope': 'final image public DAV preview protocol',
              'passed': False, 'checks': [], 'container': name,
              'verifier_sha256': hashlib.file_digest(verifier_copy.open('rb'), 'sha256').hexdigest()}
    if args.development_assets:
        report['scope'] = 'development debug binaries with real dependency image; not final image qualification'
        report['source_sha256'] = source_digest()
    created = []

    def command(*values, timeout=180):
        result = subprocess.run(['docker', *values], env=environment, capture_output=True,
                                text=True, timeout=timeout)
        if result.returncode:
            raise RuntimeError('docker ' + ' '.join(values[:2]) + ' failed: ' +
                               (result.stdout + result.stderr)[-5000:])
        return (result.stdout + result.stderr if values[:1] == ('logs',) else result.stdout).strip()

    owner = FixtureOwner(command, name)
    containers = owner.containers

    def passed(label):
        report['checks'].append(label)
        print('PASS ' + label, flush=True)

    def request(method, path, body=None, headers=None, *, main=False, secret=None, maximum=MAX_OUTPUT):
        connection = http.client.HTTPConnection('127.0.0.1', main_port if main else dav_port, timeout=90)
        request_headers = {'Host': 'localhost:8932' if main else 'localhost:8933', **(headers or {})}
        if secret is not None:
            request_headers['Authorization'] = 'Bearer ' + secret
        try:
            connection.request(method, path, body, request_headers)
            response = connection.getresponse()
            data = response.read(maximum + 1)
            assert len(data) <= maximum, 'response exceeds qualification bound'
            return response.status, {key.lower(): value for key, value in response.getheaders()}, data
        finally:
            connection.close()

    def admin(method, path, body=None):
        status, _, data = request(method, path, None if body is None else json.dumps(body),
                                  {'Content-Type': 'application/json'}, main=True,
                                  secret=admin_token, maximum=64 * 1024)
        return status, json.loads(data) if data else None

    def remove_user(username, space):
        assert admin('DELETE', '/admin/v1/users/'+username+'?agentSpaceId='+space)[0] in (200, 202, 204)
        deadline = time.monotonic() + 90
        while admin('GET', '/admin/v1/users/'+username)[0] != 404:
            assert time.monotonic() < deadline, 'temporary user cleanup timeout'
            time.sleep(.3)

    def dav(method, path='', body=None, headers=None, *, secret=None):
        return request(method, dav_root + path, body, headers, secret=dav_token if secret is None else secret)

    def preview(method='GET', path='alpha.png', *, etag=None, size=128, headers=None, secret=None):
        query = '' if method == 'OPTIONS' else '?' + urlencode({'path': path, 'size': size, 'expectedEtag': etag})
        return request(method, preview_root + query, headers=headers, secret=dav_token if secret is None else secret)

    def guest(code):
        return client.tool('bash', cmd='/usr/bin/python3 -c ' + shlex.quote(code))

    def stopped_request(path, etag):
        # The controller runs as the same file identity as the decoder. It only
        # targets this private decoder argv, not arbitrary user Python commands.
        stop = '''import os,pathlib,signal,time
until=time.monotonic()+8
while True:
 for process in pathlib.Path('/proc').glob('[0-9]*'):
  try:
   descriptor=os.pidfd_open(int(process.name))
   try:
    argv=process.joinpath('cmdline').read_bytes().split(b'\\0')
    if argv[1:3]==[b'-I',b'-c'] and argv[3].startswith(b'"""Private bounded decoder'):
     started=process.joinpath('stat').read_text().rsplit(')',1)[1].split()[19]
     signal.pidfd_send_signal(descriptor,signal.SIGSTOP);print('STOPPED='+process.name+':'+started);raise SystemExit(0)
   finally:os.close(descriptor)
  except (OSError,IndexError):pass
 assert time.monotonic()<until,'no preview decoder observed'
 time.sleep(.002)
'''
        executor = concurrent.futures.ThreadPoolExecutor(max_workers=1)
        future = executor.submit(guest, stop)
        time.sleep(.1)
        connection = http.client.HTTPConnection('127.0.0.1', dav_port, timeout=15)
        connection.request('GET', preview_root + '?' + urlencode({'path': path, 'size': 512, 'expectedEtag': etag}),
                           headers={'Host': 'localhost:8933', 'Authorization': 'Bearer ' + dav_token})
        try:
            observed = future.result(timeout=15)
            match = re.search(r'STOPPED=(\d+):(\d+)', observed)
            assert match, 'stopped-worker probe did not report its identity'
            return connection, (int(match[1]), match[2])
        except BaseException:
            connection.close()
            raise
        finally:
            executor.shutdown(wait=True)

    def assert_reaped(identity):
        pid, starttime = identity
        result = guest(inspect.getsource(original_process_present)+'''\nimport pathlib,time
process=pathlib.Path('/proc/'''+str(pid)+'''/stat');until=time.monotonic()+5
while True:
 try: alive=original_process_present(process.read_text(), '''+repr(starttime)+''')
 except FileNotFoundError:alive=False
 if not alive:break
 assert time.monotonic()<until,'cancelled decoder survived'
 time.sleep(.02)
print('REAPED')
''')
        assert 'REAPED' in result, result

    try:
        image = json.loads(command('image', 'inspect', args.image))[0]
        image_id = image['Id']
        assert image.get('Architecture') == 'amd64' and image.get('Os') == 'linux'
        labels = image.get('Config', {}).get('Labels', {})
        if not args.development_assets:
            assert labels.get('io.runcove.target') == args.target, 'image target does not match qualification target'
            report['source_sha256'] = labels.get('io.runcove.source-sha256', labels.get('io.agent-space.source-sha256'))
            assert report['source_sha256'] == args.expected_source, 'image differs from the fixed expected source'
            report['expected_source_sha256'] = args.expected_source
            report['expected_helper_build_id'] = args.expected_helper_build_id
        report['image_id'] = image_id
        development_bindings, development_entrypoint = [], []
        if args.development_assets:
            frozen = output / 'inputs'
            frozen.mkdir()
            report['binaries_sha256'] = {}
            service = 'agent-space-mcp' if args.target == 'server' else 'runcove-standalone'
            for binary in [service, 'agent-space-static-http', 'agent-space-webdav', 'agent-space-desktop']:
                shutil.copy2(ROOT / 'target/debug' / binary, frozen / binary)
                report['binaries_sha256'][binary] = hashlib.file_digest((frozen/binary).open('rb'), 'sha256').hexdigest()
                development_bindings += ['--mount', f'type=bind,src={frozen}/{binary},dst=/usr/local/bin/{binary},readonly']
            script = 'docker-entrypoint.sh' if args.target == 'server' else 'standalone-entrypoint.sh'
            shutil.copy2(ROOT / 'docker' / script, frozen / 'entrypoint.sh')
            development_bindings += ['--mount', f'type=bind,src={frozen}/entrypoint.sh,dst=/entrypoint.sh,readonly']
            development_entrypoint = ['--entrypoint', '/bin/bash']
            if args.target == 'server':
                assert labels.get('io.runcove.target') == 'server', 'development runtime image must be Server'
                for file in ['desktop.tar.gz', 'desktop.reference', 'workspace.lock.json']:
                    shutil.copy2(args.workspace_assets.resolve() / file, frozen / file)
                reference = (frozen/'desktop.reference').read_text().strip()
                assert re.fullmatch(r'runcove/runcove-workspace@sha256:[0-9a-f]{64}', reference)
                assert json.loads((frozen/'workspace.lock.json').read_text())['reference'] == reference
                report['development_workspace'] = {'reference': reference,
                    'archive_sha256': hashlib.file_digest((frozen/'desktop.tar.gz').open('rb'), 'sha256').hexdigest()}
                for file in ['desktop.tar.gz', 'desktop.reference']:
                    development_bindings += ['--mount', f'type=bind,src={frozen}/{file},dst=/opt/msb-images/{file},readonly']
        if args.target == 'standalone':
            report['worker_image_suite'] = verify_worker(image_id, output, command, owner)
            passed('image system decoders pass real WebM VINT rotation SAR and reflection regressions')
        volume = name + '-data'
        owner.volume(volume)
        config = output / 'config.toml'
        if args.target == 'server':
            config.write_text('''resource_prefix="previewqual"
[server]
listen="0.0.0.0:8932"
public_base_url="http://localhost:8932"
max_running_spaces=2
[workspace]
image={preset="runcove-desktop"}
cpus=1
memory_mib=1536
workspace_size_mib=512
tmpfs_size_mib=128
shell="/bin/bash"
exec_timeout_secs=30
[state]
dir="/var/lib/microsandbox/agent-space-state"
[admin]
token_env="AGENT_SPACE_ADMIN_TOKEN"
[webdav]
enabled=true
listen="0.0.0.0:8933"
public_base_url="http://localhost:8933"
startup_timeout_secs=90
''')
            if args.development_assets:
                config.write_text(config.read_text().replace('image={preset="runcove-desktop"}',
                    'image='+json.dumps(report['development_workspace']['reference'])))
            if args.compare_preview_load:
                with config.open('a', encoding='utf-8') as settings:
                    if args.compare_preview_load == 4:
                        settings.write('preview_max_inflight=4\n')
                    settings.write('[logging]\nlevel="debug"\n')
            common = ['-e', 'AGENT_SPACE_ADMIN_TOKEN', '--mount', 'type=volume,src='+volume+',dst=/var/lib/microsandbox',
                      '--mount', 'type=bind,src='+str(config)+',dst=/etc/agent-space/config.toml,readonly', *development_bindings]
            init = name+'-init'
            init_command = ['/entrypoint.sh', 'init-state'] if args.development_assets else ['init-state']
            owner.container(init, [*common, *development_entrypoint, image_id, *init_command])
            command('start', '-a', init)
            arguments = ['--device', '/dev/kvm', *common]
        else:
            config.write_text('''[server]
listen="0.0.0.0:8932"
[webdav]
enabled=true
listen="0.0.0.0:8933"
''')
            arguments = ['--init', '--memory', '3g', '--cpus', '2', '--shm-size', '512m',
                         '-e', 'RUNCOVE_TOKEN', '-e', 'RUNCOVE_DAV_TOKEN', '--mount',
                         'type=volume,src='+volume+',dst=/workspace', '--mount',
                         'type=bind,src='+str(config)+',dst=/etc/runcove/config.toml,readonly', *development_bindings]
        config.chmod(0o600)
        entrypoint = development_entrypoint
        commandline = []
        if args.development_assets:
            commandline = ['/entrypoint.sh'] if args.target == 'server' else ['/entrypoint.sh', '--config', '/etc/runcove/config.toml']
        owner.container(name, ['--stop-timeout', '60', '-p', '127.0.0.1::8932',
                '-p', '127.0.0.1::8933', *arguments, *entrypoint, image_id, *commandline])
        command('start', name)
        main_port = int(command('port', name, '8932/tcp').rsplit(':', 1)[1])
        dav_port = int(command('port', name, '8933/tcp').rsplit(':', 1)[1])
        deadline = time.monotonic() + 120
        while True:
            try:
                if request('GET', '/healthz', main=True)[0] == 200:
                    break
            except (OSError, http.client.HTTPException):
                pass
            assert time.monotonic() < deadline, 'fixture readiness timeout'
            time.sleep(.3)
        report['helper_build_id'] = command('exec', name, 'agent-space-webdav', '--build-id')
        if args.expected_helper_build_id:
            assert report['helper_build_id'] == args.expected_helper_build_id, 'Host helper differs from expected current source'
        if not args.development_assets:
            report['release'] = json.loads(command('exec', name, 'cat', '/opt/agent-space/release.json'))
            assert report['release']['target'] == args.target
            assert report['release']['source_sha256'] == report['source_sha256']
            binaries = list(report['release']['binaries'])
            service = 'agent-space-mcp' if args.target == 'server' else 'runcove-standalone'
            assert set(binaries) == {service, 'agent-space-static-http', 'agent-space-webdav', 'agent-space-desktop'}
            raw_hashes = command('exec', name, 'sha256sum', *['/usr/local/bin/'+binary for binary in binaries])
            hashes = {line.split()[1].rsplit('/', 1)[1]: line.split()[0] for line in raw_hashes.splitlines()}
            assert hashes == report['release']['binaries'], 'shipped binary bytes differ from release inventory'
            passed('exact image source label and actual shipped binaries match release inventory')
        if args.target == 'server':
            username = 'preview' + uuid.uuid4().hex[:8]
            status, user = admin('POST', '/admin/v1/users', {'username': username})
            assert status == 201, status
            created.append((username, user['agentSpaceId']))
            report['guest_image'] = user.get('image')
            token, space = user['token'], user['agentSpaceId']
            tokens.append(token)
            status, dav_credential = admin('POST', '/admin/v1/users/'+username+'/dav-token', {'agentSpaceId': space})
            assert status == 200
            dav_token = dav_credential['token']
            tokens.append(dav_token)
            dav_root, preview_root = '/u/'+username+'/dav/', '/u/'+username+'/previews'
            assert admin('GET', '/admin/v1/status')[1]['activeSpaces'] == 0
        else:
            username, space = None, None
            dav_root, preview_root = '/dav/', '/previews'
        assert request('OPTIONS', preview_root)[0] == 401
        assert preview('OPTIONS', secret=token)[0] == 401
        if args.target == 'server':
            assert preview('OPTIONS', secret=admin_token)[0] == 401
        status, headers, data = preview('OPTIONS')
        assert status == 200 and headers['cache-control'] == 'no-store'
        assert json.loads(data) == {'schemaVersion': 1, 'supported': True, 'sizes': [128, 256, 512],
                                    'contentType': 'image/webp', 'mediaKinds': ['IMAGE', 'VIDEO']}
        space = space or headers['agent-space-id']
        assert headers['agent-space-id'] == space
        inspect_problem(request('OPTIONS', preview_root+'?unknown=1', secret=dav_token), 400, 'invalid_preview_request')
        for method in ['GET', 'HEAD']:
            request_body_guard(dav_port, 'localhost:8933', preview_root+'?'+urlencode(
                {'path': 'unopened.png', 'size': 128, 'expectedEtag': '"not-yet-opened"'}), dav_token, method)
        report['request_body_wire_guard'] = {'get_content_length_1': 400, 'head_content_length_1': 400,
                                            'head_body_bytes': 0, 'before_workspace_activation': True}
        if args.target == 'server':
            assert admin('GET', '/admin/v1/status')[1]['activeSpaces'] == 0
            assert request('OPTIONS', '/u/another/previews', secret=dav_token)[0] == 403
        passed('authenticated capability discovery uses DAV credential and does not activate Server VM')
        headers = inspect_problem(request('POST', preview_root, secret=dav_token), 405, 'preview_method_not_allowed')
        assert set(headers['allow'].replace(' ', '').split(',')) == {'OPTIONS', 'GET', 'HEAD'}
        fixtures(output)
        for path in ['alpha.png', *VIDEO_PREVIEW_NAMES, 'cancel.png']:
            assert dav('PUT', path, (output/path).read_bytes())[0] == 201, path
        assert dav('PUT', 'ordinary.txt', b'original-bytes')[0] == 201
        passed('original DAV uploads activate the real workspace')
        inspect_problem(preview(path='x'*300, etag='"path-limit-probe"'), 400, 'invalid_preview_request')
        report['filesystem_path_too_long_problem_verified'] = True
        client = Client(main_port, token, username=username, endpoint='/mcp' if args.target == 'standalone' else None)
        guest_ids = guest('''import pathlib,subprocess
ids=[]
for process in pathlib.Path('/proc').glob('[0-9]*'):
 try:
  executable=process/'exe'
  if str(executable.readlink()).endswith('/agent-space-webdav'):
   ids.append(subprocess.check_output([str(executable),'--build-id'],text=True).strip())
 except (OSError,subprocess.SubprocessError):pass
assert ids,'no accessible actual guest helper executable'
print('GUEST_HELPER_IDS='+','.join(ids))
''')
        match = re.search(r'GUEST_HELPER_IDS=([0-9a-f,]+)', guest_ids)
        assert match, 'guest helper identity probe failed'
        report['guest_helper_build_ids'] = match[1].split(',')
        expected_helper = args.expected_helper_build_id or report['helper_build_id']
        assert all(identity == expected_helper for identity in report['guest_helper_build_ids']), 'Guest helper differs from expected source'
        report['media_dependencies'] = guest("import sys,PIL,subprocess;print(sys.version.split()[0]);print(PIL.__version__);print(subprocess.check_output(['/usr/bin/ffmpeg','-version'],text=True).splitlines()[0])")
        cover_order = guest("import subprocess,json; streams=json.loads(subprocess.check_output(['/usr/bin/ffprobe','-v','error','-show_entries','stream=index:stream_disposition=attached_pic','-of','json','/workspace/cover-first.mp4']))['streams']; assert streams[0]['disposition']['attached_pic']==1 and streams[1]['disposition']['attached_pic']==0; print('ATTACHED_PICTURE_IS_FIRST_VIDEO_STREAM')")
        assert 'ATTACHED_PICTURE_IS_FIRST_VIDEO_STREAM' in cover_order
        report['cover_fixture_first_attached_picture_confirmed_in_guest'] = True
        if args.target == 'server':
            frozen, hashes = freeze_worker(output)
            for directory in ['qualification-worker', 'qualification-worker/webdav',
                              'qualification-worker/webdav/src', 'qualification-worker/webdav/tests']:
                assert dav('MKCOL', directory)[0] == 201
            for relative in hashes:
                assert dav('PUT', 'qualification-worker/'+relative, (frozen/relative).read_bytes())[0] == 201
            log = guest("import subprocess; result=subprocess.run(['/usr/bin/python3','-I','/workspace/qualification-worker/webdav/tests/test_preview_worker.py'],capture_output=True,text=True,timeout=20); print(result.stdout+result.stderr); assert result.returncode==0")
            count = worker_test_count(log)
            (output/'worker-image.log').write_text(log+'\n')
            report['worker_image_suite'] = {'execution': 'real Server guest', 'helper_build_id': report['helper_build_id'],
                'workspace_reference': report['development_workspace']['reference'] if args.development_assets else report['release']['workspace']['reference'], 'source_sha256': hashes,
                'log_sha256': hashlib.sha256((log+'\n').encode()).hexdigest(), 'tests': count, 'passed': True}
            passed('real Server guest system decoders pass WebM VINT rotation SAR and reflection regressions')
        for path in ['alpha.png', *VIDEO_PREVIEW_NAMES]:
            status, source_headers, _ = dav('HEAD', path)
            assert status == 200
            source = source_headers['etag']
            status, headers, body = preview(path=path, etag=source)
            assert status == 200, (path, status, body)
            assert headers['agent-space-id'] == space
            image = inspect_preview(headers, body, source, 128)
            if path == 'alpha.png' or path.startswith('alpha-'):
                alpha = image.getpixel((32, 16))[3]
                assert image.size == (128, 32) and 88 <= alpha <= 92, (path, image.size, alpha)
            else:
                assert image.size == (128, 72) and image.getpixel((64, 36))[0] > 200
            status, head, data = preview('HEAD', path, etag=source)
            assert status == 200 and not data and head['content-length'] == str(len(body))
            assert head['etag'] == headers['etag'] and head['preview-source-etag'] == source
            status, unchanged, data = preview(path=path, etag=source, headers={'If-None-Match': 'W/'+headers['etag']})
            assert status == 304 and not data and unchanged['agent-space-id'] == space
            status, _, data = preview(path=path, etag=source, headers={'Range': 'bytes=0-1'})
            assert status == 200 and data == body
        report['media_fixtures'] = ['alpha.png', *VIDEO_PREVIEW_NAMES]
        passed('real PNG MP4 M4V MOV WebM OGV previews exclude first-stream attached cover and preserve alpha dimensions hash identity HEAD 304 Range')
        source = dav('HEAD', 'alpha.png')[1]['etag']
        assert dav('PUT', 'alpha.png', b'changed')[0] == 204
        inspect_problem(preview(etag=source), 412, 'file_version_conflict')
        assert dav('DELETE', 'alpha.png')[0] == 204
        inspect_problem(preview(etag=source), 404, 'file_not_found')
        assert dav('GET', 'ordinary.txt', headers={'Range': 'bytes=1-3'})[2] == b'rig'
        assert dav('PROPFIND', headers={'Depth': '1'})[0] == 207
        assert dav('OPTIONS')[1]['dav'].split(',')[0].strip() == '1'
        assert 'sabredav-partialupdate' not in dav('OPTIONS')[1]['dav']
        assert dav('TRACE', 'ordinary.txt')[0] == 405
        passed('source mutation and deletion fence cached previews while standard DAV CRUD Range and method discovery remain usable')
        if args.target == 'standalone':
            # Simulate an existing/custom rootfs lacking a decoder dependency.
            # Only this disposable container's writable layer changes; restore
            # it even if the response assertion fails.
            assert dav('PUT', 'missing-dependency.mp4', (output/'clip.mp4').read_bytes())[0] == 201
            version = dav('HEAD', 'missing-dependency.mp4')[1]['etag']
            command('exec', name, 'mv', '/usr/bin/ffprobe', '/usr/bin/ffprobe.preview-qualification')
            try:
                inspect_problem(preview(path='missing-dependency.mp4', etag=version), 503, 'preview_dependency_unavailable')
                assert dav('HEAD', 'missing-dependency.mp4')[0] == 200
                assert dav('GET', 'ordinary.txt')[2] == b'original-bytes'
            finally:
                command('exec', name, 'mv', '/usr/bin/ffprobe.preview-qualification', '/usr/bin/ffprobe')
            passed('missing runtime decoder dependency returns explicit 503 while original DAV remains usable')
        if args.compare_preview_load:
            peer = 'preview' + uuid.uuid4().hex[:8]
            status, peer_user = admin('POST', '/admin/v1/users', {'username': peer})
            assert status == 201
            peer_space = peer_user['agentSpaceId']
            created.append((peer, peer_space))
            tokens.append(peer_user['token'])
            status, peer_dav = admin('POST', '/admin/v1/users/'+peer+'/dav-token', {'agentSpaceId': peer_space})
            assert status == 200
            peer_key = peer_dav['token']
            tokens.append(peer_key)
            endpoints = [(dav_root, preview_root, dav_token, space),
                         (f'/u/{peer}/dav/', f'/u/{peer}/previews', peer_key, peer_space)]

            def container_resources():
                usage = command('exec', name, 'cat', '/sys/fs/cgroup/cpu.stat')
                cpu = next(int(line.split()[1]) for line in usage.splitlines() if line.startswith('usage_usec '))
                peak = int(command('exec', name, 'cat', '/sys/fs/cgroup/memory.peak'))
                return cpu, peak

            report['preview_load'] = {'server_preview_limit': args.compare_preview_load}
            report['preview_load'].update(preview_load(
                request, endpoints, container_resources,
                [(path, (output/path).read_bytes()) for path in ('alpha.png', 'clip.mp4')]))
            remove_user(peer, peer_space)
            created.remove((peer, peer_space))
            passed('two-space warm and cold load with verified ordinary DAV Range')
        source = dav('HEAD', 'cancel.png')[1]['etag']
        connection, pid = stopped_request('cancel.png', source)
        assert dav('GET', 'ordinary.txt')[2] == b'original-bytes'
        busy = inspect_problem(preview(path='cancel.png', etag=source, size=128), 429, 'preview_busy')
        assert busy['retry-after'] == '1' and 'x-agent-space-preview-busy-reason' not in busy
        connection.close()
        assert_reaped(pid)
        passed('client disconnect reaps a stopped real decoder and concurrent DAV reads remain available')
        if args.target == 'server':
            connection, pid = stopped_request('cancel.png', source)
            old = dav_token
            assert admin('DELETE', '/admin/v1/users/'+username+'/dav-token?agentSpaceId='+space)[0] == 204
            try:
                response = connection.getresponse()
                assert response.status >= 400, response.status
                response.read()
            except (OSError, http.client.HTTPException):
                pass
            finally:
                connection.close()
            assert_reaped(pid)
            assert preview('OPTIONS', secret=old)[0] == 401
            status, dav_credential = admin('POST', '/admin/v1/users/'+username+'/dav-token', {'agentSpaceId': space})
            assert status == 200
            dav_token = dav_credential['token']
            tokens.append(dav_token)
            assert dav('GET', 'ordinary.txt')[2] == b'original-bytes'
            passed('DAV key revocation cancels and reaps stopped decoder without damaging original file access')
            default_image = 'nikolaik/python-nodejs:python3.12-nodejs22-slim'
            cached = command('exec', name, 'msb', 'image', 'inspect', default_image)
            (output/'default-seed-inspect.txt').write_text(cached+'\n')
            default_name = 'default' + uuid.uuid4().hex[:8]
            status, default_user = admin('POST', '/admin/v1/users', {'username': default_name, 'image': default_image})
            assert status == 201, status
            created.append((default_name, default_user['agentSpaceId']))
            tokens.append(default_user['token'])
            default_client = Client(main_port, default_user['token'], username=default_name)
            assert 'BUNDLED_DEFAULT_SEED_READY' in default_client.tool('bash', cmd='printf BUNDLED_DEFAULT_SEED_READY')
            report['bundled_default_seed'] = {'reference': default_image,
                'cache_inspect_sha256': hashlib.sha256(cached.encode()).hexdigest(), 'activation_succeeded': True,
                'network_isolation_tested': False}
            passed('bundled default seed tag resolves and its ordinary distribution activates from cache')
        if args.compare_preview_load:
            log = re.sub(r'\x1b\[[0-9;]*m', '', command('logs', name))
            reasons = re.findall(r'DAV preview busy reason="(host_limit|generation_busy|generation_cleanup)"', log)
            report['preview_load']['busy_reasons'] = {reason: reasons.count(reason)
                for reason in ('host_limit', 'generation_busy', 'generation_cleanup')}
            assert 'generation_busy' in reasons, 'missing internal helper 429 diagnostic'
            if report['preview_load']['warm']['busy']:
                assert 'host_limit' in reasons, 'missing internal Host 429 diagnostic'
        if args.development_assets:
            assert source_digest() == report['source_sha256'], 'runtime source changed during qualification'
        report['passed'] = True
    except BaseException as error:
        message = str(error)
        for secret in tokens:
            message = message.replace(secret, '[REDACTED_SECRET]')
        report['error'] = type(error).__name__ + ': ' + message
        if containers:
            try:
                log = command('logs', name)[-8000:]
                for secret in tokens:
                    log = log.replace(secret, '[REDACTED_SECRET]')
                report['log_tail'] = log
            except Exception:
                pass
    finally:
        errors = []
        if args.target == 'server' and containers:
            for username, space in created:
                try:
                    remove_user(username, space)
                except Exception as error:
                    errors.append('temporary user: ' + type(error).__name__)
        errors.extend(owner.cleanup())
        report['fixture_resources'] = {'ownership_label': OWNER_LABEL+'='+name,
                                       'containers': owner.containers, 'volumes': owner.volumes}
        report['cleanup_passed'] = not errors
        if errors:
            report['cleanup_errors'] = errors
            report['passed'] = False
        json.dump(report, evidence_stream, indent=2)
        evidence_stream.write('\n')
        evidence_stream.flush()
        os.fsync(evidence_stream.fileno())
        evidence_stream.close()
        print('Evidence: ' + str(args.evidence), flush=True)
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
