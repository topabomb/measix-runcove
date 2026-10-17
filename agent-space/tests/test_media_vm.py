#!/usr/bin/env python3
"""Real MCP image and observation acceptance on disposable qualification hosts."""
import base64
import hashlib
import json
import time
import unittest
import concurrent.futures
import urllib.request
from pathlib import Path
from browser_client import Client, TARGET, BASE, space_path, text


class MediaQualification(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.client=Client('desktop');cls.client.initialize()

 @classmethod
 def finish(cls,reply):
  value=text(reply)
  while value.get('state')=='running':
   reply=cls.client.tool('browser_task',{'task_id':value['task_id']});value=text(reply)
  return value,reply

 def test_01_file_images_on_desktop_and_custom_workspaces(self):
  fixture=(Path(__file__).parent/'fixtures/read-images.py').read_text()
  self.assertFalse(self.client.tool('write',{'path':'/workspace/qualification/read-images.py','content':fixture}).get('isError'))
  formats,_=self.finish(self.client.tool('browser_run',{'code':"return await page.evaluate(()=>{const c=document.createElement('canvas');c.width=40;c.height=20;const x=c.getContext('2d');x.fillStyle='red';x.fillRect(0,0,40,20);return {jpeg:c.toDataURL('image/jpeg').split(',')[1],webp:c.toDataURL('image/webp').split(',')[1]};});"}))
  for name in ['desktop'] if TARGET=='standalone' else ['desktop','custom']:
   client=Client(name);client.initialize()
   client.tool('write',{'path':'/workspace/qualification/read-images.py','content':fixture})
   client.tool('write',{'path':'/workspace/qualification/formats.json','content':json.dumps(formats['value'])})
   meta=text(client.tool('bash',{'command':'python3 /workspace/qualification/read-images.py'}))
   self.assertFalse(meta.get('isError'),meta)
   root='/workspace/qualification/media/'
   disabled=client.tool('read',{'path':root+'wrong-extension.txt'})
   self.assertEqual(text(disabled)['error']['code'],'MULTIMODAL_REQUIRED')
   for file in ['wrong-extension.txt','transparent.png','jpeg','webp','first.gif','animated.png','animated.webp','orientation.jpeg','near-limit.png']:
    reply=client.tool('read',{'path':root+file,'multimodal':True})
    self.assertFalse(reply.get('isError'),text(reply))
    images=[c for c in reply['content'] if c['type']=='image'];self.assertEqual(len(images),1)
    raw=base64.b64decode(images[0]['data'],validate=True)
    self.assertNotIn('data',text(reply));self.assertNotIn('structuredContent',reply)
    if file=='wrong-extension.txt':
     self.assertGreater(len(raw),50*1024)
     expected=meta['sha256']
     self.assertEqual(hashlib.sha256(raw).hexdigest(),expected)
    if file in ['first.gif','animated.png','animated.webp']:self.assertTrue(text(reply)['first_frame_only'])
    if file in ['animated.png','animated.webp']:
     data='data:'+images[0]['mimeType']+';base64,'+images[0]['data']
     code="return await page.evaluate(async data=>{const im=new Image();im.src=data;await im.decode();const c=document.createElement('canvas');c.width=im.width;c.height=im.height;const x=c.getContext('2d');x.drawImage(im,0,0);return Array.from(x.getImageData(0,0,1,1).data);},"+json.dumps(data)+");"
     pixel,_=self.finish(self.client.tool('browser_run',{'code':code}));self.assertGreater(pixel['value'][0],240);self.assertLess(pixel['value'][2],20)
    if file=='transparent.png':self.assertEqual(raw[25],6);self.assertTrue(text(reply)['resized'])
    if file=='orientation.jpeg':self.assertTrue(text(reply)['orientation_corrected']);self.assertEqual((text(reply)['width'],text(reply)['height']),(20,40))
   for args in [{'offset':1},{'limit':500},{'offset':1,'limit':500}]:
    with self.assertRaisesRegex(RuntimeError,'only to text'):
     client.tool('read',dict(path=root+'transparent.png',multimodal=True,**args))
   for file in ['corrupt.png','large.png','pixels.png','unsupported.bmp']:
    reply=client.tool('read',{'path':root+file,'multimodal':True})
    self.assertTrue(reply.get('isError'),(file,text(reply)))
    self.assertFalse(any(c['type']=='image' for c in reply['content']))
   for file in ['text.png','utf16.txt','long.txt','large.txt']:
    ordinary=client.tool('read',{'path':root+file});enabled=client.tool('read',{'path':root+file,'multimodal':True})
    self.assertEqual(ordinary,enabled,file)
   if name=='custom':
    with self.assertRaisesRegex(RuntimeError,'not enabled'):client.tool('screen_observe',{'view':'desktop'})

 def test_02_screenshots_semantics_and_intervals(self):
  self.finish(self.client.tool('browser_run',{'code':"await page.goto('data:text/html,<title>Observe fixture</title><button>Continue</button>');return 7;"}))
  dom=text(self.client.tool('browser_observe',{}));self.assertIn('dom',dom);self.assertNotIn('ax',dom);self.assertNotIn('recent_tasks',dom);self.assertNotIn('can_input',dom['control'])
  ax=text(self.client.tool('browser_observe',{'view':'ax'}));self.assertIn('ax',ax);self.assertNotIn('dom',ax)
  legacy=text(self.client.tool('browser_observe',{'view':'desktop','recent_tasks_limit':1}))
  self.assertEqual(len(legacy['recent_tasks']),1)
  for view in ['page','desktop']:
   value,reply=self.finish(self.client.tool('screen_observe',{'view':view,'count':3,'interval_ms':300}))
   self.assertFalse(reply.get('isError'),value);self.assertEqual(value['captured_count'],3)
   self.assertEqual(len([c for c in reply['content'] if c['type']=='image']),3)
   self.assertLessEqual(sum(len(base64.b64decode(c['data'])) for c in reply['content'] if c['type']=='image'),2*1024*1024)
   self.assertNotIn('dom',value);self.assertNotIn('ax',value);self.assertNotIn('captures',value)
   self.assertNotIn('files',value)
   frames=value['frames'];self.assertEqual([f['index'] for f in frames],[1,2,3])
   self.assertTrue(all(b['elapsed_ms']-a['elapsed_ms']>=290 for a,b in zip(frames,frames[1:])))
  after=text(self.client.tool('browser_observe',{}));self.assertEqual(dom['page_id'],after['page_id'])
  value,reply=self.finish(self.client.tool('screen_observe',{'view':'desktop','count':8,'interval_ms':0,'page_id':'missing'}))
  self.assertFalse(reply.get('isError'),value);self.assertEqual(value['captured_count'],8)

 def test_02b_original_screenshots_are_downloadable_and_keep_capture_bytes(self):
  tools=self.client.call('tools/list',{})['tools']
  schema=next(t['inputSchema'] for t in tools if t['name']=='screen_observe')
  self.assertEqual(schema['required'],['view'])
  for name in ['page_id','count','interval_ms','save_original','save_dir']:
   self.assertIn('Optional',schema['properties'][name]['description'])
  with self.assertRaisesRegex(RuntimeError,'save_dir requires'):
   self.client.tool('screen_observe',{'view':'desktop','save_dir':'/workspace/qualification/originals'})
  with self.assertRaisesRegex(RuntimeError,'absolute'):
   self.client.tool('screen_observe',{'view':'desktop','save_original':True,'save_dir':'relative'})
  for view,count,directory in [('page',1,None),('desktop',8,'/workspace/qualification/originals')]:
   args={'view':view,'save_original':True,'count':count,'interval_ms':0}
   if directory:args['save_dir']=directory
   value,reply=self.finish(self.client.tool('screen_observe',args))
   self.assertFalse(reply.get('isError'),value);self.assertEqual(len(value['files']),count)
   paths=[]
   for frame,file in zip(value['frames'],value['files']):
    path=file['path'];paths.append(path)
    self.assertEqual(str(Path(path).parent),directory or '/workspace/agent-space/artifacts')
    self.assertRegex(Path(path).name,r'^screen-[0-9A-Za-z]{10}\.png$')
    self.assertEqual(file['frame_index'],frame['index']);self.assertEqual(file['image_id'],frame['image_id'])
    self.assertGreaterEqual(file['width'],frame['width']);self.assertGreaterEqual(file['height'],frame['height'])
    saved=self.client.tool('read',{'path':path,'multimodal':True})
    self.assertFalse(saved.get('isError'),text(saved))
    image=next(c for c in saved['content'] if c['type']=='image')
    self.assertEqual(image['mimeType'],'image/png')
    self.assertEqual(len(base64.b64decode(image['data'])),file['bytes'])
   self.assertEqual(len(paths),len(set(paths)))
   code="const fs=await import('node:fs');return "+json.dumps(paths)+".map(path=>{const b=fs.readFileSync(path);return {png:b.subarray(0,8).toString('hex'),width:b.readUInt32BE(16),height:b.readUInt32BE(20),bytes:b.length};});"
   disk,_=self.finish(self.client.tool('browser_run',{'code':code}))
   for actual,file in zip(disk['value'],value['files']):
    self.assertEqual(actual,dict(png='89504e470d0a1a0a',width=file['width'],height=file['height'],bytes=file['bytes']))
   if view=='page':
    # Use the existing authenticated workbench stream to download the original.
    base=BASE+space_path('desktop','desktop/api/')
    body=json.dumps({'token':self.client.token}).encode()
    with urllib.request.urlopen(urllib.request.Request(base+'login',data=body,headers={'Content-Type':'application/json','Origin':BASE}),timeout=10) as response:
     cookie=response.headers['Set-Cookie'].split(';',1)[0];nonce=json.load(response)['nonce']
    headers={'Cookie':cookie,'X-Desktop-Nonce':nonce,'Origin':BASE}
    with urllib.request.urlopen(urllib.request.Request(base+'files'+paths[0].removeprefix('/workspace'),headers=headers),timeout=10) as response:
     downloaded=response.read()
    self.assertEqual(len(downloaded),value['files'][0]['bytes']);self.assertTrue(downloaded.startswith(bytes.fromhex('89504e470d0a1a0a')))
  value,reply=self.finish(self.client.tool('screen_observe',{'view':'desktop','save_original':True,'save_dir':'/root/originals'}))
  self.assertTrue(reply.get('isError'));self.assertEqual(value['error']['code'],'SCREEN_SAVE_FAILED')
  self.assertEqual(value['captured_count'],1);self.assertNotIn('files',value)

 def test_03_cancel_sequence_preserves_frames_and_allows_browser_work(self):
  value=text(self.client.tool('screen_observe',{'view':'page','count':8,'interval_ms':3000,'save_original':True,'save_dir':'/workspace/qualification/cancelled-originals'}))
  self.assertEqual(value['state'],'running',value)
  task=value['task_id']
  run,reply=self.finish(self.client.tool('browser_run',{'code':'return {parallel:true};'}))
  self.assertFalse(reply.get('isError'),run)
  value,reply=self.finish(self.client.tool('browser_task',{'task_id':task,'action':'cancel'}))
  self.assertEqual(value['state'],'cancelled');self.assertGreaterEqual(value['captured_count'],1)
  self.assertLess(value['captured_count'],8)
  self.assertEqual(value['captured_count'],len([c for c in reply['content'] if c['type']=='image']))
  self.assertEqual(value['captured_count'],len(value['files']))
  repeated=self.client.tool('browser_task',{'task_id':task,'wait_secs':0});self.assertEqual(reply,repeated)

 def test_04_explicit_outputs_survive_failure_and_file_does_not_preview(self):
  code="const fs=await import('node:fs');const path=artifacts+'/partial.txt';fs.writeFileSync(path,'retained');await output.file(path);await output.image(await page.screenshot({type:'jpeg'}));throw new Error('after output');"
  value,reply=self.finish(self.client.tool('browser_run',{'code':code}))
  self.assertTrue(reply.get('isError'));self.assertEqual(len(value['files']),1)
  self.assertEqual(len([c for c in reply['content'] if c['type']=='image']),1)
  value,reply=self.finish(self.client.tool('browser_run',{'code':"const fs=await import('node:fs');const path=artifacts+'/registered.png';fs.writeFileSync(path,await page.screenshot());await output.file(path);return true;"}))
  self.assertFalse(reply.get('isError'),value);self.assertFalse(any(c['type']=='image' for c in reply['content']))
  value,reply=self.finish(self.client.tool('browser_run',{'code':'await desktop.click(10,20);'}))
  self.assertEqual(value['error']['code'],'INVALID_ARGUMENT',value)
  self.assertIn('desktop.click({image_id,x,y})',value['error']['message'])
  code="const fs=await import('node:fs');const crypto=await import('node:crypto');const bytes=fs.readFileSync('/workspace/qualification/media/large-output.png');await output.image(bytes);return {bytes:bytes.length,sha256:crypto.createHash('sha256').update(bytes).digest('hex')};"
  value,reply=self.finish(self.client.tool('browser_run',{'code':code}))
  self.assertFalse(reply.get('isError'),value)
  raw=base64.b64decode(next(c['data'] for c in reply['content'] if c['type']=='image'),validate=True)
  self.assertEqual(len(raw),value['value']['bytes']);self.assertEqual(hashlib.sha256(raw).hexdigest(),value['value']['sha256'])
  self.assertGreater(len(json.dumps(reply).encode()),4*1024*1024);self.assertLess(len(json.dumps(reply).encode()),12*1024*1024)
  code="const fs=await import('node:fs');const path=artifacts+'/before-timeout.txt';fs.writeFileSync(path,'saved');await output.file(path);while(true){}"
  value,reply=self.finish(self.client.tool('browser_run',{'code':code,'timeout_secs':2}))
  self.assertEqual(value['state'],'timed_out');self.assertEqual(len(value['files']),1)
  code="const fs=await import('node:fs');const path=artifacts+'/before-cancel.txt';fs.writeFileSync(path,'saved');await output.file(path);await new Promise(r=>setTimeout(r,600000));"
  value=text(self.client.tool('browser_run',{'code':code,'timeout_secs':60}))
  self.assertEqual(value['state'],'running')
  value,reply=self.finish(self.client.tool('browser_task',{'task_id':value['task_id'],'action':'cancel'}))
  self.assertEqual(value['state'],'cancelled');self.assertEqual(len(value['files']),1)

 def test_04b_closed_target_preserves_frames_without_switching_tabs(self):
  target=text(self.client.tool('browser_observe',{}))['page_id']
  value=text(self.client.tool('screen_observe',{'view':'page','page_id':target,'count':8,'interval_ms':3000,'save_original':True}))
  self.assertEqual(value['state'],'running',value)
  task=value['task_id']
  self.finish(self.client.tool('browser_run',{'code':"await context.newPage();await page.close();return true;",'page_id':target}))
  value,reply=self.finish(self.client.tool('browser_task',{'task_id':task}))
  self.assertEqual(value['state'],'failed',value);self.assertEqual(value['error']['code'],'PAGE_NOT_FOUND')
  self.assertEqual(value['page_id'],target);self.assertGreaterEqual(value['captured_count'],1)
  self.assertEqual(len(value['files']),value['captured_count'])
  self.assertEqual(value['captured_count'],len([c for c in reply['content'] if c['type']=='image']))

 def test_05_desktop_survives_stalled_browser_without_restarting_it(self):
  stop="import os,signal;from pathlib import Path;p=[int(x.name) for x in Path('/proc').iterdir() if x.name.isdigit() and b'--remote-debugging-port=9222' in (x/'cmdline').read_bytes()];assert len(p)==1;p=p[0];Path('/workspace/qualification/stopped-browser.pid').write_text(str(p));os.kill(p,signal.SIGSTOP)"
  # Match the actual Chromium command line, never the bash/Python source text.
  stop=stop.replace("b'--remote-debugging-port=9222' in (x/'cmdline').read_bytes()", "b'--remote-debugging-port=9222' in (x/'cmdline').read_bytes().split(bytes([0]))")
  self.client.tool('write',{'path':'/workspace/qualification/stop-browser.py','content':stop})
  try:
   reply=self.client.tool('bash',{'command':'python3 /workspace/qualification/stop-browser.py'});self.assertFalse(reply.get('isError'),text(reply))
   page,reply=self.finish(self.client.tool('screen_observe',{'view':'page'}));self.assertTrue(reply.get('isError'),page)
   desktop,reply=self.finish(self.client.tool('screen_observe',{'view':'desktop'}));self.assertFalse(reply.get('isError'),desktop)
   self.assertEqual(desktop['captured_count'],1)
   state=text(self.client.tool('bash',{'command':"python3 -c \"from pathlib import Path;p=Path('/workspace/qualification/stopped-browser.pid').read_text();print(Path('/proc/'+p+'/stat').read_text().split(') ')[1][0])\""}))
   self.assertIn('T',state['text'])
  finally:
   self.client.tool('bash',{'command':"python3 -c \"import os,signal;from pathlib import Path;p=Path('/workspace/qualification/stopped-browser.pid');os.kill(int(p.read_text()),signal.SIGCONT) if p.exists() else None\""})
  self.assertTrue(text(self.client.tool('browser_observe',{}))['health']['browser'])

 def test_06_parallel_images_and_mcp_cancellation_behind_a_file_lock(self):
  root='/workspace/qualification/media/'
  def read_image(_):
   client=Client('desktop');client.initialize()
   reply=client.tool('read',{'path':root+'large-output.png','multimodal':True})
   self.assertFalse(reply.get('isError'),text(reply));self.assertEqual(len(reply['content']),2)
  with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:list(pool.map(read_image,range(6)))
  script="""import fcntl,hashlib,os,time
from pathlib import Path
path=os.path.realpath('/workspace/qualification/media/wrong-extension.txt')
fd=os.open('/tmp/agent-space/file-operations.lock',os.O_RDWR)
offset=int.from_bytes(hashlib.sha256(os.fsencode(path)).digest()[:8],'big') & ((1<<63)-2)
fcntl.lockf(fd,fcntl.LOCK_EX,1,offset)
Path('/workspace/qualification/media/lock-ready').write_text('ready')
time.sleep(5)
"""
  self.client.tool('write',{'path':'/workspace/qualification/hold-lock.py','content':script})
  holder=Client('desktop');holder.initialize();reader=Client('desktop');reader.initialize()
  with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
   holding=pool.submit(holder.tool,'bash',{'command':'python3 /workspace/qualification/hold-lock.py'})
   until=time.monotonic()+5
   while 'ready' not in json.dumps(self.client.tool('read',{'path':root+'lock-ready'})):
    self.assertLess(time.monotonic(),until);time.sleep(.05)
   request_id=reader.counter+1
   reading=pool.submit(reader.tool,'read',{'path':root+'wrong-extension.txt','multimodal':True})
   time.sleep(.3);started=time.monotonic()
   headers={'Authorization':'Bearer '+reader.token,'Content-Type':'application/json','Accept':'application/json, text/event-stream','MCP-Protocol-Version':'2025-11-25','Mcp-Session-Id':reader.session}
   payload={'jsonrpc':'2.0','method':'notifications/cancelled','params':{'requestId':request_id,'reason':'qualification'}}
   request=urllib.request.Request(BASE+space_path('desktop','mcp'),data=json.dumps(payload).encode(),headers=headers)
   with urllib.request.urlopen(request,timeout=10) as response:response.read()
   try:self.assertTrue(reading.result(timeout=3).get('isError'))
   except RuntimeError as error:self.assertIn('cancel',str(error).lower())
   # RMCP may close a cancelled request's SSE stream without a result event.
   except StopIteration:pass
   self.assertLess(time.monotonic()-started,3)
   self.assertFalse(holding.result(timeout=10).get('isError'))
  self.assertFalse(self.client.tool('read',{'path':root+'wrong-extension.txt','multimodal':True}).get('isError'))

if __name__=='__main__':unittest.main(verbosity=2)
