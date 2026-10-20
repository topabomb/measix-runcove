#!/usr/bin/env python3
"""Foreground ownership and output bounds through the actual MCP/VM path."""
import json,time,unittest
from browser_client import Client,text
class BashQualification(unittest.TestCase):
 @classmethod
 def setUpClass(cls):cls.client=Client('desktop');cls.client.initialize()
 def test_01_flood_deadline_and_null_stdin(self):
  result=self.client.tool('bash',{'cmd':"python3 -u -c 'import os; print(\"prefix\",flush=True); b=b\"x\"*65536; exec(\"while True: os.write(1,b)\")'",'timeout_secs':2})
  self.assertTrue(result.get('isError'),result)
  raw=next(c['text'] for c in result['content'] if c['type']=='text')
  self.assertLess(len(raw.encode()),2*1024*1024);self.assertIn('TIMEOUT',raw.upper());self.assertIn('truncated',raw)
  result=text(self.client.tool('bash',{'cmd':"python3 -c 'import sys; print(len(sys.stdin.read()))'",'timeout_secs':5}))
  self.assertIn('0',json.dumps(result))
 def test_02_timeout_kills_verified_descendants(self):
  script="import subprocess,time,os,signal\np=subprocess.Popen(['python3','-c','import time,signal; signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(120)'])\nopen('/workspace/qualification/descendant.pid','w').write(str(p.pid))\nprint('partial-before-timeout',flush=True)\ntime.sleep(120)\n"
  self.client.tool('write',{'path':'/workspace/qualification/descendants.py','content':script})
  result=self.client.tool('bash',{'cmd':'python3 /workspace/qualification/descendants.py','timeout_secs':2})
  self.assertTrue(result.get('isError'));self.assertIn('partial-before-timeout',json.dumps(result))
  result=text(self.client.tool('bash',{'cmd':"python3 -c \"from pathlib import Path; p=Path('/workspace/qualification/descendant.pid').read_text(); print(Path('/proc/'+p+'/stat').exists())\"",'timeout_secs':5}))
  self.assertIn('False',json.dumps(result))
 def test_03_successful_foreground_preserves_background_and_inherited_fd_does_not_hang(self):
  started=time.monotonic();result=self.client.tool('bash',{'cmd':'sleep 60 & echo $! > /workspace/qualification/background.pid; echo foreground-finished','timeout_secs':5})
  self.assertFalse(result.get('isError'),result);self.assertLess(time.monotonic()-started,4)
  result=text(self.client.tool('bash',{'cmd':"python3 -c \"from pathlib import Path; p=Path('/workspace/qualification/background.pid').read_text().strip(); print(Path('/proc/'+p).exists())\"",'timeout_secs':5}))
  self.assertIn('True',json.dumps(result))
if __name__=='__main__':unittest.main(verbosity=2)
