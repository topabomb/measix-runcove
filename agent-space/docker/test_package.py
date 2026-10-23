"""Release gates must reject partial, stale or mismatched evidence."""
import contextlib,copy,importlib.util,io,json,os,tempfile,unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
import package
from qualification import SHARED_CHECKS
class ReleaseGates(unittest.TestCase):
 def setUp(self):
  self.info={'Id':'sha256:qualified','Os':'linux','Architecture':'amd64','Config':{'Labels':{'io.runcove.target':'server','io.runcove.source-sha256':'source','io.agent-space.state-schema':'3','org.opencontainers.image.version':'runcove-0123456789abcdef'}}}
  self.report={'passed':True,'cleanup_passed':True,'image_id':self.info['Id'],'scope':'final image','full':True,'idle_seconds':310,'checks':list(SHARED_CHECKS)}
  self.upgrade={'passed':True,'cleanup_passed':True,'image_id':self.info['Id'],'checks':['original_credentials_and_identity','workspace_profile_and_files','new_server_tools_desktop_dav_publish','restart_persistence','isolated_complete_backup_rollback']}
  self.hash=patch.object(package,'source_digest',return_value='source');self.hash.start();self.addCleanup(self.hash.stop)
 def test_server_requires_shared_suites_and_exact_upgrade_not_only_old_lifecycle(self):
  package.validate('server',self.report,self.upgrade,self.info)
  for field,value in [('full',False),('checks',[]),('cleanup_passed',False),('image_id','sha256:old')]:
   report=dict(self.report,**{field:value})
   with self.assertRaises(ValueError):package.validate('server',report,self.upgrade,self.info)
  for upgrade in [None,dict(self.upgrade,cleanup_passed=False),dict(self.upgrade,image_id='old'),dict(self.upgrade,checks=[])]:
   with self.assertRaises(ValueError):package.validate('server',self.report,upgrade,self.info)
 def test_target_source_schema_and_unsafe_application_identity_are_rejected(self):
  for key,value in [('io.runcove.target','standalone'),('io.runcove.source-sha256','old'),('io.agent-space.state-schema','2'),('org.opencontainers.image.version','schema3-old'),('org.opencontainers.image.version','runcove-$(unsafe)')]:
   info=copy.deepcopy(self.info);info['Config']['Labels'][key]=value
   with self.assertRaises(ValueError):package.validate('server',self.report,self.upgrade,info)
 def test_packaging_uses_qualified_identity_when_the_original_tag_moves(self):
  with tempfile.TemporaryDirectory() as directory:
   report=Path(directory)/'verification.json'
   report.write_text(json.dumps(dict(self.report,image='moving-tag')))
   arguments=['package','--target','server','--verification',str(report),'--files',str(report),'--storage',str(report),'--output',directory]
   with patch('sys.argv',arguments),patch.object(package.subprocess,'check_output',side_effect=RuntimeError('inspection boundary')) as inspect:
    with self.assertRaisesRegex(RuntimeError,'inspection boundary'):package.main()
   inspect.assert_called_once_with(['docker','image','inspect',self.info['Id']],text=True)

class QualificationImageIdentity(unittest.TestCase):
 def test_retag_after_inspection_cannot_change_the_qualified_image(self):
  # Stop at the first container launch. A fake daemon retags every inspected
  # name immediately, reproducing a concurrent build without starting a VM.
  class LaunchReached(Exception):pass
  for script,expected in [('verify.py','candidate'),('verify-desktop.py','candidate'),
                          ('verify-standalone.py','candidate'),('verify-production-upgrade.py','old'),
                          ('verify-workspace-import.py','old'),('verify-storage-failure.py','candidate'),
                          ('verify-dav-large.py','candidate')]:
   with self.subTest(script=script),tempfile.TemporaryDirectory() as directory:
    spec=importlib.util.spec_from_file_location('identity_probe',Path(__file__).with_name(script))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    root=Path(directory)/'agent-space';root.mkdir()
    identities={'candidate':'sha256:'+'a'*64,'old':'sha256:'+'b'*64}
    tags=dict(identities);launched=[]
    if script=='verify-workspace-import.py':
     evidence=root.parent/'docs/evidence/runcove-split-20261015';evidence.mkdir(parents=True)
     (evidence/'production-baseline.json').write_text(json.dumps({'imageId':identities['old']}))
    lifecycle=root/'lifecycle.json'
    lifecycle.write_text(json.dumps({'passed':True,'cleanup_passed':True,'image_id':identities['candidate'],'idle_seconds':310}))
    argv=[script,'--image','candidate']
    if script=='verify-desktop.py':argv+=['--lifecycle',str(lifecycle)]
    if script in ('verify-production-upgrade.py','verify-workspace-import.py'):argv+=['--old-image',identities['old'] if script=='verify-production-upgrade.py' else 'old']
    if script in ('verify-dav-large.py','verify-storage-failure.py'):argv+=['--evidence',str(root/'result.json')]
    def docker(*args,**kwargs):
     if args[:2]==('image','inspect'):
      identity=tags.get(args[2],args[2])
      if not args[2].startswith('sha256:'):tags[args[2]]='sha256:'+'c'*64
      info={'Id':identity,'Config':{'Labels':{'io.runcove.target':'standalone'}}}
      return identity if '--format' in args else json.dumps([info])
     if args[:2]==('volume','create'):
      if script=='verify.py':tags['candidate']='sha256:'+'c'*64
      return 'fixture-volume'
     if args[0] in ('run','create'):
      references=[arg for arg in args if arg in identities or arg in identities.values()]
      self.assertEqual(len(references),1,args)
      launched.append(tags.get(references[0],references[0]))
      raise LaunchReached()
     return ''
    def command(values,**kwargs):
     return SimpleNamespace(returncode=0,stdout=docker(*values[1:]),stderr='')
    with contextlib.ExitStack() as stack:
     stack.enter_context(patch('sys.argv',argv));stack.enter_context(patch.dict(os.environ,{}))
     stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
     for name in ('ROOT','PROJECT'):
      if hasattr(module,name):stack.enter_context(patch.object(module,name,root))
     if hasattr(module,'locked_msb_version'):stack.enter_context(patch.object(module,'locked_msb_version',return_value='0.7.9'))
     if hasattr(module,'docker'):stack.enter_context(patch.object(module,'docker',docker))
     else:stack.enter_context(patch.object(module.subprocess,'run',command))
     with self.assertRaises(LaunchReached):module.main()
    self.assertEqual(launched,[identities[expected]])

class ServerDesktopFailureEvidence(unittest.TestCase):
 def test_failed_suite_redacts_credentials_restores_environment_and_records_cleanup(self):
  # Exercise the complete driver with a fake daemon, including a cleanup error.
  # This must not leak fixture credentials into its caller or retained diagnostics.
  for inherited,cleanup_fails in [(None,False),('caller-secret',True)]:
   with self.subTest(inherited=inherited,cleanup_fails=cleanup_fails),tempfile.TemporaryDirectory() as directory:
    spec=importlib.util.spec_from_file_location('desktop_probe',Path(__file__).with_name('verify-desktop.py'))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    root=Path(directory);lifecycle=root/'lifecycle.json';identity='sha256:fixture'
    lifecycle.write_text(json.dumps({'passed':True,'cleanup_passed':True,'image_id':identity,'idle_seconds':310}))
    responses=iter([(200,{}),(201,{'token':'desktop-fixture-secret'}),(201,{'token':'custom-fixture-secret'})])
    def response():
     status,value=next(responses)
     return SimpleNamespace(status=status,read=lambda:json.dumps(value).encode())
    connection=SimpleNamespace(request=lambda *args:None,getresponse=response,close=lambda:None)
    stopped=False
    def docker(*args,**kwargs):
     nonlocal stopped
     if args[:2]==('image','inspect'):return identity
     if args[:2]==('volume','create'):return 'fixture-data'
     if args[0]=='inspect':return json.dumps([{'State':{'Running':not stopped,'ExitCode':0,'OOMKilled':False},'NetworkSettings':{'Ports':{'8932/tcp':[{'HostPort':'12345'}]}}}])
     if args[0]=='logs':return 'admin=admin-fixture-secret desktop=desktop-fixture-secret custom=custom-fixture-secret\n诊断\n'
     if args[0]=='stop':stopped=True
     if args[0]=='rm' and cleanup_fails:raise RuntimeError('cleanup injection')
     return ''
    environment=dict(os.environ)
    if inherited is None:environment.pop('AGENT_SPACE_ADMIN_TOKEN',None)
    else:environment['AGENT_SPACE_ADMIN_TOKEN']=inherited
    with patch.dict(os.environ,environment,clear=True),patch.object(module,'ROOT',root),patch.object(module,'docker',docker),patch.object(module.secrets,'token_urlsafe',return_value='admin-fixture-secret'),patch.object(module.http.client,'HTTPConnection',return_value=connection),patch.object(module,'run_shared',side_effect=RuntimeError('suite injection')),patch('sys.argv',['verify','--image','candidate','--lifecycle',str(lifecycle)]),contextlib.redirect_stdout(io.StringIO()):
     with self.assertRaisesRegex(RuntimeError,'cleanup failed' if cleanup_fails else 'suite injection'):module.main()
     self.assertEqual(os.environ.get('AGENT_SPACE_ADMIN_TOKEN'),inherited)
    output=next((root/'target/server-desktop-verification').iterdir())
    report=json.loads((output/'result.json').read_text());diagnostic=(output/'server-failure.log').read_text()
    self.assertFalse(report['passed']);self.assertEqual(report['cleanup_passed'],not cleanup_fails)
    self.assertEqual(diagnostic.count('[REDACTED_SECRET]'),3);self.assertNotIn('fixture-secret',diagnostic)
    self.assertIn('诊断',diagnostic);self.assertFalse((output/'users.json').exists())
    self.assertEqual(report['failure_log_sha256'],module.hashlib.sha256(diagnostic.encode()).hexdigest())

if __name__=='__main__':unittest.main()
