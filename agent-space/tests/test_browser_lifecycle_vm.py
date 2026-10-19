#!/usr/bin/env python3
"""Five-minute idle/wake qualification; run serially against a dedicated Host."""
import http.client,json,shlex,time,unittest,urllib.parse
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
  headers={'Cookie':cookie,'Origin':BASE,'X-Desktop-Nonce':nonce};started=time.monotonic();slept=False;frozen=None
  def python(code):
   reply=client.tool('bash',{'command':'python3 -c '+shlex.quote(code)});self.assertFalse(reply.get('isError'),text(reply));return text(reply)
  try:
   while time.monotonic()-started<335:
    if frozen is None and time.monotonic()-started>=290:
     frozen=python("""import json,os,signal
from pathlib import Path
found=[]
for p in Path('/proc').glob('[0-9]*/cmdline'):
 try:a=p.read_bytes().split(b'\\0')
 except OSError:continue
 if b'--remote-debugging-port=9222' in a and not any(x.startswith(b'--type=') for x in a):found.append(int(p.parent.name))
assert len(found)==1,found
pid=found[0];start=(Path('/proc')/str(pid)/'stat').read_text().rsplit(') ',1)[1].split()[19]
os.kill(pid,signal.SIGSTOP);print(json.dumps({'pid':pid,'start':start}))""")
    connection=http.client.HTTPConnection(url.hostname,url.port,timeout=20)
    connection.request('GET',prefix+'/control',headers=headers);response=connection.getresponse();self.assertEqual(response.status,200);response.read()
    connection.request('GET',prefix+'/health',headers=headers);response=connection.getresponse();self.assertEqual(response.status,200);health=json.loads(response.read());connection.close()
    if not health['desktop']:slept=True;break
    time.sleep(5)
  finally:
   if frozen:
    python(f"""import os,signal
from pathlib import Path
p=Path('/proc/{frozen['pid']}/stat')
try:matches=p.read_text().rsplit(') ',1)[1].split()[19]=={frozen['start']!r}
except OSError:matches=False
if matches:os.kill({frozen['pid']},signal.SIGCONT)
print('frozen browser cleanup complete')""")
  self.assertTrue(slept,'passive polling prevented five-minute idle close')
  self.assertIsNotNone(frozen,'unresponsive browser case was not exercised')
  self.assertGreater(time.monotonic()-started,285)
  time.sleep(4)
  observation=client.tool('browser_observe',{});observed=text(observation)
  self.assertFalse(observation.get('isError'),observed)
  self.assertIn('health',observed)
  self.assertTrue(observed['health']['browser'])
  result=text(client.tool('browser_run',{'code':"return {tabs:context.pages().map(p=>p.url()),marker:await page.evaluate(()=>localStorage.getItem('qualification'))};"}))
  while result['state']=='running':result=text(client.tool('browser_task',{'task_id':result['task_id']}))
  self.assertEqual(result['state'],'succeeded',result);self.assertEqual(result['value']['tabs'],before);self.assertEqual(result['value']['marker'],'persistent')
  print('five-minute passive polling / unresponsive-browser idle / wake verified; tab count and profile preserved',flush=True)
if __name__=='__main__':unittest.main(verbosity=2)
