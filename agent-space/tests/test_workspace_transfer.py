"""Offline transfer fault checks; run as root in a disposable container for mapping."""
import importlib.util,io,json,os,shutil,stat,tarfile,tempfile,unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('transfer',Path(__file__).resolve().parents[1]/'docker/workspace-transfer.py')
transfer=importlib.util.module_from_spec(spec);spec.loader.exec_module(transfer)
class WorkspaceTransfer(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup);self.root=Path(self.temp.name)
  self.source=self.root/'source';self.source.mkdir();(self.source/'document').write_text('original')
  (self.source/'link').symlink_to('/outside/preserved-but-never-followed')
  runtime=self.source/'.agent-space/uploads';runtime.mkdir(parents=True);(runtime/'owner').write_text('original-space')
  self.archive=self.root/'workspace.tar';transfer.export_workspace(self.source,self.archive)
 def test_export_is_readonly_and_never_replaces_archive(self):
  before=transfer.sha(self.archive)
  with self.assertRaises(FileExistsError):transfer.export_workspace(self.source,self.archive)
  self.assertEqual(transfer.sha(self.archive),before);self.assertEqual((self.source/'document').read_text(),'original')
  self.assertTrue((self.source/'link').is_symlink())
 def test_read_atime_update_is_not_a_source_write(self):
  path=self.source/'document';os.utime(path,(0,0));before=path.stat()
  archive=self.root/'relatime.tar';result=transfer.export_workspace(self.source,archive)
  self.assertFalse(result['sourceChanged']);self.assertEqual(path.stat().st_mtime_ns,before.st_mtime_ns)
  self.assertEqual(path.read_text(),'original')
 def test_preview_validates_all_bytes_without_creating_target(self):
  target=self.root/'target';result=transfer.import_workspace(self.archive,target)
  self.assertFalse(result['applied']);self.assertFalse(target.exists())
  with tarfile.open(self.archive,'a') as tar:
   info=tarfile.TarInfo('workspace/document');info.size=4;tar.addfile(info,io.BytesIO(b'evil'))
  with self.assertRaises(ValueError):transfer.import_workspace(self.archive,target,True)
  self.assertFalse(target.exists())
 @unittest.skipUnless(os.geteuid()==0,'run in disposable root container for ownership mapping')
 def test_apply_maps_root_and_1001_preserves_links_and_archives_runtime(self):
  os.chown(self.source/'document',1001,1001);self.archive.unlink();transfer.export_workspace(self.source,self.archive)
  before={p.relative_to(self.source).as_posix():(p.lstat().st_uid,p.lstat().st_gid,p.lstat().st_mode) for p in [self.source,*self.source.rglob('*')]}
  target=self.root/'target';result=transfer.import_workspace(self.archive,target,True)
  self.assertTrue(result['applied']);self.assertEqual((target/'document').read_text(),'original')
  self.assertEqual((target/'link').readlink(),Path('/outside/preserved-but-never-followed'))
  self.assertFalse((target/'.agent-space/uploads').exists())
  self.assertEqual((target/result['archivedRuntime']/'uploads/owner').read_text(),'original-space')
  self.assertTrue(all(p.lstat().st_uid==1001 and p.lstat().st_gid==1001 for p in [target,*target.rglob('*')]))
  after={p.relative_to(self.source).as_posix():(p.lstat().st_uid,p.lstat().st_gid,p.lstat().st_mode) for p in [self.source,*self.source.rglob('*')]}
  self.assertEqual(before,after)
  self.assertAlmostEqual(target.stat().st_mtime,self.source.stat().st_mtime,places=5)
  self.assertAlmostEqual((target/'.agent-space').stat().st_mtime,(self.source/'.agent-space').stat().st_mtime,places=5)
  with self.assertRaises(ValueError):transfer.import_workspace(self.archive,target,True)
 @unittest.skipUnless(os.geteuid()==0,'root container required for mixed owner fixture')
 def test_unknown_owner_is_reported_and_apply_refuses_without_guessing(self):
  for owner in [(1234,1234),(0,1001),(1001,0)]:
   os.chown(self.source/'document',*owner);self.archive.unlink();transfer.export_workspace(self.source,self.archive)
   target=self.root/'target';self.assertEqual(transfer.import_workspace(self.archive,target)['unsupportedOwners'],[owner])
   with self.assertRaises(ValueError):transfer.import_workspace(self.archive,target,True)
   self.assertFalse(target.exists())
 @unittest.skipUnless(os.geteuid()==0,'root container required for full permission mapping')
 def test_special_permission_bits_survive_owner_mapping(self):
  for name,mode in [('sticky',0o1777),('setgid',0o2775)]:
   path=self.source/name;path.mkdir();path.chmod(mode)
  (self.source/'document').chmod(0o6755)
  self.archive.unlink();transfer.export_workspace(self.source,self.archive)
  target=self.root/'target';transfer.import_workspace(self.archive,target,True)
  for name in ['sticky','setgid','document']:
   self.assertEqual(stat.S_IMODE((target/name).stat().st_mode),stat.S_IMODE((self.source/name).stat().st_mode))
   self.assertEqual((target/name).stat().st_uid,1001)
 def test_unlisted_path_traversal_is_rejected_before_writes(self):
  with tarfile.open(self.archive,'a') as tar:
   info=tarfile.TarInfo('workspace/../escape');info.size=1;tar.addfile(info,io.BytesIO(b'x'))
  with self.assertRaises(ValueError):transfer.import_workspace(self.archive,self.root/'target',True)
  self.assertFalse((self.root/'escape').exists())
 @unittest.skipUnless(os.geteuid()==0,'root container required for runtime archival boundary')
 def test_runtime_archival_never_traverses_a_source_link(self):
  shutil.rmtree(self.source/'.agent-space')
  outside=self.root/'outside';outside.mkdir();(outside/'sentinel').write_text('untouched')
  (self.source/'.agent-space').symlink_to(outside)
  legacy=self.source/'.agent-space-upload';legacy.mkdir();(legacy/'owner').write_text('original-space')
  self.archive.unlink();transfer.export_workspace(self.source,self.archive)
  for apply in (False,True):
   with self.subTest(apply=apply):
    target=self.root/'target'
    with self.assertRaises(ValueError):transfer.import_workspace(self.archive,target,apply)
    self.assertFalse(target.exists())
    self.assertEqual(sorted(p.name for p in outside.iterdir()),['sentinel'])
    self.assertEqual((outside/'sentinel').read_text(),'untouched')
 @unittest.skipUnless(os.geteuid()==0,'root container required for full import')
 def test_linux_backslash_filename_round_trips(self):
  name='a\\b.txt';(self.source/name).write_text('literal Linux filename')
  self.archive.unlink();transfer.export_workspace(self.source,self.archive)
  target=self.root/'target';transfer.import_workspace(self.archive,target,True)
  self.assertEqual((target/name).read_text(),'literal Linux filename')
if __name__=='__main__':unittest.main()
