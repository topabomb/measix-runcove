"""Shared real browser/files/UI acceptance, used by both deployment drivers."""
import hashlib,os,subprocess,sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
SHARED_CHECKS={'shared_test_browser_vm.py','shared_test_browser_structured_vm.py','shared_test_browser_pages_vm.py','shared_test_browser_profiles_vm.py','shared_test_media_vm.py','shared_test_desktop_files_vm.py','shared_test_browser_lifecycle_vm.py','shared_real_vnc_keyboard_clipboard_viewers_responsive_and_idle'}
def run_shared(output,environment,passed,*,tools_only=False):
 environment=dict(environment,AGENT_SPACE_BROWSER_TEST_DIR=str(output))
 for suite in ['test_browser_vm.py','test_browser_structured_vm.py','test_browser_pages_vm.py','test_media_vm.py','test_browser_profiles_vm.py','test_desktop_files_vm.py','test_browser_lifecycle_vm.py']:
  if tools_only and suite not in ['test_browser_vm.py','test_browser_structured_vm.py','test_browser_pages_vm.py','test_media_vm.py','test_browser_profiles_vm.py']:continue
  with (output/(suite+'.log')).open('w') as log:
   result=subprocess.run([sys.executable,str(ROOT/'tests'/suite)],env=environment,stdout=log,stderr=subprocess.STDOUT,text=True,timeout=700)
  if result.returncode:raise RuntimeError('shared qualification failed: '+suite+'; see its log')
  passed('shared_'+suite)
 if tools_only:
  return {p.name:hashlib.file_digest(p.open('rb'),'sha256').hexdigest() for p in sorted(output.glob('*.log'))}
 result=subprocess.run(['npm','test','--','--reporter=list','--output='+str(output/'ui-results')],cwd=ROOT/'desktop-ui',env=environment,capture_output=True,text=True,timeout=700)
 (output/'ui.log').write_text(result.stdout+result.stderr)
 if result.returncode:raise RuntimeError('shared UI qualification failed; see ui.log')
 passed('shared_real_vnc_keyboard_clipboard_viewers_responsive_and_idle')
 return {p.name:hashlib.file_digest(p.open('rb'),'sha256').hexdigest() for p in sorted(output.glob('*.log'))}

def verify_page_limit_restart(output,container,command,passed):
 """Lower only this driver's disposable config, then verify profile and policy."""
 import json,time,tomllib,urllib.request,shlex
 assert container==output.name and container.startswith(('runcove-desktop-','runcove-qualify-'))
 config=output/'config.toml';original=config.read_text()
 maximum=tomllib.loads(original).get('browser',{}).get('max_pages',4)
 assert maximum>=2,'restart regression requires an initial limit of at least two'
 os.environ['AGENT_SPACE_BROWSER_TEST_DIR']=str(output)
 sys.path.insert(0,str(ROOT/'tests'))
 import browser_client
 def connect():
  port=int(command('port',container,'8932/tcp').strip().rsplit(':',1)[1])
  (output/'port').write_text(str(port));browser_client.BASE='http://127.0.0.1:'+str(port)
  until=time.monotonic()+90
  while True:
   try:
    with urllib.request.urlopen(browser_client.BASE+'/healthz',timeout=5) as response:
     if response.status==200:break
   except OSError:pass
   assert time.monotonic()<until,'Page restart readiness deadline';time.sleep(.2)
  client=browser_client.Client('desktop');client.initialize();return client
 client=connect()
 from browser_client import reset_tabs
 def node(code):
  reply=client.tool('bash',{'cmd':'node -e '+shlex.quote('(async()=>{'+code+'})().then(()=>process.exit()).catch(e=>{console.error(e);process.exit(1)});')})
  assert not reply.get('isError'),browser_client.text(reply)
 # The preceding owner-loss check deliberately leaves the old profile Crashed.
 # A clean-session quota regression must not inherit that unrelated crash prompt.
 # Use a new disposable profile; never rewrite exit_type on an existing profile.
 close="const v=await(await fetch('http://127.0.0.1:9222/json/version')).json();await new Promise((resolve,reject)=>{const w=new WebSocket(v.webSocketDebuggerUrl);w.onopen=()=>w.send(JSON.stringify({id:1,method:'Browser.close'}));w.onclose=resolve;w.onerror=reject;});await new Promise(r=>setTimeout(r,700));"
 node(close+"const fs=require('node:fs'),p='/workspace/.agent-space/desktop/profile',s=JSON.parse(fs.readFileSync(p+'/Local State'));s.profile.show_picker_on_startup=true;s.profile.picker_shown=true;s.profile.last_used='Quota restart fixture';s.profile.info_cache['Quota restart fixture']={name:'Quota restart fixture',active_time:Date.now()/1000,is_using_default_name:false};fs.mkdirSync(p+'/Quota restart fixture');fs.writeFileSync(p+'/Quota restart fixture/Preferences',JSON.stringify({profile:{name:'Quota restart fixture',exit_type:'Normal'}}));fs.writeFileSync(p+'/Local State',JSON.stringify(s));")
 def select_profile():
  node("const p=(await(await fetch('http://127.0.0.1:9222/json/list')).json()).find(t=>t.url==='chrome://profile-picker/');await new Promise((resolve,reject)=>{const w=new WebSocket(p.webSocketDebuggerUrl);w.onopen=()=>w.send(JSON.stringify({id:1,method:'Runtime.evaluate',params:{expression:\"chrome.send('launchSelectedProfile',['/workspace/.agent-space/desktop/profile/Quota restart fixture'])\"}}));w.onmessage=()=>{w.close();resolve();};w.onclose=resolve;w.onerror=reject;});await new Promise(r=>setTimeout(r,1000));")
 assert client.snapshot().get('browser_ui')=='profile_picker'
 select_profile()
 reset_tabs(client,'data:text/html,<title>Page restart preservation</title>')
 value,reply=client.act([{'do':'tab','action':'new','url':'data:text/html,<title>Preserved '+str(i)+'</title>'} for i in range(maximum-1)])
 assert not reply.get('isError') and len(client.snapshot(view='tabs')['tabs'])==maximum,value
 before=sorted(tab['url'] for tab in client.snapshot(view='tabs')['tabs'])
 # Select the same clean profile after restart; its saved URLs must survive even
 # when the new quota is below their count. The picker remains user-controlled.
 node(close+"const fs=require('node:fs'),p='/workspace/.agent-space/desktop/profile',preferences=JSON.parse(fs.readFileSync(p+'/Quota restart fixture/Preferences'));if(preferences.profile.exit_type!=='Normal')throw Error('quota fixture did not close cleanly');const s=JSON.parse(fs.readFileSync(p+'/Local State'));s.profile.show_picker_on_startup=true;s.profile.picker_shown=true;s.profile.last_used='Quota restart fixture';fs.writeFileSync(p+'/Local State',JSON.stringify(s));")
 command('stop','--time','60',container,timeout=90)
 # Keep the bind file inode: Docker Desktop may retain a replaced bind inode.
 if '\n[browser]' in original:
  import re
  updated=re.sub(r'(?m)^max_pages\s*=.*$', 'max_pages=1',original)
 else:updated=original+'\n[browser]\nmax_pages=1\n'
 config.write_text(updated)
 command('start',container)
 client=connect()
 observed=client.tool('browser_snapshot',{});value,reply=client.finish(observed)
 assert not reply.get('isError') and value['state']=='succeeded',value
 value=value.get('value',value)
 assert value.get('browser_ui')=='profile_picker' and client.snapshot(view='tabs')['tabs']==[],value
 select_profile()
 observed=client.tool('browser_snapshot',{});value,reply=client.finish(observed)
 assert not reply.get('isError') and value['state']=='succeeded',value
 value=value.get('value',value)
 assert sorted(tab['url'] for tab in client.snapshot(view='tabs')['tabs'])==before,value
 value,reply=client.act([{'do':'tab','action':'new'}])
 assert reply.get('isError') and value['error']['code']=='PAGE_LIMIT_REACHED' and '(1)' in value['error']['message'],value
 reset_tabs(client,'data:text/html,<title>Existing page remains usable</title>')
 assert len(client.snapshot(view='tabs')['tabs'])==1
 passed('reduced_page_limit_after_restart_preserves_profile_and_existing_pages')
 return {'initial_limit':maximum,'restarted_limit':1,'restored_pages':maximum,'restored_after_profile_picker':True,'clean_profile_verified':True,'restored_urls_match':True,'new_page_error':'PAGE_LIMIT_REACHED','existing_page_navigation_and_close':True}
