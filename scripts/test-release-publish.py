#!/usr/bin/env python3
"""Exercise publication against separate operator and public HTTP servers."""
import argparse
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('release', Path(__file__).with_name('release.py'))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
PAYLOAD = b'public release payload'


class PublicationTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.public_requests = []
        self.operator_gets = []
        self.public_status = 200
        self.public_payload = PAYLOAD
        owner = self

        class Public(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_GET(self):
                owner.public_requests.append((self.path, self.headers.get('x-haarness-internal-token')))
                self.send_response(owner.public_status)
                self.end_headers()
                self.wfile.write(owner.public_payload)

        class Operator(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_POST(self):
                self.rfile.read(int(self.headers['content-length']))
                if self.path.startswith('/internal/releases/artifacts?'):
                    value = {'id': 'artifact-id'}
                else:
                    value = {'id': 'release-id', 'release': {'manifest': {'products': {'cli': {'artifacts': [{
                        'file': 'haarness', 'url': owner.artifact_url,
                        'size': len(PAYLOAD), 'digest': {'sha256': hashlib.sha256(PAYLOAD).hexdigest()},
                    }]}}}}}
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps(value).encode())
            def do_GET(self):
                # The old implementation incorrectly succeeds against this copy.
                owner.operator_gets.append(self.path)
                self.send_response(200)
                self.end_headers()
                self.wfile.write(PAYLOAD)

        def start(handler):
            server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            def stop():
                server.shutdown()
                server.server_close()
                thread.join()
            self.addCleanup(stop)
            return f'http://127.0.0.1:{server.server_port}'

        public = start(Public)
        operator = start(Operator)
        self.artifact_url = public + '/download/haarness?signature=abc%2Bdef'
        (self.root / 'haarness').write_bytes(PAYLOAD)
        index = {'version': '1.0.0', 'sourceRevision': 'a' * 40, 'releasedAt': '2026-10-02T00:00:00Z', 'artifacts': [{
            'file': 'haarness', 'product': 'cli', 'kind': 'binary', 'os': 'macos', 'arch': 'arm64',
            'size': len(PAYLOAD), 'sha256': hashlib.sha256(PAYLOAD).hexdigest(),
        }]}
        (self.root / 'index.json').write_text(json.dumps(index))
        self.args = argparse.Namespace(index=[str(self.root / 'index.json')], url=operator,
            channel='preview', allow_dirty=False, notes_url=None, receipt=str(self.root / 'receipt.json'))
        env = patch.dict(os.environ, HAARNESS_RELEASE_TOKEN='test-operator-secret')
        env.start()
        self.addCleanup(env.stop)

    def test_downloads_exact_public_url_without_operator_credentials(self):
        release.publish(self.args)
        self.assertEqual(self.public_requests, [('/download/haarness?signature=abc%2Bdef', None)])
        self.assertEqual(self.operator_gets, [])

    def test_broken_public_url_fails_despite_working_operator_copy(self):
        self.public_status = 404
        with self.assertRaisesRegex(RuntimeError, 'Public download failed: 404'):
            release.publish(self.args)
        self.assertEqual(self.operator_gets, [])
        self.assertEqual(json.loads(Path(self.args.receipt).read_text())['id'], 'release-id')

    def test_corrupt_public_download_fails(self):
        self.public_payload = b'corrupted payload'
        with self.assertRaisesRegex(ValueError, 'integrity failure'):
            release.publish(self.args)

    def test_rejects_insecure_remote_origin(self):
        self.artifact_url = 'http://example.com/download'
        with self.assertRaisesRegex(ValueError, 'HTTPS'):
            release.publish(self.args)
        self.assertEqual(self.operator_gets, [])


if __name__ == '__main__':
    unittest.main()
