#!/usr/bin/env python3
"""Package Haarness for Chrome as release.py collect-extension does: the zip people load, the CRX Chrome installs by
policy, and every refusal before either is written. A throwaway key stands in for the release key."""
import argparse
import base64
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location('release', Path(__file__).with_name('release.py'))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
REVISION = 'a' * 40


class ExtensionPackageTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.key = self.root / 'key.pem'
        subprocess.run(['openssl', 'genrsa', '-out', str(self.key), '2048'], check=True, capture_output=True)
        public = subprocess.run(['openssl', 'pkey', '-in', str(self.key), '-pubout', '-outform', 'DER'], check=True, capture_output=True).stdout
        # The throwaway key's ID plays Haarness for Chrome's for this test.
        self.addCleanup(setattr, release, 'EXTENSION_ID', release.EXTENSION_ID)
        release.EXTENSION_ID = release.extension_id(public)
        self.folder = self.root / 'Haarness for Chrome'
        (self.folder / 'assets').mkdir(parents=True)
        self.manifest = {'manifest_version': 3, 'name': 'Haarness', 'version': '1.2.3.1000', 'version_name': '1.2.3', 'key': base64.b64encode(public).decode()}
        (self.folder / 'manifest.json').write_text(json.dumps(self.manifest))
        (self.folder / 'background.js').write_text('export {};\n')
        (self.folder / 'assets' / 'popup.js').write_text('console.log(1);\n')

    def collect(self, out='dist', **overrides):
        args = dict(extension_dir=str(self.folder), key=str(self.key), version='1.2.3', revision=REVISION, out=str(self.root / out), allow_dirty=False)
        args.update(overrides)
        release.collect_extension(argparse.Namespace(**args))
        return self.root / out

    def test_the_zip_and_the_crx_carry_the_same_folder(self):
        out = self.collect()
        index = json.loads((out / 'index-chrome-extension.json').read_text())
        self.assertEqual((index['version'], index['sourceRevision'], index['dirty']), ('1.2.3', REVISION, False))
        self.assertEqual([(a['file'], a['product'], a['kind'], a['os'], a['arch']) for a in index['artifacts']], [
            ('haarness-chrome-1.2.3.zip', 'chrome-extension', 'zip', 'any', 'any'),
            ('haarness-chrome-1.2.3.crx', 'chrome-extension', 'crx', 'any', 'any'),
        ])
        for artifact in index['artifacts']:
            self.assertEqual(release.digest(out / artifact['file']), artifact['sha256'])
        zipped = (out / 'haarness-chrome-1.2.3.zip').read_bytes()
        self.assertEqual(release.verify_crx(out / 'haarness-chrome-1.2.3.crx'), zipped)
        names = zipfile.ZipFile(io.BytesIO(zipped)).namelist()
        self.assertEqual(names, ['assets/popup.js', 'background.js', 'manifest.json'])
        # The same folder gives the same bytes, so a rebuilt package is the published one.
        again = self.collect('again')
        for name in ('haarness-chrome-1.2.3.zip', 'haarness-chrome-1.2.3.crx'):
            self.assertEqual((out / name).read_bytes(), (again / name).read_bytes())

    def test_a_changed_package_fails_its_signature(self):
        crx = self.collect() / 'haarness-chrome-1.2.3.crx'
        data = bytearray(crx.read_bytes())
        data[-30] ^= 1
        crx.write_bytes(data)
        with self.assertRaisesRegex(ValueError, 'bad signature'):
            release.verify_crx(crx)

    def test_nothing_is_written_for_a_folder_that_cannot_ship(self):
        cases = [
            ('another version', lambda: None, dict(version='1.2.4'), 'not 1.2.4'),
            ('no signing key', lambda: None, dict(key=str(self.root / 'missing.pem')), 'No signing key'),
            ('a revision that is not a commit', lambda: None, dict(revision='v1.2.3'), 'full Git commit'),
            ('source in the build', lambda: (self.folder / 'assets' / 'popup.ts').write_text(''), {}, 'Source files'),
            ('a key the manifest does not pin', lambda: self.write_manifest(key=base64.b64encode(b'other').decode()), {}, 'manifest key gives'),
            ('Manifest V2', lambda: self.write_manifest(manifest_version=2), {}, 'Manifest V3'),
        ]
        for name, prepare, overrides, message in cases:
            with self.subTest(name):
                self.setUp()
                prepare()
                with self.assertRaisesRegex(ValueError, message):
                    self.collect(**overrides)
                self.assertFalse((self.root / 'dist').exists())

    def test_the_release_key_must_be_the_one_the_manifest_pins(self):
        other = self.root / 'other.pem'
        subprocess.run(['openssl', 'genrsa', '-out', str(other), '2048'], check=True, capture_output=True)
        with self.assertRaisesRegex(ValueError, 'not the one the manifest pins'):
            self.collect(key=str(other))

    def write_manifest(self, **changes):
        (self.folder / 'manifest.json').write_text(json.dumps({**self.manifest, **changes}))


if __name__ == '__main__':
    unittest.main()
