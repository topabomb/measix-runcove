#!/usr/bin/env python3
"""Selected old-VM workspace -> Standalone rehearsal, disposable resources only."""
import argparse, hashlib, http.client, json, os, secrets, subprocess, time, uuid
from pathlib import Path
from verify import Client, docker

ROOT = Path(__file__).resolve().parents[1]


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--old-image', required=True)
    p.add_argument('--image', required=True)
    a = p.parse_args()
    run = 'runcove-import-' + uuid.uuid4().hex[:10]
    out = ROOT / 'target/workspace-import' / run
    out.mkdir(parents=True, mode=0o700)
    report = {'passed': False, 'image_id': docker('image', 'inspect', a.image, '--format', '{{.Id}}').strip(), 'checks': []}
    a.image = report['image_id']
    containers, volumes = [], []
    admin, token = secrets.token_urlsafe(40), secrets.token_urlsafe(40)
    os.environ.update(AGENT_SPACE_ADMIN_TOKEN=admin, RUNCOVE_TOKEN=token)

    def check(value):
        report['checks'].append(value)
        print('PASS ' + value, flush=True)

    def vol(suffix):
        value = docker('volume', 'create', run + '-' + suffix).strip()
        volumes.append(value)
        return value

    def request(port, method, path, body=None, headers=None):
        c = http.client.HTTPConnection('127.0.0.1', port, timeout=120)
        c.request(method, path, body, headers or {})
        r = c.getresponse()
        result = r.status, r.read()
        c.close()
        return result

    def ready(name):
        until = time.monotonic() + 180
        while time.monotonic() < until:
            info = json.loads(docker('inspect', name))[0]
            assert info['State']['Running'], 'service stopped'
            ports = info['NetworkSettings']['Ports']
            if ports.get('8932/tcp'):
                port = int(ports['8932/tcp'][0]['HostPort'])
                try:
                    if request(port, 'GET', '/healthz')[0] == 200:
                        return port, ports
                except OSError:
                    pass
            time.sleep(.2)
        raise TimeoutError('service readiness')

    def browser(c, code):
        value = json.loads(c.tool('browser_run', code=code))
        while value.get('state') == 'running':
            value = json.loads(c.tool('browser_task', task_id=value['task_id']))
        assert value['state'] == 'succeeded', value
        return value.get('value')

    try:
        old_id = docker('image', 'inspect', a.old_image, '--format', '{{.Id}}').strip()
        baseline = json.loads((ROOT.parent / 'docs/evidence/runcove-split-20261015/production-baseline.json').read_text())
        assert old_id == baseline['imageId']
        report['old_image_id'] = old_id
        a.old_image = old_id
        data = vol('source')
        cfg = out / 'old.toml'
        cfg.write_text('resource_prefix="importqa"\n[server]\nlisten="0.0.0.0:8932"\n[engine]\ntype="msb"\n[workspace]\nimage={preset="runcove-desktop"}\ncpus=2\nmemory_mib=3072\nworkspace_size_mib=8192\nshell="/bin/bash"\n[lifecycle]\nsession_timeout_secs=900\nidle_timeout_secs=3600\ncheck_interval_secs=60\n[state]\ndir="/var/lib/microsandbox/agent-space-state"\n[admin]\ntoken_env="AGENT_SPACE_ADMIN_TOKEN"\n[webdav]\nenabled=true\nlisten="0.0.0.0:8933"\n')
        common = ['-e', 'AGENT_SPACE_ADMIN_TOKEN', '-v', data + ':/var/lib/microsandbox', '-v', str(cfg) + ':/etc/agent-space/config.toml:ro']
        docker('run', '--rm', *common, a.old_image, 'init-state')
        source = run + '-source'
        docker('run', '-d', '--name', source, '--device', '/dev/kvm', '--stop-timeout', '60', '-p', '127.0.0.1::8932', '-p', '127.0.0.1::8933', *common, a.old_image)
        containers.append(source)
        port, ports = ready(source)
        dav_port = int(ports['8933/tcp'][0]['HostPort'])
        headers = {'Authorization': 'Bearer ' + admin, 'Content-Type': 'application/json'}
        status, raw = request(port, 'POST', '/admin/v1/users', json.dumps({'username': 'selected'}), headers)
        assert status == 201, raw
        user = json.loads(raw)
        status, raw = request(port, 'POST', '/admin/v1/users/selected/dav-token', json.dumps({'agentSpaceId': user['agentSpaceId']}), headers)
        assert status == 200, raw
        dav = json.loads(raw)['token']
        c = Client(port, user['token'], username='selected', authority='127.0.0.1:' + str(port))
        c.tool('bash', command="mkdir -p /workspace/import-site /workspace/root-data; printf '<title>Imported profile</title>' >/workspace/import-site/index.html; printf root-owned >/workspace/root-data/record; chmod 755 /workspace/root-data; chmod 644 /workspace/root-data/record; ln -s root-data/record /workspace/preserved-link; nohup python3 -m http.server 8129 --bind 127.0.0.1 --directory /workspace/import-site >/tmp/import-site.log 2>&1 </dev/null &")
        browser(c, "await page.goto('http://127.0.0.1:8129/');await page.evaluate(()=>localStorage.setItem('workspace-import','preserved'));return true;")
        c.tool('write', path='/tmp/workspace-transfer.py', content=(ROOT / 'docker/workspace-transfer.py').read_text())
        # Stop the actual desktop writer, without restarting it before export.
        command = "import pathlib,os,signal,time\nfor p in pathlib.Path('/proc').iterdir():\n if p.name.isdigit():\n  try:\n   if pathlib.Path(p/'exe').resolve().name=='agent-space-desktop':\n    os.kill(int(p.name),signal.SIGTERM)\n    until=time.monotonic()+20\n    while p.exists() and p.joinpath('stat').read_text().split()[2]!='Z':\n     assert time.monotonic()<until;time.sleep(.05)\n  except FileNotFoundError:pass\n"
        c.tool('bash', command='python3 -c ' + __import__('shlex').quote(command))
        exported = c.tool('bash', command='python3 /tmp/workspace-transfer.py export --source /workspace --archive /tmp/workspace.tar')
        assert exported.lstrip().startswith('{'), exported
        export = json.JSONDecoder().raw_decode(exported.lstrip())[0]
        assert export['sourceChanged'] is False
        c.tool('bash', command='cp /tmp/workspace.tar /workspace/export.tar')
        status, raw = request(dav_port, 'GET', '/u/selected/dav/export.tar', headers={'Authorization': 'Bearer ' + dav})
        assert status == 200
        archive = out / 'workspace.tar'
        archive.write_bytes(raw)
        assert hashlib.sha256(raw).hexdigest() == export['archiveSha256']
        c.tool('bash', command='rm /workspace/export.tar')
        docker('stop', '--time', '60', source, timeout=90)
        check('exact_old_vm_selected_workspace_export_and_source_unchanged')
        target = vol('target')
        mounts = ['-v', target + ':/restore', '-v', str(out) + ':/evidence', '-v', str(ROOT / 'docker/workspace-transfer.py') + ':/transfer.py:ro']
        preview = json.loads(docker('run', '--rm', '--entrypoint', 'python3', *mounts, a.image, '/transfer.py', 'import', '--archive', '/evidence/workspace.tar', '--destination', '/restore/workspace'))
        assert not preview['applied'] and not preview['unsupportedOwners']
        applied = json.loads(docker('run', '--rm', '--entrypoint', 'python3', *mounts, a.image, '/transfer.py', 'import', '--archive', '/evidence/workspace.tar', '--destination', '/restore/workspace', '--apply'))
        assert applied['applied']
        check('archive_preflight_and_root_to_payload_mapping')
        newcfg = out / 'standalone.toml'
        newcfg.write_text('[server]\nlisten="0.0.0.0:8932"\n[webdav]\nenabled=false\n')
        name = run + '-standalone'
        docker('run', '-d', '--name', name, '--init', '--memory', '3g', '--cpus', '2', '--shm-size', '512m', '--security-opt', 'seccomp=' + str(ROOT / 'docker/playwright-seccomp.json'), '--tmpfs', '/tmp:size=256m,mode=1777', '--tmpfs', '/run:size=64m,mode=755', '-e', 'RUNCOVE_TOKEN', '-p', '127.0.0.1::8932', '--mount', 'type=volume,src=' + target + ',dst=/workspace,volume-subpath=workspace', '-v', str(newcfg) + ':/etc/runcove/config.toml:ro', a.image)
        containers.append(name)
        port, _ = ready(name)
        # Client accepts an explicit complete path for the one-space endpoint.
        client = Client(port, token, endpoint='/mcp', authority='127.0.0.1:' + str(port))
        assert 'root-owned' in client.tool('read', path='/workspace/root-data/record')
        client.tool('edit', path='/workspace/root-data/record', old_text='root-owned', new_text='payload-edit')
        assert '1001:1001' in client.tool('bash', cmd="stat -c '%u:%g' /workspace/root-data/record; test -L /workspace/preserved-link && echo link-preserved")
        client.tool('bash', cmd='nohup python3 -m http.server 8129 --bind 127.0.0.1 --directory /workspace/import-site >/tmp/import-site.log 2>&1 </dev/null &')
        assert browser(client, "return {tabs:context.pages().map(p=>p.url()),marker:await page.evaluate(()=>localStorage.getItem('workspace-import'))};") == {'tabs': ['http://127.0.0.1:8129/'], 'marker': 'preserved'}
        assert any(x['type'] == 'image' for x in client.tool_result('browser_observe', view='desktop')['content'])
        check('standalone_restores_tabs_profile_links_and_writable_user_data')
        report.update(passed=True, archive_sha256=export['archiveSha256'], entries=export['entries'], import_result=applied)
    except BaseException as error:
        detail = getattr(error, 'output', None) or str(error)
        report['error'] = detail.replace(admin, '[REDACTED_SECRET]').replace(token, '[REDACTED_SECRET]')
        print(report['error'], flush=True)
        raise
    finally:
        errors = []
        for name in reversed(containers):
            try:
                docker('rm', '-f', '-v', name)
            except Exception:
                errors.append(name)
        for name in reversed(volumes):
            try:
                docker('volume', 'rm', name)
            except Exception:
                errors.append(name)
        report['cleanup_passed'] = not errors
        if errors:
            report.update(passed=False, cleanup_errors=errors)
        (out / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
        print(out / 'result.json', flush=True)
        if errors:
            raise RuntimeError('workspace import cleanup failed')


if __name__ == '__main__':
    main()
