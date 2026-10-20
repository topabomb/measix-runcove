#!/usr/bin/env python3
"""Run before browser tests on a fresh independent qualification Host.
Covers optional MCP inputs, old artifact migration and exact DAV staging scope.
"""
import base64,json,shlex,unittest,urllib.request,urllib.error
from browser_client import BASE,USERS,Q,Client,text

class WorkspaceLayout(unittest.TestCase):
 @classmethod
 def setUpClass(cls):
  cls.client=Client('desktop');cls.client.initialize()
  cls.good('bash',{'cmd':'true','timeout_secs':60})
 @classmethod
 def good(cls,name,args):
  result=cls.client.tool(name,args)
  assert not result.get('isError'),text(result)
  return text(result)
 @classmethod
 def python(cls,code):
  result=cls.client.tool('bash',{'cmd':'python3 -c '+shlex.quote(code),'workdir':'','timeout_secs':None,'unused':True})
  assert not result.get('isError'),text(result)
  assert any(c.get('text')=='Exit code: 0' for c in result['content']),result
  return text(result)
 def test_01_optional_inputs_and_internal_files(self):
  for folder in ['.agent-space/notes','.agent-space-backup','nested/.agent-space/uploads','.agent-space-upload']:
   path='/workspace/'+folder+'/ordinary.txt'
   self.good('write',{'path':path,'content':'  untouched\n','unused':None})
   self.good('edit',{'path':path,'old_text':'untouched','new_text':'','unused':10})
   self.python('from pathlib import Path;assert Path('+repr(path)+').read_text()=='+repr('  \n'))
   self.good('read',{'path':path,'offset':None,'limit':None,'unused':False})
  path='/workspace/layout-public.txt'
  self.good('write',{'path':path,'content':'public'})
  self.good('web_publish',{'action':'list','name':None,'port':'unused','keep_alive_secs':10})
  self.good('web_publish',{'action':'publish','name':'layout','file':path,'directory':'','port':None,'keep_alive_secs':None})
  self.good('web_publish',{'action':'get','name':'layout','file':False,'keep_alive_secs':'unused'})
  result=self.good('web_publish',{'action':'unpublish','name':'layout','file':None,'directory':None,'port':None,'keep_alive_secs':10})
  self.assertEqual(result['status'],'unpublished')
 def test_02_legacy_artifacts_and_default_download_location(self):
  self.python("from pathlib import Path;import json;p=Path('/workspace/.agent-space/desktop');a=p/'artifacts';a.mkdir(parents=True);(a/'same.txt').write_text('old');(a/'other.txt').write_text('other');(a/'unknown').mkdir();n=Path('/workspace/agent-space/artifacts');n.mkdir(parents=True);(n/'same.txt').write_text('new');d=p/'profile/Default';d.mkdir(parents=True);(d/'Preferences').write_text(json.dumps({'download':{'default_directory':str(a),'prompt_for_download':False}}))")
  observed=self.client.snapshot(page_id='',recent_tasks_limit=None)
  self.assertTrue(observed.get('page_id'),observed)
  self.python("from pathlib import Path;import json;p=Path('/workspace/.agent-space/desktop');n=Path('/workspace/agent-space/artifacts');assert (n/'same.txt').read_text()=='new';assert (n/'other.txt').read_text()=='other';assert len([f for f in n.glob('*same.txt') if f.read_text()=='old'])==1;assert (p/'artifacts/unknown').is_dir();assert not (p/'artifacts/same.txt').exists();assert not Path('/workspace/agent-space/README.md').exists();assert json.loads((p/'profile/Default/Preferences').read_text())['download']['default_directory']==str(n)")
  result,reply=self.client.act([{'do':'wait','secs':1}])
  self.assertFalse(reply.get('isError'),result)
 def test_03_dav_protects_only_upload_staging(self):
  req=urllib.request.Request(BASE+'/admin/v1/users/desktop/dav-token',data=json.dumps({'agentSpaceId':USERS['desktop']['agentSpaceId']}).encode(),headers={'Authorization':'Bearer '+(Q/'admin-token').read_text(),'Content-Type':'application/json'})
  with urllib.request.urlopen(req) as response:issued=json.load(response)
  headers={'Authorization':'Basic '+base64.b64encode(('desktop:'+issued['token']).encode()).decode()}
  def request(method,path,data=None):
   req=urllib.request.Request(issued['davUrl'].rstrip('/')+'/'+path,data=data,method=method,headers=dict(headers,**({'Depth':'1'} if method=='PROPFIND' else {})))
   try:
    with urllib.request.urlopen(req,timeout=60) as res:return res.status,res.read()
   except urllib.error.HTTPError as error:return error.code,error.read()
  for path in ['.agent-space/notes/ordinary.txt','.agent-space-backup/ordinary.txt','nested/.agent-space/uploads/ordinary.txt','.agent-space-upload/ordinary.txt','agent-space/artifacts/same.txt']:
   self.assertEqual(request('GET',path)[0],200,path)
   self.assertIn(request('PUT',path,b'dav')[0],[201,204],path)
  status,listing=request('PROPFIND','.agent-space/')
  self.assertEqual(status,207);self.assertIn(b'notes',listing);self.assertNotIn(b'/.agent-space/uploads/',listing)
  for method in ['GET','PUT','PROPFIND','DELETE']:
   self.assertIn(request(method,'.agent-space/uploads/owner',b'x' if method=='PUT' else None)[0],[403,404])
  self.assertEqual(request('GET','%2e%2e/escape')[0],403)

 def test_04_custom_preferences_and_unexpected_legacy_file_are_preserved(self):
  req=urllib.request.Request(BASE+'/admin/v1/users',data=json.dumps({'username':'preferences','image':{'preset':'runcove-desktop'}}).encode(),headers={'Authorization':'Bearer '+(Q/'admin-token').read_text(),'Content-Type':'application/json'})
  with urllib.request.urlopen(req) as response:USERS['preferences']=json.load(response)
  client=Client('preferences');client.initialize()
  def run(code):
   result=client.tool('bash',{'cmd':'python3 -c '+shlex.quote(code),'timeout_secs':60})
   self.assertFalse(result.get('isError'),text(result))
   self.assertTrue(any(c.get('text')=='Exit code: 0' for c in result['content']),result)
  run("from pathlib import Path;import json;p=Path('/workspace/.agent-space/desktop');d=p/'profile/Default';d.mkdir(parents=True);(p/'artifacts').write_text('user-owned');Path('/workspace/my-downloads').mkdir();(d/'Preferences').write_text(json.dumps({'download':{'default_directory':'/workspace/my-downloads'}}))")
  observed=client.snapshot();self.assertTrue(observed.get('page_id'))
  run("from pathlib import Path;import json;p=Path('/workspace/.agent-space/desktop');assert json.loads((p/'profile/Default/Preferences').read_text())['download']['default_directory']=='/workspace/my-downloads';assert (p/'artifacts').read_text()=='user-owned';assert not Path('/workspace/agent-space/README.md').exists()")

if __name__=='__main__':unittest.main(verbosity=2)
