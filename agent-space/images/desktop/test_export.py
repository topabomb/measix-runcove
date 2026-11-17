import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('export', Path(__file__).with_name('export.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class OCIIdentity(unittest.TestCase):
    def test_config_manifest_and_index_identities_are_verified_on_the_selected_chain(self):
        with tempfile.TemporaryDirectory() as directory:
            root, blobs = Path(directory), {}

            def blob(value, media):
                raw = json.dumps(value, separators=(',', ':')).encode()
                digest = 'sha256:' + hashlib.sha256(raw).hexdigest()
                blobs['blobs/sha256/' + digest[7:]] = raw
                return {'mediaType': media, 'size': len(raw), 'digest': digest}

            config = blob({'os': 'linux', 'architecture': 'amd64'}, 'application/vnd.oci.image.config.v1+json')
            manifest = blob({'schemaVersion': 2, 'config': config, 'layers': []}, 'application/vnd.oci.image.manifest.v1+json')
            sibling = blob({'schemaVersion': 2, 'manifests': []}, 'application/vnd.oci.image.index.v1+json')
            index = blob({'schemaVersion': 2, 'manifests': [manifest]}, 'application/vnd.oci.image.index.v1+json')
            blobs['index.json'] = json.dumps({'schemaVersion': 2, 'manifests': [index, sibling]}).encode()
            source = root / 'image.tar'
            with tarfile.open(source, 'w') as archive:
                for name, raw in blobs.items():
                    item = tarfile.TarInfo(name)
                    item.size = len(raw)
                    archive.addfile(item, io.BytesIO(raw))
            for identity in (config, manifest, index):
                reference = module.export(source, root / 'result.tar', root / 'lock.json', identity['digest'])
                self.assertTrue(reference.endswith(manifest['digest']))
            with self.assertRaisesRegex(ValueError, 'built image'):
                module.export(source, root / 'result.tar', root / 'lock.json', sibling['digest'])


if __name__ == '__main__':
    unittest.main()
