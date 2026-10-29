#!/usr/bin/env python3
"""Exercise the GitHub release mirror against an in-memory GitHub that digests what it receives: the release on this
repository, and Haarness Router's on the router repository."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('release', Path(__file__).with_name('release.py'))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
REPO, TAG, COMMIT = 'haarness/haarness', 'v1.0.0', 'a' * 40
ROUTER_REPO, ROUTER_COMMIT = 'haarness/haarness-router', 'c' * 40


class FakeGitHub:
    def __init__(self):
        self.tags = {(REPO, TAG): COMMIT, (ROUTER_REPO, TAG): ROUTER_COMMIT}
        self.releases = []
        self.calls = []
        self.corrupt = set()
        self.next_id = 1

    def new_id(self):
        self.next_id += 1
        return self.next_id

    def find(self, release_id):
        return next(r for r in self.releases if r['id'] == int(release_id))

    def __call__(self, *args):
        self.calls.append(args)
        if args[:2] == ('release', 'upload'):
            tag, file, repo = args[2], Path(args[3]), args[args.index('--repo') + 1]
            r = next(r for r in self.releases if r['tag_name'] == tag and r['repo'] == repo)
            data = file.read_bytes() + (b'!' if file.name in self.corrupt else b'')
            r['assets'].append({'id': self.new_id(), 'name': file.name, 'state': 'uploaded', 'size': len(data),
                                'digest': 'sha256:' + hashlib.sha256(data).hexdigest()})
            return None
        assert args[0] == 'api', args
        method, rest = 'GET', list(args[1:])
        if rest[0] == '-X': method, rest = rest[1], rest[2:]
        rest = [a for a in rest if a not in ('--paginate', '--slurp')]
        repo, path, fields = '/'.join(rest[0].split('/')[1:3]), rest[0].split('/')[3:], {}
        for flag, value in zip(rest[1::2], rest[2::2]):
            key, value = value.split('=', 1)
            fields[key] = {'true': True, 'false': False}.get(value, value) if flag == '-F' else value
        if path[0] == 'commits':
            return {'sha': self.tags[(repo, path[1])]}
        if path == ['releases'] and method == 'GET':
            return [[r for r in self.releases if r['repo'] == repo]]
        if path == ['releases'] and method == 'POST':
            r = dict(id=self.new_id(), repo=repo, tag_name=fields['tag_name'], name=fields['name'], body=fields['body'],
                     draft=fields['draft'], prerelease=fields['prerelease'],
                     immutable=False, assets=[], html_url=f'https://github.com/{repo}/releases/tag/{fields["tag_name"]}')
            self.releases.append(r)
            return r
        if path[:2] == ['releases', 'assets'] and method == 'DELETE':
            for r in self.releases: r['assets'] = [x for x in r['assets'] if x['id'] != int(path[2])]
            return None
        r = self.find(path[1])
        if method == 'PATCH':
            r.update(draft=fields['draft'], prerelease=fields['prerelease'], latest=fields['make_latest'] == 'true')
            r['immutable'] = not r['draft']
        return r


class GitHubReleaseTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        artifacts = []
        for name in ('haarness-linux-x64', 'Haarness-1.0.0-linux-x64.deb'):
            (root / name).write_bytes(name.encode())
            artifacts.append({'file': name, 'product': 'cli', 'kind': 'binary', 'os': 'linux', 'arch': 'x64',
                              'size': len(name), 'sha256': hashlib.sha256(name.encode()).hexdigest()})
        index = {'version': '1.0.0', 'sourceRevision': COMMIT, 'dirty': False, 'releasedAt': '2026-10-22T00:00:00Z',
                 'artifacts': artifacts}
        (root / 'index.json').write_text(json.dumps(index))
        self.root = root
        self.args = argparse.Namespace(index=[str(root / 'index.json')], repo=REPO, router_repo=ROUTER_REPO, only=None,
                                       channel='stable', notes_url=None)
        self.github = FakeGitHub()
        stub = patch.object(release, 'gh', self.github)
        stub.start()
        self.addCleanup(stub.stop)

    def uploads(self, repo=REPO):
        return sorted(Path(c[3]).name for c in self.github.calls if c[:2] == ('release', 'upload') and c[5] == repo)

    def with_router(self, revisions=(ROUTER_COMMIT,)):
        index = json.loads(Path(self.args.index[0]).read_text())
        for arch, revision in zip(('x64', 'arm64'), revisions):
            name = f'haarness-router-linux-{arch}'
            (self.root / name).write_bytes(name.encode())
            index['artifacts'].append({'file': name, 'product': 'router', 'kind': 'binary', 'os': 'linux', 'arch': arch,
                                       'size': len(name), 'sha256': hashlib.sha256(name.encode()).hexdigest(),
                                       'sourceRevision': revision})
        Path(self.args.index[0]).write_text(json.dumps(index))

    def test_router_goes_to_its_own_repository(self):
        self.with_router(revisions=(ROUTER_COMMIT, ROUTER_COMMIT))
        release.github(self.args)
        haarness, router = sorted(self.github.releases, key=lambda r: r['repo'])
        self.assertEqual((haarness['repo'], router['repo']), (REPO, ROUTER_REPO))
        self.assertEqual(self.uploads(), ['Haarness-1.0.0-linux-x64.deb', 'SHA256SUMS', 'haarness-linux-x64'])
        self.assertEqual(self.uploads(ROUTER_REPO), ['SHA256SUMS', 'haarness-router-linux-arm64', 'haarness-router-linux-x64'])
        self.assertEqual((router['tag_name'], router['name'], router['draft'], router['latest']), (TAG, 'Haarness Router 1.0.0', False, True))
        self.assertIn(ROUTER_COMMIT, router['body'])
        self.assertIn('install-router.sh', router['body'])

    def test_only_mirrors_one_repository(self):
        self.with_router()
        self.args.only = 'router'
        release.github(self.args)
        self.assertEqual([r['repo'] for r in self.github.releases], [ROUTER_REPO])
        self.args.only = 'haarness'
        release.github(self.args)
        self.assertEqual(sorted(r['repo'] for r in self.github.releases), [REPO, ROUTER_REPO])

    def test_refuses_a_router_tag_on_another_commit(self):
        self.with_router()
        self.github.tags[(ROUTER_REPO, TAG)] = 'd' * 40
        self.args.only = 'router'
        with self.assertRaisesRegex(ValueError, f'{ROUTER_REPO} {TAG} is not'):
            release.github(self.args)
        self.assertEqual(self.github.releases, [])

    def test_refuses_router_builds_of_different_commits(self):
        self.with_router(revisions=(ROUTER_COMMIT, 'e' * 40))
        with self.assertRaisesRegex(ValueError, 'one router commit'):
            release.github(self.args)
        self.assertEqual(self.github.calls, [])

    def test_publishes_verified_draft_with_checksums(self):
        release.github(self.args)
        r, = self.github.releases
        self.assertEqual((r['draft'], r['prerelease'], r['latest']), (False, False, True))
        self.assertEqual(self.uploads(), ['Haarness-1.0.0-linux-x64.deb', 'SHA256SUMS', 'haarness-linux-x64'])
        sums = next(x for x in r['assets'] if x['name'] == 'SHA256SUMS')
        expected = ''.join(f'{hashlib.sha256(n.encode()).hexdigest()}  {n}\n'
                           for n in sorted(['Haarness-1.0.0-linux-x64.deb', 'haarness-linux-x64']))
        self.assertEqual(sums['digest'], 'sha256:' + hashlib.sha256(expected.encode()).hexdigest())

    def test_refuses_tag_on_another_commit(self):
        self.github.tags[(REPO, TAG)] = 'b' * 40
        with self.assertRaisesRegex(ValueError, 'is not'):
            release.github(self.args)
        self.assertEqual(self.github.releases, [])

    def test_corrupted_upload_leaves_release_a_draft(self):
        self.github.corrupt.add('haarness-linux-x64')
        with self.assertRaisesRegex(ValueError, r"wrong or missing \['haarness-linux-x64'\]"):
            release.github(self.args)
        self.assertTrue(self.github.releases[0]['draft'])

    def test_resumed_draft_replaces_stale_and_uploads_missing(self):
        self.github.corrupt.add('haarness-linux-x64')
        with self.assertRaises(ValueError):
            release.github(self.args)
        self.github.corrupt.clear()
        self.github.calls.clear()
        release.github(self.args)
        self.assertEqual(self.uploads(), ['haarness-linux-x64'])
        self.assertFalse(self.github.releases[0]['draft'])

    def test_preview_then_stable_promotes_without_uploading(self):
        self.args.channel = 'preview'
        release.github(self.args)
        r, = self.github.releases
        self.assertEqual((r['draft'], r['prerelease'], r['latest']), (False, True, False))
        self.github.calls.clear()
        self.args.channel = 'stable'
        release.github(self.args)
        self.assertEqual(self.uploads(), [])
        self.assertEqual((r['prerelease'], r['latest']), (False, True))

    def test_never_changes_published_release_with_other_bytes(self):
        release.github(self.args)
        self.github.releases[0]['assets'][0]['digest'] = 'sha256:' + '0' * 64
        self.github.calls.clear()
        with self.assertRaisesRegex(ValueError, 'differs'):
            release.github(self.args)
        self.assertFalse([c for c in self.github.calls if '-X' in c or c[0] == 'release'])

    def test_refuses_dirty_builds(self):
        index = json.loads(Path(self.args.index[0]).read_text())
        Path(self.args.index[0]).write_text(json.dumps({**index, 'dirty': True}))
        with self.assertRaisesRegex(ValueError, 'Dirty'):
            release.github(self.args)


if __name__ == '__main__':
    unittest.main()
