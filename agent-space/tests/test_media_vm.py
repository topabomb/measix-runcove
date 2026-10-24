#!/usr/bin/env python3
"""Real MCP image and observation acceptance on disposable qualification hosts."""
import base64
import hashlib
import json
import shlex
import time
import unittest
import concurrent.futures
import urllib.request
from pathlib import Path
from browser_client import Client, TARGET, BASE, Q, space_path, text, element, image_dimensions


class MediaQualification(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.client=Client('desktop');cls.client.initialize()

 @classmethod
 def finish(cls,reply):
  return cls.client.finish(reply)

 def test_01_file_images_on_desktop_and_custom_workspaces(self):
  fixture=(Path(__file__).parent/'fixtures/read-images.py').read_text()
  self.assertFalse(self.client.tool('write',{'path':'/workspace/qualification/read-images.py','content':fixture}).get('isError'))
  formats={'value':self.client.fixture_js("return await page.evaluate(()=>{const c=document.createElement('canvas');c.width=40;c.height=20;const x=c.getContext('2d');x.fillStyle='red';x.fillRect(0,0,40,20);return {jpeg:c.toDataURL('image/jpeg').split(',')[1],webp:c.toDataURL('image/webp').split(',')[1]};});")}
  for name in ['desktop'] if TARGET=='standalone' else ['desktop','custom']:
   client=Client(name);client.initialize()
   client.tool('write',{'path':'/workspace/qualification/read-images.py','content':fixture})
   client.tool('write',{'path':'/workspace/qualification/formats.json','content':json.dumps(formats['value'])})
   meta=text(client.tool('bash',{'cmd':'python3 /workspace/qualification/read-images.py'}))
   self.assertFalse(meta.get('isError'),meta)
   root='/workspace/qualification/media/'
   disabled=client.tool('read',{'path':root+'wrong-extension.txt'})
   self.assertEqual(text(disabled)['error']['code'],'MULTIMODAL_REQUIRED')
   for file in ['wrong-extension.txt','transparent.png','jpeg','webp','first.gif','animated.png','animated.webp','orientation.jpeg','near-limit.png']:
    reply=client.tool('read',{'path':root+file,'multimodal':True})
    self.assertFalse(reply.get('isError'),text(reply))
    images=[c for c in reply['content'] if c['type']=='image'];self.assertEqual(len(images),1)
    self.assertEqual(images[0]['mimeType'],'image/webp')
    base64.b64decode(images[0]['data'],validate=True)
    self.assertLessEqual(max(image_dimensions(reply)),2560)
    self.assertEqual(set(text(reply)),{'path','size','format'});self.assertEqual(text(reply)['format'],'WebP')
    self.assertNotIn('data',text(reply));self.assertNotIn('structuredContent',reply)
    if file=='wrong-extension.txt':
     expected=meta['sha256']
     # File integrity also applies to workspaces without desktop/Playwright.
     checksum="import hashlib,json;from pathlib import Path;print(json.dumps({'sha256':hashlib.sha256(Path('/workspace/qualification/media/wrong-extension.txt').read_bytes()).hexdigest()}))"
     actual=client.good('bash',{'cmd':'python3 -c '+shlex.quote(checksum)})['sha256']
     self.assertEqual(actual,expected)
    if file in ['animated.png','animated.webp']:
     data='data:'+images[0]['mimeType']+';base64,'+images[0]['data']
     code="return await page.evaluate(async data=>{const im=new Image();im.src=data;await im.decode();const c=document.createElement('canvas');c.width=im.width;c.height=im.height;const x=c.getContext('2d');x.drawImage(im,0,0);return Array.from(x.getImageData(0,0,1,1).data);},"+json.dumps(data)+");"
     pixel=self.client.fixture_js(code);self.assertGreater(pixel[0],240);self.assertLess(pixel[2],20)
    if file=='transparent.png':self.assertEqual(image_dimensions(reply)[0],2560)
    if file=='orientation.jpeg':self.assertEqual(image_dimensions(reply),(20,40))
   for args in [{'offset':1},{'limit':500},{'offset':0,'limit':0},{'offset':1,'limit':500}]:
    reply=client.tool('read',dict(path=root+'transparent.png',multimodal=True,**args));self.assertFalse(reply.get('isError'));self.assertTrue(any(c['type']=='image' for c in reply['content']))
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
  self.client.navigate('data:text/html,<title>Observe fixture</title><button>Continue</button>')
  dom=self.client.snapshot();self.assertIn('observation',dom);self.assertNotIn('recent_tasks',dom)
  recent=self.client.snapshot(view='tasks');self.assertTrue(recent['tasks']);self.assertNotIn('task_id',recent)
  for view in ['page','desktop']:
   value,reply=self.finish(self.client.tool('screen_observe',{'view':view,'count':3,'interval_secs':1}))
   self.assertFalse(reply.get('isError'),value);self.assertEqual(value['captured_count'],3)
   self.assertEqual(len([c for c in reply['content'] if c['type']=='image']),3)
   self.assertLessEqual(sum(len(base64.b64decode(c['data'])) for c in reply['content'] if c['type']=='image'),8*1024*1024)
   self.assertTrue(all(c['mimeType']=='image/webp' for c in reply['content'] if c['type']=='image'))
   if view=='page':self.assertEqual(len({c['data'] for c in reply['content'] if c['type']=='image'}),1)
   self.assertNotIn('dom',value);self.assertNotIn('ax',value);self.assertNotIn('captures',value)
   self.assertNotIn('files',value)
   frames=value['frames'];self.assertEqual([f['index'] for f in frames],[1,2,3])
   self.assertTrue(all(b['elapsed_secs']-a['elapsed_secs']>=0.89 for a,b in zip(frames,frames[1:])))
  after=self.client.snapshot();self.assertEqual(dom['page']['page_id'],after['page']['page_id'])
  value,reply=self.finish(self.client.tool('screen_observe',{'view':'desktop','count':8,'interval_secs':0,'page_id':'missing'}))
  self.assertFalse(reply.get('isError'),value);self.assertEqual(value['captured_count'],8)

 def test_02b_original_screenshots_are_downloadable_and_keep_capture_bytes(self):
  tools=self.client.call('tools/list',{})['tools']
  schema=next(t['inputSchema'] for t in tools if t['name']=='screen_observe')
  self.assertEqual(schema['required'],['view'])
  for name in ['page_id','count','interval_secs','save_original','save_dir']:
   self.assertTrue(schema['properties'][name]['description'])
  value,reply=self.finish(self.client.tool('screen_observe',{'view':'desktop','save_original':None,'save_dir':'relative','count':1,'interval_secs':999999}))
  self.assertFalse(reply.get('isError'),value);self.assertNotIn('files',value)
  with self.assertRaisesRegex(RuntimeError,'absolute'):
   self.client.tool('screen_observe',{'view':'desktop','save_original':True,'save_dir':'relative'})
  for view,count,directory in [('page',1,None),('desktop',8,'/workspace/qualification/originals')]:
   args={'view':view,'save_original':True,'count':count,'interval_secs':0}
   if directory:args['save_dir']=directory
   value,reply=self.finish(self.client.tool('screen_observe',args))
   self.assertFalse(reply.get('isError'),value);self.assertEqual(len(value['files']),count)
   paths=[]
   images=[c for c in reply['content'] if c['type']=='image']
   for frame,file,returned in zip(value['frames'],value['files'],images):
    path=file['path'];paths.append(path)
    self.assertEqual(str(Path(path).parent),directory or '/workspace/agent-space/artifacts')
    self.assertRegex(Path(path).name,r'^screen-[0-9A-Za-z]{10}\.png$')
    self.assertEqual(file['frame_index'],frame['index']);self.assertEqual(file['image_id'],frame['image_id'])
    self.assertEqual(set(file),{'path','image_id','frame_index','size','format'});self.assertEqual(file['format'],'PNG')
    self.assertEqual(set(frame),{'index','image_id','size','format'} | ({'elapsed_secs'} if count>1 else set()))
    saved=self.client.tool('read',{'path':path,'multimodal':True})
    self.assertFalse(saved.get('isError'),text(saved))
    image=next(c for c in saved['content'] if c['type']=='image')
    self.assertEqual(image['mimeType'],'image/webp')
    self.assertEqual(image['data'],returned['data'])
   self.assertEqual(len(paths),len(set(paths)))
   code="const fs=await import('node:fs'),crypto=await import('node:crypto');return "+json.dumps(paths)+".map(path=>{const b=fs.readFileSync(path);return {png:b.subarray(0,8).toString('hex'),width:b.readUInt32BE(16),height:b.readUInt32BE(20),bytes:b.length,sha256:crypto.createHash('sha256').update(b).digest('hex')};});"
   disk=self.client.fixture_js(code)
   for actual,returned in zip(disk,images):
    width,height=image_dimensions({'content':[returned]})
    self.assertEqual(actual['png'],'89504e470d0a1a0a');self.assertGreater(actual['bytes'],0)
    self.assertGreaterEqual(actual['width'],width);self.assertGreaterEqual(actual['height'],height)
   if view=='page':
    # Use the existing authenticated workbench stream to download the original.
    base=BASE+space_path('desktop','desktop/api/')
    body=json.dumps({'token':self.client.token}).encode()
    with urllib.request.urlopen(urllib.request.Request(base+'login',data=body,headers={'Content-Type':'application/json','Origin':BASE}),timeout=10) as response:
     cookie=response.headers['Set-Cookie'].split(';',1)[0];nonce=json.load(response)['nonce']
    headers={'Cookie':cookie,'X-Desktop-Nonce':nonce,'Origin':BASE}
    with urllib.request.urlopen(urllib.request.Request(base+'files'+paths[0].removeprefix('/workspace'),headers=headers),timeout=10) as response:
     downloaded=response.read()
    self.assertEqual(len(downloaded),disk[0]['bytes']);self.assertTrue(downloaded.startswith(bytes.fromhex('89504e470d0a1a0a')))
    self.assertEqual(hashlib.sha256(downloaded).hexdigest(),disk[0]['sha256'])
  value,reply=self.finish(self.client.tool('screen_observe',{'view':'desktop','save_original':True,'save_dir':'/proc/agent-space-originals'}))
  self.assertTrue(reply.get('isError'));self.assertEqual(value['error']['code'],'SCREEN_SAVE_FAILED')
  self.assertEqual(value['captured_count'],1);self.assertNotIn('files',value)

 def test_02c_private_directories_share_workspace_file_permissions(self):
  directory='/workspace/tmp/chatgpt-ops'
  created=self.client.tool('bash',{'cmd':'umask 077; mkdir -p '+directory+'; chmod 700 '+directory})
  self.assertFalse(created.get('isError'),text(created))
  path=directory+'/shared.txt'
  self.assertFalse(self.client.tool('write',{'path':path,'content':'before'}).get('isError'))
  self.assertFalse(self.client.tool('edit',{'path':path,'old_text':'before','new_text':'after'}).get('isError'))
  # Browser uploads and screenshot storage traverse the same private parent.
  self.client.navigate('http://127.0.0.1:8124/')
  view=self.client.snapshot();ref=element(view,'button','Upload fixture')
  value,reply=self.client.act([{'do':'upload','ref':ref,'paths':[path]},{'do':'wait','text':'sha256'}]);self.assertFalse(reply.get('isError'),value)
  self.assertIn('after',json.dumps(text(self.client.tool('read',{'path':path}))))
  for view in ['page','desktop']:
   value,reply=self.finish(self.client.tool('screen_observe',{'view':view,'save_original':True,'save_dir':directory+'/'+view}))
   self.assertFalse(reply.get('isError'),value)
   file=value['files'][0]
   saved=self.client.tool('read',{'path':file['path'],'multimodal':True})
   self.assertFalse(saved.get('isError'),text(saved))
   image=next(c for c in saved['content'] if c['type']=='image')
   returned=next(c for c in reply['content'] if c['type']=='image')
   self.assertEqual(image['mimeType'],'image/webp');self.assertEqual(image['data'],returned['data'])
  base=BASE+space_path('desktop','desktop/api/')
  with urllib.request.urlopen(urllib.request.Request(base+'login',data=json.dumps({'token':self.client.token}).encode(),headers={'Content-Type':'application/json','Origin':BASE}),timeout=10) as response:
   cookie=response.headers['Set-Cookie'].split(';',1)[0];nonce=json.load(response)['nonce']
  headers={'Cookie':cookie,'X-Desktop-Nonce':nonce,'Origin':BASE}
  disk=self.client.fixture_js("const fs=await import('node:fs'),crypto=await import('node:crypto');const b=fs.readFileSync("+json.dumps(file['path'])+");return {bytes:b.length,sha256:crypto.createHash('sha256').update(b).digest('hex')};")
  with urllib.request.urlopen(urllib.request.Request(base+'files'+file['path'].removeprefix('/workspace'),headers=headers),timeout=10) as response:
   original=response.read();self.assertEqual(len(original),disk['bytes']);self.assertTrue(original.startswith(bytes.fromhex('89504e470d0a1a0a')))
   self.assertEqual(hashlib.sha256(original).hexdigest(),disk['sha256'])
  # Traverse the same private parent when listing artifacts. Standalone also
  # verifies separately that a root-private target remains inaccessible.
  artifacts='/workspace/agent-space/artifacts'
  moved=self.client.tool('bash',{'cmd':'mv '+artifacts+' '+artifacts+'.permissions-check; ln -s '+directory+' '+artifacts})
  self.assertFalse(moved.get('isError'),text(moved))
  try:
   with urllib.request.urlopen(urllib.request.Request(base+'artifacts',headers=headers),timeout=10) as response:
    listed=json.load(response)
   self.assertIn('shared.txt',[f['name'] for f in listed['files']])
  finally:
   restored=self.client.tool('bash',{'cmd':'rm '+artifacts+'; mv '+artifacts+'.permissions-check '+artifacts})
   self.assertFalse(restored.get('isError'),text(restored))

 def test_03_cancel_sequence_preserves_frames_and_allows_browser_work(self):
  value=text(self.client.tool('screen_observe',{'view':'page','count':8,'interval_secs':6,'save_original':True,'save_dir':'/workspace/qualification/cancelled-originals'}))
  self.assertEqual(value['state'],'running',value)
  task=value['task_id']
  run,reply=self.client.act([{'do':'wait','secs':1}])
  self.assertFalse(reply.get('isError'),run)
  value,reply=self.finish(self.client.tool('browser_act',{'task_id':task,'op':'cancel'}))
  self.assertEqual(value['state'],'cancelled');self.assertGreaterEqual(value['captured_count'],1)
  self.assertLess(value['captured_count'],8)
  self.assertEqual(value['captured_count'],len([c for c in reply['content'] if c['type']=='image']))
  self.assertEqual(value['captured_count'],len(value['files']))
  repeated=self.client.tool('browser_act',{'op':'wait','task_id':task,'wait_secs':0});self.assertEqual(reply,repeated)

 def test_04_download_outputs_survive_failure_deadline_and_cancellation(self):
  for ending in ['failure','timeout','cancel']:
   self.client.navigate('http://127.0.0.1:8125/download');ref=element(self.client.snapshot(),'link','Download CSV')
   steps=[{'do':'download','ref':ref}]
   steps += ([{'do':'wait','text':'Never appears'}] if ending=='failure' else [{'do':'wait','secs':60}]*(3 if ending=='timeout' else 1))
   reply=self.client.tool('browser_act',{'actions':steps})
   if ending=='cancel':
    value=text(reply);self.assertEqual(value['state'],'running',value);reply=self.client.tool('browser_act',{'op':'cancel','task_id':value['task_id']})
   value,reply=self.finish(reply);self.assertTrue(reply.get('isError'),value);self.assertEqual(value['state'],{'failure':'failed','timeout':'timed_out','cancel':'cancelled'}[ending]);self.assertEqual(len(value['files']),1,value)
   self.assertFalse(any(c['type']=='image' for c in reply['content']))
   self.assertEqual(self.client.tool('browser_act',{'op':'wait','task_id':value['task_id'],'wait_secs':0}),reply)

 def test_04b_closed_target_preserves_frames_without_switching_tabs(self):
  target=self.client.snapshot()['page']['page_id']
  value=text(self.client.tool('screen_observe',{'view':'page','page_id':target,'count':8,'interval_secs':6,'save_original':True}))
  self.assertEqual(value['state'],'running',value)
  task=value['task_id']
  self.client.act([{'do':'tab','action':'new'},{'do':'tab','action':'close','page_id':target}],page_id=target)
  value,reply=self.finish(self.client.tool('browser_act',{'op':'wait','task_id':task}))
  self.assertEqual(value['state'],'failed',value);self.assertEqual(value['error']['code'],'PAGE_NOT_FOUND')
  self.assertEqual(value['page']['page_id'],target);self.assertGreaterEqual(value['captured_count'],1)
  self.assertEqual(len(value['files']),value['captured_count'])
  self.assertEqual(value['captured_count'],len([c for c in reply['content'] if c['type']=='image']))

 def test_05_desktop_survives_stalled_browser_without_restarting_it(self):
  stop="import os,signal;from pathlib import Path;p=[int(x.name) for x in Path('/proc').iterdir() if x.name.isdigit() and b'--remote-debugging-port=9222' in (x/'cmdline').read_bytes()];assert len(p)==1;p=p[0];Path('/workspace/qualification/stopped-browser.pid').write_text(str(p));os.kill(p,signal.SIGSTOP)"
  # Match the actual Chromium command line, never the bash/Python source text.
  stop=stop.replace("b'--remote-debugging-port=9222' in (x/'cmdline').read_bytes()", "b'--remote-debugging-port=9222' in (x/'cmdline').read_bytes().split(bytes([0]))")
  self.client.tool('write',{'path':'/workspace/qualification/stop-browser.py','content':stop})
  try:
   reply=self.client.tool('bash',{'cmd':'python3 /workspace/qualification/stop-browser.py'});self.assertFalse(reply.get('isError'),text(reply))
   page,reply=self.finish(self.client.tool('screen_observe',{'view':'page'}));self.assertTrue(reply.get('isError'),page)
   desktop,reply=self.finish(self.client.tool('screen_observe',{'view':'desktop'}));self.assertFalse(reply.get('isError'),desktop)
   self.assertEqual(desktop['captured_count'],1)
   state=text(self.client.tool('bash',{'cmd':"python3 -c \"from pathlib import Path;p=Path('/workspace/qualification/stopped-browser.pid').read_text();print(Path('/proc/'+p+'/stat').read_text().split(') ')[1][0])\""}))
   self.assertIn('T',state['text'])
  finally:
   self.client.tool('bash',{'cmd':"python3 -c \"import os,signal;from pathlib import Path;p=Path('/workspace/qualification/stopped-browser.pid');os.kill(int(p.read_text()),signal.SIGCONT) if p.exists() else None\""})
  self.assertTrue(self.client.snapshot().get('page',{}).get('page_id'))

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
   holding=pool.submit(holder.tool,'bash',{'cmd':'python3 /workspace/qualification/hold-lock.py'})
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

 def test_07_dense_chinese_text_uses_the_shared_image_pipeline(self):
  rows=''.join(f'<tr><td>{i:02d}</td><td>浏览器截图质量验证：原始 PNG、中文正文、代码与数字</td><td>用户空间 /workspace/reports</td><td>成功 123,456.78</td></tr>' for i in range(1,31))
  html='<meta charset="utf-8"><style>body{margin:0;width:3000px;min-height:1450px;background:white;color:#182334;font:20px sans-serif}main{padding:40px}table{border-collapse:collapse;width:100%}td{padding:8px;border-bottom:1px solid #cbd5e1}th{text-align:left}code{color:#c02655}</style><main><h1>Runcove 图片清晰度验证</h1><p>中文正文、English text, 0O1Il, <code>screen_observe(image_id)</code></p><table>'+rows+'</table></main>'
  self.client.navigate('data:text/html,<title>Image quality fixture</title>')
  self.client.fixture_js('await page.setContent('+json.dumps(html)+');return true;')
  for tool,args in [('browser_snapshot',{'image':True}),('screen_observe',{'view':'page','save_original':True})]:
   value,reply=self.finish(self.client.tool(tool,args));self.assertFalse(reply.get('isError'),value)
   data=value.get('value',value);frame=data['image'] if tool=='browser_snapshot' else data['frames'][0]
   self.assertEqual(image_dimensions(reply)[0],2560);self.assertEqual(frame['format'],'WebP')
   image=next(c for c in reply['content'] if c['type']=='image');self.assertEqual(image['mimeType'],'image/webp')
   (Q/('text-'+tool+'.webp')).write_bytes(base64.b64decode(image['data']))
   (Q/('text-'+tool+'.json')).write_text(json.dumps({'geometry':frame,'bytes':len(base64.b64decode(image['data'])),'files':data.get('files',[])},indent=2))
  self.client.navigate('data:text/html,<title>Image quality complete</title>')

if __name__=='__main__':unittest.main(verbosity=2)
