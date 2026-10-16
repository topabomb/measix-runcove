#!/usr/bin/env python3
"""Streaming workbench file qualification on a dedicated live MSB test Host."""
import http.client,json,unittest,uuid,urllib.parse,time,hashlib
from browser_client import BASE,USERS,Client,text,space_path
class DesktopFiles(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.url=urllib.parse.urlsplit(BASE);cls.prefix=space_path('desktop','desktop/api');cls.cookie='';cls.nonce=''
  connection=cls.connect();payload=json.dumps({'token':USERS['desktop']['token']})
  connection.request('POST',cls.prefix+'/login',payload,{'Origin':BASE,'Content-Type':'application/json'})
  response=connection.getresponse();assert response.status==200
  cls.cookie=response.headers['Set-Cookie'].split(';')[0];cls.nonce=json.loads(response.read())['nonce'];connection.close()
  cls.path='/agent-space/artifacts/transfer-'+str(uuid.uuid4())+'.bin'
 @classmethod
 def connect(cls):return http.client.HTTPConnection(cls.url.hostname,cls.url.port,timeout=120)
 @classmethod
 def headers(cls):return {'Origin':BASE,'Cookie':cls.cookie,'X-Desktop-Nonce':cls.nonce}
 def test_01_stream_one_gib_and_ranges(self):
  size=1024**3;block=b'0123456789abcdef'*4096
  connection=self.connect();connection.putrequest('PUT',self.prefix+'/files'+self.path)
  for key,value in dict(self.headers(),**{'Content-Length':str(size)}).items():connection.putheader(key,value)
  connection.endheaders();started=time.monotonic()
  for _ in range(size//len(block)):connection.send(block)
  response=connection.getresponse();self.assertEqual(response.status,201,response.read().decode(errors='replace'));response.read();connection.close()
  connection=self.connect();connection.request('HEAD',self.prefix+'/files'+self.path,headers=self.headers());response=connection.getresponse();self.assertEqual(response.status,200);self.assertEqual(int(response.headers['Content-Length']),size);response.read();connection.close()
  for start in [0,1234567,size-64]:
   connection=self.connect();connection.request('GET',self.prefix+'/files'+self.path,headers=dict(self.headers(),Range=f'bytes={start}-{start+63}'));response=connection.getresponse()
   self.assertEqual(response.status,206);self.assertEqual(response.read(),(block*2)[start%len(block):start%len(block)+64]);connection.close()
  connection=self.connect();connection.request('GET',self.prefix+'/files'+self.path,headers=self.headers());response=connection.getresponse();self.assertEqual(response.status,200)
  digest=hashlib.sha256();downloaded=0
  while chunk:=response.read(65536):digest.update(chunk);downloaded+=len(chunk)
  connection.close();self.assertEqual(downloaded,size)
  expected=hashlib.sha256()
  for _ in range(size//len(block)):expected.update(block)
  self.assertEqual(digest.hexdigest(),expected.hexdigest())
  print(f'1 GiB PUT/GET + ranges verified in {time.monotonic()-started:.1f}s; bounded 64 KiB client buffers')
 def test_02_no_overwrite_and_unreadable_cross_space(self):
  connection=self.connect();connection.request('PUT',self.prefix+'/files'+self.path,b'x',headers=self.headers());response=connection.getresponse();self.assertEqual(response.status,412);response.read();connection.close()
  connection=self.connect();connection.request('GET','/u/custom/desktop/api/files'+self.path,headers=self.headers());response=connection.getresponse();self.assertIn(response.status,[401,404]);response.read();connection.close()
 def test_03_interrupted_put_never_commits_partial_file(self):
  path=self.prefix+'/files'+self.path+'.partial'
  connection=self.connect();connection.putrequest('PUT',path)
  for key,value in dict(self.headers(),**{'Content-Length':str(1024**3)}).items():connection.putheader(key,value)
  connection.endheaders();connection.send(b'partial'*10000);connection.close()
  client=Client('desktop');client.initialize()
  until=time.monotonic()+15
  while True:
   result=text(client.tool('browser_run',{'code':'return {ok:true};'}))
   if result.get('state') in ['succeeded','running']:break
   self.assertLess(time.monotonic(),until);time.sleep(.1)
  connection=self.connect();connection.request('GET',path,headers=self.headers());response=connection.getresponse();self.assertEqual(response.status,404);response.read();connection.close()
if __name__=='__main__':unittest.main(verbosity=2)
