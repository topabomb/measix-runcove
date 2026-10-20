#!/usr/bin/env python3
"""Five-minute idle/wake qualification; run serially against a dedicated Host."""
import http.client,json,shlex,time,unittest,urllib.parse
from browser_client import BASE,USERS,Client,text,space_path,reset_tabs,Q
class DesktopLifecycle(unittest.TestCase):
 def test_passive_polling_does_not_keep_desktop_alive_and_wake_preserves_tabs(self):
  client=Client('desktop');client.initialize()
  reset_tabs(client)
  client.fixture_js("await page.evaluate(()=>localStorage.setItem('qualification','persistent'));return true;")
  before=sorted(t['url'] for t in client.snapshot(view='tabs')['tabs']);url=urllib.parse.urlsplit(BASE);prefix=space_path('desktop','desktop/api')
  connection=http.client.HTTPConnection(url.hostname,url.port,timeout=40)
  connection.request('POST',prefix+'/login',json.dumps({'token':USERS['desktop']['token']}),{'Origin':BASE,'Content-Type':'application/json'})
  response=connection.getresponse();self.assertEqual(response.status,200)
  cookie=response.headers['Set-Cookie'].split(';')[0];nonce=json.loads(response.read())['nonce'];connection.close()
  headers={'Cookie':cookie,'Origin':BASE,'X-Desktop-Nonce':nonce};started=time.monotonic();slept=False;frozen=None;preparing_polls=0
  def python(code):
   reply=client.tool('bash',{'cmd':'python3 -c '+shlex.quote(code)});self.assertFalse(reply.get('isError'),text(reply));return text(reply)
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
    if health.get('preparing'):preparing_polls+=1
    # null/preparing means cleanup is still holding the desktop lock. It must
    # not end the wait or resume the injected SIGSTOP during shutdown.
    if health['desktop'] is False:
     self.assertEqual(health.get('worker_contract'),1,health);slept=True;break
    time.sleep(5)
  finally:
   if frozen and not slept:
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
  stopped=python(f"""import json
from pathlib import Path
p=Path('/proc/{frozen['pid']}/stat')
try:f=p.read_text().rsplit(') ',1)[1].split();alive=f[19]=={frozen['start']!r} and f[0]!='Z'
except OSError:alive=False
state=json.loads(Path('/workspace/.agent-space/desktop/profile/Local State').read_text())
print(json.dumps({{'old_process_alive':alive,'was':state.get('was'),'last_active_profiles':state.get('profile',{{}}).get('last_active_profiles')}}))""")
  diagnostic={'expected_urls':before,'sleep_health':health,'preparing_polls':preparing_polls,'stopped':stopped}
  (Q/'lifecycle-state.json').write_text(json.dumps(diagnostic,indent=2)+'\n')
  self.assertFalse(stopped['old_process_alive'],stopped)
  self.assertIs(stopped['was']['restarted'],True,stopped)
  wait_started=time.monotonic()
  waited,reply=client.act([{'do':'wait','secs':1}],page_id='missing',then='snapshot')
  self.assertFalse(reply.get('isError'),waited);self.assertGreaterEqual(time.monotonic()-wait_started,1)
  self.assertEqual(waited['value'],{'completed':1,'of':1})
  connection=http.client.HTTPConnection(url.hostname,url.port,timeout=20)
  connection.request('GET',prefix+'/health',headers=headers);response=connection.getresponse();health_after_wait=json.loads(response.read());connection.close()
  self.assertFalse(health_after_wait['desktop'],'a pure wait woke the sleeping browser/desktop')
  diagnostic['pure_wait_while_asleep']={'result':waited,'health':health_after_wait}
  time.sleep(4)
  observed=client.snapshot();self.assertTrue(observed.get('page_id'))
  diagnostic['first_wake']=observed;(Q/'lifecycle-state.json').write_text(json.dumps(diagnostic,indent=2)+'\n')
  self.assertEqual(sorted(t['url'] for t in client.snapshot(view='tabs')['tabs']),before)
  self.assertEqual(client.fixture_js("return await page.evaluate(()=>localStorage.getItem('qualification'));"),'persistent')
  print(f'five-minute passive polling / frozen-browser termination / wake verified; ignored {preparing_polls} preparing polls; tabs and profile preserved',flush=True)
if __name__=='__main__':unittest.main(verbosity=2)
