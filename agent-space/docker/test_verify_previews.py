import hashlib
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from PIL import Image

spec = importlib.util.spec_from_file_location('verify_previews', Path(__file__).with_name('verify-previews.py'))
verifier = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verifier)


class PreviewEvidenceValidation(unittest.TestCase):
    def test_request_body_guard_checks_raw_head_wire_and_get_problem(self):
        import json
        body = json.dumps({'status': 400, 'code': 'invalid_preview_request'}).encode()
        headers = b'HTTP/1.1 400 Bad Request\r\nContent-Type: application/problem+json\r\nCache-Control: no-store\r\nContent-Length: '+str(len(body)).encode()+b'\r\n\r\n'
        verifier.inspect_body_guard_wire(headers, 'HEAD')
        verifier.inspect_body_guard_wire(headers+body, 'GET')
        with self.assertRaises(AssertionError):
            verifier.inspect_body_guard_wire(headers+body, 'HEAD')
        with self.assertRaises(AssertionError):
            verifier.inspect_body_guard_wire(headers.replace(b'400 Bad Request', b'200 OK'), 'HEAD')

    def test_problem_response_requires_mime_and_typed_status_code(self):
        import json
        result = (503, {'content-type': 'application/problem+json', 'cache-control': 'no-store'},
                  json.dumps({'status': 503, 'code': 'preview_unavailable'}).encode())
        verifier.inspect_problem(result, 503, 'preview_unavailable')
        with self.assertRaises(AssertionError):
            verifier.inspect_problem((503, {'content-type': 'text/plain'}, b'unavailable'), 503, 'preview_unavailable')
        with self.assertRaises(AssertionError):
            verifier.inspect_problem(result, 503, 'different_code')

    def test_worker_completion_requires_nonzero_unskipped_suite(self):
        self.assertEqual(verifier.worker_test_count('Ran 5 tests in 1.2s\n\nOK\n'), 5)
        for log in ['Ran 0 tests in 0s\nOK\n', 'Ran 5 tests in 1s\nFAILED (failures=1)\n',
                    'Ran 5 tests in 1s\nOK (skipped=1)\n']:
            with self.assertRaises(AssertionError):
                verifier.worker_test_count(log)

    def test_existing_evidence_is_rejected_before_docker_access(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'result.json'
            path.write_text('original evidence\n')
            result = subprocess.run([sys.executable, str(Path(__file__).with_name('verify-previews.py')),
                '--image', 'never-contact-daemon', '--target', 'standalone', '--expected-source', '0'*64,
                '--expected-helper-build-id', '0'*64, '--evidence', str(path)], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('FileExistsError', result.stderr)
            self.assertEqual(path.read_text(), 'original evidence\n')

    def image(self, size=(32, 16)):
        stream = io.BytesIO()
        Image.new('RGBA', size, (240, 10, 10, 90)).save(stream, 'WEBP')
        data = stream.getvalue()
        headers = {'content-type': 'image/webp', 'preview-source-etag': '"source"',
                   'etag': '"'+hashlib.sha256(data).hexdigest()+'"', 'content-length': str(len(data)),
                   'cache-control': 'no-store', 'x-content-type-options': 'nosniff',
                   'content-disposition': 'inline; filename="thumbnail.webp"'}
        return headers, data

    def test_actual_webp_identity_dimensions_and_alpha_are_accepted(self):
        headers, data = self.image()
        image = verifier.inspect_preview(headers, data, '"source"', 128)
        self.assertEqual(image.size, (32, 16))
        self.assertEqual(image.getpixel((0, 0))[3], 90)

    def test_invalid_metadata_corrupt_body_and_oversized_dimensions_fail_evidence(self):
        headers, data = self.image()
        for name, wrong in [('etag', '"wrong"'), ('content-length', '1'),
                            ('content-type', 'text/html'), ('preview-source-etag', '"other"')]:
            with self.subTest(name=name), self.assertRaises(AssertionError):
                verifier.inspect_preview({**headers, name: wrong}, data, '"source"', 128)
        with self.assertRaises(AssertionError):
            verifier.inspect_preview(headers, data[:-1] + b'x', '"source"', 128)
        headers, data = self.image((129, 64))
        with self.assertRaises(AssertionError):
            verifier.inspect_preview(headers, data, '"source"', 128)


class AmbiguousDockerCleanup(unittest.TestCase):
    def setUp(self):
        self.containers, self.volumes = {}, {}
        self.fail = None
        self.removed = []

    def command(self, *args, timeout=180):
        import json
        def committed():
            if self.fail == args[:2] or self.fail == args[:1]:
                raise TimeoutError('Docker reply lost after daemon commit')
        if args[:2] == ('volume', 'create'):
            label = args[args.index('--label') + 1].split('=', 1)
            self.volumes[args[-1]] = {'Labels': dict([label])}
            committed()
            return args[-1]
        if args[:1] == ('create',):
            label = args[args.index('--label') + 1].split('=', 1)
            name = args[args.index('--name') + 1]
            self.containers[name] = {'Config': {'Labels': dict([label])}, 'State': {'Running': False, 'ExitCode': 0, 'OOMKilled': False}}
            committed()
            return name
        if args[:2] in [('container', 'inspect'), ('volume', 'inspect')]:
            values = self.containers if args[0] == 'container' else self.volumes
            if args[2] not in values:
                raise RuntimeError('No such '+args[0]+': '+args[2])
            return json.dumps([values[args[2]]])
        if args[:1] == ('start',):
            self.containers[args[-1]]['State']['Running'] = True
            committed()
            return ''
        if args[:1] == ('stop',):
            self.containers[args[-1]]['State']['Running'] = False
            return ''
        if args[:1] == ('rm',):
            self.removed.append(args[-1]); self.containers.pop(args[-1]); return ''
        if args[:2] == ('volume', 'rm'):
            self.removed.append(args[-1]); self.volumes.pop(args[-1]); return ''
        raise AssertionError(args)

    def test_volume_create_reply_loss_still_cleans_owned_committed_volume(self):
        owner = verifier.FixtureOwner(self.command, 'run-identity')
        self.fail = ('volume', 'create')
        with self.assertRaises(TimeoutError):
            owner.volume('disposable-volume')
        self.assertEqual(owner.cleanup(), [])
        self.assertFalse(self.volumes)

    def test_create_reply_loss_and_init_start_timeout_still_clean_owned_containers(self):
        for failure in [('create',), ('start',)]:
            with self.subTest(failure=failure):
                owner = verifier.FixtureOwner(self.command, 'run-identity')
                self.fail = failure
                with self.assertRaises(TimeoutError):
                    owner.container('disposable-init', ['fixture-image', 'init-state'])
                    self.command('start', '-a', 'disposable-init', timeout=1)
                self.assertEqual(owner.cleanup(), [])
                self.assertFalse(self.containers)

    def test_name_collision_never_removes_unowned_resource(self):
        owner = verifier.FixtureOwner(self.command, 'run-identity')
        owner.containers.append('someone-else')
        self.containers['someone-else'] = {'Config': {'Labels': {verifier.OWNER_LABEL: 'another-run'}},
                                          'State': {'Running': True, 'ExitCode': 0, 'OOMKilled': False}}
        self.assertTrue(owner.cleanup())
        self.assertNotIn('someone-else', self.removed)

    def test_same_identity_zombie_is_not_reaped_but_pid_reuse_is(self):
        self.assertTrue(verifier.original_process_present('42 (python) Z '+'0 '*18+'123 0', '123'))
        self.assertFalse(verifier.original_process_present('42 (python) S '+'0 '*18+'124 0', '123'))


if __name__ == '__main__':
    unittest.main()
