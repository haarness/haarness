#!/usr/bin/env python3
"""Exercise publication against separate operator and public HTTP servers, and collect's record of Haarness Router."""
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
        self.current = None
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
                        'id': 'artifact-id', 'file': 'haarness', 'url': owner.artifact_url,
                        'size': len(PAYLOAD), 'digest': {'sha256': hashlib.sha256(PAYLOAD).hexdigest()},
                    }]}}}}}
                self.send_response(200)
                self.end_headers()
                self.wfile.write(json.dumps(value).encode())
            def do_GET(self):
                owner.operator_gets.append(self.path)
                # The channel's current release, which files without a CLI may only join.
                if self.path.startswith('/api/v1/releases?'):
                    self.send_response(200 if owner.current else 404)
                    self.end_headers()
                    self.wfile.write(json.dumps({'manifest': owner.current} if owner.current else {'code': 'not_found'}).encode())
                    return
                # The old implementation incorrectly succeeds against this copy.
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
        (self.root / 'haarness-router-macos-arm64').write_bytes(b'router')
        index = {'version': '1.0.0', 'sourceRevision': 'a' * 40, 'releasedAt': '2026-10-02T00:00:00Z', 'artifacts': [{
            'file': 'haarness', 'product': 'cli', 'kind': 'binary', 'os': 'macos', 'arch': 'arm64',
            'size': len(PAYLOAD), 'sha256': hashlib.sha256(PAYLOAD).hexdigest(),
        }, {
            'file': 'haarness-router-macos-arm64', 'product': 'router', 'kind': 'binary', 'os': 'macos', 'arch': 'arm64',
            'size': 6, 'sha256': hashlib.sha256(b'router').hexdigest(), 'sourceRevision': 'b' * 40,
        }]}
        (self.root / 'index.json').write_text(json.dumps(index))
        self.args = argparse.Namespace(index=[str(self.root / 'index.json')], url=operator,
            channel='preview', allow_dirty=False, allow_unsigned=False, allow_unsigned_windows=False, notes_url=None,
            receipt=str(self.root / 'receipt.json'), without_router=False)
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
        # A Mac Desktop package beside the CLI and the router (its bytes are the CLI's file here).
        index = json.loads((self.root / 'index.json').read_text())
        index['artifacts'].append({**index['artifacts'][0], 'product': 'desktop', 'kind': 'dmg', **fields})
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
        self.assertEqual(len(self.operator_posts), 4)

    def test_mac_publisher_checks_uploaded_bytes_despite_record(self):
        self.desktop(gatekeeper={'signer': 'Developer ID Application: Test (TEAM)', 'source': 'Notarized Developer ID'})
        with patch.object(release.platform, 'system', return_value='Darwin'), \
                patch.object(release, 'gatekeeper', side_effect=ValueError('Gatekeeper rejects it')) as check:
            with self.assertRaisesRegex(ValueError, 'Gatekeeper rejects it'):
                release.publish(self.args)
        check.assert_called_once_with(self.root / 'haarness')
        self.assertEqual(self.operator_posts, [])

    def index(self, change):
        index = json.loads((self.root / 'index.json').read_text())
        change(index['artifacts'])
        (self.root / 'index.json').write_text(json.dumps(index))

    def test_refuses_a_cli_without_its_router(self):
        self.index(lambda artifacts: artifacts.pop())
        with self.assertRaisesRegex(ValueError, 'No Haarness Router for macos-arm64'):
            release.publish(self.args)
        self.assertEqual(self.operator_posts, [])
        self.args.without_router = True
        release.publish(self.args)
        self.assertEqual(len(self.operator_posts), 2)

    def test_files_without_a_cli_only_join_the_current_release_of_their_commit(self):
        # A router-only index, as collect --router-only writes it.
        self.index(lambda artifacts: artifacts.pop(0))
        for current in (None, {'version': '0.9.0', 'source_revision': 'a' * 40}, {'version': '1.0.0', 'source_revision': 'c' * 40}):
            self.current, self.operator_gets = current, []
            with self.assertRaisesRegex(ValueError, "carry no CLI, so they can only join the preview channel's current release of 1.0.0"):
                release.publish(self.args)
            self.assertEqual(self.operator_gets, ['/api/v1/releases?channel=preview'])
        self.assertEqual(self.operator_posts, [])
        self.current = {'version': '1.0.0', 'source_revision': 'a' * 40}
        release.publish(self.args)
        self.assertEqual(len(self.operator_posts), 2)

    def test_refuses_router_builds_without_one_router_commit(self):
        second = self.root / 'linux'
        second.mkdir()
        (second / 'haarness-router-linux-x64').write_bytes(b'router')
        router = {'file': 'haarness-router-linux-x64', 'product': 'router', 'kind': 'binary', 'os': 'linux', 'arch': 'x64',
                  'size': 6, 'sha256': hashlib.sha256(b'router').hexdigest()}
        for revision in ('c' * 40, None):
            index = json.loads((self.root / 'index.json').read_text())
            (second / 'index.json').write_text(json.dumps({**index, 'artifacts': [{**router, 'sourceRevision': revision}]}))
            self.args.index = [str(self.root / 'index.json'), str(second / 'index.json')]
            with self.assertRaisesRegex(ValueError, 'one router commit'):
                release.publish(self.args)
        self.assertEqual(self.operator_posts, [])

    def test_stable_refuses_unsigned_mac_desktop(self):
        self.args.channel, self.args.allow_unsigned = 'stable', True
        with self.assertRaisesRegex(ValueError, 'Stable never ships'):
            release.publish(self.args)
        self.assertEqual(self.operator_posts, [])


def git(cwd, *args):
    return subprocess.run(['git', '-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', *args], cwd=cwd,
                          check=True, capture_output=True, text=True).stdout.strip()


def executable(path, output):
    path.write_text(f'#!/bin/sh\nprintf "{output}\\n"\n')
    path.chmod(0o755)
    return path


class RouterCollectTest(unittest.TestCase):
    """collect puts Haarness Router in the platform's index with its own commit, at the release's version."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        # A release checkout and a router checkout of their own, so the test reads neither of the real ones.
        repo = self.root / 'haarness'
        (repo / 'apps/desktop').mkdir(parents=True)
        (repo / 'apps/desktop/package.json').write_text('{"version": "1.0.0"}')
        git(repo, 'init', '-q'); git(repo, 'add', '.'); git(repo, 'commit', '-qm', 'release')
        self.revision = git(repo, 'rev-parse', 'HEAD')
        root = patch.object(release, 'ROOT', repo)
        root.start()
        self.addCleanup(root.stop)
        self.router = self.root / 'haarness-router'
        self.router.mkdir()
        self.cargo('1.0.0')
        git(self.router, 'init', '-q'); git(self.router, 'add', '.'); git(self.router, 'commit', '-qm', 'router')
        self.router_revision = git(self.router, 'rev-parse', 'HEAD')
        packages = self.root / 'packages'
        packages.mkdir()
        (packages / 'Haarness-1.0.0-linux-x86_64.AppImage').write_bytes(b'appimage')
        self.args = argparse.Namespace(
            binary=str(executable(self.root / 'cli-build', 'haarness 1.0.0')),
            router_binary=str(executable(self.root / 'router-build', 'haarness-router 1.0.0')),
            router_source=str(self.router), router_only=False, out=str(self.root / 'out'), allow_dirty=False,
            os='linux', arch='x64', version=None, desktop_dir=str(packages), authenticode=None)

    def cargo(self, version):
        (self.router / 'Cargo.toml').write_text(f'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "{version}"\n'
                                                 'edition = "2024"\n\n[workspace.dependencies]\nversion_check = "1.0"\n')

    def collect(self):
        release.collect(self.args)
        return json.loads((self.root / 'out/index-linux-x64.json').read_text())

    def test_records_router_beside_cli_and_desktop(self):
        index = self.collect()
        self.assertEqual((index['version'], index['sourceRevision'], index['dirty']), ('1.0.0', self.revision, False))
        self.assertEqual([(a['product'], a['file']) for a in index['artifacts']],
                         [('cli', 'haarness-linux-x64'), ('desktop', 'Haarness-1.0.0-linux-x64.AppImage'), ('router', 'haarness-router-linux-x64')])
        router = index['artifacts'][-1]
        self.assertEqual((router['kind'], router['os'], router['arch'], router['sourceRevision']), ('binary', 'linux', 'x64', self.router_revision))
        self.assertEqual(router['sha256'], hashlib.sha256(Path(self.args.router_binary).read_bytes()).hexdigest())

    def test_a_router_that_runs_here_must_report_the_release_version(self):
        executable(Path(self.args.router_binary), 'haarness-router 0.9.0')
        with self.assertRaisesRegex(ValueError, "reports 'haarness-router 0.9.0'"):
            self.collect()
        # A cross build is not run; its checkout's version still counts.
        self.args.version = '1.0.0'
        self.collect()

    def test_refuses_a_router_checkout_of_another_version(self):
        self.cargo('0.9.0')
        git(self.router, 'commit', '-qam', 'older')
        self.args.version = '1.0.0'
        with self.assertRaisesRegex(ValueError, 'is version 0.9.0, not 1.0.0'):
            self.collect()

    def test_refuses_a_dirty_router_checkout_unless_local_proof(self):
        (self.router / 'Cargo.toml').write_text((self.router / 'Cargo.toml').read_text() + '# edited\n')
        with self.assertRaisesRegex(ValueError, 'Commit the router sources'):
            self.collect()
        self.args.allow_dirty = True
        self.assertTrue(self.collect()['dirty'])

    def test_router_only_index_adds_the_router_to_a_published_release(self):
        self.args.router_only, self.args.desktop_dir = True, str(self.root / 'nowhere')
        index = self.collect()
        self.assertEqual([a['file'] for a in index['artifacts']], ['haarness-router-linux-x64'])
        self.assertEqual(index['version'], '1.0.0')
        self.args.router_binary = None
        with self.assertRaisesRegex(ValueError, '--router-only'):
            self.collect()

    def test_windows_router_carries_its_signature_record(self):
        self.args.os, self.args.version = 'windows', '1.0.0'
        (Path(self.args.desktop_dir) / 'Haarness-1.0.0-win-x64.exe').write_bytes(b'setup')
        record = {'publicTrust': True, 'files': {hashlib.sha256(Path(self.args.router_binary).read_bytes()).hexdigest():
                                                  {'signer': 'Haarness', 'programs': [{}]}}}
        (self.root / 'record.json').write_text(json.dumps(record))
        self.args.authenticode = str(self.root / 'record.json')
        release.collect(self.args)
        index = json.loads((self.root / 'out/index-windows-x64.json').read_text())
        router = next(a for a in index['artifacts'] if a['product'] == 'router')
        self.assertEqual(router['file'], 'haarness-router-windows-x64.exe')
        self.assertEqual(router['authenticode'], {'signer': 'Haarness', 'publicTrust': True, 'programs': 1})


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
