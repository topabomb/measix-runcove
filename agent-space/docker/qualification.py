"""Shared real browser/files/UI acceptance, used by both deployment drivers."""
import hashlib,os,subprocess,sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
SHARED_CHECKS={'shared_test_browser_vm.py','shared_test_media_vm.py','shared_test_desktop_files_vm.py','shared_test_browser_lifecycle_vm.py','shared_real_vnc_keyboard_clipboard_viewers_responsive_and_idle'}
def run_shared(output,environment,passed):
 environment=dict(environment,AGENT_SPACE_BROWSER_TEST_DIR=str(output))
 for suite in ['test_browser_vm.py','test_media_vm.py','test_desktop_files_vm.py','test_browser_lifecycle_vm.py']:
  result=subprocess.run([sys.executable,str(ROOT/'tests'/suite)],env=environment,capture_output=True,text=True,timeout=700)
  (output/(suite+'.log')).write_text(result.stdout+result.stderr)
  if result.returncode:raise RuntimeError('shared qualification failed: '+suite+'; see its log')
  passed('shared_'+suite)
 result=subprocess.run(['npm','test','--','--reporter=list','--output='+str(output/'ui-results')],cwd=ROOT/'desktop-ui',env=environment,capture_output=True,text=True,timeout=700)
 (output/'ui.log').write_text(result.stdout+result.stderr)
 if result.returncode:raise RuntimeError('shared UI qualification failed; see ui.log')
 passed('shared_real_vnc_keyboard_clipboard_viewers_responsive_and_idle')
 return {p.name:hashlib.file_digest(p.open('rb'),'sha256').hexdigest() for p in sorted(output.glob('*.log'))}
