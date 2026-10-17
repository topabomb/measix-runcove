#!/usr/bin/env python3
"""Full shared desktop qualification for an exact Server release image.
Run docker/verify.py first for the same image; all resources here are disposable.
"""
import argparse,hashlib,http.client,json,os,secrets,time,uuid
from pathlib import Path
from verify import docker
from qualification import run_shared
from release_identity import source_digest
ROOT=Path(__file__).resolve().parents[1]
def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--image',required=True);p.add_argument('--lifecycle',type=Path,required=True)
 p.add_argument('--development-assets',action='store_true',help='Bind current debug binaries/UI for source regression; not release qualification')
 a=p.parse_args()
 image_id=docker('image','inspect',a.image,'--format','{{.Id}}').strip();lifecycle=json.loads(a.lifecycle.read_text())
 assert lifecycle.get('passed') and lifecycle.get('cleanup_passed') and lifecycle.get('image_id')==image_id and lifecycle.get('idle_seconds',0)>300,'matching complete lifecycle qualification required'
 name='runcove-desktop-'+uuid.uuid4().hex[:10];output=ROOT/'target/server-desktop-verification'/name;output.mkdir(parents=True,mode=0o700)
 report={'target':'server','image':a.image,'image_id':image_id,'scope':'development assets' if a.development_assets else 'final image','full':True,'checks':[],'passed':False,'idle_seconds':lifecycle['idle_seconds'],'lifecycle_report_sha256':hashlib.sha256(a.lifecycle.read_bytes()).hexdigest()}
 a.image=image_id
 previous_token=os.environ.get('AGENT_SPACE_ADMIN_TOKEN')
 token=secrets.token_urlsafe(40);os.environ['AGENT_SPACE_ADMIN_TOKEN']=token;volume=None;container=None;users={}
 def request(port,method,path,body=None):
  c=http.client.HTTPConnection('127.0.0.1',port,timeout=15);c.request(method,path,json.dumps(body) if body is not None else None,{'Authorization':'Bearer '+token,'Content-Type':'application/json'});r=c.getresponse();status=r.status;raw=r.read();c.close();return status,raw
 def check(name):report['checks'].append(name);print('PASS '+name,flush=True)
 try:
  volume=docker('volume','create',name+'-data').strip()
  config=output/'config.toml';config.write_text('resource_prefix="desktopqualification"\n[server]\nlisten="0.0.0.0:8932"\nmax_running_spaces=2\n[workspace]\nimage={preset="runcove-desktop"}\ncpus=2\nmemory_mib=3072\nworkspace_size_mib=8192\n[state]\ndir="/var/lib/microsandbox/agent-space-state"\n[admin]\ntoken_env="AGENT_SPACE_ADMIN_TOKEN"\n[webdav]\nenabled=false\n');config.chmod(0o600)
  common=['-e','AGENT_SPACE_ADMIN_TOKEN','-v',volume+':/var/lib/microsandbox','-v',str(config)+':/etc/agent-space/config.toml:ro']
  if a.development_assets:
   report['source_digest']=source_digest()
   for binary in ['agent-space-mcp','agent-space-static-http','agent-space-webdav','agent-space-desktop']:
    common+=['--mount',f'type=bind,src={ROOT}/target/debug/{binary},dst=/usr/local/bin/{binary},readonly']
   common+=['--mount',f'type=bind,src={ROOT}/desktop-ui/dist,dst=/opt/agent-space/desktop-ui,readonly']
  docker('run','--rm',*common,a.image,'init-state')
  container=name;docker('run','-d','--name',container,'--device','/dev/kvm','--stop-timeout','60','-p','127.0.0.1::8932',*common,a.image)
  until=time.monotonic()+180
  while True:
   info=json.loads(docker('inspect',container))[0];assert info['State']['Running'],'Server exited before readiness'
   if info['NetworkSettings']['Ports'].get('8932/tcp'):
    port=int(info['NetworkSettings']['Ports']['8932/tcp'][0]['HostPort'])
    try:
     if request(port,'GET','/healthz')[0]==200:break
    except OSError:pass
   assert time.monotonic()<until,'Server readiness deadline';time.sleep(.3)
  users={}
  for name,image in [('desktop',{'preset':'runcove-desktop'}),('custom',{'custom':'nikolaik/python-nodejs:python3.12-nodejs22-slim'})]:
   # Literal custom selection remains a string at the public contract boundary.
   body={'username':name,'image':image if name=='desktop' else 'nikolaik/python-nodejs:python3.12-nodejs22-slim'}
   status,raw=request(port,'POST','/admin/v1/users',body);assert status==201,(status,raw.decode(errors='replace'));users[name]=json.loads(raw)
  (output/'port').write_text(str(port));(output/'target').write_text('server');secret=output/'users.json';secret.write_text(json.dumps(users));secret.chmod(0o600)
  report['suite_logs']=run_shared(output,os.environ,check)
  if a.development_assets:assert source_digest()==report['source_digest'],'source changed during qualification'
  report['passed']=True
 finally:
  if previous_token is None:os.environ.pop('AGENT_SPACE_ADMIN_TOKEN',None)
  else:os.environ['AGENT_SPACE_ADMIN_TOKEN']=previous_token
  if not report['passed'] and container:
   try:
    diagnostic=docker('logs',container)
    for secret_value in [token,*[value['token'] for value in users.values()]]:
     diagnostic=diagnostic.replace(secret_value,'[REDACTED_SECRET]')
    (output/'server-failure.log').write_text(diagnostic)
    report['failure_log_sha256']=hashlib.sha256(diagnostic.encode()).hexdigest()
   except Exception:report['failure_log_unavailable']=True
  errors=[]
  if container:
   try:
    docker('stop','--time','60',container,timeout=90);state=json.loads(docker('inspect',container))[0]['State'];assert state['ExitCode']==0 and not state['OOMKilled'];docker('rm','-v',container)
   except Exception:errors.append('container '+container)
  if volume:
   try:docker('volume','rm',volume)
   except Exception:errors.append('volume '+volume)
  (output/'users.json').unlink(missing_ok=True);report['cleanup_passed']=not errors
  if errors:report['passed']=False;report['cleanup_errors']=errors
  (output/'result.json').write_text(json.dumps(report,indent=2)+'\n');print(output/'result.json',flush=True)
  if errors:raise RuntimeError('Server desktop qualification cleanup failed')
if __name__=='__main__':main()
