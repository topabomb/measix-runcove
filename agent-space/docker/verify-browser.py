#!/usr/bin/env python3
'''Disposable real-image fixture for interactive browser/GUI acceptance.

Start with --image and --output; read ready.json for URLs. No production data.
The transparent loopback proxy records authority/origin/fetch metadata/status,
never credential values. Touch OUTPUT/stop to stop gracefully and clean up.
Use browser UI to run the displayed probes; inspect requests.jsonl as well as
visible results. This fixture alone is NOT a passing browser verification.
'''
import argparse
import base64
import hashlib
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import subprocess
import threading
import time
import uuid
from urllib.parse import urlsplit

from verify import Client, docker

PAGE = r'''<!doctype html><meta charset="utf-8"><title>Agent Space path publication verification</title>
<base href="__PREFIX__"><link rel="stylesheet" href="assets/site.css"><script src="assets/site.js" defer></script><img id="logo" src="assets/logo.svg" width="32" alt="fixture logo"><a href="deep/page/">Open deep page</a>
<style>body{font:16px system-ui;max-width:900px;margin:40px auto;padding:20px}button,a{display:inline-block;margin:8px;padding:10px}pre{white-space:pre-wrap;background:#eee;padding:16px}</style>
<h1 id="title"></h1><p>Path routing and independent Basic credentials. Same-origin pages share browser authority.</p>
<button id="normal">Verify own application</button><button id="probe">Verify shared-origin behavior</button>
<a id="other">Visit other space</a><a id="download">Download DAV HTML fixture</a>
<pre id="result">Ready</pre>
<script>
const data=__DATA__;const result=document.querySelector('#result');
document.querySelector('#title').textContent=data.name+' publication';
document.querySelector('#other').href=data.other;
document.querySelector('#download').href=data.download;
const output=(line)=>{result.textContent+='\n'+line;};
document.querySelector('#normal').onclick=async()=>{
 result.textContent='Own application checks';
 const response=await fetch('payload.txt');
 output('relative fetch: '+response.status+' '+await response.text());
 output('external JS: '+window.fixtureScript);
 output('external CSS: '+getComputedStyle(document.body).getPropertyValue('--fixture').trim());
 output('image loaded: '+(document.querySelector('#logo').naturalWidth>0));
 const api=await fetch('api/items.json?test=1');output('API: '+api.status+' '+await api.text());
 const nested=await fetch('deep/page/');output('deep page: '+nested.status);
 localStorage.setItem('fixture-owner',data.name);output('localStorage owner: '+localStorage.getItem('fixture-owner'));
 output('canonical path: '+location.pathname);
};
document.querySelector('#probe').onclick=async()=>{
 result.textContent='Shared-origin behavior (visit both publications first)';
 const r=await fetch(data.other+'payload.txt',{credentials:'include'});
 output('other publication with cached Basic: '+r.status+' '+await r.text());
 output('shared localStorage owner: '+localStorage.getItem('fixture-owner'));
 const admin=await fetch(data.primary+'/admin/v1/users',{credentials:'include'});
 output('admin without admin token: '+admin.status);
 try{await fetch(data.dav,{credentials:'include'});output('DAV unexpectedly readable');}
 catch(e){output('DAV cross-origin read blocked: '+e.name);}
 output('Paths and Basic realms are not script isolation; publish trusted content.');
};
</script>'''

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, mode=0o700, exist_ok=False)
    name = 'as-browser-' + uuid.uuid4().hex[:10]
    volume = name + '-data'
    backend = None
    lock = threading.Lock()
    requests = (root/'requests.jsonl').open('a', buffering=1)
    hop = {'connection','keep-alive','proxy-authenticate','proxy-authorization','te','trailer','transfer-encoding','upgrade'}

    class Proxy(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'
        def log_message(self, *args):
            pass
        def handle_request(self):
            if backend is None:
                self.send_error(503)
                return
            headers = {key:value for key,value in self.headers.items() if key.lower() not in hop}
            if self.headers.get('Transfer-Encoding', '').lower() == 'chunked':
                chunks = []
                while True:
                    size = int(self.rfile.readline().split(b';', 1)[0].strip(), 16)
                    if size == 0:
                        while True:
                            trailer = self.rfile.readline()
                            if trailer == b'\r\n':
                                break
                            if not trailer:
                                raise ConnectionError('truncated chunk trailers')
                        break
                    chunk = self.rfile.read(size)
                    assert len(chunk) == size and self.rfile.read(2) == b'\r\n'
                    chunks.append(chunk)
                raw = b''.join(chunks)
            else:
                raw = self.rfile.read(int(self.headers.get('Content-Length', '0')))
            upstream = dav_backend if self.headers.get('Host') == urlsplit(dav_origin).netloc else backend
            connection = http.client.HTTPConnection('127.0.0.1', upstream, timeout=70)
            try:
                connection.request(self.command, self.path, raw or None, headers)
                response = connection.getresponse()
                body = response.read()
                record = {'method':self.command, 'host':self.headers.get('Host'), 'path':self.path,
                          'user_agent':self.headers.get('User-Agent'), 'origin':self.headers.get('Origin'), 'site':self.headers.get('Sec-Fetch-Site'),
                          'mode':self.headers.get('Sec-Fetch-Mode'), 'dest':self.headers.get('Sec-Fetch-Dest'),
                          'user':self.headers.get('Sec-Fetch-User'),
                          'authenticated':self.headers.get('Authorization') is not None,
                          'status':response.status,
                          'challenge':response.getheader('WWW-Authenticate'),
                          'location':response.getheader('Location'),
                          'disposition':response.getheader('Content-Disposition'),
                          'csp':response.getheader('Content-Security-Policy'),
                          'oac':response.getheader('Origin-Agent-Cluster'),
                          'coop':response.getheader('Cross-Origin-Opener-Policy')}
                with lock:
                    requests.write(json.dumps(record)+'\n')
                self.send_response(response.status)
                for key,value in response.getheaders():
                    if key.lower() not in hop | {'content-length'}:
                        self.send_header(key,value)
                self.send_header('Content-Length', (response.getheader('Content-Length') or '0') if self.command == 'HEAD' else str(len(body)))
                self.end_headers()
                if self.command != 'HEAD':
                    self.wfile.write(body)
            finally:
                connection.close()
        do_GET = do_HEAD = do_POST = do_PUT = do_OPTIONS = do_PROPFIND = do_DELETE = do_MKCOL = do_COPY = do_MOVE = do_LOCK = do_UNLOCK = handle_request

    proxy = ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
    proxy.daemon_threads = True
    threading.Thread(target=proxy.serve_forever, daemon=True).start()
    port = proxy.server_port
    primary = f'http://localhost:{port}'
    dav_origin = f'http://127.0.0.1:{port}'
    cfg = root/'config.toml'
    cfg.write_text(f'''resource_prefix = "browserverify"
[server]
listen = "0.0.0.0:8932"
public_base_url = "{primary}"
max_sessions = 3
[web_publish]
enabled = true
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
session_timeout_secs = 1800
idle_timeout_secs = 3600
check_interval_secs = 10
[state]
dir = "/var/lib/microsandbox/agent-space-state"
[admin]
token_env = "AGENT_SPACE_ADMIN_TOKEN"
[webdav]
enabled = true
public_base_url = "{dav_origin}"
''')
    cfg.chmod(0o600)
    os.environ['AGENT_SPACE_ADMIN_TOKEN'] = secrets.token_hex(32)
    mounted = ['-e','AGENT_SPACE_ADMIN_TOKEN','-v',volume+':/var/lib/microsandbox',
               '-v',str(cfg)+':/etc/agent-space/config.toml:ro']
    started = False
    def api(method, path, value=None):
        conn = http.client.HTTPConnection('127.0.0.1', backend, timeout=90)
        try:
            conn.request(method, '/admin/v1/users'+path, None if value is None else json.dumps(value),
                         {'Host':f'localhost:{port}', 'Authorization':'Bearer '+os.environ['AGENT_SPACE_ADMIN_TOKEN'],
                          'Content-Type':'application/json'})
            response = conn.getresponse(); body = response.read()
            assert response.status in (200,201,202), (method,path,response.status)
            return json.loads(body) if body else None
        finally:
            conn.close()
    try:
        docker('volume','create',volume)
        docker('run','--rm',*mounted,args.image,'init-state')
        docker('run','-d','--name',name,'--device','/dev/kvm','--stop-timeout','60',
               '-p','127.0.0.1::8932','-p','127.0.0.1::8933',*mounted,args.image)
        started = True
        deadline = time.monotonic()+120
        while True:
            info = json.loads(docker('inspect',name))[0]
            assert info['State']['Running'], 'Fixture failed; inspect host.log'
            if 'listening on' in docker('logs',name):
                backend = int(info['NetworkSettings']['Ports']['8932/tcp'][0]['HostPort'])
                dav_backend = int(info['NetworkSettings']['Ports']['8933/tcp'][0]['HostPort']);break
            assert time.monotonic()<deadline
            time.sleep(.5)
        users, clients, urls = {}, {}, {}
        for username in ('alice','bob'):
            user = api('POST','',{'username':username});users[username] = user
            clients[username] = Client(backend,user['token'],username=username,authority=f'localhost:{port}')
            clients[username].tool('bash',cmd='mkdir -p /workspace/browser/assets /workspace/browser/api /workspace/browser/deep/page; printf '+username+'-payload > /workspace/browser/payload.txt')
            if username=='alice':
                clients[username].tool('bash',cmd='python3 -m http.server 38080 --bind 127.0.0.1 --directory /workspace/browser >/tmp/browser-app.log 2>&1 < /dev/null &')
                time.sleep(.5)
                target={'port':38080}
            else: target={'directory':'/workspace/browser'}
            published=json.loads(clients[username].tool('web_publish',action='publish',name='browser',**target))
            urls[username]=published['url']
            users[username]['publish_password']=published['password']
            users[username]['publish_username']=published['username']
        key = api('POST','/alice/dav-token',{'agentSpaceId':users['alice']['agentSpaceId']})
        # Credential file is local, test-only and private; not printed in logs/evidence.
        secret_file = root/'dav-credential.json'
        secret_file.write_text(json.dumps(key));secret_file.chmod(0o600)
        clients['alice'].tool('write',path='/workspace/dav-html.html',content='<script>document.title="DAV SCRIPT EXECUTED"</script><h1>Must download, never execute</h1>')
        for username, other in [('alice','bob'),('bob','alice')]:
            data={'name':username,'other':urls[other], 'primary':primary,
                  'dav':key['davUrl']+'dav-html.html', 'download':key['davUrl']+'dav-html.html'}
            page=PAGE.replace('__DATA__',json.dumps(data)).replace('__PREFIX__',urlsplit(urls[username]).path)
            for path,content in {'index.html':page,'deep/page/index.html':page,'assets/site.js':'window.fixtureScript="loaded";','assets/site.css':'body{--fixture:loaded}','assets/logo.svg':'<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" fill="green"/></svg>','api/items.json':'{"items":[1,2,3]}'}.items():
                clients[username].tool('write',path='/workspace/browser/'+path,content=content)
        ready=root/'ready.json'
        ready.write_text(json.dumps({'image':args.image,'image_id':docker('image','inspect',args.image,'--format','{{.Id}}').strip(),
            'container':name,'volume':volume,'port':port,'backend_port':backend,'primary':primary,'alice':urls['alice'],'bob':urls['bob'],
            'alice_fixture_username':users['alice']['publish_username'],'bob_fixture_username':users['bob']['publish_username'],'alice_fixture_password':users['alice']['publish_password'],'bob_fixture_password':users['bob']['publish_password'],'dav_url':key['davUrl']},indent=2)+'\n')
        ready.chmod(0o600)
        print('READY '+str(root/'ready.json'),flush=True)
        while not (root/'stop').exists():
            if (root/'replace').exists() and not (root/'replaced.json').exists():
                updated=json.loads(clients['alice'].tool('web_publish',action='publish',name='browser',directory='/workspace/browser'))
                assert updated['url']==urls['alice'] and updated['username']==users['alice']['publish_username'] and updated['password']==users['alice']['publish_password']
                clients['alice'].tool('write',path='/workspace/browser/payload.txt',content='alice-replaced-directory')
                (root/'replaced.json').write_text(json.dumps({'same_url_and_credentials':True,'type':updated['type']}))
            time.sleep(.25)
    finally:
        proxy.shutdown();proxy.server_close();requests.close()
        if started:
            docker('stop','--time','60',name,timeout=90)
            (root/'host.log').write_text(docker('logs',name))
            docker('rm','-v',name)
        docker('volume','rm',volume)
        print('Fixture stopped and owned resources removed',flush=True)


if __name__=='__main__':
    main()
