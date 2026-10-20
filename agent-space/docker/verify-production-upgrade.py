#!/usr/bin/env python3
"""Rehearse the exact pre-refactor production image using only newly created data.
Never attaches a production volume or reads production credentials.
"""
import argparse,hashlib,http.client,json,os,secrets,subprocess,time,uuid
from pathlib import Path
from verify import Client,docker
ROOT=Path(__file__).resolve().parents[1]
STATE='/var/lib/microsandbox/agent-space-state/state.json'
def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--image',required=True);p.add_argument('--old-image',required=True);a=p.parse_args()
 baseline=json.loads((ROOT.parent/'docs/evidence/runcove-split-20261015/production-baseline.json').read_text())
 old_id=docker('image','inspect',a.old_image,'--format','{{.Id}}').strip()
 assert old_id==baseline['imageId'],'source must be the exact audited production image'
 run='runcove-upgrade-'+uuid.uuid4().hex[:10];root=ROOT/'target/production-upgrade'/run;root.mkdir(parents=True,mode=0o700)
 admin_token=secrets.token_urlsafe(40);os.environ['AGENT_SPACE_ADMIN_TOKEN']=admin_token
 report={'target':'server','image':a.image,'image_id':docker('image','inspect',a.image,'--format','{{.Id}}').strip(),'old_image_id':old_id,'checks':[],'passed':False}
 a.image=report['image_id'];a.old_image=old_id
 containers=[];volumes=[]
 def check(name):report['checks'].append(name);print('PASS '+name,flush=True)
 def volume(suffix):
  name=docker('volume','create',run+'-'+suffix).strip();volumes.append(name);return name
 cfg=root/'old.toml'
 cfg.write_text('resource_prefix="rehearsal"\n[server]\nlisten="0.0.0.0:8932"\nmax_sessions=2\n[engine]\ntype="msb"\n[workspace]\nimage={preset="runcove-desktop"}\ncpus=2\nmemory_mib=3072\nworkspace_size_mib=8192\ntmpfs_size_mib=512\nshell="/bin/bash"\nexec_timeout_secs=30\n[lifecycle]\nsession_timeout_secs=900\nidle_timeout_secs=3600\ncheck_interval_secs=60\n[state]\ndir="/var/lib/microsandbox/agent-space-state"\n[admin]\ntoken_env="AGENT_SPACE_ADMIN_TOKEN"\n[webdav]\nenabled=true\nlisten="0.0.0.0:8933"\n');cfg.chmod(0o600)
 def common(data,config):return ['-e','AGENT_SPACE_ADMIN_TOKEN','-v',data+':/var/lib/microsandbox','-v',str(config)+':/etc/agent-space/config.toml:ro']
 def ready(name):
  until=time.monotonic()+180
  while time.monotonic()<until:
   info=json.loads(docker('inspect',name))[0];assert info['State']['Running'],'isolated service exited'
   mappings=info['NetworkSettings']['Ports']
   if mappings.get('8932/tcp'):
    main_port=int(mappings['8932/tcp'][0]['HostPort']);dav_port=int(mappings['8933/tcp'][0]['HostPort'])
    try:
     if request(main_port,'GET','/healthz')[0]==200:return main_port,dav_port
    except OSError:pass
   time.sleep(.3)
  raise TimeoutError('isolated service readiness')
 def launch(image,data,config,suffix):
  name=run+'-'+suffix
  docker('run','-d','--name',name,'--device','/dev/kvm','--stop-timeout','60','-p','127.0.0.1::8932','-p','127.0.0.1::8933',*common(data,config),image);containers.append(name)
  return name,*ready(name)
 def stop(name):
  docker('stop','--time','60',name,timeout=90);state=json.loads(docker('inspect',name))[0]['State'];assert state['ExitCode']==0 and not state['OOMKilled'],state
 def request(port,method,path,body=None,headers=None):
  c=http.client.HTTPConnection('127.0.0.1',port,timeout=120);c.request(method,path,body,headers or {});r=c.getresponse();status=r.status;raw=r.read();c.close();return status,raw
 def admin(port,method,path,body=None):
  status,raw=request(port,method,'/admin/v1/users'+path,json.dumps(body) if body is not None else None,{'Authorization':'Bearer '+admin_token,'Content-Type':'application/json'})
  assert status in (200,201),(status,raw.decode(errors='replace'));return json.loads(raw)
 def snapshot(name):return json.loads(docker('exec',name,'cat',STATE))
 def client(port,token):return Client(port,token,username='desktop',authority='127.0.0.1:'+str(port))
 def browser(c,code):
  value=json.loads(c.tool('browser_run',code=code))
  while value.get('state')=='running':value=json.loads(c.tool('browser_task',task_id=value['task_id']))
  assert value['state']=='succeeded',value;return value.get('value')
 def verify_space(name,port,dav_port,token,dav_token,original,expected_tools,bash_key='cmd'):
  assert snapshot(name)==original,'persistent identity/credential/resource/image changed'
  c=client(port,token);assert {tool['name'] for tool in c.request('tools/list')['tools']}==expected_tools
  text=c.tool('bash',**{bash_key:'cat /root/runcove-migration-marker /workspace/runcove-migration-marker'})
  assert 'root-before' in text and 'workspace-before' in text
  c.tool('bash',**{bash_key:'nohup python3 -m http.server 8128 --bind 127.0.0.1 --directory /workspace/migration-site >/tmp/migration-site.log 2>&1 </dev/null &'})
  assert browser(c,"await page.goto('http://127.0.0.1:8128/');return await page.evaluate(()=>localStorage.getItem('migration-profile'));")=='preserved'
  assert any(x['type']=='image' for x in c.tool_result('browser_observe',view='desktop')['content'])
  status,body=request(dav_port,'PROPFIND','/u/desktop/dav/',headers={'Authorization':'Bearer '+dav_token,'Depth':'1'});assert status==207
  assert request(dav_port,'GET','/u/desktop/dav/runcove-migration-marker',headers={'Authorization':'Bearer '+dav_token})==(200,b'workspace-before')
  return c
 try:
  data=volume('data');docker('run','--rm',*common(data,cfg),a.old_image,'init-state')
  source,port,dav_port=launch(a.old_image,data,cfg,'source')
  user=admin(port,'POST','',{'username':'desktop'});token=user['token'];space=user['agentSpaceId']
  dav_token=admin(port,'POST','/desktop/dav-token',{'agentSpaceId':space})['token']
  c=client(port,token)
  original_tools={tool['name'] for tool in c.request('tools/list')['tools']}
  c.tool('bash',command="printf root-before >/root/runcove-migration-marker; printf workspace-before >/workspace/runcove-migration-marker; mkdir -p /workspace/migration-site; printf '<title>Migration</title>' >/workspace/migration-site/index.html; nohup python3 -m http.server 8128 --bind 127.0.0.1 --directory /workspace/migration-site >/tmp/migration-site.log 2>&1 </dev/null &")
  browser(c,"await page.goto('http://127.0.0.1:8128/');await page.evaluate(()=>localStorage.setItem('migration-profile','preserved'));return true;")
  original=snapshot(source);original_hash=hashlib.sha256(json.dumps(original,sort_keys=True).encode()).hexdigest();stop(source)
  backup=root/'complete-data.tar.gz'
  docker('run','--rm','--entrypoint','tar','-v',data+':/data:ro','-v',str(root)+':/backup',a.old_image,'czf','/backup/'+backup.name,'-C','/data','.',timeout=600)
  report['backup_sha256']=hashlib.file_digest(backup.open('rb'),'sha256').hexdigest();report['original_state_sha256']=original_hash
  # Preview and apply conversion using read-only input and a separate output.
  converted=root/'new.toml'
  for apply in (False,True):
   result=docker('run','--rm','--entrypoint','agent-space-mcp','-v',str(root)+':/migration',a.image,'--config','/migration/old.toml','convert-config','--output','/migration/new.toml','--apply' if apply else '--dry-run')
   assert json.loads(result)['applied']==apply
   if not apply:assert not converted.exists()
  candidate,port,dav_port=launch(a.image,data,converted,'candidate')
  current_tools={'bash','read','write','edit','web_publish','browser_observe','screen_observe','browser_run','browser_task'}
  c=verify_space(candidate,port,dav_port,token,dav_token,original,current_tools);check('original_credentials_and_identity');check('workspace_profile_and_files')
  publication=json.loads(c.tool('web_publish',action='publish',name='migration',file='/workspace/runcove-migration-marker'))
  assert '/u/desktop/pub/migration/' in json.dumps(publication)
  c.tool('write',path='/workspace/candidate-only',content='preserve candidate separately')
  check('new_server_tools_desktop_dav_publish');stop(candidate);docker('start',candidate);port,dav_port=ready(candidate)
  # Explicit background apps are not assumed to survive a VM/container restart.
  c=client(port,token);assert 'workspace-before' in c.tool('read',path='/workspace/runcove-migration-marker');assert snapshot(candidate)==original
  check('restart_persistence');stop(candidate)
  rollback_data=volume('rollback')
  docker('run','--rm','--entrypoint','tar','-v',rollback_data+':/data','-v',str(root)+':/backup:ro',a.old_image,'xzf','/backup/'+backup.name,'-C','/data',timeout=600)
  rollback,port,dav_port=launch(a.old_image,rollback_data,cfg,'rollback');c=client(port,token)
  c.tool('bash',command='nohup python3 -m http.server 8128 --bind 127.0.0.1 --directory /workspace/migration-site >/tmp/migration-site.log 2>&1 </dev/null &')
  verify_space(rollback,port,dav_port,token,dav_token,original,original_tools,bash_key='command')
  assert 'backup-boundary-ok' in c.tool('bash',command='test ! -e /workspace/candidate-only && echo backup-boundary-ok')
  stop(rollback);check('isolated_complete_backup_rollback');report['passed']=True
 except BaseException as error:
  report['error']=str(error).replace(admin_token,'[REDACTED_SECRET]');raise
 finally:
  errors=[]
  for name in reversed(containers):
   try:docker('rm','-f','-v',name)
   except Exception:errors.append('container '+name)
  for name in reversed(volumes):
   try:docker('volume','rm',name)
   except Exception:errors.append('volume '+name)
  report['cleanup_passed']=not errors
  if errors:report['passed']=False;report['cleanup_errors']=errors
  (root/'result.json').write_text(json.dumps(report,indent=2)+'\n');print(root/'result.json',flush=True)
  if errors:raise RuntimeError('migration qualification cleanup failed')
if __name__=='__main__':main()
