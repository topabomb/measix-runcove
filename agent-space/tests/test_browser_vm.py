#!/usr/bin/env python3
"""Real MSB browser qualification; use only an independent test deployment.
AGENT_SPACE_BROWSER_TEST_DIR contains users.json and port (untracked, mode 600).
The test users are 'desktop' and 'custom', the latter uses a literal OCI reference.
"""
import base64,hashlib,json,time,unittest
from pathlib import Path
from browser_client import Client,text,Q,TARGET,space_path

class BrowserQualification(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.client=Client('desktop');cls.client.initialize()
  def tool(name,args):
   result=cls.client.tool(name,args);assert not result.get('isError'),text(result);return text(result)
  fixture=(Path(__file__).parent/'fixtures/browser-site.cjs').read_text()
  tool('write',{'path':'/workspace/qualification/server.cjs','content':fixture})
  starter=(Path(__file__).parent/'fixtures/start-browser-site.py').read_text()
  tool('write',{'path':'/workspace/qualification/start-site.py','content':starter})
  started=tool('bash',{'command':"python3 /workspace/qualification/start-site.py && python3 -c \"from pathlib import Path;Path('/workspace/qualification/upload.bin').write_bytes(b'u'*1048576)\""})
  assert 'Qualification fixture ready' in json.dumps(started),started
  tool('bash',{'command':"python3 -c \"from pathlib import Path;p=Path('/workspace/qualification/private uploads');p.mkdir(exist_ok=True);p.chmod(0o700);f=p/'原生 reference.bin';f.write_bytes(b'u'*1048576);f.chmod(0o600)\""})
 @classmethod
 def browser(cls,code,timeout=60,success=True):
  reply=cls.client.tool('browser_run',{'code':code,'timeout_secs':timeout});value=text(reply)
  while value.get('state')=='running':
   reply=cls.client.tool('browser_task',{'task_id':value['task_id']});value=text(reply)
  if success: assert not reply.get('isError'),value
  else:
   assert reply.get('isError'),value
   assert 'observation' not in value,value
   assert len(value.get('error',{}).get('message',''))<=2048,value
  assert 'recent_tasks' not in value and 'observation' not in value
  return value,reply
 def test_01_catalogue_and_defaults(self):
  for name,count in ([('desktop',9)] if TARGET=='standalone' else [('custom',5),('desktop',9)]):
   client=Client(name);client.initialize();tools=client.call('tools/list')['tools'];self.assertEqual(len(tools),count)
   if name=='custom':
    with self.assertRaisesRegex(RuntimeError,'not enabled'):client.tool('browser_observe',{})
   for item in tools:self.assertNotIn('中文',item['description'])
  observed=text(self.client.tool('browser_observe',{}));self.assertTrue(observed['health']['browser']);self.assertTrue(observed['health']['page'])
  observed=text(self.client.tool('browser_observe',{'recent_tasks_limit':10}));self.assertLessEqual(len(observed['recent_tasks']),10)
  with self.assertRaisesRegex(RuntimeError,'recent_tasks_limit'):self.client.tool('browser_observe',{'recent_tasks_limit':11})
 def test_01b_concise_failures_and_string_arguments(self):
  tools={item['name']:item for item in self.client.call('tools/list')['tools']}
  for tool,field,choice in [('browser_observe','view','dom'),('browser_task','action','wait'),('web_publish','action','publish')]:
   desc=tools[tool]['inputSchema']['properties'][field]['description']
   self.assertIn(choice,desc)
  value,_=self.browser("require('node:fs');",success=False)
  self.assertIn('await import',value['error']['message']);self.assertNotIn('browser-worker.cjs:',value['error']['message'])
  direct=text(self.client.tool('browser_run',{'code':'return 1;','page_id':'missing-page'}))
  self.assertEqual(direct['state'],'failed');self.assertEqual(direct['side_effects'],'not_started')
  self.assertNotIn('observation',direct);self.assertEqual('PAGE_NOT_FOUND',direct['error']['code'])
  polled=text(self.client.tool('browser_task',{'task_id':direct['task_id'],'action':'wait','wait_secs':0}))
  self.assertEqual(direct,polled)
  value,_=self.browser("throw new Error('START '+ '中'.repeat(8000)+' END');",success=False)
  self.assertLessEqual(len(value['error']['message']),2048);self.assertIn('[error truncated]',value['error']['message']);self.assertTrue(value['error']['message'].endswith('END'))
  value,_=self.browser("await new Promise(r=>setTimeout(r,9000)); throw new Error('late failure');",success=False)
  self.assertIn('late failure',value['error']['message'])
  observed=text(self.client.tool('browser_observe',{'view':'dom'}));self.assertIn('dom',observed)

 def test_01c_optional_target_and_observation_identity(self):
  def run(code,page_id=None):
   reply=self.client.tool('browser_run',{'code':code,'page_id':page_id})
   value=text(reply)
   while value.get('state')=='running':
    reply=self.client.tool('browser_task',{'task_id':value['task_id']});value=text(reply)
   self.assertFalse(reply.get('isError'),value)
   return value
  original=text(self.client.tool('browser_observe',{}))['page_id']
  self.assertTrue(original)
  invalid=self.client.tool('browser_observe',{'page_id':'None'})
  self.assertTrue(invalid.get('isError'))
  self.assertEqual(text(invalid)['error']['code'],'PAGE_NOT_FOUND');self.assertIn('tabs',text(invalid))
  self.assertTrue(text(self.client.tool('browser_observe',{'page_id':None}))['health']['page'])
  capture=self.client.tool('browser_observe',{'view':'desktop','page_id':'None'})
  self.assertFalse(capture.get('isError'),text(capture))
  self.assertTrue(any(c['type']=='image' for c in capture['content']))
  self.assertEqual(run('return 1;')['value'],1)
  # Script results contain no automatic observation; explicit reads target the selected tab.
  value=run("const tab=await context.newPage();await tab.goto('data:text/html,<title>Other tab</title>');await tab.bringToFront();return true;",original)
  self.assertNotIn('observation',value)
  observed=text(self.client.tool('browser_observe',{}))
  other=next(t['page_id'] for t in observed['tabs'] if t['title']=='Other tab')
  value=run("await page.goto('data:text/html,<title>Selected target</title>');return await page.title();",original)
  self.assertEqual(value['value'],'Selected target');self.assertNotIn('observation',value)
  observed=text(self.client.tool('browser_observe',{'page_id':original}))
  self.assertEqual(observed['dom']['title'],'Selected target');self.assertNotIn('ax',observed)
  value=run('await page.close();return true;',other)
  self.assertNotIn('observation',value)
  self.assertTrue(text(self.client.tool('browser_observe',{}))['health']['page'])

 def test_02_upload_and_download(self):
  value,_=self.browser("await page.goto('http://127.0.0.1:8124');await page.locator('#file').setInputFiles('/workspace/qualification/upload.bin');await page.locator('#status').filter({hasText:'sha256'}).waitFor();return JSON.parse(await page.locator('#status').textContent());")
  self.assertEqual(value['value'],{'bytes':1048576,'sha256':hashlib.sha256(b'u'*1048576).hexdigest()})
  value,_=self.browser("const pending=page.waitForEvent('download');await page.getByRole('link',{name:'Download',exact:true}).click();return await output.download(await pending);")
  file=value['value'];self.assertEqual(file['bytes'],65536);self.assertEqual(len(value['files']),1)
  self.assertTrue(file['path'].startswith('/workspace/agent-space/artifacts/'),file)
  value,_=self.browser("const fs=await import('node:fs');const crypto=await import('node:crypto');return crypto.createHash('sha256').update(fs.readFileSync("+json.dumps(file['path'])+")).digest('hex');")
  self.assertEqual(value['value'],hashlib.sha256(bytes([123])*65536).hexdigest())
  # The event-based chooser also transmits the contents of a private file.
  value,_=self.browser("await page.reload();const pending=page.waitForEvent('filechooser');await page.locator('#file').click();await (await pending).setFiles('/workspace/qualification/private uploads/原生 reference.bin');await page.locator('#status').filter({hasText:'sha256'}).waitFor();return JSON.parse(await page.locator('#status').textContent());")
  self.assertEqual(value['value'],{'bytes':1048576,'sha256':hashlib.sha256(b'u'*1048576).hexdigest()})

 def test_02b_browser_and_desktop_share_workspace_identity(self):
  value,_=self.browser("const fs=await import('node:fs');const path=await import('node:path');const children=[];for(const pid of fs.readdirSync('/proc').filter(p=>/^\\d+$/.test(p))){try{const args=fs.readFileSync('/proc/'+pid+'/cmdline','utf8').split('\\0');const name=path.basename(args[0]);if(['chrome','Xtigervnc','lxqt-session','qterminal'].includes(name)){const uid=Number(fs.readFileSync('/proc/'+pid+'/status','utf8').match(/^Uid:\\s+(\\d+)/m)[1]);children.push({name,uid,noSandbox:args.includes('--no-sandbox')});}}catch{}}return {uid:process.getuid(),gid:process.getgid(),home:process.env.HOME,user:process.env.USER,children};")
  expected=1001 if TARGET=='standalone' else 0
  self.assertEqual(value['value']['uid'],expected);self.assertEqual(value['value']['gid'],expected)
  self.assertEqual(value['value']['home'],'/home/desktop' if TARGET=='standalone' else '/root')
  self.assertEqual(value['value']['user'],'desktop' if TARGET=='standalone' else 'root')
  children=value['value']['children'];self.assertTrue(any(p['name']=='chrome' for p in children));self.assertTrue(any(p['name']=='Xtigervnc' for p in children))
  self.assertTrue(all(p['uid']==expected for p in children),children)
  self.assertTrue(any(p['name']=='chrome' and p['noSandbox']==(expected==0) for p in children),children)

 def test_02c_download_from_desktop_without_playwright_listener(self):
  for failed in [False,True]:
   code="const fs=await import('node:fs');fs.rmSync('/workspace/agent-space/artifacts/fixture.bin',{force:true});await page.goto('http://127.0.0.1:8124');await page.bringToFront();await page.getByRole('link',{name:'Download',exact:true}).focus();"
   self.browser(code+("throw Error('qualification failure before user download');" if failed else 'return true;'),success=not failed)
   # A user keypress does not attach Playwright or install a temporary download directory.
   reply=self.client.tool('bash',{'command':"DISPLAY=:1 xdotool key Return; python3 -c \"import time,hashlib;from pathlib import Path;p=Path('/workspace/agent-space/artifacts/fixture.bin');deadline=time.monotonic()+15\nwhile not p.exists() and time.monotonic()<deadline: time.sleep(.1)\nassert p.read_bytes()==bytes([123])*65536;print(hashlib.sha256(p.read_bytes()).hexdigest())\""})
   self.assertFalse(reply.get('isError'),text(reply));self.assertIn(hashlib.sha256(bytes([123])*65536).hexdigest(),json.dumps(text(reply)))
 def test_03_deadline_and_cancellation(self):
  value,_=self.browser('while(true){}',2,False);self.assertEqual(value['state'],'timed_out')
  started=text(self.client.tool('browser_run',{'code':'await new Promise(r=>setTimeout(r,600000));','timeout_secs':30}))
  self.assertEqual(started['state'],'running')
  reply=self.client.tool('browser_task',{'task_id':started['task_id'],'action':'cancel','wait_secs':20});value=text(reply)
  self.assertTrue(reply.get('isError'));self.assertEqual(value['state'],'cancelled')
  value,_=self.browser('return {ok:true};');self.assertTrue(value['value']['ok'])
 def test_04_bounds_and_images(self):
  value,_=self.browser("for(let i=0;i<15000;i++)console.log('flood '+i+' '+ 'x'.repeat(100));return {ok:true};")
  logs=value['logs'];self.assertTrue(logs['truncated']);self.assertLessEqual(len(logs.get('stdout','').encode()),32768)
  value,_=self.browser("return 'x'.repeat(50000);");self.assertIn('output_file',value['value']);self.assertEqual(len(value['files']),1)
  observed=self.client.tool('browser_observe',{'view':'desktop'});data=text(observed)
  self.assertEqual(data['frames'][0]['width'],1440);self.assertEqual(data['frames'][0]['height'],900)
  self.assertEqual(len([c for c in observed['content'] if c['type']=='image']),1)
  with self.assertRaisesRegex(RuntimeError,'65536'):self.client.tool('browser_run',{'code':'//'+ '中'*23000})
  value,_=self.browser('//'+ '中'*21000+'\nreturn 7;');self.assertEqual(value['value'],7)
 def test_05_window_and_health(self):
  for mode in ['fullscreen','maximized']:
   value,_=self.browser("return await desktop.setWindowMode('"+mode+"');")
   self.assertEqual(value['value']['bounds']['windowState'],mode)
  value,_=self.browser("await page.goto('http://127.0.0.1:8124');await page.locator('#clipboard').fill('中文\\nhello 🙂');return await page.locator('#clipboard').inputValue();")
  self.assertEqual(value['value'],'中文\nhello 🙂')

 def test_05b_invalid_worker_result_is_terminal_and_releases_control(self):
  value,_=self.browser("const fs=await import('node:fs');fs.writeFileSync(process.env.AGENT_RESULT,'[]');process.stdout.write('malformed result',()=>process.exit(0));",success=False)
  self.assertEqual(value['state'],'failed',value)
  self.assertEqual('SCRIPT_ERROR',value['error']['code'])
  value,_=self.browser('return {recovered:true};')
  self.assertTrue(value['value']['recovered'])

 def test_06_native_chooser_and_stale_coordinates(self):
  self.browser("await page.goto('http://127.0.0.1:8124');await page.bringToFront();await page.locator('#file').focus();await desktop.press('space');return true;")
  self.assertTrue(text(self.client.tool('browser_observe',{}))['health']['native_window'])
  self.browser("await desktop.press('ctrl+l');await desktop.type('/workspace/qualification/private uploads/原生 reference.bin');return true;")
  # Fixed image's GTK chooser places Open at the lower right of its window.
  # Read the actual window geometry; coordinates still require a fresh capture.
  value,_=self.browser("const {execFileSync}=await import('node:child_process');const id=execFileSync('xdotool',['getactivewindow'],{encoding:'utf8'}).trim();return Object.fromEntries(execFileSync('xdotool',['getwindowgeometry','--shell',id],{encoding:'utf8'}).trim().split('\\n').map(s=>s.split('='))); ")
  geometry={k:int(v) for k,v in value['value'].items()}
  reply=self.client.tool('screen_observe',{'view':'desktop'});observed=text(reply);self.assertEqual(observed['captured_count'],1,observed)
  (Q/'native-upload.png').write_bytes(base64.b64decode(next(c['data'] for c in reply['content'] if c['type']=='image')))
  capture=observed['frames'][0]
  self.browser("await desktop.click("+json.dumps({'image_id':capture['image_id'],'x':geometry['X']+geometry['WIDTH']-55,'y':geometry['Y']+geometry['HEIGHT']-28})+");return true;")
  value,_=self.browser("await page.locator('#status').filter({hasText:'sha256'}).waitFor();return JSON.parse(await page.locator('#status').textContent());")
  self.assertEqual(value['value'],{'bytes':1048576,'sha256':hashlib.sha256(b'u'*1048576).hexdigest()})
  value,_=self.browser("await desktop.click({image_id:'"+capture['image_id']+"',x:10,y:10});",success=False)
  self.assertEqual('STALE_OBSERVATION',value['error']['code'])
  self.assertFalse(text(self.client.tool('browser_observe',{}))['health']['native_window'])
 def test_07_unresponsive_page_can_be_recovered_without_a_fresh_attachment(self):
  self.browser("const other=await context.newPage();await other.goto('http://127.0.0.1:8124');await other.bringToFront();return true;")
  self.browser("await page.evaluate(()=>{while(true){}});",timeout=2,success=False)
  observed=text(self.client.tool('browser_observe',{}));self.assertTrue(observed['health']['browser']);self.assertFalse(observed['health']['page'])
  self.browser("await desktop.press('ctrl+w');return true;")
  observed=text(self.client.tool('browser_observe',{}));self.assertTrue(observed['health']['page'],observed)

 def test_08_lost_http_waiter_never_replays_or_releases_the_running_task(self):
  import http.client,urllib.parse,time,json
  # The dedicated fixture page has persistent localStorage; reset only this test marker.
  result=text(self.client.tool('browser_run',{'code':"await page.goto('http://127.0.0.1:8124/');await page.evaluate(()=>localStorage.setItem('lostreply-count','0'));"}))
  while result['state']=='running':result=text(self.client.tool('browser_task',{'task_id':result['task_id']}))
  self.assertEqual(result['state'],'succeeded')
  from browser_client import BASE
  address=urllib.parse.urlsplit(BASE);connection=http.client.HTTPConnection(address.hostname,address.port,timeout=10)
  self.client.counter+=1
  payload={'jsonrpc':'2.0','id':self.client.counter,'method':'tools/call','params':{'name':'browser_run','arguments':{'code':"await page.evaluate(()=>localStorage.setItem('lostreply-count',String(Number(localStorage.getItem('lostreply-count'))+1)));await new Promise(r=>setTimeout(r,12000));return {once:true};",'timeout_secs':20}}}
  connection.request('POST',space_path('desktop','mcp'),json.dumps(payload),{'Authorization':'Bearer '+self.client.token,'Content-Type':'application/json','Accept':'application/json, text/event-stream','MCP-Protocol-Version':'2025-11-25','Mcp-Session-Id':self.client.session})
  time.sleep(1);connection.close()
  busy=text(self.client.tool('browser_run',{'code':'return 99;'}));self.assertIn('BUSY',json.dumps(busy))
  task=busy['active_task_id']
  result=text(self.client.tool('browser_task',{'task_id':task}))
  while result['state']=='running':result=text(self.client.tool('browser_task',{'task_id':task}))
  self.assertEqual(result['state'],'succeeded',result);self.assertEqual(result['value'],{'once':True})
  result=text(self.client.tool('browser_run',{'code':"return await page.evaluate(()=>localStorage.getItem('lostreply-count'));"}))
  while result['state']=='running':result=text(self.client.tool('browser_task',{'task_id':result['task_id']}))
  self.assertEqual(result['value'],'1')

 def test_09_root_browser_reopens_uid1001_profile(self):
  if TARGET!='server':self.skipTest('Server upgrade from the previous UID1001 desktop')
  value,_=self.browser("await page.goto('http://127.0.0.1:8124/');await page.evaluate(()=>localStorage.setItem('identity-restore','preserved'));await context.addCookies([{name:'identity-restore',value:'preserved',url:'http://127.0.0.1:8124',expires:Math.floor(Date.now()/1000)+3600}]);return context.pages().map(p=>p.url());")
  before=value['value']
  # Close only this disposable browser through its public CDP endpoint, then
  # reproduce the complete UID1001 profile left by the previous Server helper.
  close="const r=await fetch('http://127.0.0.1:9222/json/version');const ws=new WebSocket((await r.json()).webSocketDebuggerUrl);await new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(Error('close deadline')),10000);ws.onopen=()=>ws.send(JSON.stringify({id:1,method:'Browser.close'}));ws.onclose=()=>{clearTimeout(timer);resolve();};ws.onerror=reject;});"
  reply=self.client.tool('write',{'path':'/workspace/qualification/close-browser.mjs','content':close});self.assertFalse(reply.get('isError'),text(reply))
  reply=self.client.tool('bash',{'command':"node /workspace/qualification/close-browser.mjs && python3 -c \"import os,time;from pathlib import Path;time.sleep(1);p=Path('/workspace/.agent-space/desktop/profile');entries=[p,*p.rglob('*')];[(os.chown(f,1001,1001,follow_symlinks=False)) for f in entries];print('legacy profile entries:',len(entries))\""})
  self.assertFalse(reply.get('isError'),text(reply));self.assertIn('legacy profile entries:',json.dumps(text(reply)))
  reply=self.client.tool('browser_observe',{});self.assertFalse(reply.get('isError'),text(reply))
  value,_=self.browser("const fs=await import('node:fs');return {tabs:context.pages().map(p=>p.url()),marker:await page.evaluate(()=>localStorage.getItem('identity-restore')),cookie:(await context.cookies()).find(c=>c.name==='identity-restore')?.value,profileOwner:fs.statSync('/workspace/.agent-space/desktop/profile').uid,uid:process.getuid()};")
  # The display is still running: reopening Chromium can use the existing
  # UID1001 directory directly, without rewriting profile ownership or data.
  self.assertEqual(value['value'],{'tabs':before,'marker':'preserved','cookie':'preserved','profileOwner':1001,'uid':0})

if __name__=='__main__':unittest.main(verbosity=2)
