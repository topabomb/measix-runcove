"""Execute the default-image bootstrap with an isolated image-cache adapter."""
import gzip
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


class DefaultImageBootstrap(unittest.TestCase):
    def test_fixed_digest_archive_provides_default_alias_once_on_a_clean_cache(self):
        script = Path(__file__).with_name('docker-entrypoint.sh').read_text()
        bootstrap = script.split('# 5. Pre-load bundled sandbox image', 1)[1].split('\n', 1)[1].split('# The desktop preset', 1)[0]
        default = 'nikolaik/python-nodejs:python3.12-nodejs22-slim'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'bin').mkdir()
            state = root / 'cache.json'
            state.write_text(json.dumps({'images': [], 'loads': 0}))
            raw = io.BytesIO()
            with tarfile.open(fileobj=raw, mode='w') as archive:
                data = json.dumps([{'RepoTags': ['nikolaik/python-nodejs@sha256:' + 'f' * 64]}]).encode()
                entry = tarfile.TarInfo('manifest.json')
                entry.size = len(data)
                archive.addfile(entry, io.BytesIO(data))
            source = root / 'seed.tar.gz'
            source.write_bytes(gzip.compress(raw.getvalue()))
            command = root / 'bin/msb'
            command.write_text('''#!/usr/bin/env python3
import json,os,pathlib,sys,tarfile
state=pathlib.Path(os.environ['BOOTSTRAP_TEST_CACHE']); data=json.loads(state.read_text()); args=sys.argv[1:]
if args[:2]==['image','list']: print('\\n'.join(data['images'])); raise SystemExit(0)
if args[:2]==['image','inspect']: raise SystemExit(0 if args[2] in data['images'] else 1)
if args[:2]==['image','load']:
 with tarfile.open(args[args.index('-i')+1]) as archive: tags=json.load(archive.extractfile('manifest.json'))[0]['RepoTags']
 data['images']+=([args[args.index('--tag')+1]] if '--tag' in args else tags); data['loads']+=1; state.write_text(json.dumps(data)); raise SystemExit(0)
raise SystemExit(1)
''')
            command.chmod(0o755)
            bootstrap = bootstrap.replace('/opt/msb-images/default.tar.gz', str(source)).replace('/tmp/default.tar', str(root / 'load.tar'))
            environment = dict(os.environ, PATH=str(root / 'bin') + os.pathsep + os.environ['PATH'], BOOTSTRAP_TEST_CACHE=str(state))
            for _ in range(2):
                subprocess.run(['bash', '-ec', bootstrap], env=environment, capture_output=True, check=True)
                self.assertIn(default, json.loads(state.read_text())['images'])
            self.assertEqual(json.loads(state.read_text())['loads'], 1)


if __name__ == '__main__':
    unittest.main()
