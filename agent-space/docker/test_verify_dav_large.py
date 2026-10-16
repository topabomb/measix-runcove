"""Failure-path tests; no Docker daemon or credentials required."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('verify_dav_large', Path(__file__).with_name('verify-dav-large.py'))
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)


class EvidenceFailures(unittest.TestCase):
    def test_setup_and_cleanup_failure_replace_stale_success(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / 'result.json'
            report.write_text('{"passed": true}')
            calls = []

            def docker(*args):
                calls.append(args[:2])
                if args[:2] == ('volume', 'create'):
                    return ''
                raise RuntimeError('injected failure')

            with patch.object(verify, 'docker', docker), patch.dict(os.environ, {'AGENT_SPACE_ADMIN_TOKEN': 'original'}), patch('sys.argv', ['verify', '--image', 'fixture', '--evidence', str(report)]):
                with self.assertRaises(RuntimeError):
                    verify.main()
                self.assertEqual(os.environ['AGENT_SPACE_ADMIN_TOKEN'], 'original')
            evidence = json.loads(report.read_text())
            self.assertFalse(evidence['passed'])
            self.assertEqual(evidence['cleanup_errors'], ['fixture volume: RuntimeError'])
            self.assertIn(('volume', 'rm'), calls)
            self.assertNotIn(('rm', '-f'), calls)

    def test_existing_container_inspection_failure_is_recorded(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / 'result.json'
            report.write_text('{"passed": true}')
            with patch.object(verify, 'docker', side_effect=RuntimeError('inspect failed')) as docker, patch('sys.argv', ['verify', '--container', 'existing', '--evidence', str(report)]):
                with self.assertRaisesRegex(RuntimeError, 'inspect failed'):
                    verify.main()
            self.assertFalse(json.loads(report.read_text())['passed'])
            docker.assert_called_once_with('inspect', 'existing')

    def test_failed_fixture_inspection_removes_anonymous_volumes(self):
        # The image declares a log VOLUME in addition to our named data volume.
        # A failure after launch must collect both without touching external fixtures.
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / 'result.json'
            calls = []

            def docker(*args):
                calls.append(args)
                if args[:2] == ('image', 'inspect'):
                    return 'sha256:fixture'
                if args[0] == 'inspect':
                    raise RuntimeError('fixture inspection failed')
                return ''

            with patch.object(verify, 'docker', docker), patch.dict(os.environ, {'AGENT_SPACE_ADMIN_TOKEN': 'original'}), patch('sys.argv', ['verify', '--image', 'fixture', '--evidence', str(report)]):
                with self.assertRaisesRegex(RuntimeError, 'fixture inspection failed'):
                    verify.main()
                self.assertEqual(os.environ['AGENT_SPACE_ADMIN_TOKEN'], 'original')
            evidence = json.loads(report.read_text())
            self.assertFalse(evidence['passed'])
            self.assertTrue(evidence['cleanup_passed'])
            self.assertTrue(any(args[:3] == ('rm', '-f', '-v') for args in calls))
            self.assertTrue(any(args[:2] == ('volume', 'rm') for args in calls))


if __name__ == '__main__':
    unittest.main()
