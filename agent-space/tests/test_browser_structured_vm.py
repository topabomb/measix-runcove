#!/usr/bin/env python3
"""Frame, popup, floating panel and reference behavior through the real MCP owner."""
import base64,json,shlex,time,unittest
from browser_client import Client,element,reset_tabs,text,Q,image_dimensions

class StructuredBrowser(unittest.TestCase):
 @classmethod
 def setUpClass(cls):cls.client=Client('desktop');cls.client.initialize()
 def setUp(self):reset_tabs(self.client,'http://127.0.0.1:8125/login')
 def act(self,steps,**args):
  value,reply=self.client.act(steps,**args);self.assertFalse(reply.get('isError'),value);return value
 def test_actual_act_feedback_example(self):
  view=self.client.navigate('http://127.0.0.1:8125/panels')
  request={'actions':[{'do':'click','ref':element(view,'button','Actions')},{'do':'wait','text':'Choose item'}]}
  value,reply=self.client.finish(self.client.tool('browser_act',request))
  self.assertFalse(reply.get('isError'),value)
  self.assertEqual(value['completed_steps'],2);self.assertEqual(value['total_steps'],2)
  self.assertIn('menu "Actions menu"',value['observation_diff'])
  self.assertNotIn('layers',value);self.assertNotIn('value',value)
  (Q/'observation-feedback.json').write_text(json.dumps({'request':request,'response':value},ensure_ascii=False,indent=2)+'\n')
 def test_observation_baselines_are_per_connection_and_old_queries_do_not_rewind(self):
  a=Client('desktop');a.initialize();b=Client('desktop');b.initialize()
  self.client.fixture_js("await page.evaluate(()=>document.body.innerHTML='<section aria-label=Upload><button id=b aria-expanded=false>Add files</button><p role=status>Ready</p></section>');return true;")
  first=a.snapshot();b.snapshot();target=first['page']['page_id']
  self.assertNotIn('value',first);self.assertNotIn('_observation',json.dumps(first))
  self.client.fixture_js("await page.evaluate(()=>document.querySelector('#b').setAttribute('aria-expanded','true'));return true;",target)
  def observe_diff(client):
   return client.act([{'do':'wait','gone':'absent-observation-marker'}])[0]
  av=observe_diff(a);bv=observe_diff(b)
  for observed in [av,bv]:
   self.assertIn('observation_diff',observed);self.assertIn('[expanded]',observed['observation_diff'])
   self.assertIn('- ',observed['observation_diff']);self.assertNotIn('layers',observed)
  a.tool('browser_act',{'op':'wait','task_id':first['task_id'],'wait_secs':0})
  self.assertEqual(observe_diff(a)['observation_diff'],'')
  c=Client('desktop');c.initialize()
  c.tool('browser_act',{'op':'wait','task_id':av['task_id'],'wait_secs':0})
  self.assertIn('observation',observe_diff(c))
  self.client.fixture_js("await page.evaluate(()=>document.querySelector('[role=status]').textContent='Selected');return true;",target)
  a.read_browser()
  delta=observe_diff(a)['observation_diff'];self.assertIn('Ready',delta);self.assertIn('Selected',delta)
 def test_task_list_is_space_scoped_bounded_and_queryable(self):
  for _ in range(11):self.act([{'do':'wait','secs':0}])
  first=self.client.snapshot(view='tasks',page_id=False,ref={},image='ignored',recent_tasks_limit=100)
  self.assertEqual(len(first['tasks']),10);self.assertTrue(first['truncated']);self.assertNotIn('task_id',first)
  self.assertEqual(first,self.client.snapshot(view='tasks'))
  for task in first['tasks']:
   self.assertEqual(set(task),{'task_id','tool','description','state','started_at'})
   self.assertLessEqual(len(task['description']),120)
   self.assertRegex(task['started_at'],r'^\d{2}-\d{2} \d{2}:\d{2}:\d{2}Z$')
  response=self.client.tool('browser_act',{'actions':[{'do':'wait','secs':60}]})
  pending=text(response);self.assertEqual(pending['state'],'running')
  other=Client('desktop');other.initialize()
  listed=other.snapshot(view='tasks');self.assertEqual(listed['tasks'][0]['task_id'],pending['task_id'])
  self.assertEqual(listed['tasks'][0]['description'],'Wait 60s')
  cancelled,reply=other.finish(other.tool('browser_act',{'op':'cancel','task_id':pending['task_id']}))
  self.assertEqual(cancelled['state'],'cancelled');self.assertTrue(reply.get('isError'))
  self.assertNotIn('recent_tasks',self.client.snapshot(recent_tasks_limit=10))
 def test_parameter_normalization_and_semantic_wait(self):
  initial=self.client.snapshot();email=element(initial,'textbox','Email')
  value,reply=self.client.act([{'do':'fill','ref':email,'text':'kept@example.com'}],wait_secs=999)
  self.assertFalse(reply.get('isError'),value);self.assertNotIn('tabs',value);self.assertNotIn('health',value)
  self.assertIn('control',value)
  for action in ['wait','cancel']:
   response=self.client.tool('browser_act',{'op':action,'task_id':value['task_id'],'actions':[],
    'page_id':'irrelevant','then':'snapshot','wait_secs':0})
   self.assertFalse(response.get('isError'),text(response))
  self.assertEqual(self.client.read_browser(extract='fields',max_chars=0)['fields'][0]['value'],'kept@example.com')
  view=self.client.navigate('http://127.0.0.1:8125/scope')
  matches=self.client.read_browser(ref=element(view,'region','First form'),query='NAME',max_chars=999999)['matches']
  self.assertEqual([m['name'] for m in matches],['First name'])
  self.act([{'do':'fill','ref':matches[0]['ref'],'text':''},{'do':'wait','text':'FIRST NAME'}])
  with self.assertRaises(RuntimeError):self.client.tool('browser_read',{'query':'name','extract':'fields'})
  with self.assertRaises(RuntimeError):self.client.tool('browser_act',{'op':'wait','task_id':value['task_id'],'actions':[{'do':'navigate','mode':'reload'}]})
  with self.assertRaises(RuntimeError):self.client.tool('browser_act',{'task_id':value['task_id'],'actions':[{'do':'navigate','mode':'reload'}]})
  self.client.fixture_js("await page.evaluate(()=>document.body.innerHTML='<input aria-label=Prompt><img alt=Generated><p hidden>Hidden</p>');return true;")
  self.act([{'do':'wait','text':'prompt'},{'do':'wait','text':'GENERATED'},{'do':'wait','gone':'Hidden'}])
  failed,reply=self.client.act([{'do':'wait','gone':'prompt'}])
  self.assertTrue(reply.get('isError'));self.assertEqual(failed['error']['code'],'TIMEOUT');self.assertIn('page_id',failed['page']);self.assertNotIn('stderr',failed.get('logs',{}))
 def test_compact_snapshot_list_and_stable_task_timing(self):
  value,reply=self.client.finish(self.client.tool('browser_snapshot',{}))
  self.assertFalse(reply.get('isError'),value)
  self.assertNotIn('started_at',value)
  self.assertNotIn('duration_secs',value)
  for key in ['tabs','health','layers','truncated']:self.assertNotIn(key,value)
  again=text(self.client.tool('browser_act',{'op':'wait','task_id':value['task_id'],'wait_secs':0}))
  self.assertNotIn('started_at',again);self.assertNotIn('duration_secs',again)
  listed=self.client.good('browser_snapshot',{'view':'tabs','page_id':'irrelevant','ref':'invalid','image':True})
  self.assertTrue(listed['tabs']);self.assertNotIn('observation',listed);self.assertNotIn('image',listed)
  self.assertTrue(self.client.good('browser_snapshot',{'view':' '})['page']['page_id'])
  for tool,args,has_start in [('browser_read',{},False),('screen_observe',{'view':'page'},False),('screen_observe',{'view':'page','count':2,'interval_secs':0},True),('browser_act',{'actions':[{'do':'wait','secs':0}]},True)]:
   timing,reply=self.client.finish(self.client.tool(tool,args));self.assertFalse(reply.get('isError'),timing)
   self.assertEqual('started_at' in timing,has_start,timing);self.assertEqual('duration_secs' in timing,tool=='browser_act',timing)
   if has_start:self.assertRegex(timing['started_at'],r'^\d{2}-\d{2} \d{2}:\d{2}:\d{2}Z$')

  page=value['page']['page_id']
  self.act([{'do':'tab','action':'new','url':'data:text/html,<title>Other '+str(i)+'</title>'} for i in range(3)],then='diff')
  current=self.client.good('browser_snapshot',{'page_id':page})
  pages=self.client.good('browser_snapshot',{'view':'tabs'})
  self.assertEqual(len(pages['tabs']),4);self.assertNotIn('tabs',current)
  query=self.client.read_browser(page_id=page,query='Email')
  self.assertNotIn('iframe',query['matches'][0]);self.assertNotIn('frame_path',query['matches'][0]);self.assertNotIn('truncated',query)
  (Q/'compact-feedback.json').write_text(json.dumps({'task':value,'current_page':current,'pages':pages,'query':query},ensure_ascii=False,indent=2)+'\n')

 def test_sixty_second_wait_and_default_condition_deadline(self):
  # Exercise the 30-second initial response and repeated 30-second task waits.
  began=time.monotonic()
  response=self.client.tool('browser_act',{'actions':[{'do':'wait','secs':60}], 'then':'diff'})
  initial=text(response);self.assertEqual(initial['state'],'running')
  value,reply=self.client.finish(response)
  fixed=time.monotonic()-began
  self.assertFalse(reply.get('isError'),value);self.assertGreaterEqual(fixed,60)
  self.assertEqual(initial['started_at'],value['started_at'])
  began=time.monotonic()
  value,reply=self.client.act([{'do':'wait','text':'UNMATCHED_SECONDS_DEADLINE'}],then='diff')
  condition=time.monotonic()-began
  self.assertTrue(reply.get('isError'),value);self.assertEqual(value['error']['code'],'TIMEOUT')
  self.assertGreaterEqual(condition,60);self.assertLess(condition,100)
  self.client.fixture_js("await page.evaluate(()=>{const p=document.createElement('p');p.textContent='Delayed busy marker';document.body.append(p);setTimeout(()=>p.remove(),11000)});return true;")
  self.act([{'do':'wait','gone':'Delayed busy marker'}],then='diff')
  (Q/'seconds-wait-timing.json').write_text(json.dumps({'fixed_secs':60,'fixed_elapsed_secs':fixed,'default_condition_elapsed_secs':condition,'initial_state':initial['state']},indent=2)+'\n')
 def test_frames_nested_cross_site_and_dynamic(self):
  for route in ['/iframe','/nested','/xorigin']:
   view=self.client.navigate('http://127.0.0.1:8125'+route)
   query=element(view,'textbox','Search articles');go=element(view,'button','Go')
   self.act([{'do':'fill','ref':query,'text':'widgets'},{'do':'click','ref':go},{'do':'wait','text':'3 articles found'}])
   self.assertIn('3 articles found',self.client.read_browser()['text'])
   fields=self.client.read_browser(extract='fields');self.assertTrue(any(f['ref']==query for f in fields['fields']),fields)
   if route=='/xorigin':self.assertTrue(self.client.fixture_js("return (await(await fetch('http://127.0.0.1:9222/json/list')).json()).some(t=>t.type==='iframe');"))
  view=self.client.navigate('http://127.0.0.1:8125/dynframe')
  self.act([{'do':'click','ref':element(view,'button','Open chat')},{'do':'wait','text':'Search articles'}])
  view=self.client.snapshot();query=element(view,'textbox','Search articles')
  self.act([{'do':'click','ref':element(view,'button','Close chat')}])
  view=self.client.snapshot();self.act([{'do':'click','ref':element(view,'button','Open chat')},{'do':'wait','text':'Search articles'}])
  value,reply=self.client.act([{'do':'fill','ref':query,'text':'stale'}]);self.assertTrue(reply.get('isError'));self.assertEqual(value['error']['code'],'STALE_REF')
 def test_fields_and_replacement_identity(self):
  fields=self.client.read_browser(extract='fields');email=next(f for f in fields['fields'] if f['name']=='Email')
  self.act([{'do':'fill','ref':email['ref'],'text':'direct@example.com'}])
  self.client.fixture_js("await page.locator('input[name=email]').evaluate(e=>e.outerHTML=e.outerHTML);return true;")
  value,reply=self.client.act([{'do':'fill','ref':email['ref'],'text':'wrong'}]);self.assertTrue(reply.get('isError'));self.assertEqual(value['error']['code'],'STALE_REF');self.assertEqual(value['side_effects'],'not_started')
  view=self.client.navigate('http://127.0.0.1:8125/scope');scope=element(view,'region','First form');fields=self.client.read_browser(ref=scope,extract='fields')['fields']
  self.assertEqual(len(fields),1);self.assertEqual(fields[0]['name'],'First name');self.act([{'do':'fill','ref':fields[0]['ref'],'text':'scoped'}])
 def test_popups_are_identified_readable_and_closable(self):
  view=self.client.navigate('http://127.0.0.1:8125/popup');result=self.act([{'do':'click','ref':element(view,'link','Open invoices in new tab')}]);self.assertEqual(len(result['popups']),1,result)
  page=result['popups'][0]['page_id'];tables=self.client.read_browser(page_id=page,extract='table');self.assertEqual(len(tables['tables'][0]['rows']),30)
  self.act([{'do':'tab','action':'focus','page_id':page}]);self.assertEqual(self.client.snapshot()['page']['page_id'],page)
  self.act([{'do':'tab','action':'close','page_id':page}]);self.assertEqual(len(self.client.snapshot(view='tabs')['tabs']),1)
 def test_js_menu_popover_tooltip_and_nested_modal(self):
  view=self.client.navigate('http://127.0.0.1:8125/panels');self.act([{'do':'click','ref':element(view,'button','Actions')}]);view=self.client.snapshot()
  self.assertIn('menu',view['observation']);self.act([{'do':'click','ref':element(view,'menuitem','Choose item')}])
  view=self.client.snapshot();self.act([{'do':'hover','ref':element(view,'button','Hover for help')}]);view=self.client.snapshot();self.assertIn('tooltip',view['observation'])
  self.client.fixture_js('await page.mouse.move(0,0);return true;')
  view=self.client.snapshot();self.act([{'do':'click','ref':element(view,'button','Open popover')}]);view=self.client.snapshot();self.assertIn('Popover content',view['observation'])
  self.act([{'do':'click','ref':element(view,'button','Confirm popover')}]);view=self.client.snapshot();self.act([{'do':'click','ref':element(view,'button','Open nested modal')}]);view=self.client.snapshot()
  self.act([{'do':'click','ref':element(view,'button','Open inner modal')}]);view=self.client.snapshot();self.assertEqual(view['observation'].count('dialog '),2)
  self.act([{'do':'click','ref':element(view,'button','Finish nested')}]);self.assertIn('nested finished',self.client.read_browser()['text'])
 def test_dialog_policy_and_bounded_reads(self):
  view=self.client.navigate('http://127.0.0.1:8125/dialogs');buttons=self.client.fixture_js('return await page.locator("button").allTextContents();')
  # Observe labels rather than relying on fixture IDs.
  confirm=next(name for name in buttons if 'delete' in name.lower())
  result=self.act([{'do':'dialog','accept':True},{'do':'click','ref':element(view,'button',confirm)}]);self.assertEqual(result['dialogs'][0]['action'],'accepted')
  read=self.client.read_browser(max_chars=5);self.assertLessEqual(len(read['text']),5);self.assertTrue(read['truncated'])
  self.client.navigate('http://127.0.0.1:8125/table');table=self.client.read_browser(extract='table');self.assertNotIn('truncated',table);self.assertEqual(len(table['tables'][0]['rows']),30)
 def test_screenshot_coordinates_and_navigation_invalidation(self):
  self.client.navigate('http://127.0.0.1:8125/canvas');value,reply=self.client.finish(self.client.tool('browser_snapshot',{'image':True}));view=value;image=view['image']
  width,height=image_dimensions(reply);self.assertEqual(set(image),{'image_id','size','format'});self.assertLessEqual(max(width,height),2560)
  box=self.client.fixture_js("return await page.locator('canvas').boundingBox();")
  document=self.client.fixture_js('return await page.evaluate(()=>({width:Math.max(innerWidth,document.documentElement.scrollWidth),height:Math.max(innerHeight,document.documentElement.scrollHeight)}));')
  self.act([{'do':'click','image_id':image['image_id'],'x':(box['x']+340)*width/document['width'],'y':(box['y']+140)*height/document['height']}]);self.assertIn('seat booked',self.client.read_browser()['text'])
  # screen_observe page captures use the same image contract after resizing.
  shot=self.client.good('screen_observe',{'view':'page'});frame=shot['frames'][0]
  self.act([{'do':'click','image_id':frame['image_id'],'x':10,'y':10}])
  self.client.navigate('http://127.0.0.1:8125/login');value,reply=self.client.act([{'do':'click','image_id':image['image_id'],'x':10,'y':10}]);self.assertTrue(reply.get('isError'));self.assertEqual(value['error']['code'],'STALE_OBSERVATION')
 def test_long_page_images_preserve_bottom_pixels_and_scaled_coordinates(self):
  self.client.navigate('http://127.0.0.1:8125/long')
  for tool,args in [('browser_snapshot',{'image':True}),('screen_observe',{'view':'page','save_original':True})]:
   value,reply=self.client.finish(self.client.tool(tool,args));self.assertFalse(reply.get('isError'),value)
   data=value.get('value',value);image=data['image'] if tool=='browser_snapshot' else data['frames'][0]
   width,height=image_dimensions(reply);self.assertEqual(height,2560)
   payload=next(c for c in reply['content'] if c['type']=='image');self.assertEqual(payload['mimeType'],'image/webp')
   self.assertEqual(base64.b64decode(payload['data'])[8:12],b'WEBP')
   (Q/(tool+'-long.webp')).write_bytes(base64.b64decode(payload['data']))
   code="return await page.evaluate(async data=>{const i=new Image();i.src=data;await i.decode();const c=document.createElement('canvas');c.width=i.width;c.height=i.height;const g=c.getContext('2d');g.drawImage(i,0,0);return {width:i.width,height:i.height,pixel:[...g.getImageData(Math.floor(i.width/2),i.height-12,1,1).data]};},"+json.dumps('data:image/webp;base64,'+payload['data'])+");"
   decoded=self.client.fixture_js(code);self.assertEqual((decoded['width'],decoded['height']),(width,height));self.assertGreater(decoded['pixel'][0],220);self.assertLess(decoded['pixel'][1],40)
   point=self.client.fixture_js("return await page.locator('#bottom').evaluate(e=>{const r=e.getBoundingClientRect();return {x:r.x+scrollX+r.width/2,y:r.y+scrollY+r.height/2,width:Math.max(innerWidth,document.documentElement.scrollWidth),height:Math.max(innerHeight,document.documentElement.scrollHeight)};});")
   self.assertGreater(point['height'],6000)
   self.act([{'do':'click','image_id':image['image_id'],'x':point['x']*width/point['width'],'y':point['y']*height/point['height']}]);self.assertIn('bottom clicked',self.client.read_browser()['text'])
   if tool=='screen_observe':
    file=data['files'][0];self.assertEqual(file['format'],'PNG')
    saved=self.client.tool('read',{'path':file['path'],'multimodal':True});self.assertEqual(next(c['data'] for c in saved['content'] if c['type']=='image'),payload['data'])
 def test_pending_native_js_dialog_uses_desktop_without_passive_dismissal(self):
  view=self.client.navigate('http://127.0.0.1:8125/dialogs');target=view['page']['page_id']
  expression="document.querySelector('#out').textContent=confirm('Pending user decision')?'accepted pending':'dismissed pending'"
  script="(async()=>{const t=(await(await fetch('http://127.0.0.1:9222/json/list')).json()).find(t=>t.id==="+json.dumps(target)+");const w=new WebSocket(t.webSocketDebuggerUrl);w.onopen=()=>{w.send(JSON.stringify({id:1,method:'Runtime.evaluate',params:{expression:"+json.dumps(expression)+"}}));setTimeout(()=>process.exit(),300);};})();"
  self.client.good('bash',{'cmd':'node -e '+shlex.quote(script)})
  try:
   observed=self.client.snapshot();self.assertEqual(observed['browser_ui'],'page_blocked');self.assertFalse(observed['health']['page'])
   shot=self.client.tool('screen_observe',{'view':'desktop'});value,shot=self.client.finish(shot);self.assertEqual(value['captured_count'],1)
   (Q/'pending-js-dialog.webp').write_bytes(base64.b64decode(next(c['data'] for c in shot['content'] if c['type']=='image')))
   self.act([{'do':'press','target':'desktop','key':'Enter'}]);self.assertIn('accepted pending',self.client.read_browser()['text'])
  finally:
   if self.client.snapshot().get('health',{}).get('page') is False:self.client.act([{'do':'press','target':'desktop','key':'Escape'}])
 def test_unicode_text_limits_are_valid_in_the_rust_task_owner(self):
  self.client.fixture_js("await page.evaluate(()=>{document.title='😀'.repeat(201);document.body.innerHTML='<main>'+ '😀'.repeat(5000)+'</main>';});return true;")
  read=self.client.read_browser(max_chars=1);self.assertEqual(read['text'],'😀');self.assertEqual(read['chars'],5000);self.assertTrue(read['truncated'])
  self.assertTrue(self.client.snapshot()['observation'])
  self.client.fixture_js("await page.evaluate(()=>document.body.innerHTML='<textarea aria-label=Multiline></textarea>');return true;")
  field=self.client.read_browser(extract='fields')['fields'][0];multiline='中文\nhello "quoted" \\ 🙂'
  self.act([{'do':'fill','ref':field['ref'],'text':multiline}])
  self.assertEqual(self.client.read_browser(extract='fields')['fields'][0]['value'],multiline)

if __name__=='__main__':unittest.main(verbosity=2)
