#!/usr/bin/env python3
"""Offline schema2 CLI qualification; clones a dedicated Host's state only."""
import json,os,subprocess,tempfile,unittest
from pathlib import Path
Q=Path(os.environ['AGENT_SPACE_BROWSER_TEST_DIR'])
ROOT=Path(__file__).resolve().parents[1]
class MigrationCLI(unittest.TestCase):
 def test_preview_apply_and_immutable_legacy_identity(self):
  original=json.loads((Q/'state/state.json').read_text())
  original['schemaVersion']=2
  for user in original['users']:user.pop('image',None)
  with tempfile.TemporaryDirectory() as directory:
   state=Path(directory)/'state';state.mkdir()
   source=json.dumps(original,indent=2).encode();(state/'state.json').write_bytes(source)
   config=(Q/'config.toml').read_text().replace(str(Q/'state'),str(state)).replace('image={preset="runcove-desktop"}','image="nikolaik/python-nodejs:python3.12-nodejs22-slim"')
   path=Path(directory)/'config.toml';path.write_text(config)
   env=dict(os.environ,QUAL_ADMIN_TOKEN=(Q/'admin-token').read_text().strip())
   command=[str(ROOT/'target/debug/agent-space-mcp'),'--config',str(path),'migrate-state']
   preview=subprocess.run(command+['--dry-run'],env=env,capture_output=True,text=True,timeout=60)
   self.assertEqual(preview.returncode,0,preview.stderr);self.assertEqual((state/'state.json').read_bytes(),source)
   self.assertFalse((state/'state.schema2.backup.json').exists())
   applied=subprocess.run(command+['--apply'],env=env,capture_output=True,text=True,timeout=60)
   self.assertEqual(applied.returncode,0,applied.stderr)
   self.assertEqual((state/'state.schema2.backup.json').read_bytes(),source)
   result=json.loads((state/'state.json').read_text());self.assertEqual(result['schemaVersion'],3)
   for before,after in zip(original['users'],result['users']):
    self.assertEqual(after['image']['kind'],'custom');self.assertTrue(after['image']['reference'])
    after.pop('image');self.assertEqual(before,after)
   again=subprocess.run(command+['--apply'],env=env,capture_output=True,text=True,timeout=60)
   self.assertNotEqual(again.returncode,0)
if __name__=='__main__':unittest.main(verbosity=2)
