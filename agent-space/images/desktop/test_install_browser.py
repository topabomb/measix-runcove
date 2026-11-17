import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name('install_browser.py')


class BrowserSeed(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location('install_browser', SCRIPT)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.archive = self.root/'browser.zip'
        self.archive.write_bytes(b'fixed browser archive fixture')
        self.lock = self.root/'lock.json'
        self.data = {'schemaVersion': 1, 'browser': 'chromium', 'playwrightVersion': '1.63.0',
                     'revision': '1243', 'browserVersion': '153.0.8010.12',
                     'url': 'https://cdn.playwright.dev/builds/cft/153.0.8010.12/linux64/chrome-linux64.zip',
                     'sha256': hashlib.sha256(self.archive.read_bytes()).hexdigest()}
        self.lock.write_text(json.dumps(self.data))
        self.core = self.root/'node_modules/playwright-core'
        self.core.mkdir(parents=True)
        (self.core/'package.json').write_text(json.dumps({'version': '1.63.0'}))
        self.browsers = {'browsers': [{'name': 'chromium', 'revision': '1243', 'browserVersion': '153.0.8010.12'}]}
        (self.core/'browsers.json').write_text(json.dumps(self.browsers))
        self.cli = self.root/'cli.py'
        self.cli.write_text('import sys;sys.exit(0)\n')
        self.real_popen, self.children = subprocess.Popen, []

    def spawn(self, *args, **kwargs):
        process = self.real_popen(*args, **kwargs)
        self.children.append(process)
        return process

    def install(self, **kwargs):
        with patch.object(self.module.subprocess, 'Popen', self.spawn):
            return self.module.install(self.lock, self.archive, self.root, node=sys.executable, cli=self.cli, **kwargs)

    def assert_reaped(self):
        for process in self.children:
            self.assertIsNotNone(process.returncode)
            with self.assertRaises(ChildProcessError):
                os.waitpid(process.pid, os.WNOHANG)

    def test_official_cli_fetches_only_exact_archive_from_private_chromium_host(self):
        self.cli.write_text('''import os,sys,urllib.request,urllib.error
assert sys.argv[1:]==['install','--with-deps','--no-shell','chromium']
assert 'PLAYWRIGHT_DOWNLOAD_HOST' not in os.environ
assert 'PLAYWRIGHT_FFMPEG_DOWNLOAD_HOST' not in os.environ
assert '127.0.0.1' in os.environ.get('NO_PROXY','').split(',')
host=os.environ['PLAYWRIGHT_CHROMIUM_DOWNLOAD_HOST']
assert host.startswith('http://127.0.0.1:')
path='/builds/cft/153.0.8010.12/linux64/chrome-linux64.zip'
for suffix in ['/etc/passwd',path+'?other=1','/']:
 try:urllib.request.urlopen(host+suffix)
 except urllib.error.HTTPError as error:assert error.code==404
 else:raise AssertionError('unexpected file access')
request=urllib.request.Request(host+path,method='HEAD')
try:urllib.request.urlopen(request)
except urllib.error.HTTPError as error:assert error.code==405
else:raise AssertionError('unexpected method')
with urllib.request.urlopen(host+path) as response:
 assert response.read()==b'fixed browser archive fixture'
''')
        with patch.dict(os.environ, {'PLAYWRIGHT_DOWNLOAD_HOST': 'https://must-not-use.invalid',
                                     'PLAYWRIGHT_FFMPEG_DOWNLOAD_HOST': 'https://must-not-use.invalid',
                                     'NO_PROXY': '', 'no_proxy': '', 'HTTP_PROXY': 'http://127.0.0.1:1',
                                     'http_proxy': 'http://127.0.0.1:1'}):
            self.install()
            self.assertEqual(os.environ['PLAYWRIGHT_DOWNLOAD_HOST'], 'https://must-not-use.invalid')
        self.assertEqual(len(self.children), 2)
        self.assert_reaped()

    def test_archive_or_package_identity_mismatch_fails_before_starting_children(self):
        for field, wrong in [('revision', '1244'), ('browserVersion', '153.0.8010.13')]:
            with self.subTest(field=field):
                changed = json.loads(json.dumps(self.browsers))
                changed['browsers'][0][field] = wrong
                (self.core/'browsers.json').write_text(json.dumps(changed))
                with self.assertRaises(ValueError):
                    self.install()
                self.assertFalse(self.children)
        (self.core/'browsers.json').write_text(json.dumps(self.browsers))
        self.archive.write_bytes(b'corrupted')
        with self.assertRaises(ValueError):
            self.install()
        self.assertFalse(self.children)

    def test_cli_failure_or_spawn_error_reaps_seed_server(self):
        self.cli.write_text('import sys;sys.exit(7)\n')
        with self.assertRaises(subprocess.CalledProcessError):
            self.install()
        self.assert_reaped()
        self.children.clear()
        with patch.object(self.module.subprocess, 'Popen', self.spawn), self.assertRaises(FileNotFoundError):
            self.module.install(self.lock, self.archive, self.root, node='/no-such-node', cli=self.cli)
        self.assertEqual(len(self.children), 1)
        self.assert_reaped()

    def test_seed_startup_failure_and_timeout_reap_the_spawned_process(self):
        for program in ['import sys;sys.exit(9)', 'import time;time.sleep(60)']:
            self.children.clear()
            def broken(*args, **kwargs):
                return self.spawn([sys.executable, '-c', program], **kwargs)
            with patch.object(self.module.subprocess, 'Popen', broken), self.assertRaises(RuntimeError):
                self.module.install(self.lock, self.archive, self.root, node=sys.executable, cli=self.cli,
                                    startup_timeout=.1)
            self.assertEqual(len(self.children), 1)
            self.assert_reaped()

    def test_sigterm_reaps_seed_and_cli_before_installer_exits(self):
        record = self.root/'ready.json'
        self.cli.write_text('''import json,os,pathlib,time
pathlib.Path(os.environ['BROWSER_SEED_TEST_RECORD']).write_text(json.dumps({'cli':os.getpid(),'owner':os.getppid()}))
time.sleep(60)
''')
        environment = dict(os.environ, BROWSER_SEED_TEST_RECORD=str(record))
        owner = self.real_popen([sys.executable, '-I', str(SCRIPT), 'install', '--lock', str(self.lock),
                                '--archive', str(self.archive), '--root', str(self.root), '--node', sys.executable,
                                '--cli', str(self.cli)], env=environment)
        descriptors = []
        try:
            deadline = time.monotonic()+3
            while not record.exists():
                self.assertLess(time.monotonic(), deadline, 'fake installer did not start')
                time.sleep(.01)
            children = Path(f'/proc/{owner.pid}/task/{owner.pid}/children').read_text().split()
            self.assertEqual(len(children), 2)
            descriptors = [os.pidfd_open(int(pid)) for pid in children]
            owner.send_signal(signal.SIGTERM)
            self.assertEqual(owner.wait(timeout=5), 143)
            for pid in children:
                self.assertFalse(Path('/proc/'+pid).exists(), 'child survived or remained a zombie')
        finally:
            if owner.poll() is None:
                owner.kill(); owner.wait()
            for descriptor in descriptors:
                try:signal.pidfd_send_signal(descriptor, signal.SIGKILL)
                except ProcessLookupError:pass
                os.close(descriptor)


if __name__ == '__main__':
    unittest.main()
