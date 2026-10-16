"""Exercise verifier negotiation over real HTTP; no Docker or credentials needed."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import threading
import unittest

from verify import Client


class ProtocolNegotiation(unittest.TestCase):
    def setUp(self):
        self.version = '2025-11-25'
        self.sse = False
        self.requests = []
        self.request_status = 200
        self.notification_status = 202
        self.notification_body = b''
        self.response_overrides = {}
        self.content_type_override = None
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                fixture.requests.append((payload, dict(self.headers)))
                if payload['method'] == 'notifications/initialized':
                    self.send_response(fixture.notification_status)
                    self.send_header('Content-Length', str(len(fixture.notification_body)))
                    self.end_headers()
                    self.wfile.write(fixture.notification_body)
                    return
                result = ({'protocolVersion': fixture.version, 'capabilities': {'tools': {}}}
                          if payload['method'] == 'initialize' else {'tools': []})
                body = json.dumps({'jsonrpc': '2.0', 'id': payload['id'], 'result': result,
                                   **fixture.response_overrides})
                if fixture.sse:
                    body = 'id: priming\ndata:\n\nevent: message\ndata: ' + body + '\n\n'
                body = body.encode()
                self.send_response(fixture.request_status)
                content_type = ('text/event-stream' if fixture.sse else 'application/json')
                self.send_header('Content-Type', fixture.content_type_override if fixture.content_type_override is not None else content_type)
                self.send_header('Mcp-Session-Id', 'negotiated-session')
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.worker = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.worker.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.worker.join()

    def client(self, **options):
        return Client(self.server.server_port, token='fixture', **options)

    def assert_followup_headers(self, version):
        self.assertGreaterEqual(len(self.requests), 3)
        self.assertNotIn('MCP-Protocol-Version', self.requests[0][1])
        for payload, headers in self.requests[1:]:
            self.assertEqual(headers['MCP-Protocol-Version'], version, payload)
            self.assertEqual(headers['Mcp-Session-Id'], 'negotiated-session', payload)

    def test_current_image_defaults_to_latest(self):
        client = self.client()
        client.request('tools/list')
        self.assertEqual(self.requests[0][0]['params']['protocolVersion'], '2025-11-25')
        self.assert_followup_headers('2025-11-25')

    def test_three_versions_support_json_and_sse(self):
        for sse in (False, True):
            for version in ('2025-11-25', '2025-06-18', '2025-03-26'):
                with self.subTest(version=version, sse=sse):
                    self.version, self.sse, self.requests = version, sse, []
                    self.content_type_override = ('text/event-stream' if sse else 'application/json') + '; charset=utf-8'
                    self.client(protocol_version=version).request('tools/list')
                    self.assertEqual(self.requests[0][0]['params']['protocolVersion'], version)
                    self.assert_followup_headers(version)

    def test_unexpected_or_missing_versions_stop_before_initialized(self):
        for version in ('2024-11-05', '2025-06-18', '1900-01-01', None):
            with self.subTest(version=version):
                self.version, self.requests = version, []
                with self.assertRaises(AssertionError):
                    self.client()
                self.assertEqual(len(self.requests), 1)

    def test_historical_image_requires_explicit_expectation(self):
        self.version = '2024-11-05'
        self.client(protocol_version='2025-03-26', expected_version='2024-11-05').request('tools/list')
        self.assert_followup_headers('2024-11-05')

    def test_initialized_notification_requires_202_and_empty_body(self):
        for status, body in ((200, b''), (202, b'not-empty')):
            with self.subTest(status=status, body=body):
                self.notification_status, self.notification_body = status, body
                with self.assertRaises(AssertionError):
                    self.client()

    def test_json_response_requires_corresponding_rpc_envelope(self):
        for overrides in ({'id': 99}, {'id': None}, {'id': True},
                          {'jsonrpc': '1.0'}, {'jsonrpc': None}):
            with self.subTest(overrides=overrides):
                self.response_overrides = overrides
                with self.assertRaises(AssertionError):
                    self.client()

    def test_rpc_request_requires_200_response(self):
        self.request_status = 202
        with self.assertRaises(AssertionError):
            self.client()

    def test_response_requires_a_supported_content_type(self):
        for sse, content_type in ((False, 'text/plain'), (False, ''),
                                  (False, 'application/json-invalid'),
                                  (True, 'text/event-stream-invalid')):
            with self.subTest(sse=sse, content_type=content_type):
                self.sse, self.content_type_override = sse, content_type
                with self.assertRaises(AssertionError):
                    self.client()


if __name__ == '__main__':
    unittest.main()
