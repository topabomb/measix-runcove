#!/usr/bin/env python3
"""Actual ENOSPC and rejected-upload bounds on a disposable workspace."""
import argparse, http.client, json, os, secrets, shlex, time, uuid
from pathlib import Path
from verify import Client, docker

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--development-helper', type=Path, help='Development-only bind mount; not final-image qualification')
    args = parser.parse_args()
    name = 'runcove-enospc-' + uuid.uuid4().hex[:10]
    args.evidence.parent.mkdir(parents=True, exist_ok=True)
    info = json.loads(docker('image', 'inspect', args.image))[0]
    target = info['Config']['Labels'].get('io.runcove.target')
    assert target in ('server', 'standalone')
    report = {'image_id': info['Id'], 'target': target, 'passed': False, 'scope': 'development helper' if args.development_helper else 'final image'}
    token, dav, admin_token = secrets.token_urlsafe(40), secrets.token_urlsafe(40), secrets.token_urlsafe(40)
    os.environ.update(RUNCOVE_TOKEN=token, RUNCOVE_DAV_TOKEN=dav, AGENT_SPACE_ADMIN_TOKEN=admin_token)
    created, volume, config, client = False, None, None, None
    prefix = '/dav/'
    try:
        # Only /workspace is tiny. No mount capability or privileged mode is
        # granted to the product; Docker establishes all tmpfs mounts.
        helper_mount = ['-v', str(args.development_helper.resolve()) + ':/usr/local/bin/agent-space-webdav:ro'] if args.development_helper else []
        if target == 'standalone':
            docker('run', '-d', '--name', name, '--init', '--memory', '512m', '--cpus', '1', '-p', '127.0.0.1::8933', '-e', 'RUNCOVE_TOKEN', '-e', 'RUNCOVE_DAV_TOKEN', '--tmpfs', '/workspace:size=8m,mode=755,uid=1001,gid=1001', *helper_mount, report['image_id'])
            created = True
        # The shipped sample leaves DAV disabled; use the existing config file
        # before restarting this disposable instance, without new config scope.
            docker('exec', name, 'python3', '-c', 'import sys;open("/etc/runcove/config.toml","w").write(sys.argv[1])', '[server]\nlisten="0.0.0.0:8932"\n[webdav]\nenabled=true\nlisten="0.0.0.0:8933"\nio_idle_timeout_secs=2\n')
            docker('restart', name)
        else:
            assert not args.development_helper, 'Server qualification uses the final helper image'
            volume = docker('volume', 'create', name + '-data').strip()
            config = args.evidence.parent.resolve() / (name + '.toml')
            config.write_text('resource_prefix="storageqa"\n[server]\nlisten="0.0.0.0:8932"\nmax_running_spaces=1\n[workspace]\nimage="nikolaik/python-nodejs:python3.12-nodejs22-slim"\ncpus=1\nmemory_mib=256\nworkspace_size_mib=128\nshell="/bin/bash"\n[state]\ndir="/var/lib/microsandbox/agent-space-state"\n[admin]\ntoken_env="AGENT_SPACE_ADMIN_TOKEN"\n[webdav]\nenabled=true\nlisten="0.0.0.0:8933"\nio_idle_timeout_secs=2\n')
            config.chmod(0o600)
            common = ['-e', 'AGENT_SPACE_ADMIN_TOKEN', '-v', volume + ':/var/lib/microsandbox', '-v', str(config) + ':/etc/agent-space/config.toml:ro']
            docker('run', '--rm', *common, report['image_id'], 'init-state')
            docker('run', '-d', '--name', name, '--device', '/dev/kvm', '--stop-timeout', '60', '-p', '127.0.0.1::8932', '-p', '127.0.0.1::8933', *common, report['image_id'])
            created = True
            main_port = int(docker('port', name, '8932/tcp').rsplit(':', 1)[1])
            def admin(method, path, value=None):
                c = http.client.HTTPConnection('127.0.0.1', main_port, timeout=30)
                try:
                    c.request(method, path, json.dumps(value) if value is not None else None, {'Authorization': 'Bearer ' + admin_token, 'Content-Type': 'application/json'})
                    r = c.getresponse(); status, raw = r.status, r.read()
                    assert status in (200, 201), (status, raw)
                    return json.loads(raw)
                finally:
                    c.close()
            until = time.monotonic() + 60
            while True:
                try:
                    admin('GET', '/admin/v1/users')
                    break
                except OSError:
                    assert time.monotonic() < until; time.sleep(.2)
            user = admin('POST', '/admin/v1/users', {'username': 'storage'})
            client = Client(main_port, user['token'], username='storage', authority='127.0.0.1:' + str(main_port))
            client.tool('bash', cmd='mount -t tmpfs -o size=8m tmpfs /workspace')
            dav = admin('POST', '/admin/v1/users/storage/dav-token', {'agentSpaceId': user['agentSpaceId']})['token']
            prefix = '/u/storage/dav/'
        port = int(docker('port', name, '8933/tcp').rsplit(':', 1)[1])

        def request(method, body=None):
            c = http.client.HTTPConnection('127.0.0.1', port, timeout=30)
            try:
                c.request(method, prefix + 'preserved.txt', body, {'Authorization': 'Bearer ' + dav})
                r = c.getresponse()
                return r.status, r.read()
            finally:
                c.close()

        until = time.monotonic() + 45
        last = None
        while True:
            try:
                last = request('OPTIONS')
                if last[0] == 200:
                    break
            except OSError:
                pass
            assert time.monotonic() < until, ('DAV readiness', last, docker('logs', '--tail', '8', name))
            time.sleep(.2)
        assert request('PUT', b'original-before-enospc')[0] == 201
        for attempt in range(20):
            status, body = request('PUT', b'x' * (16 * 1024 * 1024))
            assert status == 507, (attempt, status, body)
            assert request('GET') == (200, b'original-before-enospc')
            assert request('PUT', b'normal-after-failure')[0] == 204
            assert request('GET') == (200, b'normal-after-failure')
            assert request('PUT', b'original-before-enospc')[0] == 204
        # The loopback helper must deliver an early 412 before the advertised
        # body finishes, and close a stalled rejected upload at the fixed bound.
        probe = '''import glob,json,socket,time,threading
instance=next(json.load(open(p))["instance"] for p in glob.glob("/tmp/agent-space/*/instance.json") if json.load(open(p))["instance"]["service"]=="webdav")
s=socket.create_connection(("127.0.0.1",instance["port"]));s.settimeout(5)
start=time.monotonic();s.sendall(b"PUT /dav/preserved.txt HTTP/1.1\\r\\nHost: localhost\\r\\nIf-None-Match: *\\r\\nContent-Length: 16777216\\r\\nConnection: close\\r\\n\\r\\n"+b"x"*1024)
head=b""
while b"\\r\\n\\r\\n" not in head:head+=s.recv(4096)
assert head.startswith(b"HTTP/1.1 412"),head
assert time.monotonic()-start<1.5,"rejection waited for upload EOF"
stop=threading.Event();progress=[]
def send():
 try:
  while not stop.wait(.04):s.sendall(b"x"*1024);progress.append(1)
 except OSError:pass
sender=threading.Thread(target=send,daemon=True);sender.start()
try:
 while s.recv(4096):pass
except ConnectionResetError:pass
elapsed=time.monotonic()-start;stop.set();s.close();sender.join(1)
assert elapsed<3.5 and len(progress)>10,(elapsed,len(progress))
print(json.dumps({"early_status":412,"connection_closed_seconds":elapsed,"progress_frames":len(progress)}))
'''
        probe = probe.replace('/dav/preserved.txt', prefix + 'preserved.txt')
        result = client.tool('bash', cmd='python3 -c ' + shlex.quote(probe)) if client else docker('exec', name, 'python3', '-c', probe)
        report['rejected_upload'] = json.JSONDecoder().raw_decode(result.lstrip())[0]
        report.update(passed=True, attempts=20, checks=['real 8 MiB filesystem ENOSPC returns 507', 'failed overwrite preserves original bytes', 'temporary upload cleanup permits next write', 'early rejected PUT response and fixed total drain deadline'])
    except BaseException:
        if created:
            report['log_tail'] = docker('logs', '--tail', '35', name).replace(token, '[REDACTED_SECRET]').replace(dav, '[REDACTED_SECRET]').replace(admin_token, '[REDACTED_SECRET]')
            report['memory_events'] = docker('exec', name, 'cat', '/sys/fs/cgroup/memory.events')
            print(report['log_tail'], flush=True)
        raise
    finally:
        try:
            if created:
                if target == 'server':
                    docker('stop', '--time', '60', name, timeout=90)
                docker('rm', '-f', '-v', name)
            if volume:
                docker('volume', 'rm', volume)
            if config:
                config.unlink(missing_ok=True)
            report['cleanup_passed'] = True
        except Exception:
            report.update(passed=False, cleanup_passed=False)
            raise
        finally:
            args.evidence.write_text(json.dumps(report, indent=2) + '\n')
            print(args.evidence, flush=True)


if __name__ == '__main__':
    main()
