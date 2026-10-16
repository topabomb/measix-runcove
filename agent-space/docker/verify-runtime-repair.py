#!/usr/bin/env python3
"""Exercise unchanged legacy credentials, cold repair and isolated rollback."""
import argparse
import copy
from datetime import datetime, timezone
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import time
import uuid

from verify import Client, docker

PROJECT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('repair_container', PROJECT / 'docker/repair-container.py')
repair_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(repair_module)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base-image', required=True)
    parser.add_argument('--repair-image', required=True)
    args = parser.parse_args()
    run = 'as-runtime-repair-' + uuid.uuid4().hex[:10]
    root = PROJECT / 'target/sqlite-repair-validation' / run
    root.mkdir(parents=True, mode=0o700)
    cfg = root / 'config.toml'
    # Public fixture credential, never a real deployment token.
    token = 'sqlite-runtime-repair-fixture-token'
    cfg.write_text('''resource_prefix = "sqlitelegacy"
[server]
listen = "0.0.0.0:8932"
max_sessions = 2
[engine]
type = "msb"
[workspace]
image = "nikolaik/python-nodejs:python3.12-nodejs22-slim"
cpus = 1
memory_mib = 256
workspace_size_mib = 128
tmpfs_size_mib = 64
shell = "/bin/bash"
exec_timeout_secs = 15
[lifecycle]
session_timeout_secs = 900
idle_timeout_secs = 3600
check_interval_secs = 0
[logging]
level = "info"
[users.verify]
token = "sqlite-runtime-repair-fixture-token"
workspace_name = "sqlitelegacy-workspace"
''')
    cfg.chmod(0o600)
    data, logs = run + '-data', run + '-logs'
    containers, volumes = [], [data, logs]
    report = {'base_image_id': docker('image', 'inspect', args.base_image, '--format', '{{.Id}}').strip(),
              'repair_image_id': docker('image', 'inspect', args.repair_image, '--format', '{{.Id}}').strip(),
              'passed': False, 'checks': [], 'started_at': datetime.now(timezone.utc).isoformat()}

    def ready(name):
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            info = json.loads(docker('inspect', name))[0]
            if not info['State']['Running']:
                raise RuntimeError('Fixture container stopped')
            try:
                docker('exec', name, 'curl', '-fsS', 'http://127.0.0.1:8932/healthz')
                return int(info['NetworkSettings']['Ports']['8932/tcp'][0]['HostPort'])
            except subprocess.CalledProcessError:
                time.sleep(.5)
        raise TimeoutError('Fixture did not become healthy')

    def launch(image, name, data_volume, log_volume):
        docker('run', '-d', '--name', name, '--device', '/dev/kvm', '--stop-timeout', '60',
               '-p', '127.0.0.1::8932', '-v', data_volume + ':/var/lib/microsandbox',
               '-v', log_volume + ':/var/log/agent-space', '-v', str(cfg) + ':/etc/agent-space/config.toml:ro', image)
        containers.append(name)
        return ready(name)

    try:
        for volume in volumes:
            docker('volume', 'create', volume)
        port = launch(args.base_image, run, data, logs)
        client = Client(port, token, endpoint='/mcp',
                        protocol_version='2025-03-26', expected_version='2024-11-05')
        names = {tool['name'] for tool in client.request('tools/list')['tools']}
        assert names == {'bash', 'read', 'write', 'edit'}, names
        assert 'legacy-written' in client.tool('bash', command='printf legacy-root > /root/sqlite-repair-fixture; printf legacy-workspace > /workspace/sqlite-repair-fixture; sync; echo legacy-written')
        report['checks'].append('original_static_four_tools_and_two_disks')
        print('PASS: original legacy application and fixture files', flush=True)
        backup = root / 'cold-backup'
        subprocess.run(['python3', str(PROJECT / 'docker/repair-container.py'), '--container', run,
                        '--image', args.repair_image, '--backup-dir', str(backup), '--apply', '--remove-old'], check=True)
        repair = json.loads((backup / 'result.json').read_text())
        assert repair['passed'] and repair['disk_hashes_preserved'] and repair['credentials_preserved']
        report['repair'] = repair
        assert not json.loads(docker('inspect', run))[0]['HostConfig']['VolumesFrom']
        docker('restart', '--time', '60', run)
        port = ready(run)
        client = Client(port, token, endpoint='/mcp',
                        protocol_version='2025-03-26', expected_version='2024-11-05')
        assert {tool['name'] for tool in client.request('tools/list')['tools']} == names
        output = client.tool('bash', command='cat /root/sqlite-repair-fixture /workspace/sqlite-repair-fixture')
        assert 'legacy-rootlegacy-workspace' in output, output
        report['checks'].extend(['original_static_token_and_mcp_endpoint', 'root_and_workspace_files_preserved', 'cold_backup_and_restart'])
        manifest = json.loads(docker('exec', run, 'cat', '/opt/agent-space/runtime.json'))
        executable = docker('exec', run, 'sh', '-c', 'for task_exe in /proc/[0-9]*/exe; do case "$(readlink "$task_exe" 2>/dev/null)" in */bin/msb) sha256sum "$task_exe";; esac; done')
        assert executable and all(line.split()[0] == manifest['msb_sha256'] for line in executable.splitlines())
        report['checks'].append('actual_vm_uses_sqlite351_runtime')
        print('PASS: repaired old version keeps original token, endpoint and files', flush=True)
        # A post-backup write must not leak into an isolated old-version restore.
        client.tool('bash', command='printf after-backup > /workspace/sqlite-repair-fixture; sync')
        docker('stop', '--time', '60', run)
        restored_data, restored_logs = run + '-restore-data', run + '-restore-logs'
        volumes.extend([restored_data, restored_logs])
        for volume, archive in ((restored_data, 'data.tar.gz'), (restored_logs, 'logs.tar.gz')):
            docker('volume', 'create', volume)
            docker('run', '--rm', '--entrypoint', '/bin/tar', '-v', volume + ':/restore',
                   '-v', str(backup) + ':/backup:ro', args.repair_image, '--sparse', '-xzf', '/backup/' + archive, '-C', '/restore')
        restore_name = run + '-restore'
        source = copy.deepcopy(json.loads(docker('inspect', run))[0])
        for mount in source['Mounts']:
            if mount['Destination'] in ('/var/lib/microsandbox', '/var/log/agent-space'):
                mount['Name'] = restored_data if mount['Destination'] == '/var/lib/microsandbox' else restored_logs
            elif mount['Destination'] == '/etc/agent-space/config.toml':
                mount['Source'] = str(backup / 'config.toml')
        base_image = json.loads(docker('image', 'inspect', args.base_image))[0]
        restored_id = repair_module.create_clone(source, base_image, restore_name)
        containers.append(restore_name)
        docker('start', restored_id)
        port = ready(restore_name)
        assert json.loads(docker('inspect', restored_id))[0]['Config']['Env'] == source['Config']['Env']
        restored_client = Client(port, token, endpoint='/mcp',
                                 protocol_version='2025-03-26', expected_version='2024-11-05')
        output = restored_client.tool('bash', command='cat /root/sqlite-repair-fixture /workspace/sqlite-repair-fixture')
        assert 'legacy-rootlegacy-workspace' in output and 'after-backup' not in output, output
        report['checks'].append('isolated_complete_volume_rollback')
        # Fault injection at verification must stop the real replacement and
        # leave both original and repaired storage available for investigation.
        fault_backup = root / 'verification-failure-backup'
        original_docker, original_argv = repair_module.docker, sys.argv

        def injected_docker(*arguments, **kwargs):
            if len(arguments) >= 3 and arguments[0] == 'exec' and arguments[2] == 'sha256sum':
                return '0' * 64 + '  /injected-runtime-mismatch\n'
            return original_docker(*arguments, **kwargs)

        repair_module.docker = injected_docker
        sys.argv = ['repair-container.py', '--container', restore_name, '--image', args.repair_image,
                    '--backup-dir', str(fault_backup), '--apply']
        try:
            try:
                repair_module.main()
            except RuntimeError as error:
                assert 'verified msb' in str(error), error
            else:
                raise AssertionError('Injected verification failure was accepted')
        finally:
            repair_module.docker, sys.argv = original_docker, original_argv
        failed = json.loads((fault_backup / 'result.json').read_text())
        containers.append(failed['previous_container_name'])
        assert not failed['passed'] and failed['phase'] == 'verify_replacement'
        assert failed['replacement_stopped']
        replacement = repair_module.inspect(restore_name)
        assert not replacement['State']['Running'] and replacement['State']['Pid'] == 0
        assert replacement['HostConfig']['RestartPolicy']['Name'] == 'no'
        assert not repair_module.inspect(failed['previous_container_name'])['State']['Running']
        report['checks'].append('verification_failure_stops_replacement_and_preserves_backup')
        print('PASS: failed verification stops the replacement and preserves both sides', flush=True)
        report['passed'] = True
        print('PASS: full cold backup restores original version on independent volumes', flush=True)
    finally:
        for name in containers:
            try:
                docker('stop', '--time', '60', name)
                docker('rm', name)
            except subprocess.CalledProcessError:
                pass
        if report['passed']:
            for volume in volumes:
                docker('volume', 'rm', volume)
        report['completed_at'] = datetime.now(timezone.utc).isoformat()
        (root / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({'passed': report['passed'], 'result': str(root / 'result.json')}), flush=True)


if __name__ == '__main__':
    main()
