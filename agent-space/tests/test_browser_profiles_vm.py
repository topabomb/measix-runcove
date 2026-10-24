#!/usr/bin/env python3
"""Profile-picker compatibility on a disposable qualification space, without Google credentials."""
import json,shlex,time,unittest,urllib.request
from browser_client import BASE,Client,Q,space_path,text,reset_tabs

class BrowserProfiles(unittest.TestCase):
 def test_picker_reopen_profiles_and_last_used_preference(self):
  client=Client('desktop');client.initialize()
  maximum=int((Q/'max_pages').read_text()) if (Q/'max_pages').exists() else 4
  def tool(name,args):
   return client.good(name,args)
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
   value=client.snapshot();self.assertEqual(value.get('browser_ui'),'profile_picker');self.assertEqual(client.snapshot(view='tabs')['tabs'],[])
   tool('screen_observe',{'view':'desktop'})
   value,reply=client.act([{'do':'press','target':'desktop','key':'Escape'}]);self.assertFalse(reply.get('isError'),value)
   reply=client.tool('browser_read',{});value,reply=client.finish(reply);self.assertTrue(reply.get('isError'));self.assertEqual(value['error']['code'],'PROFILE_SELECTION_REQUIRED')
   api('/control',{'mode':'HUMAN'})
   for _ in range(2):self.assertTrue(api('/viewer',{})['desktop'])
   value=client.snapshot();self.assertEqual(value.get('browser_ui'),'profile_picker');tool('screen_observe',{'view':'desktop'})
   value,reply=client.act([{'do':'press','target':'desktop','key':'Escape'}]);self.assertTrue(reply.get('isError'));self.assertIn('HUMAN_CONTROL',json.dumps(value))
   node("const targets=await(await fetch('http://127.0.0.1:9222/json/list')).json();if(targets.filter(t=>t.type==='page').length||!targets.some(t=>t.url==='chrome://profile-picker/'))throw Error('picker bypassed');")
   api('/control',{'mode':'AI'})
   # Only the fixture selects a profile; product code never chooses an account.
   node("const picker=(await(await fetch('http://127.0.0.1:9222/json/list')).json()).find(t=>t.url==='chrome://profile-picker/');await new Promise((resolve,reject)=>{const w=new WebSocket(picker.webSocketDebuggerUrl);w.onopen=()=>w.send(JSON.stringify({id:1,method:'Runtime.evaluate',params:{expression:"+json.dumps("chrome.send('launchSelectedProfile',["+json.dumps(root+'/'+fixture)+"])")+"}}));w.onmessage=()=>{w.close();resolve();};w.onclose=resolve;w.onerror=reject;});")
   until=time.monotonic()+10
   while True:
    value=client.snapshot()
    if value.get('page',{}).get('page_id') and value.get('browser_ui')!='profile_picker':break
    self.assertLess(time.monotonic(),until,value);time.sleep(.1)
   view=client.navigate('http://127.0.0.1:8124/');selected_id=view['page']['page_id']
   client.fixture_js("await page.evaluate(()=>localStorage.setItem('page-profile-fixture','selected'));return true;",selected_id)
   api('/control',{'mode':'HUMAN'})
   observed,_=client.finish(client.tool('browser_snapshot',{}));self.assertEqual(observed['control']['mode'],'HUMAN')
   client.read_browser();tool('screen_observe',{'view':'desktop'})
   value,reply=client.act([{'do':'wait','secs':1}]);self.assertTrue(reply.get('isError'));self.assertIn('HUMAN_CONTROL',json.dumps(value));api('/control',{'mode':'AI'})
   bash('DISPLAY=:1 '+shlex.quote('/opt/agent-tools/browsers/chromium-1243/chrome-linux64/chrome')+' --user-data-dir='+shlex.quote(root)+' --profile-directory=Default'+(' --no-sandbox' if (Q/'target').read_text().strip()=='server' else ''))
   time.sleep(1)
   other=next(t['page_id'] for t in client.snapshot(view='tabs')['tabs'] if t['page_id']!=selected_id)
   client.navigate('data:text/html,<title>Other profile fixture</title>',page_id=other)
   count=len(client.snapshot(view='tabs')['tabs'])
   if count<maximum:
    value,reply=client.act([{'do':'tab','action':'new'} for _ in range(maximum-count)]);self.assertFalse(reply.get('isError'),value)
   value,reply=client.act([{'do':'tab','action':'new'}]);self.assertTrue(reply.get('isError'));self.assertEqual(value['error']['code'],'PAGE_LIMIT_REACHED')
   value,reply=client.act([{'do':'tab','action':'focus','page_id':selected_id}]);self.assertFalse(reply.get('isError'),value)
   before=sorted(t['url'] for t in client.snapshot(view='tabs')['tabs'])
   started=time.monotonic();print('Waiting for real 300-second multi-profile idle',flush=True)
   while api('/health').get('desktop') is not False:
    self.assertLess(time.monotonic()-started,340,'desktop did not sleep');time.sleep(2)
   self.assertGreater(time.monotonic()-started,285)
   python(f"from pathlib import Path;import json;state=json.loads((Path({root!r})/'Local State').read_text());assert state['was']['restarted'] is True;assert state['profile']['show_picker_on_startup'] is True")
   value=client.snapshot();self.assertEqual(sorted(t['url'] for t in client.snapshot(view='tabs')['tabs']),before)
   self.assertEqual(client.fixture_js("return await page.evaluate(()=>localStorage.getItem('page-profile-fixture'));",value['page']['page_id']),'selected')
   python(f"from pathlib import Path;import json;state=json.loads((Path({root!r})/'Local State').read_text());assert state['profile']['show_picker_on_startup'] is True")
   print('PASS idle wake restores profiles and selection without changing picker preference',flush=True)
   node(close);self.assertEqual(client.snapshot().get('browser_ui'),'profile_picker')
   node(close)
   python(f"from pathlib import Path;import json;p=Path({root!r})/'Local State';data=json.loads(p.read_text());data['profile']['show_picker_on_startup']=False;data['profile']['last_used']={fixture!r};p.write_text(json.dumps(data))")
   value=client.snapshot();self.assertTrue(client.snapshot(view='tabs')['tabs'])
   self.assertEqual(client.fixture_js("return await page.evaluate(()=>localStorage.getItem('page-profile-fixture'));",value['page']['page_id']),'selected')
  finally:
   api('/control',{'mode':'AI'});node(close)
   python(f"from pathlib import Path;import shutil;p=Path({root!r});backup=p/'page-quota-local-state.backup';(p/'Local State').write_bytes(backup.read_bytes());backup.unlink();shutil.rmtree(p/{fixture!r})")
   client.snapshot()

if __name__=='__main__':unittest.main(verbosity=2)
