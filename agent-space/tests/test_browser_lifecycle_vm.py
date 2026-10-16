#!/usr/bin/env python3
"""Five-minute idle/wake qualification; run serially against a dedicated Host."""
import http.client,json,time,unittest,urllib.parse
from browser_client import BASE,USERS,Client,text,space_path
class DesktopLifecycle(unittest.TestCase):
 def test_passive_polling_does_not_keep_desktop_alive_and_wake_preserves_tabs(self):
  client=Client('desktop');client.initialize()
  code="for(const p of context.pages())if(p!==page)await p.close();await page.goto('http://127.0.0.1:8124/');await page.evaluate(()=>localStorage.setItem('qualification','persistent'));return context.pages().map(p=>p.url());"
  result=text(client.tool('browser_run',{'code':code}))
  while result['state']=='running':result=text(client.tool('browser_task',{'task_id':result['task_id']}))
  self.assertEqual(result['state'],'succeeded',result)
  before=result['value'];url=urllib.parse.urlsplit(BASE);prefix=space_path('desktop','desktop/api')
  connection=http.client.HTTPConnection(url.hostname,url.port,timeout=40)
  connection.request('POST',prefix+'/login',json.dumps({'token':USERS['desktop']['token']}),{'Origin':BASE,'Content-Type':'application/json'})
  response=connection.getresponse();self.assertEqual(response.status,200)
  cookie=response.headers['Set-Cookie'].split(';')[0];nonce=json.loads(response.read())['nonce'];connection.close()
  headers={'Cookie':cookie,'Origin':BASE,'X-Desktop-Nonce':nonce};started=time.monotonic();slept=False
  while time.monotonic()-started<330:
   connection=http.client.HTTPConnection(url.hostname,url.port,timeout=10)
   connection.request('GET',prefix+'/control',headers=headers);response=connection.getresponse();self.assertEqual(response.status,200);response.read()
   connection.request('GET',prefix+'/health',headers=headers);response=connection.getresponse();self.assertEqual(response.status,200);health=json.loads(response.read());connection.close()
   if not health['desktop']:slept=True;break
   time.sleep(5)
  self.assertTrue(slept,'passive polling prevented five-minute idle close')
  self.assertGreater(time.monotonic()-started,285)
  time.sleep(4)
  observation=client.tool('browser_observe',{});observed=text(observation)
  self.assertFalse(observation.get('isError'),observed)
  self.assertIn('health',observed)
  self.assertTrue(observed['health']['browser'])
  result=text(client.tool('browser_run',{'code':"return {tabs:context.pages().map(p=>p.url()),marker:await page.evaluate(()=>localStorage.getItem('qualification'))};"}))
  while result['state']=='running':result=text(client.tool('browser_task',{'task_id':result['task_id']}))
  self.assertEqual(result['state'],'succeeded',result);self.assertEqual(result['value']['tabs'],before);self.assertEqual(result['value']['marker'],'persistent')
  print('five-minute passive polling / idle / wake verified; tab count and profile preserved',flush=True)
if __name__=='__main__':unittest.main(verbosity=2)
