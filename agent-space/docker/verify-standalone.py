#!/usr/bin/env python3
"""Disposable real-container qualification. No existing container or volume is used."""
import argparse,hashlib,http.client,json,os,secrets,shutil,subprocess,sys,time,uuid
from pathlib import Path
from release_identity import source_digest

ROOT=Path(__file__).resolve().parents[1]

def main():
 parser=argparse.ArgumentParser(description=__doc__)
 parser.add_argument('--image',required=True)
 suites=parser.add_mutually_exclusive_group()
 suites.add_argument('--full',action='store_true',help='Run shared browser, 1 GiB file, UI and real 300-second idle suites')
 suites.add_argument('--tools-only',action='store_true',help='Run browser and image regressions, including real 300-second profile recovery; skip file-stream and UI suites')
 parser.add_argument('--development-assets',action='store_true',help='Bind current debug binaries/UI for early development checks; not release qualification')
 parser.add_argument('--max-pages',type=int,help='Override the shared browser Page limit for this independent qualification')
 args=parser.parse_args()
 name='runcove-qualify-'+uuid.uuid4().hex[:10]
 output=ROOT/'target/standalone-verification'/name;output.mkdir(parents=True)
 token=secrets.token_urlsafe(40);dav=secrets.token_urlsafe(40)
 environment=dict(os.environ,RUNCOVE_TOKEN=token,RUNCOVE_DAV_TOKEN=dav,RUNCOVE_PRIVATE_MARKER='must-not-reach-payload')
 report={'target':'standalone','scope':'development assets' if args.development_assets else 'final image','image':args.image,'checks':[],'passed':False}
 container=None;volume=None
 def command(*values,timeout=120):
  result=subprocess.run(list(values),capture_output=True,text=True,timeout=timeout,env=environment)
  if result.returncode:raise RuntimeError(' '.join(values[:3])+': '+(result.stderr+result.stdout)[-5000:])
  return (result.stdout+result.stderr if values[:2]==('docker','logs') else result.stdout).strip()
 def check(name):report['checks'].append(name);print('PASS '+name,flush=True)
 def request(method,path,body=None,headers=None,port=None):
  connection=http.client.HTTPConnection('127.0.0.1',port or main_port,timeout=60)
  connection.request(method,path,body,headers or {})
  response=connection.getresponse();status=response.status;response_headers=dict(response.getheaders());data=response.read();connection.close()
  return status,response_headers,data
 try:
  report['image_id']=json.loads(command('docker','image','inspect',args.image))[0]['Id']
  args.image=report['image_id']
  volume=command('docker','volume','create',name+'-workspace')
  config=output/'config.toml'
  # Whitespace-only optional origins have the same semantics as an empty value.
  config.write_text('[server]\nlisten="0.0.0.0:8932"\n[webdav]\nenabled=true\nlisten="0.0.0.0:8933"\npublic_base_url="   "\n');config.chmod(0o600)
  if args.max_pages is not None:
   assert args.max_pages>=2,'shared Page regressions require at least two pages'
   config.write_text(config.read_text()+f'\n[browser]\nmax_pages={args.max_pages}\n')
   (output/'max_pages').write_text(str(args.max_pages));report['max_pages']=args.max_pages
  create=['docker','create','--name',name,'--init','--memory','3g','--cpus','2','--shm-size','512m','--security-opt','seccomp='+str(ROOT/'docker/playwright-seccomp.json'),'-p','127.0.0.1::8932','-p','127.0.0.1::8933','--tmpfs','/tmp:size=256m,mode=1777','--tmpfs','/run:size=64m,mode=755','--env','RUNCOVE_TOKEN','--env','RUNCOVE_DAV_TOKEN','--env','RUNCOVE_PRIVATE_MARKER','--mount','type=volume,src='+volume+',dst=/workspace','--mount','type=bind,src='+str(config)+',dst=/etc/runcove/config.toml,readonly']
  if args.development_assets:
   report['source_digest']=source_digest()
   frozen=output/'inputs';frozen.mkdir()
   report['binaries_sha256']={}
   for file in ['runcove-standalone','agent-space-static-http','agent-space-webdav','agent-space-desktop']:
    shutil.copy2(ROOT/'target/debug'/file,frozen/file)
    report['binaries_sha256'][file]=hashlib.file_digest((frozen/file).open('rb'),'sha256').hexdigest()
    create+=['--mount',f'type=bind,src={frozen}/{file},dst=/usr/local/bin/{file},readonly']
   shutil.copytree(ROOT/'desktop-ui/dist',frozen/'ui')
   shutil.copy2(ROOT/'docker/standalone-entrypoint.sh',frozen/'entrypoint.sh')
   create+=['--mount',f'type=bind,src={frozen}/ui,dst=/opt/agent-space/desktop-ui,readonly','--mount',f'type=bind,src={frozen}/entrypoint.sh,dst=/entrypoint.sh,readonly','--env','AGENT_SPACE_DESKTOP_UI=/opt/agent-space/desktop-ui','--entrypoint','/bin/bash',args.image,'/entrypoint.sh','--config','/etc/runcove/config.toml']
  else:create+=[args.image]
  container=command(*create)
  command('docker','start',container)
  main_port=int(command('docker','port',container,'8932/tcp').rsplit(':',1)[1]);dav_port=int(command('docker','port',container,'8933/tcp').rsplit(':',1)[1])
  until=time.monotonic()+45
  while True:
   try:
    if request('GET','/healthz')[0]==200:break
   except OSError:pass
   if time.monotonic()>until:raise RuntimeError('Standalone readiness failed: '+command('docker','logs',container)[-3000:])
   time.sleep(.2)
  inspect=json.loads(command('docker','inspect',container))[0]
  assert not inspect['HostConfig']['Privileged'] and not inspect['HostConfig']['Devices']
  assert command('docker','exec',container,'bash','-c','test ! -e /dev/kvm && ! command -v msb && ! test -e /var/run/docker.sock')==''
  check('starts_without_kvm_msb_or_docker_socket')
  for path in ['/admin/v1/users','/admin/v1/status','/u/other/mcp','/u/workspace/desktop/']:
   assert request('GET',path)[0]==404,path
  assert request('POST','/mcp',b'{}',{'Content-Type':'application/json'})[0]==401
  check('single_space_routes_have_no_management_or_user_selection')
  (output/'port').write_text(str(main_port));(output/'target').write_text('standalone')
  private=output/'users.json';private.write_text(json.dumps({'desktop':{'token':token}}));private.chmod(0o600)
  os.environ['AGENT_SPACE_BROWSER_TEST_DIR']=str(output)
  environment['AGENT_SPACE_BROWSER_TEST_DIR']=str(output)
  sys.path.insert(0,str(ROOT/'tests'))
  from browser_client import Client,text
  client=Client('desktop');client.initialize()
  assert len(client.call('tools/list')['tools'])==9
  def tool(name,args):
   result=client.tool(name,args);assert not result.get('isError'),text(result);return text(result)
  output_value=tool('bash',{'cmd':"id -u; python3 -c 'import os;print([k for k in os.environ if k.startswith(\"RUNCOVE_\")])'"})
  assert '1001' in json.dumps(output_value) and '[]' in json.dumps(output_value),output_value
  command('docker','exec',container,'bash','-c','umask 077; printf root-private >/tmp/root-private')
  assert client.tool('read',{'path':'/tmp/root-private'}).get('isError')
  check('shared_nine_tools_and_nonroot_secret_isolation')
  # Exercise actual root-controller startup against user-prepared symlinks.
  command('docker','exec',container,'bash','-c','mkdir -m 700 /tmp/root-private-directory')
  tool('bash',{'cmd':'mkdir -p /workspace/.agent-space/desktop/profile; ln -s /tmp/root-private-directory /workspace/.agent-space/desktop/profile/Default'})
  assert client.tool('browser_snapshot',{}).get('isError')
  tool('bash',{'cmd':'rm /workspace/.agent-space/desktop/profile/Default; mkdir /workspace/.agent-space/desktop/profile/Default'})
  tool('write',{'path':'/workspace/.agent-space/desktop/profile/Default/Preferences','content':json.dumps({'download':{'default_directory':'/workspace/.agent-space/desktop/artifacts'}})})
  tool('bash',{'cmd':'ln -s /tmp/root-private /workspace/.agent-space/desktop/profile/Default/Preferences.agent-space-migrate'})
  assert client.tool('browser_snapshot',{}).get('isError')
  assert command('docker','exec',container,'cat','/tmp/root-private')=='root-private'
  tool('bash',{'cmd':'rm /workspace/.agent-space/desktop/profile/Default/Preferences.agent-space-migrate'})
  check('profile_preparation_cannot_follow_user_links_with_root_privileges')
  observed=client.tool('screen_observe',{'view':'desktop'})
  assert not observed.get('isError'),text(observed)
  assert text(observed)['captured_count']==1 and any(c['type']=='image' for c in observed['content'])
  check('real_shared_desktop_screenshot')
  # The fixed browser API accepts no code or worker-result path. Inject a
  # user-owned link separately while a real browser task runs as UID1001.
  task=text(client.tool('browser_act',{'actions':[{'do':'wait','secs':10}]}))
  job=command('docker','exec',container,'python3','-c',"from pathlib import Path;import sys;dirs=list(Path('/tmp/agent-space').glob('*/job-'+sys.argv[1]));assert len(dirs)==1;print(dirs[0])",task['task_id'])
  command('docker','exec','--user','1001:1001',container,'python3','-c',"from pathlib import Path;import sys;(Path(sys.argv[1])/'observation.json').symlink_to('/tmp/root-private')",job)
  completed,_=client.finish(client.tool('browser_act',{'op':'wait','task_id':task['task_id']}));assert completed['state']=='succeeded',completed
  assert command('docker','exec',container,'cat','/tmp/root-private')=='root-private'
  check('root_task_owner_keeps_final_results_without_following_user_paths')
  auth={'Authorization':'Bearer '+dav}
  status,_,data=request('PUT','/dav/interop.txt',b'from-dav',auth,dav_port)
  assert status==201,(status,data.decode(errors='replace'))
  tool('edit',{'path':'/workspace/interop.txt','old_text':'from-dav','new_text':'edited'})
  assert request('GET','/dav/interop.txt',headers=auth,port=dav_port)[2]==b'edited'
  base='http://127.0.0.1:'+str(main_port)
  status,headers,data=request('POST','/desktop/api/login',json.dumps({'token':token}),{'Content-Type':'application/json','Origin':base})
  assert status==200,(status,data)
  cookie=headers['set-cookie' if 'set-cookie' in headers else 'Set-Cookie'].split(';')[0];nonce=json.loads(data)['nonce']
  ui_headers={'Cookie':cookie,'Origin':base,'X-Desktop-Nonce':nonce}
  path='/desktop/api/files/agent-space/artifacts/from-workbench.txt'
  assert request('PUT',path,b'from-ui',ui_headers)[0]==201
  tool('edit',{'path':'/workspace/agent-space/artifacts/from-workbench.txt','old_text':'from-ui','new_text':'ui-edited'})
  assert request('GET',path,headers=ui_headers)[2]==b'ui-edited'
  assert command('docker','exec',container,'stat','-c','%u:%g','/workspace/interop.txt','/workspace/agent-space/artifacts/from-workbench.txt').splitlines()==['1001:1001','1001:1001']
  check('dav_workbench_and_mcp_edits_share_writable_user_ownership')
  command('docker','exec',container,'bash','-c','printf private-metadata >/tmp/root-private-directory/hidden-root-file')
  tool('bash',{'cmd':'mv /workspace/agent-space/artifacts /workspace/agent-space/artifacts.saved; ln -s /tmp/root-private-directory /workspace/agent-space/artifacts'})
  try:
   status,_,body=request('GET','/desktop/api/artifacts',headers=ui_headers)
   assert status==503 and b'hidden-root-file' not in body,(status,body)
  finally:
   tool('bash',{'cmd':'rm /workspace/agent-space/artifacts; mv /workspace/agent-space/artifacts.saved /workspace/agent-space/artifacts'})
  status,_,body=request('GET','/desktop/api/artifacts',headers=ui_headers)
  assert status==200 and b'from-workbench.txt' in body,(status,body)
  check('artifact_listing_respects_payload_permissions_through_user_symlinks')
  published=tool('web_publish',{'action':'publish','name':'proof','file':'/workspace/interop.txt'})
  assert '/pub/proof/' in json.dumps(published) and '/u/' not in json.dumps(published),published
  check('publication_uses_single_space_address')
  # Even a stopped data child cannot outlive a killed control owner.
  ownership_probe="""import os,pathlib,signal,time
items=[]
for p in pathlib.Path('/proc').glob('[0-9]*'):
 try:
  if p.joinpath('cmdline').read_bytes().split(b'\\0',1)[0].endswith(b'/agent-space-webdav'):
   uid=int(next(x for x in p.joinpath('status').read_text().splitlines() if x.startswith('Uid:')).split()[1])
   if uid==0:items.append(int(p.name))
 except (OSError,ValueError):pass
assert len(items)==1,items
parent=items[0]
children=pathlib.Path(f'/proc/{parent}/task/{parent}/children').read_text().split()
assert len(children)==1,children
child=int(children[0]);os.kill(child,signal.SIGSTOP);os.kill(parent,signal.SIGKILL)
until=time.monotonic()+5
while pathlib.Path(f'/proc/{child}').exists():
 assert time.monotonic()<until,'data child survived its control owner'
 time.sleep(.02)
print('stopped data child reaped after parent death')"""
  assert 'reaped' in command('docker','exec',container,'python3','-c',ownership_probe)
  assert request('GET','/dav/interop.txt',headers=auth,port=dav_port)[2]==b'edited'
  check('stopped_data_child_dies_with_killed_control_owner_and_helper_recovers')
  if args.full or args.tools_only:
   from qualification import run_shared
   report['suite_logs']=run_shared(output,environment,check,tools_only=args.tools_only)
  # Proxy rewrites Host; workbench's own Origin/Cookie/nonce boundary applies.
  proxy_origin='https://runcove-entry.example'
  proxy_headers={'Origin':proxy_origin,'Host':'upstream.internal:8932','Content-Type':'application/json'}
  status,proxy_reply,body=request('POST','/desktop/api/login',json.dumps({'token':token}),proxy_headers)
  assert status==200,(status,body)
  proxy_cookie=next(v for k,v in proxy_reply.items() if k.lower()=='set-cookie').split(';')[0]
  proxy_headers.update(Cookie=proxy_cookie,**{'X-Desktop-Nonce':json.loads(body)['nonce']})
  assert request('GET','/desktop/api/control',headers=proxy_headers)[0]==200
  proxy_headers['Origin']='https://wrong.example'
  assert request('GET','/desktop/api/control',headers=proxy_headers)[0]==403
  check('rewritten_host_workbench_preserves_own_origin_and_nonce_checks')
  assert not client.tool('browser_snapshot',{}).get('isError')
  desktop_probe=ownership_probe.replace("b'/agent-space-webdav'","b'/agent-space-desktop'")
  # Desktop descendants are intentionally retained as unconfirmed until Docker
  # restarts the namespace. Recreating another owner is forbidden in this state.
  desktop_probe=desktop_probe[:desktop_probe.index('children=')]+"assert any(p.read_text().split() for p in pathlib.Path(f'/proc/{parent}/task').glob('*/children'));os.kill(parent,signal.SIGKILL);print('desktop owner killed')"
  command('docker','exec',container,'python3','-c',desktop_probe)
  assert client.tool('browser_snapshot',{}).get('isError')
  status,_,body=request('GET','/desktop/api/control',headers=ui_headers)
  assert status==200 and json.loads(body)['control']=='BLOCKED',(status,body)
  assert 'still-available' in json.dumps(tool('bash',{'cmd':'echo still-available'}))
  check('lost_desktop_owner_blocks_recreation_and_control_until_container_restart')
  stream=http.client.HTTPConnection('127.0.0.1',main_port,timeout=15)
  stream.request('GET','/mcp',headers={'Authorization':'Bearer '+token,'Accept':'text/event-stream','Mcp-Session-Id':client.session,'MCP-Protocol-Version':'2025-11-25'})
  stream_response=stream.getresponse();assert stream_response.status==200
  report['full']=args.full and not args.development_assets
  report['shared_full']=args.full
  report['tools_only']=args.tools_only
  command('docker','stop','--time','45',container,timeout=60)
  stream_response.read();stream.close()
  check('active_mcp_sse_closes_on_graceful_shutdown')
  inspect=json.loads(command('docker','inspect',container))[0]
  assert inspect['State']['ExitCode']==0 and not inspect['State']['OOMKilled'],inspect['State']
  command('docker','start',container)
  main_port=int(command('docker','port',container,'8932/tcp').rsplit(':',1)[1]);dav_port=int(command('docker','port',container,'8933/tcp').rsplit(':',1)[1])
  import browser_client
  browser_client.BASE='http://127.0.0.1:'+str(main_port)
  ui_headers['Origin']=browser_client.BASE
  until=time.monotonic()+30
  while True:
   try:
    if request('GET','/healthz')[0]==200:break
   except OSError:pass
   assert time.monotonic()<until,'restart deadline';time.sleep(.2)
  assert request('GET',path,headers=ui_headers)[0]==401
  client=Client('desktop');client.initialize()
  assert 'ui-edited' in json.dumps(tool('read',{'path':'/workspace/agent-space/artifacts/from-workbench.txt'}))
  assert client.snapshot().get('page',{}).get('page_id')
  check('graceful_restart_preserves_workspace_and_expires_page_session')
  if args.full or args.tools_only:
   from qualification import verify_page_limit_restart
   report['page_limit_restart']=verify_page_limit_restart(output,name,lambda *a,**kw:command('docker',*a,**kw),check)
  if args.development_assets:assert source_digest()==report['source_digest'],'source changed during qualification'
  report['passed']=True
 except BaseException as error:
  report['error']=str(error).replace(token,'[REDACTED_SECRET]').replace(dav,'[REDACTED_SECRET]')
  if container:
   try:report['container_state']=json.loads(command('docker','inspect',container))[0]['State']
   except Exception:pass
   try:report['log_tail']=command('docker','logs',container)[-5000:].replace(token,'[REDACTED_SECRET]').replace(dav,'[REDACTED_SECRET]')
   except Exception:pass
   try:report['path_permissions']=command('docker','exec',container,'stat','-c','%a %u:%g %n','/opt/agent-space','/opt/agent-space/bin','/tmp/agent-space','/workspace/.agent-space')
   except Exception:pass
  raise
 finally:
  errors=[]
  if container:
   try:command('docker','stop','--time','45',container,timeout=60);command('docker','rm',container)
   except Exception as error:errors.append(str(error))
  if volume:
   try:command('docker','volume','rm',volume)
   except Exception as error:errors.append(str(error))
  for name in ['users.json']:(output/name).unlink(missing_ok=True)
  report['cleanup_passed']=not errors
  if errors:report['passed']=False;report['cleanup_errors']=errors
  (output/'result.json').write_text(json.dumps(report,indent=2)+'\n')
  print(str(output/'result.json'),flush=True)
  if errors:raise RuntimeError('qualification cleanup failed')

if __name__=='__main__':main()
