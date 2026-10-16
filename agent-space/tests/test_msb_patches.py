"""Audited MSB patch provenance, exact application, and upgrade refusal."""
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
PATCHES = ROOT / 'docker/patches'
spec = importlib.util.spec_from_file_location('msb_patch', ROOT / 'docker/apply-msb-patch.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
MANIFEST = json.loads((PATCHES / 'manifest.json').read_text())


class AuditedPatches(unittest.TestCase):
    def source(self, directory):
        source = Path(directory)
        (source / 'Cargo.toml').write_text('[workspace.package]\nversion = "0.7.2"\n')
        for patch in MANIFEST['patches']:
            for entry in patch['files']:
                official = Path(entry['file'])
                crate = 'microsandbox' if official.parts[0] == 'sdk' else 'microsandbox-runtime'
                target = source / official
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / 'vendor' / crate / Path(*official.parts[2:]), target)
            subprocess.run(['patch', '--batch', '--fuzz=0', '-R', '-p1', '-i', str(PATCHES / patch['patch'])], cwd=source, check=True, stdout=subprocess.DEVNULL)
        return source

    def test_exact_input_and_matching_vendor(self):
        module.verify_vendor(ROOT / 'vendor', PATCHES)
        with tempfile.TemporaryDirectory() as directory:
            source = self.source(directory)
            module.apply(source, PATCHES)
            for patch in MANIFEST['patches']:
                for entry in patch['files']:
                    self.assertEqual(module.digest(source / entry['file']), entry['patched_sha256'])

    def test_reapplication_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            source = self.source(directory)
            module.apply(source, PATCHES)
            with self.assertRaises(SystemExit):
                module.apply(source, PATCHES)

    def test_modified_input_fails_before_any_patch(self):
        with tempfile.TemporaryDirectory() as directory:
            source = self.source(directory)
            target = source / MANIFEST['patches'][-1]['files'][-1]['file']
            target.write_bytes(target.read_bytes() + b'\n// unexpected upstream change\n')
            first = source / MANIFEST['patches'][0]['files'][0]['file']
            before = first.read_bytes()
            with self.assertRaises(SystemExit):
                module.apply(source, PATCHES)
            self.assertEqual(first.read_bytes(), before)

    def test_version_change_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            source = self.source(directory)
            (source / 'Cargo.toml').write_text('[workspace.package]\nversion = "0.7.8"\n')
            with self.assertRaises(SystemExit):
                module.apply(source, PATCHES)


if __name__ == '__main__':
    unittest.main()
