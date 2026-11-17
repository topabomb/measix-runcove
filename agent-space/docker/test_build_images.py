#!/usr/bin/env python3
"""Exercise the real build entry scripts against an isolated Docker/MSB daemon double."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

PROJECT = Path(__file__).resolve().parents[1]
def config(name):
    return json.dumps({'architecture': 'amd64', 'os': 'linux', 'config': {'Labels': {'fixture': name}}}, sort_keys=True, separators=(',', ':'))


WORKSPACE = 'sha256:' + hashlib.sha256(config('workspace-current').encode()).hexdigest()
OTHER = 'sha256:' + hashlib.sha256(config('workspace-other').encode()).hexdigest()
OLD_APP = 'sha256:' + hashlib.sha256(config('application-old').encode()).hexdigest()
SOURCE = 'f' * 64
REVISION, EPOCH = '0' * 40, 1700000000
VERSION = 'runcove-' + hashlib.sha256(json.dumps({'sourceSha256': SOURCE, 'sourceRevision': REVISION,
    'sourceDateEpoch': EPOCH}, sort_keys=True, separators=(',', ':')).encode()).hexdigest()[:16]

DOCKER = r'''#!/usr/bin/env python3
import fcntl,hashlib,io,json,os,pathlib,sys,tarfile
p=pathlib.Path(os.environ['BUILD_TEST_STATE']); lock=open(str(p)+'.lock','a+b'); fcntl.flock(lock,fcntl.LOCK_EX); state=json.loads(p.read_text()); args=sys.argv[1:]
def finish(text=''):
 p.write_text(json.dumps(state)); print(text,end=''); raise SystemExit(0)
def flag(name): return args[args.index(name)+1] if name in args else None
def identity(data): return 'sha256:'+hashlib.sha256(data).hexdigest()
def config(name): return json.dumps({'architecture':'amd64','os':'linux','config':{'Labels':{'fixture':name}}},sort_keys=True,separators=(',',':')).encode()
if args==['info']: finish()
if args[:2]==['info','--format']:
 finish(state.get('driver_status_raw',json.dumps(state.get('driver_status',[['driver-type','io.containerd.snapshotter.v1']]))))
if args[0]=='build':
 kind='workspace' if '/images/desktop/' in flag('-f') else 'application'
 buildargs=dict(args[index+1].split('=',1) for index,value in enumerate(args) if value=='--build-arg')
 value=json.loads(config(('workspace-other' if pathlib.Path(args[-1],'guest/desktop/other-id').exists() else 'workspace-current') if kind=='workspace' else 'application-current'))
 if kind=='application':
  value['config']['Labels'].update({'org.opencontainers.image.version':buildargs.get('APP_VERSION',''),
   'org.opencontainers.image.revision':buildargs.get('SOURCE_REVISION',''),
   'io.runcove.source-sha256':buildargs.get('SOURCE_DIGEST','')})
 if state.get('vary_output_mtime'):
  normalized='rewrite-timestamp=true' in (flag('--output') or '') and buildargs.get('SOURCE_DATE_EPOCH','').isdigit()
  timestamp=buildargs['SOURCE_DATE_EPOCH'] if normalized else str(2000000000+len(state['builds']))
  value.update(created=timestamp,fixtureFileMtime=timestamp)
 data=json.dumps(value,sort_keys=True,separators=(',',':')).encode()
 config_id=identity(data)
 manifest=json.dumps({'schemaVersion':2,'mediaType':'application/vnd.oci.image.manifest.v1+json','config':{'mediaType':'application/vnd.oci.image.config.v1+json','digest':config_id,'size':len(data)},'layers':[]},separators=(',',':')).encode()
 distribution_id=identity(manifest)
 iid=distribution_id if state.get('containerd_store') else config_id
 if state.get('vary_provenance') and '--provenance=false' not in args: iid=identity((iid+str(len(state['builds']))).encode())
 state['builds'].append(kind); state['images'][iid]=data.decode()
 state.setdefault('build_arguments',[]).append(args)
 if kind=='workspace': state['workspace_build_arguments']=args
 if flag('--iidfile'): pathlib.Path(flag('--iidfile')).write_text(config_id+'\n')
 if flag('--metadata-file'): pathlib.Path(flag('--metadata-file')).write_text(json.dumps({'containerimage.config.digest':config_id,'containerimage.digest':distribution_id}))
 if flag('-t'): state['tags'][flag('-t')]=iid
 if kind=='workspace' and state.get('flip_shared_tag'): state['tags']['runcove-workspace:build']=identity(config('workspace-other'))
 if kind=='workspace' and state.get('mutate_before_copy'):
  pathlib.Path(os.environ['BUILD_TEST_PROJECT'],'guest/desktop/worker.cjs').write_text('changed during build')
  state['copied_worker']=pathlib.Path(args[-1],'guest/desktop/worker.cjs').read_text()
 finish(iid+'\n')
if args[:2]==['image','inspect']:
 name=next(a for a in args[2:] if not a.startswith('-') and not a.startswith('{{'))
 iid=state['tags'].get(name,name)
 if state.get('fail_inspect_name')==name:
  state.pop('fail_inspect_name'); p.write_text(json.dumps(state)); print('Cannot connect to the Docker daemon',file=sys.stderr); raise SystemExit(1)
 if iid not in state['images']: print('Error response from daemon: No such image: '+name,file=sys.stderr); raise SystemExit(1)
 if flag('--format'):
  fmt=flag('--format')
  if '.Id' in fmt: finish(iid+'\n')
  if '.Size' in fmt: finish('1024\n')
  if '.Architecture' in fmt: finish('amd64\n')
  if '.Os' in fmt: finish('linux\n')
  finish('{}\n')
 finish(json.dumps([{'Id':iid,'Os':'linux','Architecture':'amd64','Config':json.loads(state['images'][iid])['config']}]))
if args[0]=='tag': state['tags'][args[2]]=state['tags'].get(args[1],args[1]); finish()
if args[:2]==['image','rm']: finish()
if args[0]=='save':
 name=next(a for a in args[1:] if a.startswith('sha256:') or a in state['tags']); iid=state['tags'].get(name,name)
 state['saved'].append(iid); p.write_text(json.dumps(state)); output=pathlib.Path(flag('-o'))
 if state.get('bad_save'): output.write_bytes(b'bad OCI archive'); raise SystemExit(0)
 if state.get('save_other'): iid=identity(config('workspace-other'))
 image_config=state['images'][iid].encode(); config_id=identity(image_config)
 manifest=json.dumps({'schemaVersion':2,'mediaType':'application/vnd.oci.image.manifest.v1+json','config':{'mediaType':'application/vnd.oci.image.config.v1+json','digest':config_id,'size':len(image_config)},'layers':[]},separators=(',',':')).encode()
 digest=identity(manifest)
 index=json.dumps({'schemaVersion':2,'manifests':[{'mediaType':'application/vnd.oci.image.manifest.v1+json','digest':digest,'size':len(manifest),'platform':{'os':'linux','architecture':'amd64'}}]}).encode()
 with tarfile.open(output,'w') as archive:
  for name,data in [('index.json',index),('oci-layout',b'{"imageLayoutVersion":"1.0.0"}'),('blobs/sha256/'+config_id[7:],image_config),('blobs/sha256/'+digest[7:],manifest)]:
   item=tarfile.TarInfo(name); item.size=len(data); archive.addfile(item,io.BytesIO(data))
 finish()
raise SystemExit('unsupported fake docker call: '+repr(args))
'''

MSB = r'''#!/usr/bin/env python3
import json,os,pathlib,sys
if sys.argv[1:]==['--version']: print('msb 0.7.14'); raise SystemExit(0)
if sys.argv[1:3]==['image','pull']:
 p=pathlib.Path(os.environ['BUILD_TEST_STATE']); value=json.loads(p.read_text()); value['seed_present']=True; value['seed_pulls']=value.get('seed_pulls',0)+1; p.write_text(json.dumps(value)); raise SystemExit(0)
if sys.argv[1:3]==['image','save']:
 p=pathlib.Path(os.environ['BUILD_TEST_STATE']); value=json.loads(p.read_text()); value['seed_exports']+=1; p.write_text(json.dumps(value))
 if not value.get('seed_present',True): print('image not found',file=sys.stderr); raise SystemExit(1)
 pathlib.Path(sys.argv[sys.argv.index('-o')+1]).write_bytes(b'fixed seed OCI archive')
 raise SystemExit(0)
raise SystemExit(1)
'''

GIT = r'''#!/usr/bin/env python3
import os,sys
if sys.argv[-2:]==['rev-parse','HEAD']:print(os.environ.get('BUILD_TEST_HEAD','0'*40))
elif '--format=%ct' in sys.argv:print(os.environ.get('BUILD_TEST_EPOCH','1700000000'))
else:raise SystemExit(1)
'''


class BuildScripts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / 'project'
        for name in ('docker', 'docker/patches', 'images/desktop', 'guest/desktop', 'bin'):
            (self.root / name).mkdir(parents=True, exist_ok=True)
        for name in ('docker/build.sh', 'docker/build-standalone.sh', 'images/desktop/build.sh', 'images/desktop/export.py'):
            shutil.copy2(PROJECT / name, self.root / name)
        driver = PROJECT / 'docker/build_images.py'
        if driver.exists():
            shutil.copy2(driver, self.root / 'docker/build_images.py')
        self.write('images/desktop/Dockerfile', 'FROM fixture\nCOPY guest/desktop/ /worker/\n')
        if (PROJECT/'images/desktop/install_browser.py').exists():
            shutil.copy2(PROJECT/'images/desktop/install_browser.py', self.root/'images/desktop/install_browser.py')
        self.write('images/desktop/chromium-download.lock.json', json.dumps({'schemaVersion':1,'browser':'chromium',
            'playwrightVersion':'1.63.0','revision':'1243','browserVersion':'153.0.8010.12',
            'url':'https://cdn.playwright.dev/builds/cft/153.0.8010.12/linux64/chrome-linux64.zip','sha256':'a'*64}))
        self.write('guest/desktop/worker.cjs', 'initial worker\n')
        self.write('images/desktop-image.lock.json', json.dumps({'reference': 'fixture@sha256:old'}))
        self.write('.dockerignore', '*.md\n**/node_modules/\n')
        self.write('docker/Dockerfile', 'FROM fixture\n')
        self.write('docker/Dockerfile.standalone', 'FROM fixture\n')
        self.write('docker/release_identity.py', 'def source_digest(): return "' + SOURCE + '"\nif __name__ == "__main__": print(source_digest())\n')
        self.write('Cargo.lock', '[[package]]\nname="microsandbox"\nversion="0.7.14"\n')
        self.write('docker/patches/manifest.json', '{"version":"0.7.14"}')
        self.write('docker/configs.example/config.toml', '# fixture config\n')
        self.write('bin/docker', DOCKER, executable=True)
        self.write('bin/git', GIT, executable=True)
        self.state = self.root / 'daemon.json'
        self.state.write_text(json.dumps({'tags': {}, 'images': {WORKSPACE: config('workspace-current'), OTHER: config('workspace-other'), OLD_APP: config('application-old')}, 'builds': [], 'saved': [], 'seed_exports': 0}))
        self.msb = self.root / 'msb'
        self.write('msb/bin/msb', MSB, executable=True)
        self.write('msb/lib/libkrunfw.so.5.1.0', 'runtime library')
        records = []
        for name in ('bin/msb', 'lib/libkrunfw.so.5.1.0'):
            records.append(hashlib.sha256((self.msb / name).read_bytes()).hexdigest() + '  ' + name)
        self.write('msb-runtime.sha256', '\n'.join(records) + '\n')
        self.environment = dict(os.environ, PATH=str(self.root / 'bin') + os.pathsep + os.environ['PATH'], MSB_HOME=str(self.msb), BUILD_TEST_STATE=str(self.state), BUILD_TEST_PROJECT=str(self.root))
        self.environment.pop('IMAGE_NAME', None)

    def write(self, name, content, executable=False):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        if executable:
            path.chmod(0o755)

    def run_script(self, name, *arguments):
        return subprocess.run(['bash', str(self.root / name), *map(str, arguments)], cwd=self.root, env=self.environment,
                              capture_output=True, text=True, timeout=30)

    def daemon(self, **changes):
        state = json.loads(self.state.read_text())
        state.update(changes)
        self.state.write_text(json.dumps(state))
        return state

    def build_workspace(self):
        result = self.run_script('images/desktop/build.sh')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_workspace_exports_its_own_iid_when_shared_tag_moves(self):
        self.daemon(flip_shared_tag=True)
        self.build_workspace()
        self.assertEqual((self.root / 'docker/.build/images/workspace.image-id').read_text().strip(), WORKSPACE)
        self.assertEqual(self.daemon()['saved'], [WORKSPACE])

    def test_workspace_passes_locked_browser_inputs_and_rejects_invalid_lock_before_build(self):
        self.build_workspace()
        arguments = self.daemon()['workspace_build_arguments']
        self.assertIn('CHROMIUM_URL=https://cdn.playwright.dev/builds/cft/153.0.8010.12/linux64/chrome-linux64.zip', arguments)
        self.assertIn('CHROMIUM_SHA256='+'a'*64, arguments)
        lock = self.root/'images/desktop/chromium-download.lock.json'
        changed = json.loads(lock.read_text())
        changed['url'] = 'https://untrusted.invalid/browser.zip'
        lock.write_text(json.dumps(changed))
        result = self.run_script('images/desktop/build.sh')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.daemon()['builds'], ['workspace'])

    def test_legacy_or_unknown_image_store_is_rejected_before_expensive_builds(self):
        for value in [[], [['Backing Filesystem', 'extfs']], None, 'unrecognized']:
            with self.subTest(status=value):
                self.daemon(driver_status=value)
                result = self.run_script('docker/build.sh')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('containerd image store', result.stderr)
                state = self.daemon()
                self.assertFalse(state['builds'])
                self.assertFalse(state['saved'])
                self.assertEqual(state['seed_exports'], 0)
                self.assertFalse((self.root/'docker/.build').exists())
        self.daemon(driver_status_raw='not-json')
        result = self.run_script('images/desktop/build.sh')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('containerd image store', result.stderr)
        self.assertFalse(self.daemon()['builds'])

    def test_containerd_store_resolves_config_iid_and_repeat_build_is_stable(self):
        self.daemon(containerd_store=True, vary_provenance=True)
        for _ in range(2):
            result = self.run_script('docker/build-standalone.sh')
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.daemon()['builds'], ['workspace', 'application', 'application'])

    def test_repeat_build_normalizes_created_and_filesystem_mtime(self):
        self.daemon(vary_output_mtime=True, containerd_store=True)
        for _ in range(2):
            result = self.run_script('docker/build-standalone.sh')
            self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        state = self.daemon()
        self.assertEqual(state['builds'], ['workspace', 'application', 'application'])
        for arguments in state['build_arguments']:
            self.assertIn('SOURCE_DATE_EPOCH='+str(EPOCH), arguments)
            self.assertIn('type=image,unpack=false,rewrite-timestamp=true', arguments)
        primary = 'runcove-standalone:'+VERSION
        image = json.loads(state['images'][state['tags'][primary]])
        self.assertEqual(image['created'], str(EPOCH))
        self.assertEqual(image['fixtureFileMtime'], str(EPOCH))

    def test_same_source_new_head_and_epoch_publish_new_tag_and_preserve_old(self):
        self.daemon(vary_output_mtime=True)
        result = self.run_script('docker/build-standalone.sh')
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        first = dict(self.daemon()['tags'])
        first_source = (self.root/'docker/.build/images/desktop.source').read_text()
        self.environment.update(BUILD_TEST_HEAD='1'*40, BUILD_TEST_EPOCH=str(EPOCH+1))
        result = self.run_script('docker/build-standalone.sh')
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        state = self.daemon()
        self.assertNotEqual((self.root/'docker/.build/images/desktop.source').read_text(), first_source)
        for name, identity in first.items():
            self.assertEqual(state['tags'][name], identity)
        primary = {name: iid for name, iid in state['tags'].items() if name.startswith('runcove-standalone:runcove-')}
        self.assertEqual(len(primary), 2)
        labels = [json.loads(state['images'][iid])['config']['Labels'] for iid in primary.values()]
        self.assertEqual({value['org.opencontainers.image.revision'] for value in labels}, {REVISION, '1'*40})
        self.assertEqual({value['io.runcove.source-sha256'] for value in labels}, {SOURCE})

    def test_invalid_git_epoch_has_no_build_or_cache_side_effect(self):
        for epoch in ['-1', '+123', '123.5', 'not-an-epoch', '253402300800']:
            self.environment['BUILD_TEST_EPOCH'] = epoch
            result = self.run_script('docker/build-standalone.sh')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('epoch', result.stderr)
            self.assertFalse(self.daemon()['builds'])
            self.assertFalse((self.root/'docker/.build').exists())

    def test_future_git_epoch_is_rejected_before_build_or_cache_side_effect(self):
        self.environment['BUILD_TEST_EPOCH'] = str(int(time.time())+86400)
        result = self.run_script('docker/build-standalone.sh')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('future', result.stderr)
        self.assertFalse(self.daemon()['builds'])
        self.assertFalse((self.root/'docker/.build').exists())

    def test_nested_worker_change_invalidates_workspace_cache(self):
        self.write('guest/desktop/nested/worker.cjs', 'one')
        self.build_workspace()
        self.build_workspace()
        self.assertEqual(self.daemon()['builds'], ['workspace'])
        self.write('guest/desktop/nested/worker.cjs', 'two')
        self.build_workspace()
        self.assertEqual(self.daemon()['builds'], ['workspace', 'workspace'])

    def test_failed_export_preserves_entire_previous_cache(self):
        self.build_workspace()
        names = ['images/desktop-image.lock.json'] + ['docker/.build/images/' + name for name in
                 ('desktop.tar.gz', 'desktop.reference', 'desktop.source', 'desktop.archive.sha256', 'workspace.image-id')]
        before = {name: (self.root / name).read_bytes() for name in names}
        self.write('images/desktop/Dockerfile', 'FROM fixture\n# changed\n')
        self.daemon(bad_save=True)
        result = self.run_script('images/desktop/build.sh')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual({name: (self.root / name).read_bytes() for name in names}, before)

    def test_standalone_preserves_existing_content_tag(self):
        tag = 'runcove-standalone:' + VERSION
        self.daemon(tags={tag: OLD_APP})
        result = self.run_script('docker/build-standalone.sh')
        self.assertNotEqual(result.returncode, 0, 'a content tag conflict must stop publication')
        self.assertEqual(self.daemon()['tags'][tag], OLD_APP)

    def test_server_rejects_or_refreshes_corrupted_seed_cache(self):
        self.write('docker/.build/images/default.tar.gz', 'corrupt archive')
        self.write('docker/.build/.stamp/image', '0.7.14\n')
        result = self.run_script('docker/build.sh')
        if result.returncode == 0:
            self.assertGreater(self.daemon()['seed_exports'], 0, 'corrupt cache was copied into an image')
        self.assertNotEqual((self.root / 'docker/.build/images/default.tar.gz').read_bytes(), b'corrupt archive')

    def test_server_checks_runtime_before_building_workspace(self):
        self.environment['MSB_HOME'] = str(self.root / 'missing-runtime')
        result = self.run_script('docker/build.sh')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.daemon()['builds'], [], 'prerequisite failure must have no build/cache side effects')

    def test_server_accepts_commented_runtime_manifest_and_rejects_unowned_records(self):
        manifest = self.root / 'msb-runtime.sha256'
        records = manifest.read_text()
        manifest.write_text('# Official SDK seed inputs\n# Runtime identity is separate\n\n' + records)
        result = self.run_script('docker/build.sh')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        before = self.daemon()['builds']
        for invalid in ['not-a-sha  bin/msb\n', 'f' * 64 + '  ../outside\n', records.splitlines()[1] + '\n']:
            manifest.write_text(invalid)
            result = self.run_script('docker/build.sh')
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(self.daemon()['builds'], before)

    def test_first_server_build_materializes_the_pinned_seed_without_preinstalled_cache(self):
        self.daemon(seed_present=False)
        result = self.run_script('docker/build.sh')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.daemon()['seed_pulls'], 1)
        self.assertTrue((self.root / 'docker/.build/images/default.tar.gz').is_file())

    def test_inspect_failure_cannot_overwrite_content_tag(self):
        tag = 'runcove-standalone:' + VERSION
        self.daemon(tags={tag: OLD_APP}, fail_inspect_name=tag)
        result = self.run_script('docker/build-standalone.sh')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.daemon()['tags'][tag], OLD_APP)

    def test_saved_archive_must_belong_to_built_image(self):
        self.daemon(save_other=True)
        result = self.run_script('images/desktop/build.sh')
        self.assertNotEqual(result.returncode, 0, 'valid OCI archive belongs to another image')
        self.assertFalse((self.root / 'docker/.build/images/desktop.commit.json').exists())

    def test_workspace_supports_another_output_filesystem(self):
        with tempfile.TemporaryDirectory(prefix='runcove-build-test-', dir='/dev/shm') as output:
            result = self.run_script('images/desktop/build.sh', output)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual((Path(output) / 'workspace.image-id').read_text().strip(), WORKSPACE)

    def test_docker_uses_fixed_source_context(self):
        self.daemon(mutate_before_copy=True)
        self.build_workspace()
        self.assertEqual(self.daemon()['copied_worker'], 'initial worker\n')

    def test_bad_head_has_no_build_side_effect(self):
        self.write('bin/git', '#!/bin/sh\nexit 1\n', executable=True)
        result = self.run_script('docker/build-standalone.sh')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.daemon()['builds'], [])

    def test_interrupted_publication_recovers_old_cache_before_retry(self):
        self.build_workspace()
        names = ['images/desktop-image.lock.json'] + ['docker/.build/images/' + name for name in
                 ('desktop.tar.gz', 'desktop.reference', 'desktop.source', 'desktop.archive.sha256', 'workspace.image-id', 'desktop.commit.json')]
        before = {name: (self.root / name).read_bytes() for name in names}
        self.write('images/desktop/Dockerfile', 'FROM fixture\n# changed\n')
        self.write('injection/sitecustomize.py', '''import os,signal
original=os.replace
def replace(source,destination):
    result=original(source,destination)
    if str(destination).endswith('/desktop.source'):
        os.kill(os.getpid(),signal.SIGTERM)
    return result
os.replace=replace
''')
        self.environment['PYTHONPATH'] = str(self.root / 'injection')
        result = self.run_script('images/desktop/build.sh')
        self.assertNotEqual(result.returncode, 0)
        del self.environment['PYTHONPATH']
        self.daemon(bad_save=True)
        result = self.run_script('images/desktop/build.sh')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual({name: (self.root / name).read_bytes() for name in names}, before)

    def clone_checkout(self):
        other = Path(self.temp.name) / 'project-b'
        shutil.copytree(self.root, other, ignore=shutil.ignore_patterns('.build', 'injection'))
        self.write('guest/desktop/other-id', 'other checkout')
        # The marker belongs to B, not A.
        (other / 'guest/desktop/other-id').write_text('other checkout')
        (self.root / 'guest/desktop/other-id').unlink()
        return other

    def test_two_checkouts_serialize_shared_output(self):
        output = Path(self.temp.name) / 'shared-output'
        other = self.clone_checkout()
        paused, resume = self.root / 'paused', self.root / 'resume'
        self.write('injection/sitecustomize.py', '''import os,pathlib,time
original=os.replace
def replace(source,destination):
    result=original(source,destination)
    if str(destination).endswith('/desktop.tar.gz'):
        root=pathlib.Path(os.environ['BUILD_TEST_PROJECT']); (root/'paused').touch()
        until=time.monotonic()+10
        while not (root/'resume').exists() and time.monotonic()<until: time.sleep(.01)
    return result
os.replace=replace
''')
        environment = dict(self.environment, PYTHONPATH=str(self.root / 'injection'))
        first = subprocess.Popen(['bash', str(self.root / 'images/desktop/build.sh'), str(output)], env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        second = None
        try:
            until = time.monotonic() + 10
            while not paused.exists() and first.poll() is None and time.monotonic() < until:
                time.sleep(.01)
            self.assertTrue(paused.exists())
            second = subprocess.Popen(['bash', str(other / 'images/desktop/build.sh'), str(output)], env=self.environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            time.sleep(.2)
            self.assertIsNone(second.poll(), 'B published while A held the shared output')
            resume.touch()
            for process in (first, second):
                stdout, stderr = process.communicate(timeout=15)
                self.assertEqual(process.returncode, 0, stdout + stderr)
            record = json.loads((output / 'desktop.commit.json').read_text())
            self.assertEqual(record['archive'], hashlib.sha256((output / 'desktop.tar.gz').read_bytes()).hexdigest())
            self.assertEqual(record['imageId'], OTHER)
        finally:
            resume.touch()
            for process in (first, second):
                if process is not None and process.poll() is None:
                    process.kill()
                    process.communicate(timeout=5)

    def test_other_checkout_recovers_interrupted_shared_output(self):
        output = Path(self.temp.name) / 'shared-output'
        self.assertEqual(self.run_script('images/desktop/build.sh', output).returncode, 0)
        other = self.clone_checkout()
        before = {path.name: path.read_bytes() for path in output.iterdir() if path.is_file() and path.name != '.build.lock'}
        self.write('images/desktop/Dockerfile', 'FROM fixture\n# A changed\n')
        self.write('injection/sitecustomize.py', '''import os,signal
original=os.replace
def replace(source,destination):
    result=original(source,destination)
    if str(destination).endswith('/desktop.source'): os.kill(os.getpid(),signal.SIGTERM)
    return result
os.replace=replace
''')
        self.environment['PYTHONPATH'] = str(self.root / 'injection')
        self.assertNotEqual(self.run_script('images/desktop/build.sh', output).returncode, 0)
        del self.environment['PYTHONPATH']
        self.daemon(bad_save=True)
        result = subprocess.run(['bash', str(other / 'images/desktop/build.sh'), str(output)], env=self.environment, capture_output=True, text=True, timeout=15)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual({name: (output / name).read_bytes() for name in before}, before)


class ReleaseSourceIdentity(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.normal = self.base/'ordinary/repository/agent-space'
        required = ('Cargo.toml', 'Cargo.lock', 'msb-runtime.sha256', '.dockerignore',
            'docker/Dockerfile', 'docker/docker-entrypoint.sh', 'docker/build.sh', 'docker/build_images.py',
            'docker/release_identity.py', 'docker/release-metadata.py', 'docker/build-standalone.sh',
            'docker/Dockerfile.standalone', 'docker/standalone-entrypoint.sh', 'docker/playwright-seccomp.json',
            'docker/compose.server.yaml', 'docker/compose.standalone.yaml', 'docker/verify-sqlite-link.py',
            'docker/sqlite-check.c', 'docker/apply-msb-patch.py', 'agent-space-mcp/Cargo.toml',
            'src/lib.rs', 'src/target/generated.rs')
        for name in required:
            path = self.normal/name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name+'\n')
        self.others = []
        for name in ('target', 'dist', 'node_modules'):
            path = self.base/name/'repository/agent-space'
            shutil.copytree(self.normal, path)
            self.others.append(path)
        spec = importlib.util.spec_from_file_location('real_release_identity', PROJECT/'docker/release_identity.py')
        self.identity = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.identity)

    def digest(self, project):
        self.identity.PROJECT = project
        return self.identity.source_digest()

    def test_same_source_digest_is_independent_of_checkout_ancestor_names(self):
        expected = self.digest(self.normal)
        for root in self.others:
            with self.subTest(ancestor=root.parts[-3]):
                self.assertEqual(self.digest(root), expected)

    def test_rust_mutation_is_detected_under_each_checkout_ancestor(self):
        results = []
        for root in [self.normal, *self.others]:
            before = self.digest(root)
            (root/'src/lib.rs').write_text('pub fn changed() {}\n')
            after = self.digest(root)
            self.assertNotEqual(after, before, str(root))
            results.append(after)
        self.assertEqual(len(set(results)), 1)

    def test_generated_target_directory_inside_source_stays_excluded(self):
        for root in [self.normal, *self.others]:
            before = self.digest(root)
            (root/'src/target/generated.rs').write_text('different generated output\n')
            self.assertEqual(self.digest(root), before)


if __name__ == '__main__':
    unittest.main()
