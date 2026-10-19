#!/usr/bin/env python3
"""Shared Page admission regressions; only run against a dedicated test Host."""
import json,time,unittest
from browser_client import Client,text,Q

class PageLimits(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.client=Client('desktop');cls.client.initialize()
  cls.maximum=int((Q/'max_pages').read_text()) if (Q/'max_pages').exists() else 4
 @classmethod
 def finish(cls,reply):
  value=text(reply)
  while value.get('state')=='running':
   reply=cls.client.tool('browser_task',{'task_id':value['task_id']});value=text(reply)
  return value,reply
 @classmethod
 def run_code(cls,code,**args):return cls.finish(cls.client.tool('browser_run',{'code':code,**args}))
 def setUp(self):
  result,reply=self.run_code("for(const p of context.pages())if(p!==page)await p.close();await page.goto('http://127.0.0.1:8124/');await page.bringToFront();return context.pages().length;")
  self.assertFalse(reply.get('isError'),result);self.assertEqual(result['value'],1)
 def tearDown(self):
  result,reply=self.run_code("for(const p of context.pages())if(p!==page)await p.close();return true;")
  self.assertFalse(reply.get('isError'),result)
 def fill(self):
  result,reply=self.run_code(f"while(context.pages().length<{self.maximum})await context.newPage();return context.pages().length;")
  self.assertFalse(reply.get('isError'),result);self.assertEqual(result['value'],self.maximum)
 def count(self):
  observed=self.client.tool('browser_observe',{})
  self.assertFalse(observed.get('isError'),text(observed))
  return len(text(observed)['tabs'])
 def limited(self,code):
  value,reply=self.run_code(code)
  self.assertTrue(reply.get('isError'),value);self.assertEqual(value['state'],'failed')
  self.assertEqual(value['error']['code'],'PAGE_LIMIT_REACHED',value)
  self.assertIn(str(self.maximum),value['error']['message'])
  self.assertEqual(set(value['error']),{'code','message'})
  self.assertFalse({'usage','remaining','max_pages','page_count'}&set(value))
  polled=self.client.tool('browser_task',{'task_id':value['task_id'],'wait_secs':0})
  self.assertEqual(polled,reply)
  return value
 def test_01_full_browser_keeps_observation_navigation_close_and_outputs(self):
  shape=set(text(self.client.tool('browser_observe',{})))
  self.fill()
  value=self.limited("const fs=await import('node:fs');fs.writeFileSync('/workspace/page-limit-marker.txt','preserved');await output.file('/workspace/page-limit-marker.txt');await context.newPage();")
  self.assertEqual(value['files'][0]['path'],'/workspace/page-limit-marker.txt')
  self.assertEqual(set(text(self.client.tool('browser_observe',{}))),shape)
  image=self.client.tool('screen_observe',{'view':'page'})
  self.assertFalse(image.get('isError'),text(image))
  value,reply=self.run_code("await page.goto('http://127.0.0.1:8124/');const other=context.pages().find(p=>p!==page);if(other)await other.close();await context.newPage();return context.pages().length;")
  self.assertFalse(reply.get('isError'),value);self.assertEqual(value['value'],self.maximum)
 def test_02_concurrent_creation_has_one_shared_limit(self):
  value,reply=self.run_code(f"const results=await Promise.allSettled(Array.from({{length:{self.maximum*2}}},()=>context.newPage()));return {{opened:results.filter(r=>r.status==='fulfilled').length,errors:results.filter(r=>r.status==='rejected').map(r=>r.reason.message),count:context.pages().length}};")
  self.assertFalse(reply.get('isError'),value)
  self.assertEqual(value['value']['opened'],self.maximum-1)
  self.assertEqual(value['value']['count'],self.maximum)
  self.assertTrue(all(e.startswith('PAGE_LIMIT_REACHED:') for e in value['value']['errors']))
 def test_03_popup_and_new_window_use_the_same_limit(self):
  value,reply=self.run_code("const pending=page.waitForEvent('popup');await page.evaluate(()=>window.open('about:blank','_blank'));const popup=await pending;await popup.close();return true;")
  self.assertFalse(reply.get('isError'),value)
  self.fill()
  self.limited("const pending=page.waitForEvent('popup');await page.evaluate(()=>window.open('about:blank','_blank'));await pending;await new Promise(r=>setTimeout(r,3000));")
  self.assertEqual(self.count(),self.maximum)
  for _ in range(20):
   self.limited("await page.evaluate(()=>{window.open('about:blank','_blank');return true;});return true;")
   self.assertEqual(self.count(),self.maximum)
  self.limited("const cdp=await context.browser().newBrowserCDPSession();await cdp.send('Target.createTarget',{url:'about:blank',newWindow:true});await new Promise(r=>setTimeout(r,3000));")
  self.assertEqual(self.count(),self.maximum)
 def test_04_native_new_window_does_not_poison_later_observation(self):
  self.fill()
  result=self.client.tool('bash',{'command':'DISPLAY=:1 xdotool key --clearmodifiers ctrl+n; sleep 1'})
  self.assertFalse(result.get('isError'),text(result))
  self.assertEqual(self.count(),self.maximum)
  value,reply=self.run_code('return 23;')
  self.assertFalse(reply.get('isError'),value);self.assertEqual(value['value'],23)
 def test_05_timeout_and_cancel_do_not_release_live_pages(self):
  for cancel in [False,True]:
   self.setUp()
   code="await context.newPage();await new Promise(r=>setTimeout(r,60000));"
   started=self.client.tool('browser_run',{'code':code,'timeout_secs':60 if cancel else 2})
   if cancel:
    value=text(started);self.assertEqual(value['state'],'running',value)
    started=self.client.tool('browser_task',{'task_id':value['task_id'],'action':'cancel'})
   value,reply=self.finish(started)
   self.assertEqual(value['state'],'cancelled' if cancel else 'timed_out',value)
   self.assertEqual(self.count(),2)
   self.fill();self.limited('await context.newPage();')
 def test_06_closing_last_page_completes_and_next_observation_reopens(self):
  value,reply=self.run_code('await page.close();return true;')
  self.assertFalse(reply.get('isError'),value);self.assertTrue(value['value'])
  self.assertGreater(self.count(),0)

if __name__=='__main__':unittest.main(verbosity=2)
