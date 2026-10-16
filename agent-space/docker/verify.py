#!/usr/bin/env python3
"""Verify the current Agent Space Docker image with disposable data volumes.

The suite covers the public MCP contract, configured/tool timeouts, in-flight
reclamation fencing, stop/start persistence, 0.7.x pause/resume, container
restart and dynamic account management. The released static-user upgrade and backup rollback
are exercised separately by verify-static-upgrade.py.
"""
import argparse
import concurrent.futures
import http.client
import json
import re
from pathlib import Path
import subprocess
import time
import tomllib
import uuid
from urllib.parse import urlsplit


PROJECT = Path(__file__).resolve().parents[1]
PROTOCOL_VERSIONS = ('2025-11-25', '2025-06-18', '2025-03-26')


def locked_msb_version():
    lock = tomllib.loads((PROJECT / 'Cargo.lock').read_text())
    return next(p['version'] for p in lock['package'] if p['name'] == 'microsandbox')


def docker(*args, timeout=180):
    return subprocess.check_output(
        ['docker', *args], text=True, stderr=subprocess.STDOUT, timeout=timeout
    )


class Client:
    def __init__(self, port, token='docker-verification-token', username='verify', endpoint=None,
                 authority='localhost:8932', *, protocol_version=PROTOCOL_VERSIONS[0], expected_version=None):
        self.port, self.token, self.session, self.sequence = port, token, None, 0
        self.protocol_version = None
        self.authority = authority
        self.endpoint = endpoint or f'/u/{username}/mcp'
        result = self.request('initialize', {
            'protocolVersion': protocol_version,
            'capabilities': {},
            'clientInfo': {'name': 'docker-verification', 'version': '1'},
        })
        negotiated = result.get('protocolVersion')
        assert negotiated == (expected_version or protocol_version), (
            'unexpected negotiated protocol version', protocol_version, negotiated,
        )
        self.protocol_version = negotiated
        assert self.session, 'missing MCP session ID'
        self.request('notifications/initialized', notification=True)

    def request(self, method, params=None, notification=False):
        self.sequence += 1
        payload = {'jsonrpc': '2.0', 'method': method}
        if params is not None:
            payload['params'] = params
        if not notification:
            payload['id'] = self.sequence
        headers = {
            'Authorization': 'Bearer ' + self.token,
            'Host': self.authority,
            'Content-Type': 'application/json',
            'Accept': 'application/json, text/event-stream',
        }
        if self.protocol_version:
            headers['MCP-Protocol-Version'] = self.protocol_version
        if self.session:
            headers['Mcp-Session-Id'] = self.session
        connection = http.client.HTTPConnection('127.0.0.1', self.port, timeout=90)
        try:
            connection.request('POST', self.endpoint, json.dumps(payload), headers)
            response = connection.getresponse()
            self.session = response.getheader('Mcp-Session-Id') or self.session
            if notification:
                assert response.status == 202, ('notification HTTP status', response.status)
                assert not response.read(), 'notification response must have no body'
                return None
            assert response.status == 200, ('request HTTP status', response.status)
            content_type = response.getheader('Content-Type', '').split(';', 1)[0].strip().lower()
            assert content_type in ('application/json', 'text/event-stream'), 'unsupported MCP response content type'
            if content_type == 'text/event-stream':
                while line := response.readline():
                    if line.startswith(b'data:') and line[5:].strip():
                        value = json.loads(line[5:])
                        if value.get('id') == self.sequence:
                            break
                else:
                    raise AssertionError('SSE ended without JSON-RPC response')
            else:
                value = json.load(response)
            assert (isinstance(value, dict) and value.get('jsonrpc') == '2.0'
                    and value.get('id') == self.sequence and not isinstance(value.get('id'), bool)), 'invalid JSON-RPC response envelope'
            assert 'error' not in value, value
            return value['result']
        finally:
            connection.close()

    def tool_result(self, tool_name, **arguments):
        return self.request('tools/call', {'name': tool_name, 'arguments': arguments})

    def tool(self, tool_name, **arguments):
        result = self.tool_result(tool_name, **arguments)
        assert not result.get('isError'), result
        return '\n'.join(item.get('text', '') for item in result.get('content', []))

    def tool_error(self, tool_name, **arguments):
        result = self.tool_result(tool_name, **arguments)
        assert result.get('isError'), result
        return '\n'.join(item.get('text', '') for item in result.get('content', []))


def main():
    version = locked_msb_version()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    parser.add_argument(
        '--idle-seconds', type=int, default=310,
        help='Keep the original MCP session idle past the former 300-second expiry',
    )
    args = parser.parse_args()
    # A build may move the supplied tag while a long qualification is running.
    # Every fixture and the resulting report must use this one immutable ID.
    args.image = docker('image', 'inspect', args.image, '--format', '{{.Id}}').strip()
    run = 'as-verify-' + uuid.uuid4().hex[:10]
    root = PROJECT / 'target' / 'docker-verification' / run
    root.mkdir(parents=True)
    containers, volumes, checks = [], [], []
    result = {"target":"server","image":args.image,"checks":checks,"passed":False}

    def passed(message):
        checks.append(message)
        print('PASS: ' + message, flush=True)

    def launch(volume, suffix):
        name = run + '-' + suffix
        config = root / (suffix + '.toml')
        config.write_text(f'''resource_prefix = "dockerverify"
[webdav]
enabled = false
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
exec_timeout_secs = 2
max_tcp_connections = 128
max_udp_connections = 64
[lifecycle]
session_timeout_secs = 6
idle_timeout_secs = 10
check_interval_secs = 1
[logging]
level = "info"
[users.verify]
token = "docker-verification-token"
workspace_name = "dockerverify-workspace"
''')
        docker(
            'run', '-d', '--name', name, '--device', '/dev/kvm',
            '--stop-timeout', '60', '-p', '127.0.0.1::8932',
            '-v', volume + ':/var/lib/microsandbox',
            '-v', str(config) + ':/etc/agent-space/config.toml:ro', args.image,
        )
        containers.append(name)
        return name, wait_ready(name)

    def wait_ready(name, token='docker-verification-token'):
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            state = json.loads(docker('inspect', name))[0]
            assert state['State']['Running'], docker('logs', name)
            if 'listening on' in docker('logs', '--since', state['State']['StartedAt'], name):
                port = int(state['NetworkSettings']['Ports']['8932/tcp'][0]['HostPort'])
                connection = http.client.HTTPConnection('127.0.0.1', port, timeout=5)
                try:
                    connection.request('GET', '/healthz', headers={'Host': 'localhost:8932'})
                    response = connection.getresponse()
                    assert response.status == 200, response.status
                    response.read()
                finally:
                    connection.close()
                return port if token is None else Client(port, token)
            time.sleep(1)
        raise AssertionError('startup timeout: ' + docker('logs', name))

    def stop(name):
        docker('stop', '--time', '60', name, timeout=90)
        state = json.loads(docker('inspect', name))[0]['State']
        assert state['ExitCode'] == 0 and not state['OOMKilled'], state

    def wait_workspace_status(name, expected, timeout=60):
        deadline = time.monotonic() + timeout
        state = ''
        while time.monotonic() < deadline:
            state = docker('exec', name, 'msb', 'list')
            lowered = state.lower()
            if 'dockerverify-workspace' in state and expected in lowered:
                return
            time.sleep(1)
        raise AssertionError(f'workspace did not become {expected}: {state}')

    def pause_then_call(name, client, tool, **arguments):
        docker('exec', name, 'msb', 'pause', '-q', 'dockerverify-workspace')
        wait_workspace_status(name, 'paused')
        result = client.tool(tool, **arguments)
        wait_workspace_status(name, 'running')
        return result

    try:
        volume = run + '-fresh-data'
        docker('volume', 'create', volume)
        volumes.append(volume)
        name, client = launch(volume, 'fresh')
        negotiated_versions = []
        for requested in PROTOCOL_VERSIONS:
            protocol_client = client if requested == client.protocol_version else Client(client.port, protocol_version=requested)
            definitions = protocol_client.request('tools/list')['tools']
            assert len(definitions) == 5 and {tool['name'] for tool in definitions} == {'bash', 'read', 'write', 'edit', 'web_publish'}
            assert all(tool.get('description') and tool['inputSchema'].get('type') == 'object'
                       and isinstance(tool['inputSchema'].get('properties'), dict) for tool in definitions)
            protocol_client.request('ping')
            negotiated_versions.append({'requested': requested, 'negotiated': protocol_client.protocol_version})
        passed('three protocol versions negotiate exactly and discover complete tools in their HTTP sessions')
        assert version in docker('exec', name, 'msb', '--version')
        assert 'dockerverify-workspace' not in docker('exec', name, 'msb', 'list')
        tools = client.request('tools/list')['tools']
        assert {t['name'] for t in tools} == {'bash', 'read', 'write', 'edit', 'web_publish'}
        bash_schema = next(t['inputSchema'] for t in tools if t['name'] == 'bash')
        assert 'timeout_secs' in bash_schema['properties']
        assert 'timeout' not in bash_schema['properties']
        assert 'seconds' in bash_schema['properties']['timeout_secs']['description']
        assert 'dockerverify-workspace' not in docker('exec', name, 'msb', 'list')
        assert 'agent-space-static-http' in docker('exec', name, 'sh', '-c', 'command -v agent-space-static-http')
        passed('image/version, helper, and five-tool contract are exact; metadata does not wake VM')

        client.tool('bash', command='mkdir -p /workspace/report && printf "<p>published-site</p>" > /workspace/report/index.html && printf "selected-file" > /workspace/report/data.txt')
        published = json.loads(client.tool('web_publish', action='publish', name='site', directory='/workspace/report', keep_alive_minutes=1))
        published_at = time.monotonic()
        assert set(published) == {'name', 'type', 'url', 'username', 'password'}
        assert published['type']=='directory' and re.fullmatch(r'web[0-9]{2}', published['username'])
        assert len(published['password'])==6 and published['password'].isdigit()
        token = __import__('base64').b64encode((published['username']+':'+published['password']).encode()).decode()
        def published_get(item, headers=None, path=None, method='GET'):
            connection = http.client.HTTPConnection('127.0.0.1', client.port, timeout=20)
            try:
                connection.request(method, path or item['url'], headers=headers or {})
                response = connection.getresponse()
                return response.status, response.read(), dict(response.getheaders())
            finally:
                connection.close()
        status, _, challenge = published_get(published)
        assert status==401 and any(k.lower()=='www-authenticate' for k in challenge)
        assert re.fullmatch(r'Basic realm="pub_[a-f0-9]{32}", charset="UTF-8"', next(v for k,v in challenge.items() if k.lower()=="www-authenticate"))
        status, _, redirect = published_get(published, path=published['url'].rstrip('/')+'?x=%2F')
        assert status==308 and redirect['location']==urlsplit(published['url']).path+'?x=%2F'
        assert not any(k.lower()=='www-authenticate' for k in redirect)
        status, body, _ = published_get(published, {'Authorization':'Basic '+token})
        assert status==200 and b'published-site' in body
        # An idle VM must survive its 6-second session timeout while published.
        time.sleep(9)
        wait_workspace_status(name,'running')
        got=json.loads(client.tool('web_publish',action='get',name='site'))
        assert got==published, 'get must not extend keepalive'
        # No tools or public visits until the finite keepalive has actually ended.
        time.sleep(max(0, published_at + 63 - time.monotonic()))
        wait_workspace_status(name,'stopped')
        assert published_get(published, {'X-Publish-Key':published['password']})[0]==404
        passed('finite keepalive protects idle VM, get does not extend it, expiry reclaims VM and invalidates URL')
        client.tool('bash', command='echo runtime-restarted')
        old=published
        published=json.loads(client.tool('web_publish',action='publish',name='site',directory='/workspace/report',keep_alive_minutes=1))
        assert published['url']==old['url'] and published_get(old,{'X-Publish-Key':old['password']})[0]==401
        file=json.loads(client.tool('web_publish',action='publish',name='file',file='/workspace/report/data.txt',keep_alive_minutes=1))
        assert published_get(file,{'X-Publish-Key':file['password']})[:2]==(200,b'selected-file')
        assert published_get(file,{'X-Publish-Key':file['password'],'Range':'bytes=2-5'})[:2]==(206,b'lect')
        status,body,headers=published_get(file,{'X-Publish-Key':file['password']},method='HEAD')
        assert status==200 and not body and next((v for k,v in headers.items() if k.lower()=='content-length'),None)=='13'
        assert published_get(file,{'X-Publish-Key':file['password']},path=file['url']+'index.html')[0]==404
        assert published_get(file,{'Authorization':'Basic '+token})[0]==401
        client.tool('bash',command='mkdir -p /workspace/report/sub; printf nested > /workspace/report/sub/item.txt')
        status,_,redirect=published_get(published,{'X-Publish-Key':published['password']},path=published['url']+'sub?q=1')
        assert status==307 and redirect['location']==urlsplit(published['url']).path+'sub/?q=1'
        client.tool('bash',command='mv /workspace/report/data.txt /workspace/report/data.original; mkdir /workspace/report/data.txt; printf forbidden > /workspace/report/data.txt/index.html')
        assert published_get(file,{'X-Publish-Key':file['password']})[0]==404
        client.tool('bash',command='rmdir /workspace/report/data.txt 2>/dev/null || { rm /workspace/report/data.txt/index.html; rmdir /workspace/report/data.txt; }; mv /workspace/report/data.original /workspace/report/data.txt')

        api_source = """from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import functools, hashlib, json
class App(SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path=='/headers':
            body=json.dumps(dict(self.headers)).encode()
            self.send_response(200)
            self.send_header('Set-Cookie','app=fixture; Domain=localhost; Path=/api; HttpOnly')
            self.send_header('Service-Worker-Allowed','/')
            self.send_header('Clear-Site-Data','"storage"')
            self.send_header('Content-Length',str(len(body))); self.end_headers(); self.wfile.write(body)
        elif self.path=='/redirect':
            self.send_response(302); self.send_header('Location','/login'); self.send_header('Content-Length','0'); self.end_headers()
        else: super().do_GET()
    def do_POST(self):
        remaining=int(self.headers['Content-Length']); digest=hashlib.sha256(); total=remaining
        while remaining:
            data=self.rfile.read(min(65536,remaining))
            if not data: break
            digest.update(data); remaining-=len(data)
        body=json.dumps({'sha256':digest.hexdigest(),'length':total-remaining,'path':self.path,'authorization':self.headers.get('Authorization'),'publish_key_present':'X-Publish-Key' in self.headers}).encode()
        self.send_response(200); self.send_header('Content-Length',str(len(body))); self.end_headers(); self.wfile.write(body)
ThreadingHTTPServer(('127.0.0.1',38080),functools.partial(App,directory='/workspace/report')).serve_forever()
"""
        client.tool('write',path='/workspace/api.py',content=api_source)
        client.tool('bash',command='python3 /workspace/api.py >/tmp/agent-port-http.log 2>&1 < /dev/null & echo port-server-started')
        for attempt in range(20):
            try:
                port_publication=json.loads(client.tool('web_publish',action='publish',name='api',port=38080,keep_alive_minutes=1))
                break
            except AssertionError:
                if attempt==19: raise
                time.sleep(.2)
        assert len({published['url'],file['url'],port_publication['url']})==3
        assert published_get(port_publication,{'X-Publish-Key':port_publication['password']})[0]==200
        assert len({published['username'],file['username'],port_publication['username']})==3
        status,body,headers=published_get(port_publication,{'X-Publish-Key':port_publication['password'],'X-Forwarded-Prefix':'/spoof'},path=port_publication['url']+'headers')
        forwarded={k.lower():v for k,v in json.loads(body).items()}
        headers={k.lower():v for k,v in headers.items()}
        prefix=urlsplit(port_publication['url']).path
        assert status==200 and forwarded['x-forwarded-prefix']==prefix.rstrip('/')
        assert headers['set-cookie']=='app=fixture; HttpOnly; Path='+prefix+'api'
        assert 'service-worker-allowed' not in headers and 'clear-site-data' not in headers
        status,_,headers=published_get(port_publication,{'X-Publish-Key':port_publication['password']},path=port_publication['url']+'redirect')
        assert status==302 and headers['location']=='/login', 'application redirects are not rewritten'

        payload=bytes(range(256))*16384
        connection=http.client.HTTPConnection('127.0.0.1',client.port,timeout=30)
        connection.request('POST',port_publication['url']+'upload?from=android',body=payload,headers={'X-Publish-Key':port_publication['password'],'Authorization':'Bearer application-test-token','Content-Type':'application/octet-stream'})
        response=connection.getresponse(); upload=json.loads(response.read()); assert response.status==200; connection.close()
        assert upload=={'sha256':__import__('hashlib').sha256(payload).hexdigest(),'length':len(payload),'path':'/upload?from=android','authorization':'Bearer application-test-token','publish_key_present':False}
        client.tool('bash',command="python3 -c \"open('/workspace/report/blob.bin','wb').write(bytes(range(256))*16384)\"")
        large=json.loads(client.tool('web_publish',action='publish',name='large',file='/workspace/report/blob.bin',keep_alive_minutes=1))
        status,download,_=published_get(large,{'X-Publish-Key':large['password']})
        assert status==200 and __import__('hashlib').sha256(download).digest()==__import__('hashlib').sha256(payload).digest()
        client.tool('web_publish',action='unpublish',name='large')
        passed('real VM 4 MiB upload/download hashes match; app Authorization and query survive, publication key stays on Host')
        listed=json.loads(client.tool('web_publish',action='list'))
        assert {item['name'] for item in listed}=={'site','file','api'} and all('password' not in item for item in listed)
        same=json.loads(client.tool('web_publish',action='publish',name='api',port=38080,keep_alive_minutes=1))
        assert same['url']==port_publication['url'] and same['password']==port_publication['password']
        # Same name is a stable authenticated slot; prepare first, then switch.
        original_realm=next(v for k,v in published_get(port_publication)[2].items() if k.lower()=='www-authenticate')
        for target,kind,content in [({'file':'/workspace/report/data.txt'},'file',b'selected-file'),({'directory':'/workspace/report'},'directory',b'published-site'),({'port':38080},'port',b'published-site')]:
            updated=json.loads(client.tool('web_publish',action='publish',name='api',keep_alive_minutes=1,**target))
            assert all(updated[k]==port_publication[k] for k in ('url','username','password'))
            assert updated['type']==kind
            assert content in published_get(updated,{'X-Publish-Key':updated['password']})[1]
            assert next(v for k,v in published_get(updated)[2].items() if k.lower()=='www-authenticate')==original_realm
        for target in [{'port':39999},{'file':'/workspace/missing-publication-file'},{'directory':'/workspace/missing-publication-directory'}]:
            try:
                client.tool('web_publish',action='publish',name='api',**target)
            except AssertionError:
                pass
            else:
                raise AssertionError('invalid replacement unexpectedly succeeded')
            current=json.loads(client.tool('web_publish',action='get',name='api'))
            assert current['type']=='port' and current['password']==port_publication['password']
            assert published_get(current,{'X-Publish-Key':current['password']})[0]==200
        passed('same-name file/directory/port replacement preserves URL, Basic realm and credentials; failed preparation keeps old service')
        client.tool('web_publish',action='unpublish',name='file')
        assert published_get(file,{'X-Publish-Key':file['password']})[0]==404
        assert published_get(published,{'X-Publish-Key':published['password']})[0]==200
        client.tool('web_publish',action='unpublish',name='site')
        client.tool('web_publish',action='unpublish',name='api')
        assert 'published-site' in client.tool('bash',command='python3 -c "import urllib.request; print(urllib.request.urlopen(\'http://127.0.0.1:38080/\', timeout=3).read().decode())"')
        passed('file/directory/port coexist with independent paths and credentials, automatic auth, ranges, stable retry and scoped close; app survives')

        started = time.monotonic()
        timeout_text = client.tool_error('bash', command='sleep 4')
        timeout_result = json.loads(timeout_text)
        assert time.monotonic() - started < 5, timeout_text
        assert timeout_result['error'] == 'EXECUTION_TIMEOUT' and timeout_result['cleanup_confirmed'], timeout_text
        assert timeout_result['side_effects'] == 'may_have_occurred', timeout_text
        assert 'manual-ok' in client.tool(
            'bash', command='sleep 3; echo manual-ok', timeout_secs=5
        )
        passed('configured fallback and manual bash timeout both use seconds')

        assert 'Wrote' in client.tool(
            'write', path='/workspace/a.txt', content='old'
        )
        paused_publication=json.loads(client.tool('web_publish',action='publish',name='paused',file='/workspace/a.txt',keep_alive_minutes=1))
        docker('exec',name,'msb','pause','-q','dockerverify-workspace')
        wait_workspace_status(name,'paused')
        assert published_get(paused_publication,{'X-Publish-Key':paused_publication['password']})[:2]==(200,b'old')
        wait_workspace_status(name,'running')
        assert 'bash-resumed' in pause_then_call(
            name, client, 'bash', command='echo bash-resumed'
        )
        assert 'old' in pause_then_call(
            name, client, 'read', path='/workspace/a.txt'
        )
        assert 'Wrote' in pause_then_call(
            name, client, 'write', path='/workspace/b.txt', content='data'
        )
        assert 'Replaced' in pause_then_call(
            name, client, 'edit', path='/workspace/a.txt', old_text='old', new_text='new'
        )
        assert json.loads(client.tool('web_publish',action='get',name='paused'))==paused_publication
        assert published_get(paused_publication,{'X-Publish-Key':paused_publication['password']})[:2]==(200,b'new')
        client.tool('web_publish',action='unpublish',name='paused')
        passed('direct file URL and bash/read/write/edit resume a paused VM; publication identity is preserved')

        client.tool(
            'bash',
            command='echo docker-root > /root/docker-marker; '
                    'echo docker-volume > /workspace/docker-marker; echo written',
        )
        with concurrent.futures.ThreadPoolExecutor() as pool:
            work = pool.submit(
                client.tool, 'bash',
                command='sleep 12; echo long-call-survived', timeout_secs=30,
            )
            time.sleep(8)
            wait_workspace_status(name, 'running')
            assert 'long-call-survived' in work.result()
        passed('in-flight command survives both short reclamation thresholds')

        original_session = client.session
        wait_workspace_status(name, 'stopped')
        wake = client.tool(
            'bash', command='cat /root/docker-marker /workspace/docker-marker'
        )
        assert 'docker-root' in wake and 'docker-volume' in wake
        assert client.session == original_session
        passed('same MCP session wakes a stopped VM and retains both disks')

        print(f'Waiting {args.idle_seconds}s to verify idle MCP session retention...', flush=True)
        time.sleep(args.idle_seconds)
        assert 'docker-volume' in client.tool(
            'bash', command='cat /workspace/docker-marker'
        )
        assert client.session == original_session
        passed(f'original MCP session still executes after {args.idle_seconds}s idle')

        restart_publication=json.loads(client.tool('web_publish',action='publish',name='restart',file='/workspace/docker-marker',keep_alive_minutes=1))
        stop(name)
        docker('start', name)
        client = wait_ready(name)
        assert json.loads(client.tool('web_publish',action='list'))==[]
        assert published_get(restart_publication,{'X-Publish-Key':restart_publication['password']})[0]==404
        wait_workspace_status(name,'stopped')
        passed('Host restart invalidates all publication state; old URL does not wake the VM')
        result = client.tool(
            'bash', command='cat /root/docker-marker /workspace/docker-marker'
        )
        assert 'docker-root' in result and 'docker-volume' in result, result
        passed('graceful container restart preserves root disk and workspace volume')
        stop(name)

        dynamic_volume = run + '-dynamic-data'
        docker('volume', 'create', dynamic_volume)
        volumes.append(dynamic_volume)
        dynamic_config = root / 'dynamic.toml'
        dynamic_config.write_text('''resource_prefix = "dockerverify"
[webdav]
enabled = false
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
exec_timeout_secs = 10
[lifecycle]
session_timeout_secs = 900
idle_timeout_secs = 3600
check_interval_secs = 1
[logging]
level = "info"
[state]
dir = "/var/lib/microsandbox/agent-space-state"
[admin]
token_env = "AGENT_SPACE_ADMIN_TOKEN"
''')
        config_mount = str(dynamic_config) + ':/etc/agent-space/config.toml:ro'
        admin_env = 'AGENT_SPACE_ADMIN_TOKEN=docker-verification-admin'
        docker(
            'run', '--rm', '-e', admin_env,
            '-v', dynamic_volume + ':/var/lib/microsandbox',
            '-v', config_mount, args.image, 'init-state',
        )
        dynamic_name = run + '-dynamic'
        docker(
            'run', '-d', '--name', dynamic_name, '--device', '/dev/kvm',
            '--stop-timeout', '60', '-p', '127.0.0.1::8932',
            '-e', admin_env,
            '-v', dynamic_volume + ':/var/lib/microsandbox',
            '-v', config_mount, args.image,
        )
        containers.append(dynamic_name)
        dynamic_port = wait_ready(dynamic_name, token=None)

        def admin(method, path, body=None):
            connection = http.client.HTTPConnection('127.0.0.1', dynamic_port, timeout=30)
            try:
                headers = {'Authorization': 'Bearer docker-verification-admin', 'Host': 'localhost:8932'}
                if body is not None:
                    headers['Content-Type'] = 'application/json'
                connection.request(
                    method, path, None if body is None else json.dumps(body), headers
                )
                response = connection.getresponse()
                raw = response.read()
                return response.status, json.loads(raw) if raw else None
            finally:
                connection.close()

        status, created = admin('POST', '/admin/v1/users', {'username': 'alice'})
        assert status == 201, (status, created)
        assert '0.0.0.0' not in created['mcpUrl'], created
        assert created['mcpUrl'].endswith('/u/alice/mcp'), created
        space, user_token = created['agentSpaceId'], created['token']
        status, detail = admin('GET', '/admin/v1/users/alice')
        assert status == 200, (status, detail)
        sandbox_name, volumes_owned = detail['sandboxName'], detail['ownedVolumeNames']
        dynamic_client = Client(dynamic_port, user_token, username='alice')
        assert 'dynamic-user-ok' in dynamic_client.tool('bash', command='echo dynamic-user-ok')
        dynamic_client.tool('bash', command='mkdir -p /workspace/report && echo dynamic-publication > /workspace/report/index.html')
        dynamic_publication = json.loads(dynamic_client.tool(
            'web_publish', action='publish', name='report', directory='/workspace/report',
        ))
        connection = http.client.HTTPConnection('127.0.0.1', dynamic_port, timeout=15)
        connection.request('GET', dynamic_publication['url'], headers={'X-Publish-Key':dynamic_publication['password']})
        response = connection.getresponse()
        assert response.status == 200 and b'dynamic-publication' in response.read()
        connection.close()
        passed('fresh image initializes state and serves a managed user without Hub')

        status, _ = admin('PATCH', '/admin/v1/users/alice', {
            'agentSpaceId': space, 'enabled': False,
        })
        assert status in (200, 202), status
        connection = http.client.HTTPConnection('127.0.0.1', dynamic_port, timeout=15)
        connection.request('GET', dynamic_publication['url'], headers={'X-Publish-Key':dynamic_publication['password']})
        response = connection.getresponse()
        assert response.status == 404, response.status
        response.read()
        connection.close()
        for _ in range(60):
            status, _ = admin('PATCH', '/admin/v1/users/alice', {
                'agentSpaceId': space, 'enabled': True,
            })
            if status == 200:
                break
            time.sleep(1)
        assert status == 200, status
        status, rotated = admin('POST', '/admin/v1/users/alice/token', {
            'agentSpaceId': space,
        })
        assert status == 200, (status, rotated)
        dynamic_client = Client(dynamic_port, rotated['token'], username='alice')
        assert 'dynamic-publication' in dynamic_client.tool(
            'bash', command='cat /workspace/report/index.html'
        )
        passed('disable closes publications and new token restores retained workspace files')

        status, _ = admin('DELETE', '/admin/v1/users/alice?agentSpaceId=' + space)
        assert status == 202, status
        for _ in range(60):
            status, _ = admin('GET', '/admin/v1/users/alice')
            if status == 404:
                break
            time.sleep(1)
        assert status == 404, status
        assert sandbox_name not in docker('exec', dynamic_name, 'msb', 'list')
        volume_list = docker('exec', dynamic_name, 'msb', 'volume', 'list')
        assert all(volume not in volume_list for volume in volumes_owned), volume_list
        passed('managed delete removes its sandbox and named volume from the runtime catalog')
        stop(dynamic_name)

        result = {
            'image': args.image,
            'image_id': args.image,
            'microsandbox': version,
            'idle_seconds': args.idle_seconds,
            'protocol_versions': negotiated_versions,
            'checks': checks,
            'passed': True,
        }
        (root / 'result.json').write_text(json.dumps(result, ensure_ascii=False, indent=2))
        print('VERIFICATION_PASS ' + str(root), flush=True)
    finally:
        cleanup_errors=[]
        for name in reversed(containers):
            try:
                (root / (name + '.log')).write_text(docker('logs', name))
                docker('rm', '-f', '-v', name)
            except subprocess.CalledProcessError:
                cleanup_errors.append('container '+name)
        for volume in volumes:
            try:
                docker('volume', 'rm', volume)
            except subprocess.CalledProcessError:
                cleanup_errors.append('volume '+volume)
        result['cleanup_passed']=not cleanup_errors
        if cleanup_errors:
            result['passed']=False
            result['cleanup_errors']=cleanup_errors
        (root/'result.json').write_text(json.dumps(result,indent=2)+'\n')
        if cleanup_errors:raise RuntimeError('Server qualification cleanup failed')



if __name__ == '__main__':
    main()
