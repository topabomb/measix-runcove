import os,sys,runpy,subprocess,hashlib
from pathlib import Path
root=Path(__file__).resolve().parents[3]/'agent-space'
sys.path.insert(0,str(root/'docker'))
import qualification

def focused_suites(output,environment,passed,**kwargs):
 os.environ['AGENT_SPACE_BROWSER_TEST_DIR']=str(output)
 environment=dict(environment,AGENT_SPACE_BROWSER_TEST_DIR=str(output))
 sys.path.insert(0,str(root/'tests'))
 from test_browser_vm import BrowserQualification
 BrowserQualification.setUpClass()
 BrowserQualification('test_01_exact_catalogue_and_validation').test_01_exact_catalogue_and_validation()
 passed('tool_review_catalog_and_fixture_setup')
 for suite in ['test_browser_structured_vm.py']:
  with (output/(suite+'.log')).open('w') as log:
   done=subprocess.run([sys.executable,str(root/'tests'/suite)],env=environment,stdout=log,stderr=subprocess.STDOUT,timeout=700)
  if done.returncode:raise RuntimeError('tool review failed: '+suite+'; '+str(output))
  passed('tool_review_'+suite)
 return {p.name:hashlib.file_digest(p.open('rb'),'sha256').hexdigest() for p in sorted(output.glob('*.log'))}
qualification.run_shared=focused_suites
# Only the changed tool contract is qualified here. Lifecycle qualification has
# its own suite; no claim is made that it passed in this focused run.
qualification.verify_page_limit_restart=lambda *a,**kw:{'skipped':True,'reason':'outside focused browser tool review'}
target=sys.argv[1]
script='verify-standalone.py' if target=='standalone' else 'verify-desktop.py'
image='runcove-workspace:build' if target=='standalone' else 'runcove-server:runcove-61a1d41f93a18d9b'
sys.argv=[script,'--image',image,'--development-assets','--tools-only']
runpy.run_path(str(root/'docker'/script),run_name='__main__')
