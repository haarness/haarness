#!/usr/bin/env python3
"""Exercise publication against separate operator and public HTTP servers."""
import argparse
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import platform
import subprocess
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
        self.operator_posts = []
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
                owner.operator_posts.append(self.path)
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
            channel='preview', allow_dirty=False, allow_unsigned=False, notes_url=None,
            receipt=str(self.root / 'receipt.json'))
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

    def desktop(self, **fields):
        index = json.loads((self.root / 'index.json').read_text())
        index['artifacts'][0].update(product='desktop', kind='dmg', **fields)
        (self.root / 'index.json').write_text(json.dumps(index))

    def test_publisher_off_mac_refuses_mac_desktop_without_gatekeeper_record(self):
        self.desktop()
        with patch.object(release.platform, 'system', return_value='Linux'):
            with self.assertRaisesRegex(ValueError, 'no Gatekeeper record'):
                release.publish(self.args)
        self.assertEqual(self.operator_posts, [])

    def test_publisher_off_mac_accepts_collect_record(self):
        self.desktop(gatekeeper={'signer': 'Developer ID Application: Test (TEAM)', 'source': 'Notarized Developer ID'})
        with patch.object(release.platform, 'system', return_value='Linux'):
            release.publish(self.args)
        self.assertEqual(len(self.operator_posts), 2)

    def test_mac_publisher_checks_uploaded_bytes_despite_record(self):
        self.desktop(gatekeeper={'signer': 'Developer ID Application: Test (TEAM)', 'source': 'Notarized Developer ID'})
        with patch.object(release.platform, 'system', return_value='Darwin'), \
                patch.object(release, 'gatekeeper', side_effect=ValueError('Gatekeeper rejects it')) as check:
            with self.assertRaisesRegex(ValueError, 'Gatekeeper rejects it'):
                release.publish(self.args)
        check.assert_called_once_with(self.root / 'haarness')
        self.assertEqual(self.operator_posts, [])

    def test_stable_refuses_unsigned_mac_desktop(self):
        self.args.channel, self.args.allow_unsigned = 'stable', True
        with self.assertRaisesRegex(ValueError, 'Stable never ships'):
            release.publish(self.args)
        self.assertEqual(self.operator_posts, [])


@unittest.skipUnless(platform.system() == 'Darwin', 'codesign and Gatekeeper are macOS-only')
class GatekeeperTest(unittest.TestCase):
    def test_rejects_ad_hoc_signed_app(self):
        # How 0.1.1 and 0.1.2 shipped: a browser download of it was "damaged" and went to the Trash.
        with tempfile.TemporaryDirectory() as d:
            app = Path(d) / 'Haarness.app'
            (app / 'Contents/MacOS').mkdir(parents=True)
            (app / 'Contents/Info.plist').write_text('<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>'
                '<key>CFBundleExecutable</key><string>Haarness</string>'
                '<key>CFBundleIdentifier</key><string>dev.haarness.test</string></dict></plist>')
            (app / 'Contents/MacOS/Haarness').write_bytes(Path('/usr/bin/true').read_bytes())
            (app / 'Contents/MacOS/Haarness').chmod(0o755)
            subprocess.run(['codesign', '--force', '--sign', '-', str(app)], check=True, capture_output=True)
            package = Path(d) / 'Haarness-1.0.0-mac-arm64.zip'
            subprocess.run(['ditto', '-c', '-k', '--keepParent', str(app), str(package)], check=True)
            with self.assertRaisesRegex(ValueError, 'signed ad-hoc, not with a Developer ID'):
                release.gatekeeper(package)


if __name__ == '__main__':
    unittest.main()
