#!/usr/bin/env python3
"""Shared admission for structured tabs, JS popups and native windows."""
import json,shlex,time,unittest
from browser_client import Client,text,Q,element,reset_tabs
class PageLimits(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.client=Client('desktop');cls.client.initialize();cls.maximum=int((Q/'max_pages').read_text()) if (Q/'max_pages').exists() else 4
 def setUp(self):reset_tabs(self.client)
 def tearDown(self):reset_tabs(self.client)
 def fill(self):
  count=len(self.client.snapshot(view='tabs')['tabs'])
  value,reply=self.client.act([{'do':'tab','action':'new'} for _ in range(self.maximum-count)]) if count<self.maximum else ({}, {})
  self.assertFalse(reply.get('isError'),value);self.assertEqual(self.count(),self.maximum)
 def count(self):return len(self.client.snapshot(view='tabs')['tabs'])
 def limited(self,steps,**args):
  value,reply=self.client.act(steps,**args);self.assertTrue(reply.get('isError'),value);self.assertEqual(value['state'],'failed');self.assertEqual(value['error']['code'],'PAGE_LIMIT_REACHED',value)
  self.assertEqual(self.client.tool('browser_act',{'op':'wait','task_id':value['task_id'],'wait_secs':0}),reply)
  return value
 def test_01_full_browser_keeps_navigation_close_and_observation(self):
  self.fill();self.limited([{'do':'tab','action':'new'}]);self.client.good('screen_observe',{'view':'page'})
  self.client.navigate('http://127.0.0.1:8124/');tabs=self.client.snapshot(view='tabs')['tabs']
  value,reply=self.client.act([{'do':'tab','action':'close','page_id':tabs[-1]['page_id']},{'do':'tab','action':'new'}]);self.assertFalse(reply.get('isError'),value);self.assertEqual(self.count(),self.maximum)
 def test_02_js_popup_and_native_window_share_the_limit(self):
  view=self.client.navigate('http://127.0.0.1:8125/popup');self.fill();view=self.client.snapshot(page_id=view['page']['page_id'])
  self.limited([{'do':'click','ref':element(view,'link','Open invoices in new tab')}],page_id=view['page']['page_id']);self.assertEqual(self.count(),self.maximum)
  # Real browser keyboard creates a native Chrome window through the same guard.
  self.client.act([{'do':'press','target':'desktop','key':'Control+n'}]);self.assertEqual(self.count(),self.maximum)
  # A separate CDP client simulates a hostile page opening a burst of windows.
  script="""(async()=>{
const target=(await(await fetch('http://127.0.0.1:9222/json/list')).json()).find(t=>t.id===PAGE_ID);
if(!target)throw Error('fixture target missing');
let count=0;for(let i=0;i<12;i++)count+=Number(await new Promise((resolve,reject)=>{
 const w=new WebSocket(target.webSocketDebuggerUrl),timer=setTimeout(()=>{w.close();reject(Error('burst injection timed out'));},5000);
 w.onopen=()=>w.send(JSON.stringify({id:1,method:'Runtime.evaluate',params:{expression:"!!window.open('about:blank','_blank')",returnByValue:true,userGesture:true}}));
 w.onmessage=e=>{const m=JSON.parse(e.data);if(m.id!==1)return;clearTimeout(timer);w.close();m.error||m.result.exceptionDetails?reject(Error(JSON.stringify(m.error||m.result.exceptionDetails))):resolve(m.result.result.value);};
 w.onerror=()=>{clearTimeout(timer);reject(Error('burst CDP connection failed'));};
}));
console.log('popup_requests='+count);await new Promise(r=>setTimeout(r,600));
})().then(()=>process.exit()).catch(e=>{console.error(e);process.exit(1)});""".replace('PAGE_ID',json.dumps(view['page']['page_id']))
  result=self.client.good('bash',{'cmd':'node -e '+shlex.quote(script)})
  self.assertIn('popup_requests=12',result['text'])
  self.assertEqual(self.count(),self.maximum)
 def test_03_deadline_and_cancel_retain_live_pages(self):
  for cancel in [False,True]:
   reset_tabs(self.client)
   started=self.client.tool('browser_act',{'actions':[{'do':'tab','action':'new'},{'do':'wait','secs':10},{'do':'wait','secs':10}],'timeout_secs':60 if cancel else 3})
   if cancel:
    value=text(started);self.assertEqual(value['state'],'running',value);started=self.client.tool('browser_act',{'op':'cancel','task_id':value['task_id']})
   value,_=self.client.finish(started);self.assertEqual(value['state'],'cancelled' if cancel else 'timed_out',value);self.assertEqual(self.count(),2)
   self.fill();self.limited([{'do':'tab','action':'new'}])
 def test_04_closing_last_tab_is_explicit_and_snapshot_does_not_create_one(self):
  for _ in range(3):
   reset_tabs(self.client)
   target=self.client.snapshot()['page']['page_id'];value,reply=self.client.act([{'do':'tab','action':'close','page_id':target}]);self.assertFalse(reply.get('isError'),value)
   # Chromium can quit before replying when its last window closes. A later
   # start owns the native startup tab; snapshot never calls the page guard.
   view=self.client.snapshot(view='tabs');self.assertIsInstance(view['tabs'],list)
   self.assertNotIn(target,[t['page_id'] for t in view['tabs']])
  value,reply=self.client.act([{'do':'tab','action':'new','url':'http://127.0.0.1:8124/'}]);self.assertFalse(reply.get('isError'),value)
if __name__=='__main__':unittest.main(verbosity=2)
