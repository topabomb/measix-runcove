#!/usr/bin/env python3
"""Verify SQLite mitigation with isolated users, real VMs, idle stops and restart."""
import concurrent.futures
import hashlib
import http.client
import importlib.util
import json
import os
from pathlib import Path
import secrets
import sqlite3
import subprocess
import time
import traceback
import uuid

PROJECT = Path(__file__).resolve().parents[1]
ROOT = PROJECT / 'target/sqlite-runtime-verification'
ROOT.mkdir(parents=True, exist_ok=True)
spec = importlib.util.spec_from_file_location('docker_verify', PROJECT / 'docker/verify.py')
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)
TOKENS = []


def redact(text):
    for token in TOKENS:
        text = text.replace(token, '[REDACTED_SECRET]')
    return text


def docker(*args, timeout=180):
    p = subprocess.run(['docker', *args], text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)
    if p.returncode:
        raise RuntimeError(redact(p.stdout))
    return p.stdout


def trial(label, image, rounds):
    name = 'as-sqlite351-' + label + '-' + uuid.uuid4().hex[:8]
    root = ROOT / name
    root.mkdir()
    volume = name + '-data'
    log_volume = name + '-logs'
    token = secrets.token_urlsafe(32)
    TOKENS.append(token)
    env = 'AGENT_SPACE_ADMIN_TOKEN=' + token
    config = root / 'config.toml'
    config.write_text('''resource_prefix = "sqlite351test"
[server]
listen = "0.0.0.0:8932"
public_base_url = "http://localhost:8932"
max_sessions = 2
mcp_session_timeout_secs = 0
[web_publish]
enabled = true
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
session_timeout_secs = 6
idle_timeout_secs = 20
check_interval_secs = 1
[logging]
level = "info"
[state]
dir = "/var/lib/microsandbox/agent-space-state"
[admin]
token_env = "AGENT_SPACE_ADMIN_TOKEN"
''')
    mount = str(config) + ':/etc/agent-space/config.toml:ro'
    report = {'label': label, 'image': image, 'roundsRequested': rounds, 'roundsCompleted': 0, 'rounds': [], 'success': False}
    started = time.monotonic()
    port = None

    def request(method, path, body=None, auth=True):
        connection = http.client.HTTPConnection('127.0.0.1', port, timeout=35)
        try:
            headers = {'Host': 'localhost:8932'}
            if auth:
                headers['Authorization'] = 'Bearer ' + token
            if body is not None:
                headers['Content-Type'] = 'application/json'
            connection.request(method, path, None if body is None else json.dumps(body), headers)
            response = connection.getresponse()
            raw = response.read()
            return response.status, json.loads(raw) if raw else None
        finally:
            connection.close()

    def detail(username):
        status, body = request('GET', '/admin/v1/users/' + username)
        assert status == 200, (status, body)
        return body

    def healthy():
        status, body = request('GET', '/healthz', auth=False)
        assert status == 200, (status, body)

    def no_deleted_shm():
        deleted = docker('exec', name, 'find', '/proc/1/fd', '-maxdepth', '1', '-type', 'l', '-lname', '*msb.db-shm (deleted)', '-print').strip()
        assert not deleted, 'Service still maps deleted SHM: ' + deleted

    def wait_ready():
        nonlocal port
        mapping = docker('port', name, '8932/tcp').strip()
        port = int(mapping.rsplit(':', 1)[1])
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            try:
                healthy()
                return
            except (OSError, AssertionError, json.JSONDecodeError):
                state = json.loads(docker('inspect', name))[0]['State']
                if not state['Running']:
                    raise RuntimeError('Container exited: ' + redact(docker('logs', name)))
                time.sleep(1)
        raise TimeoutError('Startup did not become healthy')

    def wait_stopped(users):
        deadline = time.monotonic() + 50
        while time.monotonic() < deadline:
            healthy()
            observed = {username: detail(username) for username in users}
            if all(not body.get('stopPending') and body['resources']['runtime']['value'] in ('stopped', 'notfound') for body in observed.values()):
                for body in observed.values():
                    assert body['resources']['memory']['status'] == 'unavailable', body['resources']
                no_deleted_shm()
                return {username: body['resources'] for username, body in observed.items()}
            time.sleep(.5)
        raise TimeoutError('VM stop failed: ' + redact(json.dumps(observed)))

    def connect(username, user):
        status, rotated = request('POST', '/admin/v1/users/' + username + '/token', {'agentSpaceId': user['space']})
        assert status == 200, (status, redact(json.dumps(rotated)))
        TOKENS.append(rotated['token'])
        return verify.Client(port, rotated['token'], username=username)

    try:
        for item in (volume, log_volume):
            docker('volume', 'create', item)
        docker('run', '--rm', '-e', env, '-v', volume + ':/var/lib/microsandbox', '-v', log_volume + ':/var/log/agent-space', '-v', mount, image, 'init-state')
        docker('run', '-d', '--name', name, '--device', '/dev/kvm', '--stop-timeout', '60', '-p', '127.0.0.1::8932', '-e', env, '-v', volume + ':/var/lib/microsandbox', '-v', log_volume + ':/var/log/agent-space', '-v', mount, image)
        wait_ready()
        report['binaryHashes'] = docker('exec', name, 'sha256sum', '/usr/local/bin/agent-space-mcp', '/usr/local/bin/msb', '/var/lib/microsandbox/.microsandbox/bin/msb').strip().splitlines()
        report['runtimeVersion'] = docker('exec', name, 'msb', '--version').strip()
        users = {}
        for username in ('alice', 'bob'):
            status, created = request('POST', '/admin/v1/users', {'username': username})
            assert status == 201, status
            TOKENS.append(created['token'])
            users[username] = {'space': created['agentSpaceId'], 'client': verify.Client(port, created['token'], username=username)}
            assert detail(username)['resources']['runtime']['value'] == 'notfound'
        print(label + ': initialized two isolated users', flush=True)

        for iteration in range(1, rounds + 1):
            for username, user in users.items():
                if iteration > 1 and iteration <= rounds - 2:
                    status, body = request('PATCH', '/admin/v1/users/' + username, {'agentSpaceId': user['space'], 'enabled': True})
                    assert status == 200, (status, body)
                    user['client'] = connect(username, user)
                if iteration == rounds - 1:
                    status, body = request('PATCH', '/admin/v1/users/' + username, {'agentSpaceId': user['space'], 'enabled': True})
                    assert status == 200, (status, body)
                    user['client'] = connect(username, user)
                command = 'test "$(cat /workspace/counter 2>/dev/null || echo 0)" = "' + str(iteration - 1) + '" && printf "' + str(iteration) + '\\n" > /workspace/counter && sync && echo persisted-' + str(iteration)
                output = user['client'].tool('bash', cmd=command)
                assert 'persisted-' + str(iteration) in output, output
            if iteration == 1:
                report['actualVmExecutables'] = docker('exec', name, 'sh', '-c', 'for task_exe in /proc/[0-9]*/exe; do target=$(readlink "$task_exe" 2>/dev/null); case "$target" in */bin/msb) sha256sum "$task_exe";; esac; done').strip().splitlines()
                assert len(report['actualVmExecutables']) >= 2, report['actualVmExecutables']
                expected_hash = report['binaryHashes'][1].split()[0]
                assert all(line.split()[0] == expected_hash for line in report['actualVmExecutables'])
            time.sleep(2)
            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                observations = list(pool.map(detail, ['alice', 'bob'] * 6))
            for body in observations:
                resources = body['resources']
                assert resources['runtime']['value'] == 'running', resources
                assert resources['disk']['status'] == 'current', resources
                assert resources['memory']['status'] in ('current', 'unavailable'), resources
                assert resources['allocation']['memoryLimitBytes']['value'] == 256 * 1024 * 1024, resources
            healthy()
            no_deleted_shm()
            explicit_stop = iteration <= rounds - 2
            if explicit_stop:
                for username, user in users.items():
                    status, body = request('PATCH', '/admin/v1/users/' + username, {'agentSpaceId': user['space'], 'enabled': False})
                    assert status in (200, 202), (status, body)
                stopped = wait_stopped(users)
            else:
                # Continuous resource reads must allow idle reclamation to occur.
                stopped = wait_stopped(users)
            time.sleep(1)
            repeated = {username: detail(username)['resources'] for username in users}
            for username in users:
                assert repeated[username]['runtime']['value'] == 'stopped', repeated[username]
                assert repeated[username]['disk']['status'] == 'historical', repeated[username]
                assert repeated[username]['disk']['observedAt'] == stopped[username]['disk']['observedAt']
            report['rounds'].append({'iteration': iteration, 'stop': 'admin-disable' if explicit_stop else 'idle-with-resource-queries', 'usersStopped': len(users), 'health': 200, 'deletedShm': False})
            report['roundsCompleted'] = iteration
            print(label + ': PASS VM cycle ' + str(iteration) + '/' + str(rounds), flush=True)

        docker('restart', '-t', '60', name)
        wait_ready()
        for username, user in users.items():
            assert detail(username)['agentSpaceId'] == user['space']
            user['client'] = connect(username, user)
            output = user['client'].tool('bash', cmd='cat /workspace/counter')
            assert str(rounds) in output, output
        wait_stopped(users)
        report['restartPreservedSpacesAndWorkspaceCounters'] = True
        report['success'] = True
        print(label + ': PASS restart, credentials, identity and persistent workspace data', flush=True)
    except Exception:
        report['error'] = redact(traceback.format_exc())
        print(label + ': FAIL ' + report['error'], flush=True)
    finally:
        try:
            logs = redact(docker('logs', name))
            (root / 'container.log').write_text(logs)
            report['databaseMalformedLogCount'] = logs.count('database disk image is malformed')
            report['shutdownRequestedLogCount'] = logs.count('ShutdownRequested')
            report['deletedShmBeforeShutdown'] = docker('exec', name, 'find', '/proc/1/fd', '-maxdepth', '1', '-type', 'l', '-lname', '*msb.db-shm (deleted)', '-print').strip()
            docker('stop', '-t', '60', name, timeout=90)
            state = json.loads(docker('inspect', name))[0]['State']
            report['exitCode'] = state['ExitCode']
            report['oomKilled'] = state['OOMKilled']
            copied = root / 'database'
            copied.mkdir()
            docker('cp', name + ':/var/lib/microsandbox/.microsandbox/db/.', str(copied))
            (copied / 'msb.db-shm').unlink(missing_ok=True)
            probe = subprocess.run(['docker', 'run', '--rm', '--user', f'{os.getuid()}:{os.getgid()}',
                                    '--mount', f'type=bind,src={copied},dst=/probe',
                                    '--entrypoint', '/usr/local/bin/agent-space-sqlite-check', image, '/probe/msb.db'],
                                   text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            report['sqlite351IndependentReadOnlyProbe'] = {'exitCode': probe.returncode, 'output': probe.stdout}
            with sqlite3.connect((copied / 'msb.db').as_uri() + '?mode=ro', uri=True) as db:
                report['independentIntegrity'] = db.execute('PRAGMA integrity_check').fetchall()
                report['independentForeignKeyViolations'] = db.execute('PRAGMA foreign_key_check').fetchall()
                report['runStatuses'] = db.execute('SELECT status,count(*) FROM run GROUP BY status').fetchall()
                report['sandboxCount'] = db.execute('SELECT count(*) FROM sandbox').fetchone()[0]
                report['volumeCount'] = db.execute('SELECT count(*) FROM volume').fetchone()[0]
            if report['success']:
                assert probe.returncode == 0
                assert report['independentIntegrity'] == [('ok',)]
                assert not report['independentForeignKeyViolations']
                assert report['runStatuses'] == [('Terminated', rounds * 2 + 2)], report['runStatuses']
                assert report['sandboxCount'] == 2
                assert report['volumeCount'] == 2
                assert report['databaseMalformedLogCount'] == 0
                assert report['exitCode'] == 0 and not report['oomKilled']
        except Exception:
            report['success'] = False
            report['finalizationError'] = redact(traceback.format_exc())
            print(label + ': finalization error ' + report['finalizationError'], flush=True)
        for args in [('rm', '-f', name), ('volume', 'rm', volume, log_volume)]:
            subprocess.run(['docker', *args], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        report['durationSeconds'] = round(time.monotonic() - started, 2)
        report['artifacts'] = str(root)
        (root / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
    return report


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument('--label', required=True)
    parser.add_argument('--image', required=True)
    parser.add_argument('--rounds', type=int, default=10)
    args = parser.parse_args()
    if args.rounds < 2 or not args.label.replace('-', '').isalnum():
        parser.error('Use at least two rounds and an alphanumeric label')
    report = trial(args.label, args.image, args.rounds)
    print(json.dumps({key: report[key] for key in ('label', 'success', 'roundsCompleted', 'durationSeconds', 'artifacts')}), flush=True)
    raise SystemExit(0 if report['success'] else 1)
