#!/usr/bin/env python3
"""Structured browser contract through the real MCP/VM task owners."""
import base64,hashlib,json,shlex,time,unittest
from pathlib import Path
from browser_client import Client,text,Q,TARGET,space_path,element,reset_tabs

class BrowserQualification(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.client=Client('desktop');cls.client.initialize()
  root=Path(__file__).parent/'fixtures'
  fixture=(root/'browser-site.cjs').read_text()+"\nrequire('./structured-site.cjs').start(8125);"
  for name,content in [('server.cjs',fixture),('structured-site.cjs',(root/'structured-browser-site.cjs').read_text()),('start-site.py',(root/'start-browser-site.py').read_text())]:
   cls.client.good('write',{'path':'/workspace/qualification/'+name,'content':content})
  cls.client.good('bash',{'cmd':"python3 /workspace/qualification/start-site.py && python3 -c \"from pathlib import Path;p=Path('/workspace/qualification/private uploads');p.mkdir(exist_ok=True);p.chmod(0o700);f=p/'原生 reference.bin';f.write_bytes(b'u'*1048576);f.chmod(0o600);Path('/workspace/qualification/upload.bin').write_bytes(b'u'*1048576)\""})
 def setUp(self):reset_tabs(self.client)
 def act(self,steps,success=True,**args):
  value,reply=self.client.act(steps,**args)
  self.assertEqual(not bool(reply.get('isError')),success,value)
  return value,reply
 def test_00_write_cannot_clear_files_and_bash_preserves_output_without_timing(self):
  path='/workspace/write-empty-guard.txt'
  self.client.good('write',{'path':path,'content':'keep me'})
  with self.assertRaisesRegex(RuntimeError,'nonempty'):
   self.client.tool('write',{'path':path,'content':''})
  self.assertIn('keep me',json.dumps(self.client.good('read',{'path':path})))
  for output,exit_code in [('plain output',0),('一行中文',7),('{"started_at":"user value","duration_secs":42}',0)]:
   import shlex
   result=self.client.tool('bash',{'cmd':'printf %s '+shlex.quote(output)+'; exit '+str(exit_code)})
   self.assertEqual(result['content'],[{'type':'text','text':output},{'type':'text','text':'Exit code: '+str(exit_code)}])
  schema=next(t['inputSchema'] for t in self.client.call('tools/list')['tools'] if t['name']=='web_publish')
  self.assertIn('keep_alive_secs',schema['properties']);self.assertNotIn('keep_alive_minutes',schema['properties'])
  value=self.client.good('web_publish',{'action':'publish','name':'seconds-check','file':path,'keep_alive_secs':60})
  self.assertTrue(value['url']);self.client.good('web_publish',{'action':'unpublish','name':'seconds-check'})
 def test_01_exact_catalogue_and_validation(self):
  for name,count in ([('desktop',9)] if TARGET=='standalone' else [('custom',5),('desktop',9)]):
   client=Client(name);client.initialize();tools={t['name']:t for t in client.call('tools/list')['tools']};self.assertEqual(len(tools),count)
   self.assertFalse({'browser_run','browser_observe','browser_task'}&tools.keys())
   if name=='desktop':
    self.assertTrue({'browser_snapshot','browser_act','browser_read','screen_observe'}<=tools.keys())
    (Q/'browser-tools.json').write_text(json.dumps([tools[key] for key in ['browser_snapshot','browser_act','browser_read','screen_observe']],ensure_ascii=False,indent=2)+'\n')
  observed=self.client.snapshot();self.assertTrue(observed.get('page',{}).get('page_id'));self.assertNotIn('health',observed);self.assertLessEqual(len(self.client.snapshot(view='tasks')['tasks']),10)
  for name,args in [('browser_act',{'code':'return 1'}),('browser_act',{'actions':[{'do':'wait','secs':1},{'do':'click','ref':'bad'}]}),('browser_read',{'query':'a','extract':'fields'}),('browser_act',{'op':'wait','task_id':'missing','actions':[{'do':'navigate','mode':'reload'}]})]:
   with self.assertRaises(RuntimeError):self.client.tool(name,args)
  began=time.monotonic();value,_=self.act([{'do':'wait','secs':1}],page_id='missing-page')
  self.assertGreaterEqual(time.monotonic()-began,1);self.assertEqual(value['completed_steps'],1);self.assertNotIn('page',value);self.assertNotIn('observation',value)
  self.assertEqual(text(self.client.tool('browser_act',{'op':'wait','task_id':value['task_id'],'wait_secs':0})),value)
 def test_02_upload_download_and_private_paths(self):
  view=self.client.snapshot();file_ref=element(view,'button','Upload fixture') if 'button "Upload fixture"' in view['observation'] else element(view,'textbox','Upload fixture')
  self.act([{'do':'upload','ref':file_ref,'paths':['/workspace/qualification/private uploads/原生 reference.bin']},{'do':'wait','text':'sha256'}])
  read=self.client.read_browser();self.assertIn(hashlib.sha256(b'u'*1048576).hexdigest(),read['text'])
  view=self.client.snapshot();value,_=self.act([{'do':'download','ref':element(view,'link','Download')}])
  file=value['files'][0];self.assertEqual(file['bytes'],65536)
  readback=self.client.good('bash',{'cmd':'sha256sum '+shlex.quote(file['path'])})
  self.assertIn(hashlib.sha256(bytes([123])*65536).hexdigest(),json.dumps(readback))
 def test_03_deadline_cancellation_and_stable_late_failure(self):
  value,_=self.act([{'do':'wait','secs':60}]*3,success=False);self.assertEqual(value['state'],'timed_out')
  started=text(self.client.tool('browser_act',{'actions':[{'do':'wait','secs':60}]}))
  self.assertEqual(started['state'],'running',started)
  value,reply=self.client.finish(self.client.tool('browser_act',{'op':'cancel','task_id':started['task_id']}));self.assertEqual(value['state'],'cancelled');self.assertTrue(reply.get('isError'))
  value,_=self.act([{'do':'wait','secs':9},{'do':'wait','text':'Never appears'}],success=False)
  self.assertEqual(value['state'],'failed');self.assertEqual(value['failed_step']['step_number'],2);self.assertEqual(value['error']['code'],'TIMEOUT');self.assertEqual(value['side_effects'],'not_started')
  self.assertEqual(text(self.client.tool('browser_act',{'op':'wait','task_id':value['task_id'],'wait_secs':0})),value)
  # Browser/user text never controls the supervisor's cleanup decision.
  value,_=self.act([{'do':'wait','text':'CLEANUP_UNCONFIRMED'}],success=False)
  self.assertEqual(value['error']['code'],'TIMEOUT');self.assertNotEqual(value.get('cleanup_confirmed'),False)
  self.act([{'do':'wait','secs':1}])
 def test_04_target_selection_and_tabs(self):
  original=self.client.snapshot()['page']['page_id'];value,_=self.act([{'do':'tab','action':'new','url':'data:text/html,<title>Other tab</title><button>Other</button>'}]);other=value['page']['page_id']
  self.assertNotEqual(other,original)
  value,_=self.act([{'do':'navigate','url':'data:text/html,<title>Selected target</title><button>Selected</button>'}],page_id=original)
  self.assertEqual(value['page']['title'],'Selected target')
  self.act([{'do':'tab','action':'focus','page_id':other}]);self.assertEqual(self.client.snapshot()['page']['page_id'],other)
  self.act([{'do':'tab','action':'close','page_id':other}]);self.assertEqual(self.client.snapshot()['page']['page_id'],original)
 def test_05_identity_window_and_unicode(self):
  expected=1001 if TARGET=='standalone' else 0
  readback=self.client.good('bash',{'cmd':"python3 -c \"import os;from pathlib import Path;rows=[]\nfor p in Path('/proc').glob('[0-9]*/cmdline'):\n try:\n  a=p.read_bytes().split(bytes([0]));name=Path(os.fsdecode(a[0])).name\n  if name in ['chrome','Xtigervnc','lxqt-session','qterminal']:rows.append((name,int(next(x for x in (p.parent/'status').read_text().splitlines() if x.startswith('Uid:')).split()[1]),b'--no-sandbox' in a))\n except OSError:pass\nassert rows and all(x[1]=="+str(expected)+" for x in rows),rows;assert any(x[0]=='chrome' and x[2]=="+str(expected==0)+" for x in rows);print('identity verified')\""})
  self.assertIn('identity verified',json.dumps(readback))
  for mode in ['fullscreen','maximized','normal']:self.act([{'do':'window','mode':mode}])
  view=self.client.snapshot();self.act([{'do':'fill','ref':element(view,'textbox','Clipboard fixture'),'text':'中文\nhello 🙂'}])
  fields=self.client.read_browser(extract='fields');self.assertIn('中文',json.dumps(fields,ensure_ascii=False))
  self.client.navigate('http://127.0.0.1:8125/long')
  frame=self.client.good('screen_observe',{'view':'desktop'})['frames'][0]
  self.act([{'do':'scroll','target':'desktop','image_id':frame['image_id'],'x':500,'y':400,'delta':3}])
  self.assertGreater(self.client.fixture_js('return await page.evaluate(()=>scrollY);'),0)
  frame=self.client.good('screen_observe',{'view':'desktop'})['frames'][0]
  self.act([{'do':'drag','image_id':frame['image_id'],'x':500,'y':400,'to_x':700,'to_y':500}])
  mouse=self.client.good('bash',{'cmd':'DISPLAY=:1 xdotool getmouselocation --shell'})
  self.assertIn('X=700',mouse['text']);self.assertIn('Y=500',mouse['text'])
 def test_06_native_chooser_and_stale_coordinates(self):
  self.client.fixture_js("await page.locator('#file').focus();return true;")
  self.act([{'do':'press','target':'desktop','key':'Space'}]);self.assertTrue(self.client.snapshot()['health']['native_window'])
  self.act([{'do':'press','target':'desktop','key':'Control+l'},{'do':'desktop_type','text':'/workspace/qualification/private uploads/原生 reference.bin'}])
  geometry=self.client.good('bash',{'cmd':'DISPLAY=:1 xdotool getactivewindow getwindowgeometry --shell'})
  geometry=dict(line.split('=',1) for line in geometry['text'].splitlines() if '=' in line)
  value,reply=self.client.finish(self.client.tool('screen_observe',{'view':'desktop'}));frame=value['frames'][0]
  (Q/'native-upload.png').write_bytes(base64.b64decode(next(c['data'] for c in reply['content'] if c['type']=='image')))
  self.act([{'do':'click','image_id':frame['image_id'],'x':int(geometry['X'])+int(geometry['WIDTH'])-55,'y':int(geometry['Y'])+int(geometry['HEIGHT'])-28}])
  self.act([{'do':'wait','text':'sha256'}]);self.assertIn(hashlib.sha256(b'u'*1048576).hexdigest(),self.client.read_browser()['text'])
  value,_=self.act([{'do':'click','image_id':frame['image_id'],'x':10,'y':10}],success=False);self.assertEqual(value['error']['code'],'STALE_OBSERVATION')
 def test_07_unresponsive_tab_observation_and_explicit_close(self):
  value,_=self.act([{'do':'tab','action':'new','url':'http://127.0.0.1:8124/'}]);target=value['page']['page_id']
  # Inject a frozen renderer through CDP; the product has no arbitrary JS tool.
  self.client.good('bash',{'cmd':"node -e "+shlex.quote("(async()=>{const t=(await(await fetch('http://127.0.0.1:9222/json/list')).json()).find(t=>t.id==="+json.dumps(target)+");const w=new WebSocket(t.webSocketDebuggerUrl);w.onopen=()=>{w.send(JSON.stringify({id:1,method:'Runtime.evaluate',params:{expression:'while(true){}'}}));setTimeout(()=>process.exit(),300)};})();")})
  observed=self.client.snapshot(page_id=target);self.assertFalse(observed['health']['page']);self.assertEqual(observed['page']['page_id'],target)
  self.client.good('screen_observe',{'view':'desktop'})
  self.act([{'do':'tab','action':'close','page_id':target}],page_id=target)
  self.assertNotIn('health',self.client.snapshot())
 def test_08_lost_http_waiter_does_not_replay_or_release_slot(self):
  import http.client,urllib.parse
  from browser_client import BASE
  self.client.fixture_js("await page.evaluate(()=>localStorage.setItem('lostreply-count','0'));return true;")
  ref=element(self.client.snapshot(),'button','Count once')
  address=urllib.parse.urlsplit(BASE);connection=http.client.HTTPConnection(address.hostname,address.port,timeout=10)
  self.client.counter+=1
  payload={'jsonrpc':'2.0','id':self.client.counter,'method':'tools/call','params':{'name':'browser_act','arguments':{'actions':[{'do':'click','ref':ref},{'do':'wait','secs':10},{'do':'wait','secs':2}]}}}
  connection.request('POST',space_path('desktop','mcp'),json.dumps(payload),{'Authorization':'Bearer '+self.client.token,'Content-Type':'application/json','Accept':'application/json, text/event-stream','MCP-Protocol-Version':'2025-11-25','Mcp-Session-Id':self.client.session})
  time.sleep(1);connection.close()
  busy=text(self.client.tool('browser_act',{'actions':[{'do':'wait','secs':1}]}));self.assertEqual(busy['error']['code'],'BUSY')
  result,reply=self.client.finish(self.client.tool('browser_act',{'op':'wait','task_id':busy['active_task_id']}));self.assertFalse(reply.get('isError'),result)
  self.assertEqual(self.client.fixture_js("return await page.evaluate(()=>localStorage.getItem('lostreply-count'));"),'1')
 def test_09_root_browser_reopens_uid1001_profile(self):
  if TARGET!='server':self.skipTest('Server upgrade from the previous UID1001 desktop')
  value=self.client.fixture_js("await page.goto('http://127.0.0.1:8124/');await page.evaluate(()=>localStorage.setItem('identity-restore','preserved'));await context.addCookies([{name:'identity-restore',value:'preserved',url:'http://127.0.0.1:8124',expires:Math.floor(Date.now()/1000)+3600}]);return context.pages().map(p=>p.url());")
  before=value
  # Close only this disposable browser through its public CDP endpoint, then
  # reproduce the complete UID1001 profile left by the previous Server helper.
  close="const r=await fetch('http://127.0.0.1:9222/json/version');const ws=new WebSocket((await r.json()).webSocketDebuggerUrl);await new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(Error('close deadline')),10000);ws.onopen=()=>ws.send(JSON.stringify({id:1,method:'Browser.close'}));ws.onclose=()=>{clearTimeout(timer);resolve();};ws.onerror=reject;});"
  reply=self.client.tool('write',{'path':'/workspace/qualification/close-browser.mjs','content':close});self.assertFalse(reply.get('isError'),text(reply))
  reply=self.client.tool('bash',{'cmd':"node /workspace/qualification/close-browser.mjs && python3 -c \"import os,time;from pathlib import Path;time.sleep(1);p=Path('/workspace/.agent-space/desktop/profile');entries=[p,*p.rglob('*')];[(os.chown(f,1001,1001,follow_symlinks=False)) for f in entries];print('legacy profile entries:',len(entries))\""})
  self.assertFalse(reply.get('isError'),text(reply));self.assertIn('legacy profile entries:',json.dumps(text(reply)))
  self.client.snapshot()
  value=self.client.fixture_js("const fs=await import('node:fs');return {tabs:context.pages().map(p=>p.url()),marker:await page.evaluate(()=>localStorage.getItem('identity-restore')),cookie:(await context.cookies()).find(c=>c.name==='identity-restore')?.value,profileOwner:fs.statSync('/workspace/.agent-space/desktop/profile').uid,uid:process.getuid()};")
  # The display is still running: reopening Chromium can use the existing
  # UID1001 directory directly, without rewriting profile ownership or data.
  self.assertEqual(value,{'tabs':before,'marker':'preserved','cookie':'preserved','profileOwner':1001,'uid':0})

 def test_transient_status_read_failure_does_not_cancel_the_task(self):
  reset_tabs(self.client)
  self.client.fixture_js("await page.evaluate(()=>{window.statusClicks=0;document.body.innerHTML='<button onclick=window.statusClicks++>Count once</button>'});return true;")
  button=element(self.client.snapshot(),'button','Count once')
  response=self.client.tool('browser_act',{'actions':[{'do':'click','ref':button},{'do':'wait','secs':45}],'then':'diff'})
  initial=text(response);self.assertEqual(initial['state'],'running',initial)
  code="""import os,signal,time,json
from pathlib import Path
found=[]
for p in Path('/proc').glob('[0-9]*/exe'):
 try:
  if p.resolve().name=='agent-space-desktop':found.append(int(p.parent.name))
 except OSError:pass
assert len(found)==1,found
pid=found[0]
try:
 os.kill(pid,signal.SIGSTOP);time.sleep(5)
finally:os.kill(pid,signal.SIGCONT)
print(json.dumps({'paused_seconds':5}))"""
  self.client.good('bash',{'cmd':'python3 -c '+shlex.quote(code)})
  value,reply=self.client.finish(response)
  self.assertFalse(reply.get('isError'),value);self.assertEqual(value['state'],'succeeded',value)
  self.assertGreaterEqual(value['duration_secs'],45)
  self.assertEqual(self.client.fixture_js('return await page.evaluate(()=>window.statusClicks);'),1)
  (Q/'transient-status-read.json').write_text(json.dumps(value,indent=2)+'\n')

if __name__=='__main__':unittest.main(verbosity=2)
