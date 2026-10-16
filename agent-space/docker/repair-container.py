#!/usr/bin/env python3
"""Cold-back up and replace a container with a verified runtime-only repair.

Linux/WSL, stdlib only. Credentials stay in memory and are transferred through
the Docker API. No database repair, user migration or command replay occurs.
"""
import argparse
import copy
from datetime import datetime, timezone
import hashlib
import http.client
import json
import os
from pathlib import Path
import posixpath
import socket
import subprocess
import time
import tomllib
from urllib.parse import quote, urlsplit


def docker(*args, timeout=180, binary=False, combine_output=False):
    process = subprocess.run(['docker', *args], text=not binary, stdout=subprocess.PIPE,
                             stderr=subprocess.STDOUT if combine_output else subprocess.PIPE, timeout=timeout)
    if process.returncode:
        error = process.stdout if combine_output else process.stderr
        if binary:
            error = error.decode(errors='replace')
        raise RuntimeError(error.strip() or 'Docker command failed')
    return process.stdout


def inspect(name):
    return json.loads(docker('inspect', name))[0]


class DockerConnection(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__('localhost', timeout=120)
        self.path = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)


def docker_endpoint():
    # Match Docker CLI endpoint selection, including explicit DOCKER_HOST.
    endpoint = os.environ.get('DOCKER_HOST') if not os.environ.get('DOCKER_CONTEXT') else None
    endpoint = endpoint or docker('context', 'inspect', '--format', '{{.Endpoints.docker.Host}}').strip()
    if not endpoint.startswith('unix://'):
        raise RuntimeError('Use this tool on the Linux/WSL Docker host with its Unix socket')
    return endpoint[7:]


def create_clone(old, image, name):
    endpoint = docker_endpoint()
    config = copy.deepcopy(old['Config'])
    host = copy.deepcopy(old['HostConfig'])
    config['Image'] = image['Id']
    config['Entrypoint'] = image['Config']['Entrypoint']
    # Preserve custom container labels, while retaining the repair-image identity.
    previous_labels = json.loads(docker('image', 'inspect', old['Image']))[0]['Config'].get('Labels') or {}
    custom = {key: value for key, value in (config.get('Labels') or {}).items()
              if previous_labels.get(key) != value}
    config['Labels'] = {**(image['Config'].get('Labels') or {}), **custom}
    host['Binds'] = None
    # Materialize resolved mounts; avoid stale/self-referential VolumesFrom chains.
    host['VolumesFrom'] = None
    host['Mounts'] = []
    for mount in old['Mounts']:
        value = {'Type': mount['Type'], 'Source': mount.get('Name') if mount['Type'] == 'volume' else mount['Source'],
                 'Target': mount['Destination'], 'ReadOnly': not mount['RW']}
        if mount['Type'] == 'volume':
            value['VolumeOptions'] = {'NoCopy': True}
        else:
            value['BindOptions'] = {'Propagation': mount.get('Propagation') or 'rprivate'}
        host['Mounts'].append(value)
    config['HostConfig'] = host
    config['NetworkingConfig'] = {'EndpointsConfig': {
        network: {key: value for key, value in settings.items()
                  if key in ('IPAMConfig', 'Links', 'Aliases', 'DriverOpts', 'GwPriority') and value is not None}
        for network, settings in old['NetworkSettings']['Networks'].items()
    }}
    connection = DockerConnection(endpoint)
    try:
        connection.request('POST', '/containers/create?name=' + quote(name, safe=''),
                           json.dumps(config), {'Content-Type': 'application/json'})
        response = connection.getresponse()
        body = json.loads(response.read())
        if response.status != 201:
            raise RuntimeError('Docker container creation failed; status ' + str(response.status))
        return body['Id']
    finally:
        connection.close()


def private_write(path, value):
    path.write_text(value)
    path.chmod(0o600)


def mounted_probe(image, directory):
    return docker('run', '--rm', '--user', f'{os.getuid()}:{os.getgid()}',
                  '--mount', f'type=bind,src={directory},dst=/probe',
                  '--entrypoint', '/usr/local/bin/agent-space-sqlite-check',
                  image, '/probe/msb.db', combine_output=True)


def cold_archives(original, image, backup):
    """Archive the two standard storage mounts only after all processes stop."""
    stopped = inspect(original['Id'])['State']
    if stopped['Running'] or stopped['Pid'] != 0:
        raise RuntimeError('Cold archives require a fully stopped container')
    destinations = {mount['Destination']: mount for mount in original['Mounts']}
    archives = {}
    read_mounts = []
    for destination in ('/var/lib/microsandbox', '/var/log/agent-space'):
        if destination not in destinations:
            continue
        mount = destinations[destination]
        source = mount.get('Name') if mount['Type'] == 'volume' else mount['Source']
        value = f'type={mount["Type"]},src={source},dst={destination},readonly'
        if mount['Type'] == 'volume':
            value += ',volume-nocopy'
        read_mounts.extend(['--mount', value])
    for mount_path, filename in (('/var/lib/microsandbox', 'data.tar.gz'), ('/var/log/agent-space', 'logs.tar.gz')):
        if mount_path not in destinations:
            continue
        command = f'tar --sparse -czf /backup/{filename} -C {mount_path} . && tar -tzf /backup/{filename} >/dev/null && chmod 600 /backup/{filename} && chown {os.getuid()}:{os.getgid()} /backup/{filename} && sha256sum /backup/{filename}'
        value = docker('run', '--rm', *read_mounts,
                       '--mount', f'type=bind,src={backup},dst=/backup',
                       '--entrypoint', '/bin/sh', image, '-c', command, timeout=1800)
        archives[filename] = value.split()[0]
    private_write(backup / 'SHA256SUMS', ''.join(f'{digest}  {filename}\n' for filename, digest in archives.items()))
    return archives, read_mounts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--container', required=True)
    parser.add_argument('--image', required=True)
    parser.add_argument('--backup-dir', type=Path, required=True)
    parser.add_argument('--apply', action='store_true', help='Stop, back up, validate and replace; default only prints the plan')
    parser.add_argument('--remove-old', action='store_true', help='Remove the stopped old container only after verification and restart pass')
    args = parser.parse_args()
    original = inspect(args.container)
    image = json.loads(docker('image', 'inspect', args.image))[0]
    labels = image['Config'].get('Labels') or {}
    if labels.get('io.agent-space.runtime-repair-base') != original['Image']:
        raise SystemExit('Repair image does not match this container base image')
    if original['Config']['Entrypoint'] != ['docker-entrypoint.sh']:
        raise SystemExit('Custom entrypoint requires separate review')
    environment = dict(item.split('=', 1) for item in original['Config']['Env'])
    if environment.get('HOME') != '/var/lib/microsandbox' or environment.get('MSB_HOME', '/var/lib/microsandbox/.microsandbox') != '/var/lib/microsandbox/.microsandbox':
        raise SystemExit('Custom SDK storage location requires a separate complete-backup procedure')
    if original['HostConfig']['AutoRemove'] or original['HostConfig']['VolumesFrom'] and not original['Mounts']:
        raise SystemExit('Unsupported container lifecycle or unresolved mounts')
    destinations = {mount['Destination']: mount for mount in original['Mounts']}
    for destination in ('/var/lib/microsandbox', '/etc/agent-space/config.toml'):
        if destination not in destinations:
            raise SystemExit('Missing persistent mount: ' + destination)
    if len(destinations) != len(original['Mounts']):
        raise SystemExit('Duplicate mount destinations require separate review')
    for mount in original['Mounts']:
        target = posixpath.normpath(mount['Destination'])
        if any(target.startswith(parent + '/') for parent in ('/var/lib/microsandbox', '/var/log/agent-space', '/etc/agent-space/config.toml')):
            raise SystemExit('Nested storage mounts require a separate complete-backup procedure')
    if original['HostConfig'].get('ContainerIDFile') or original['HostConfig'].get('NetworkMode', '').startswith('container:') or original['HostConfig'].get('IpcMode', '').startswith('container:'):
        raise SystemExit('Container identity or shared namespaces require separate review')
    docker_endpoint()  # Reject unsupported endpoints before changing the deployment.
    if any(mount['Type'] not in ('volume', 'bind') for mount in original['Mounts']):
        raise SystemExit('Unsupported mount type; review it before replacing the container')
    if original['HostConfig'].get('PidMode') not in ('', 'private'):
        raise SystemExit('Shared PID namespaces require separate VM shutdown checks')
    # A second live container sharing storage invalidates a cold backup.
    for other_id in docker('ps', '-q').split():
        other = inspect(other_id)
        if other['Id'] == original['Id']:
            continue
        data = destinations['/var/lib/microsandbox']
        if any(mount.get('Name') == data.get('Name') and data.get('Name') or
               mount['Source'] == data['Source'] for mount in other['Mounts']):
            raise SystemExit('Another live container shares the data mount: ' + other['Name'])
    plan = {'container': args.container, 'old_image_id': original['Image'],
            'new_image_id': image['Id'], 'backup_dir': str(args.backup_dir.resolve()),
            'mounts_preserved': [{key: mount.get(key) for key in ('Type', 'Name', 'Source', 'Destination', 'RW')}
                                 for mount in original['Mounts']],
            'ports_preserved': original['HostConfig']['PortBindings'],
            'restart_policy_preserved': original['HostConfig']['RestartPolicy'],
            'credentials': 'transferred in memory; never printed or written to an env file',
            'operations': ['stop and verify no processes remain', 'cold backup data/logs/config',
                           'check an independent database copy', 'reuse original mounts and credentials',
                           'verify health, raw user disk hashes and restart']}
    if not args.apply:
        print(json.dumps(plan, indent=2))
        return
    backup = args.backup_dir.resolve()
    if backup.exists():
        raise SystemExit('Use a new backup directory; existing backups are never overwritten')
    backup.mkdir(parents=True, mode=0o700)
    backup.chmod(0o700)
    private_write(backup / 'plan.json', json.dumps(plan, indent=2) + '\n')
    started = datetime.now(timezone.utc).isoformat()
    result = {'started_at': started, 'old_container_id': original['Id'],
              'old_image_id': original['Image'], 'new_image_id': image['Id'], 'passed': False}
    private_write(backup / 'result.json', json.dumps(result, indent=2) + '\n')
    new_id = None
    try:
        result['phase'] = 'stop_original'
        print('Stopping original container; no commands will be replayed.', flush=True)
        docker('update', '--restart', 'no', original['Id'])
        docker('stop', '--time', '60', original['Id'], timeout=90)
        stopped = inspect(original['Id'])['State']
        if stopped['Running'] or stopped['Pid'] != 0:
            raise RuntimeError('Container still has live processes; do not back up or restart')
        result['old_exit_code'] = stopped['ExitCode']
        result['old_oom_killed'] = stopped['OOMKilled']
        private_write(backup / 'container.log', docker('logs', original['Id'], combine_output=True))
        docker('cp', original['Id'] + ':/etc/agent-space/config.toml', str(backup / 'config.toml'))
        (backup / 'config.toml').chmod(0o600)
        config_bytes = (backup / 'config.toml').read_bytes()
        config = tomllib.loads(config_bytes.decode())
        state_path = config.get('state', {}).get('dir')
        if state_path and not posixpath.normpath(state_path).startswith('/var/lib/microsandbox/'):
            raise RuntimeError('StateStore is outside the data mount; preserve this stopped container and back up its full storage separately')
        result['config_sha256'] = hashlib.sha256(config_bytes).hexdigest()
        result['phase'] = 'cold_backup'
        print('Creating and checking complete sparse cold backups.', flush=True)
        archives, read_mounts = cold_archives(original, image['Id'], backup)
        result['backup_sha256'] = archives
        if stopped['ExitCode'] != 0 or stopped['OOMKilled']:
            raise RuntimeError('Shutdown was not clean. The cold backup is preserved; verify VM/filesystem state manually before replacing the container.')
        # This is a disposable copy. Preserve the original DB, WAL and SHM untouched.
        result['phase'] = 'check_database'
        probe = backup / 'database-probe'
        probe.mkdir(mode=0o700)
        docker('cp', original['Id'] + ':/var/lib/microsandbox/.microsandbox/db/.', str(probe))
        (probe / 'msb.db-shm').unlink(missing_ok=True)
        precheck = mounted_probe(image['Id'], probe)
        private_write(backup / 'database-before.txt', precheck)
        if 'active_runs=0\n' not in precheck:
            raise RuntimeError('Runtime catalog still has active runs. Preserve the backup and reconcile them manually before restart.')
        print('Database copy passed. Recording both root and workspace disk hashes.', flush=True)
        hash_command = "set -euo pipefail; cd /var/lib/microsandbox && find .microsandbox/volumes .microsandbox/sandboxes -type f \\( -name disk.raw -o -name upper.ext4 \\) -print0 | sort -z | xargs -0 -r sha256sum"
        before_disks = docker('run', '--rm', *read_mounts,
                              '--entrypoint', '/bin/bash', image['Id'], '-c', hash_command, timeout=1800)
        private_write(backup / 'disks-before.sha256', before_disks)
        state_before = None
        if state_path:
            state_file = backup / 'state-before.json'
            docker('cp', original['Id'] + ':' + state_path + '/state.json', str(state_file))
            state_file.chmod(0o600)
            state_before = state_file.read_bytes()
        previous_name = args.container + '-pre-sqlite351-' + datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
        docker('rename', original['Id'], previous_name)
        result['previous_container_name'] = previous_name
        private_write(backup / 'result.json', json.dumps(result, indent=2) + '\n')
        result['phase'] = 'replace_container'
        new_id = create_clone(original, image, args.container)
        result['new_container_id'] = new_id
        private_write(backup / 'result.json', json.dumps(result, indent=2) + '\n')
        docker('start', new_id)

        def verify_health():
            deadline = time.monotonic() + 180
            while time.monotonic() < deadline:
                current = inspect(new_id)
                if not current['State']['Running']:
                    raise RuntimeError('Replacement container stopped; preserve both containers and backup')
                try:
                    docker('exec', new_id, 'curl', '-fsS', '--max-time', '5', 'http://127.0.0.1:8932/healthz')
                    return
                except RuntimeError:
                    time.sleep(1)
            raise RuntimeError('Replacement did not become healthy; do not restart the old writer automatically')

        result['phase'] = 'verify_replacement'
        verify_health()
        print('Health recovered. Verifying mounts, credentials and preserved data.', flush=True)
        current = inspect(new_id)
        if docker('exec', new_id, 'cat', '/etc/agent-space/config.toml', binary=True) != config_bytes:
            raise RuntimeError('Configuration file changed')
        if current['Config']['Env'] != original['Config']['Env']:
            raise RuntimeError('Environment changed, including possible credentials')
        if current['HostConfig']['PortBindings'] != original['HostConfig']['PortBindings']:
            raise RuntimeError('Port bindings changed')
        actual_mounts = [{key: mount.get(key) for key in ('Type', 'Name', 'Source', 'Destination', 'RW')}
                         for mount in current['Mounts']]
        if sorted(actual_mounts, key=lambda m: m['Destination']) != sorted(plan['mounts_preserved'], key=lambda m: m['Destination']):
            raise RuntimeError('Persistent mounts changed')
        manifest = json.loads(docker('exec', new_id, 'cat', '/opt/agent-space/runtime.json'))
        hashes = docker('exec', new_id, 'sha256sum', '/usr/local/bin/msb', '/var/lib/microsandbox/.microsandbox/bin/msb')
        if any(line.split()[0] != manifest['msb_sha256'] for line in hashes.splitlines()):
            raise RuntimeError('Cached or image runtime does not match the verified msb')
        after_disks = docker('exec', new_id, '/bin/bash', '-c', hash_command, timeout=1800)
        private_write(backup / 'disks-after.sha256', after_disks)
        if before_disks != after_disks:
            raise RuntimeError('A user disk changed during metadata-only repair; preserve the backup for investigation')
        if state_before is not None:
            state_after_file = backup / 'state-after.json'
            docker('cp', new_id + ':' + state_path + '/state.json', str(state_after_file))
            state_after_file.chmod(0o600)
            if state_before != state_after_file.read_bytes():
                raise RuntimeError('User state or credentials changed')
        configured_origin = config.get('server', {}).get('public_base_url')
        if configured_origin and config.get('admin', {}).get('token_env'):
            environment = dict(item.split('=', 1) for item in original['Config']['Env'])
            token = environment.get(config['admin']['token_env'])
            port = int(current['NetworkSettings']['Ports']['8932/tcp'][0]['HostPort'])
            connection = http.client.HTTPConnection('127.0.0.1', port, timeout=30)
            try:
                connection.request('GET', '/admin/v1/users', headers={'Authorization': 'Bearer ' + token, 'Host': urlsplit(configured_origin).netloc})
                response = connection.getresponse()
                if response.status != 200:
                    raise RuntimeError('Original administrator credential failed')
                result['admin_auth_verified'] = True
                result['admin_list_shape'] = type(json.loads(response.read())).__name__
            finally:
                connection.close()
        docker('restart', '--time', '60', new_id, timeout=90)
        verify_health()
        time.sleep(2)
        current = inspect(new_id)
        if current['State']['OOMKilled']:
            raise RuntimeError('Replacement was OOM-killed')
        if 'database disk image is malformed' in docker('logs', new_id, combine_output=True):
            raise RuntimeError('Replacement still reports SQLite corruption')
        private_write(backup / 'container-after.log', docker('logs', new_id, combine_output=True))
        result.update({'passed': True, 'credentials_preserved': True, 'config_preserved': True,
                       'user_state_preserved': True, 'state_store_present': state_before is not None,
                       'disk_hashes_preserved': True,
                       'disk_files': len(before_disks.splitlines()), 'restart_verified': True,
                       'runtime': manifest, 'health': 'HTTP 200', 'completed_at': datetime.now(timezone.utc).isoformat()})
        result['phase'] = 'complete'
        if args.remove_old:
            docker('rm', original['Id'])  # never -v: backup and original volumes remain
            result['old_container_removed'] = True
        private_write(backup / 'result.json', json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2))
    except Exception as error:
        result.update(passed=False, error=str(error), failed_at=datetime.now(timezone.utc).isoformat())
        if new_id:
            try:
                docker('update', '--restart', 'no', new_id)
                docker('stop', '--time', '60', new_id, timeout=90)
                stopped_new = inspect(new_id)['State']
                result['replacement_stopped'] = not stopped_new['Running'] and stopped_new['Pid'] == 0
                private_write(backup / 'container-after.log', docker('logs', new_id, combine_output=True))
            except Exception as stop_error:
                result['replacement_stop_error'] = str(stop_error)
        private_write(backup / 'result.json', json.dumps(result, indent=2) + '\n')
        raise


if __name__ == '__main__':
    main()
