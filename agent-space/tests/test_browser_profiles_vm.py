#!/usr/bin/env python3
"""Profile-picker compatibility on a disposable qualification space, without Google credentials."""
import json,shlex,time,unittest,urllib.request
from browser_client import BASE,Client,Q,space_path,text

class BrowserProfiles(unittest.TestCase):
 def test_picker_reopen_profiles_and_last_used_preference(self):
  client=Client('desktop');client.initialize()
  maximum=int((Q/'max_pages').read_text()) if (Q/'max_pages').exists() else 4
  def tool(name,args):
   reply=client.tool(name,args);value=text(reply)
   self.assertFalse(reply.get('isError'),value);return value
  def run(code,**args):
   reply=client.tool('browser_run',{'code':code,**args});value=text(reply)
   while value.get('state')=='running':
    reply=client.tool('browser_task',{'task_id':value['task_id']});value=text(reply)
   return value,reply
  def bash(code):return tool('bash',{'cmd':code})
  def node(code):return bash('node -e '+shlex.quote('(async()=>{'+code+'})().then(()=>process.exit()).catch(e=>{console.error(e);process.exit(1)});'))
  def python(code):return bash('python3 -c '+shlex.quote(code))
  close="""let version;try{version=await(await fetch('http://127.0.0.1:9222/json/version')).json();}catch{return;}
await new Promise((resolve,reject)=>{const ws=new WebSocket(version.webSocketDebuggerUrl),timer=setTimeout(()=>reject(Error('close deadline')),5000);ws.onopen=()=>ws.send(JSON.stringify({id:1,method:'Browser.close'}));ws.onclose=()=>{clearTimeout(timer);resolve();};ws.onerror=reject;});await new Promise(r=>setTimeout(r,700));"""
  root='/workspace/.agent-space/desktop/profile'
  fixture='Page quota profile fixture'
  ui=BASE+space_path('desktop','desktop/api')
  request=urllib.request.Request(ui+'/login',data=json.dumps({'token':client.token}).encode(),headers={'Origin':BASE,'Content-Type':'application/json'})
  with urllib.request.urlopen(request) as response:
   cookie=response.headers['Set-Cookie'].split(';')[0];nonce=json.load(response)['nonce']
  def api(path,body=None):
   request=urllib.request.Request(ui+path,data=json.dumps(body).encode() if body is not None else None,headers={'Cookie':cookie,'Origin':BASE,'X-Desktop-Nonce':nonce,'Content-Type':'application/json'})
   with urllib.request.urlopen(request,timeout=60) as response:return json.load(response)
  node(close)
  python(f"""from pathlib import Path
import json,time
p=Path({root!r});state=p/'Local State';backup=p/'page-quota-local-state.backup'
assert not backup.exists();backup.write_bytes(state.read_bytes())
data=json.loads(state.read_text());profile=data.setdefault('profile',{{}})
profile['show_picker_on_startup']=True;profile['picker_shown']=True;profile['last_used']={fixture!r}
profile.setdefault('info_cache',{{}})[{fixture!r}]={{'name':{fixture!r},'active_time':time.time(),'is_using_default_name':False}}
d=p/{fixture!r};assert not d.exists();d.mkdir();(d/'Preferences').write_text(json.dumps({{'profile':{{'name':{fixture!r},'exit_type':'Normal'}}}}))
state.write_text(json.dumps(data))""")
  try:
   # No implicit tab, Playwright attachment, or account choice at the picker.
   reply=client.tool('browser_observe',{});value=text(reply)
   self.assertTrue(reply.get('isError'),value);self.assertEqual(value['error']['code'],'PROFILE_SELECTION_REQUIRED');self.assertEqual(value['tabs'],[])
   tool('screen_observe',{'view':'desktop'})
   value,reply=run('await output.image(await desktop.screenshot());return true;')
   self.assertFalse(reply.get('isError'),value);self.assertTrue(value['value']);self.assertTrue(any(c['type']=='image' for c in reply['content']))
   value,reply=run('return await page.title();')
   self.assertTrue(reply.get('isError'),value);self.assertEqual(value['error']['code'],'PROFILE_SELECTION_REQUIRED')
   api('/control',{'mode':'HUMAN'})
   for _ in range(2):self.assertTrue(api('/viewer',{})['desktop'])
   # The authenticated reopen endpoint leaves the picker and HUMAN ownership intact.
   reply=client.tool('browser_run',{'code':'return 1;'});self.assertIn('HUMAN_CONTROL',json.dumps(text(reply)))
   node("const targets=await(await fetch('http://127.0.0.1:9222/json/list')).json();if(targets.filter(t=>t.type==='page').length||!targets.some(t=>t.url==='chrome://profile-picker/'))throw Error('picker bypassed');")
   api('/control',{'mode':'AI'})
   # Test fixture invokes Chromium's own picker action; product code never selects a profile.
   value,reply=run("const picker=(await(await fetch('http://127.0.0.1:9222/json/list')).json()).find(t=>t.url==='chrome://profile-picker/');await new Promise((resolve,reject)=>{const w=new WebSocket(picker.webSocketDebuggerUrl);w.onopen=()=>w.send(JSON.stringify({id:1,method:'Runtime.evaluate',params:{expression:"+json.dumps("chrome.send('launchSelectedProfile',["+json.dumps(root+'/'+fixture)+"])")+"}}));w.onmessage=()=>{w.close();resolve();};w.onclose=resolve;w.onerror=reject;});return true;")
   self.assertFalse(reply.get('isError'),value)
   until=time.monotonic()+10
   while True:
    observed=client.tool('browser_observe',{});value=text(observed)
    if value.get('health',{}).get('page'):break
    self.assertLess(time.monotonic(),until,value);time.sleep(.05)
   value,reply=run("await page.goto('http://127.0.0.1:8124/');await page.evaluate(()=>localStorage.setItem('page-profile-fixture','selected'));return context.pages().length;")
   self.assertFalse(reply.get('isError'),value);self.assertEqual(value['value'],1)
   selected_id=tool('browser_observe',{})['page_id']
   # A second regular profile still shares the same Page quota.
   # No URL argument: opening the profile and appending a URL can create two
   # Pages, correctly exceeding a limit of two when the first profile is open.
   bash('DISPLAY=:1 '+shlex.quote('/opt/agent-tools/browsers/chromium-1243/chrome-linux64/chrome')+' --user-data-dir='+shlex.quote(root)+' --profile-directory=Default'+(' --no-sandbox' if (Q/'target').read_text().strip()=='server' else ''))
   time.sleep(1)
   value,reply=run("const other=context.pages().find(p=>p!==page);if(!other)throw Error('second profile did not open');await other.goto('data:text/html,<title>Other profile fixture</title>');return true;",page_id=selected_id)
   self.assertFalse(reply.get('isError'),value)
   value=tool('browser_observe',{});self.assertTrue(any(t['title']=='Other profile fixture' for t in value['tabs']),value)
   value,reply=run(f"while(context.pages().length<{maximum})await context.newPage();return context.pages().length;")
   self.assertFalse(reply.get('isError'),value);self.assertEqual(value['value'],maximum)
   value,reply=run('await context.newPage();')
   self.assertTrue(reply.get('isError'),value);self.assertEqual(value['error']['code'],'PAGE_LIMIT_REACHED')
   value,reply=run("const selected=context.pages().find(p=>p.url()==='http://127.0.0.1:8124/');await selected.bringToFront();return context.pages().map(p=>p.url()).sort();")
   self.assertFalse(reply.get('isError'),value);before=value['value']
   started=time.monotonic();print('Waiting for real 300-second multi-profile idle',flush=True)
   while api('/health').get('desktop') is not False:
    self.assertLess(time.monotonic()-started,340,'desktop did not sleep');time.sleep(2)
   self.assertGreater(time.monotonic()-started,285)
   python(f"from pathlib import Path;import json;state=json.loads((Path({root!r})/'Local State').read_text());assert state['was']['restarted'] is True;assert state['profile']['show_picker_on_startup'] is True")
   # This is the first browser call after sleep: no manual profile choice or retry.
   value,reply=run("return {tabs:context.pages().map(p=>p.url()).sort(),marker:await page.evaluate(()=>localStorage.getItem('page-profile-fixture'))};")
   self.assertFalse(reply.get('isError'),value);self.assertEqual(value['value'],{'tabs':before,'marker':'selected'})
   # Chromium may defer flushing its consumed restart preference to disk. The
   # explicit-close/fresh-launch assertion below verifies consumption behavior.
   python(f"from pathlib import Path;import json;state=json.loads((Path({root!r})/'Local State').read_text());assert state['profile']['show_picker_on_startup'] is True")
   print('PASS idle wake restores all profiles and the last selection without changing picker preference',flush=True)
   # An explicit close after waking is a fresh launch and must show the picker.
   node(close)
   reply=client.tool('browser_observe',{});value=text(reply)
   self.assertTrue(reply.get('isError'),value);self.assertEqual(value['error']['code'],'PROFILE_SELECTION_REQUIRED')
   node(close)
   python(f"""from pathlib import Path
import json
p=Path({root!r})/'Local State';data=json.loads(p.read_text());data['profile']['show_picker_on_startup']=False;data['profile']['last_used']={fixture!r};p.write_text(json.dumps(data))""")
   value=tool('browser_observe',{});self.assertTrue(value['tabs'])
   value,reply=run("return await page.evaluate(()=>localStorage.getItem('page-profile-fixture'));")
   self.assertFalse(reply.get('isError'),value);self.assertEqual(value['value'],'selected')
   python(f"""from pathlib import Path
import json
p=Path({root!r})/'Local State';assert json.loads(p.read_text())['profile']['show_picker_on_startup'] is False""")
  finally:
   api('/control',{'mode':'AI'})
   node(close)
   python(f"""from pathlib import Path
import shutil
p=Path({root!r});backup=p/'page-quota-local-state.backup';(p/'Local State').write_bytes(backup.read_bytes());backup.unlink();shutil.rmtree(p/{fixture!r})""")
   tool('browser_observe',{})

if __name__=='__main__':unittest.main(verbosity=2)
