#!/usr/bin/env python3
"""Prove a Linux release runs beyond the machine that built it.

The CLI may ask for no newer glibc than the floor and links only the C
library itself. The Desktop packages carry that same CLI, a launcher entry
that owns haarness:// links, and complete .deb metadata. install.sh installs
the real CLI and AppImage from a loopback release service, the AppImage with
the launcher command, menu entry and icon the .deb has. With Docker, the CLI
also loads OpenTUI and answers a turn on the oldest supported distributions,
none of which has Git installed.
"""
import errno, hashlib, http.server, json, os, re, shutil, subprocess, sys, tempfile, threading
from pathlib import Path
from e2e_env import clean_env

ROOT = Path(__file__).resolve().parents[1]
FLOOR = tuple(int(part) for part in os.environ.get('HAARNESS_GLIBC_FLOOR', '2.17').split('.'))
# Oldest first: glibc 2.17, 2.26, 2.28, 2.31, 2.35. None of them ships Git.
DISTROS = os.environ.get('HAARNESS_LINUX_DISTROS', 'centos:7 amazonlinux:2 rockylinux:8 debian:11 ubuntu:22.04').split()
LIBRARIES = {'libc.so.6', 'libm.so.6', 'libpthread.so.0', 'libdl.so.2', 'librt.so.1', 'libutil.so.1',
             'libgcc_s.so.1', 'ld-linux-x86-64.so.2', 'ld-linux-aarch64.so.1'}

def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def check_binary(binary):
    symbols = subprocess.check_output(['objdump', '-T', str(binary)], text=True)
    versions = {tuple(map(int, match)) for match in re.findall(r'GLIBC_(\d+)\.(\d+)', symbols)}
    newest = max(versions)
    assert newest <= FLOOR, f'{binary} needs glibc {newest[0]}.{newest[1]}, newer than the {FLOOR[0]}.{FLOOR[1]} floor'
    needed = set(re.findall(r'NEEDED\s+(\S+)', subprocess.check_output(['objdump', '-p', str(binary)], text=True)))
    assert needed <= LIBRARIES, f'{binary} links {sorted(needed - LIBRARIES)} beyond the C library'
    print(f'PASS CLI needs glibc {newest[0]}.{newest[1]} at most and links only {sorted(needed)}')

def extract_appimage(appimage, scratch):
    # Extraction needs no FUSE, so it works on build machines and in containers. An x86-64 emulator (Rosetta in
    # Docker Desktop on Apple silicon, qemu-user) may refuse to start an AppImage, whose ELF header carries the
    # AppImage magic bytes: then its squashfs, which starts where the runtime's ELF ends, is read directly.
    # Returns the extracted folder and whether the AppImage runs here.
    try:
        subprocess.run([str(appimage), '--appimage-extract'], cwd=scratch, check=True, stdout=subprocess.DEVNULL)
        return scratch / 'squashfs-root', True
    except OSError as error:
        if error.errno != errno.ENOEXEC or not shutil.which('unsquashfs'):
            raise
        with open(appimage, 'rb') as f:
            header = f.read(64)
        field = lambda start, end: int.from_bytes(header[start:end], 'little')
        offset = field(0x28, 0x30) + field(0x3a, 0x3c) * field(0x3c, 0x3e)
        subprocess.run(['unsquashfs', '-q', '-n', '-o', str(offset), '-d', str(scratch / 'squashfs-root'), str(appimage)],
                       check=True, stdout=subprocess.DEVNULL)
        print(f'NOTE {appimage.name} cannot start here (exec format error); read its squashfs at offset {offset}')
        return scratch / 'squashfs-root', False

def desktop_file(text):
    assert 'MimeType=x-scheme-handler/haarness;' in text, text
    # The AppImage's own entry starts AppRun --no-sandbox, as electron-builder writes it.
    assert re.search(r'^Exec=\S+( --no-sandbox)? %U$', text, re.M), text
    assert re.search(r'^StartupWMClass=haarness-desktop$', text, re.M), text
    return text

def install(payload, version, product, home):
    """Run install.sh against a loopback release service that serves payload."""
    content = Path(payload).read_bytes()
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args): pass
        def do_GET(self):
            self.send_response(200)
            self.send_header('x-content-sha256', hashlib.sha256(content).hexdigest())
            self.send_header('x-haarness-version', version)
            self.send_header('content-length', str(len(content)))
            self.end_headers()
            self.wfile.write(content)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        env = clean_env('XDG_')
        env.update(HOME=str(home), HAARNESS_RELEASE_URL=f'http://127.0.0.1:{server.server_port}',
                   HAARNESS_CHANNEL='preview', HAARNESS_VERSION=version, HAARNESS_PRODUCT=product)
        result = subprocess.run(['sh', str(ROOT / 'install.sh')], env=env, capture_output=True, text=True, timeout=600)
        assert result.returncode == 0, result.stdout + result.stderr
        return result.stdout
    finally:
        server.shutdown(); server.server_close(); thread.join()

def check_installer(binary, appimage, version, appimage_runs=True):
    with tempfile.TemporaryDirectory(prefix='haarness-linux-install-') as home:
        home = Path(home)
        install(binary, version, 'cli', home)
        installed = home / '.local/bin/haarness'
        assert subprocess.check_output([str(installed), '--version'], text=True).strip() == f'haarness {version}'
        output = install(appimage, version, 'desktop', home)
        app, launcher = home / '.local/bin/Haarness.AppImage', home / '.local/bin/haarness-desktop'
        assert digest(app) == digest(appimage) and os.access(app, os.X_OK)
        assert os.access(launcher, os.X_OK) and str(app) in launcher.read_text() and 'APPIMAGE_EXTRACT_AND_RUN' in launcher.read_text()
        entry = (home / '.local/share/applications/haarness-desktop.desktop').read_text()
        desktop_file(entry)
        assert f'Exec="{launcher}" %U' in entry, entry
        icon = home / '.local/share/icons/haarness-desktop.png'
        if appimage_runs:
            assert icon.read_bytes().startswith(b'\x89PNG') and f'Icon={icon}' in entry, entry
        else:
            # install.sh takes the icon out by running the AppImage, which cannot start here: the entry names the
            # theme's icon instead. A machine of this architecture runs the full check.
            assert 'Icon=haarness-desktop\n' in entry and not icon.exists(), entry
        if shutil.which('desktop-file-validate'):
            subprocess.run(['desktop-file-validate', str(home / '.local/share/applications/haarness-desktop.desktop')], check=True)
        assert 'haarness-desktop' in output, output
    print('PASS install.sh installs the CLI, and the AppImage with a haarness-desktop command, menu entry and '
          + ('icon' if appimage_runs else 'the theme icon name (the AppImage cannot start here to give its own)'))

def check_desktop(binary, version):
    release = ROOT / 'apps/desktop/release'
    # electron-builder names Linux packages by each format's own word for the
    # architecture (x86_64, amd64); release.py collect publishes them as x64.
    names = {'x86_64': ('x64', 'x86_64', 'amd64'), 'aarch64': ('arm64', 'aarch64')}[os.uname().machine]
    def package(kind):
        found = [release / f'Haarness-{version}-linux-{name}.{kind}' for name in names]
        found = [path for path in found if path.is_file()]
        assert len(found) == 1, (kind, sorted(p.name for p in release.iterdir()))
        return found[0]
    deb, appimage = package('deb'), package('AppImage')
    # The AppImage's runtime starts before anything it carries, with the system's libraries alone, so each must be one
    # every system has: electron-builder's legacy arm64 runtime asks for libz.so, which only zlib's development
    # package installs, and 0.1.3's arm64 AppImage did not start on a stock Ubuntu.
    runtime = set(re.findall(r'NEEDED\s+(\S+)', subprocess.check_output(['objdump', '-p', str(appimage)], text=True)))
    unversioned = sorted(name for name in runtime if not re.search(r'\.so\.\d', name))
    assert not unversioned, f'{appimage.name} needs {unversioned}, which only development packages provide'
    expected = digest(binary)
    field = lambda name: subprocess.check_output(['dpkg-deb', '-f', str(deb), name], text=True).strip()
    assert field('Package') == 'haarness-desktop', field('Package')
    assert field('Version') and field('Homepage') == 'https://haarness.ai', (field('Version'), field('Homepage'))
    assert re.fullmatch(r'.+ <[^@\s]+@[^@\s]+>', field('Maintainer')), field('Maintainer')
    with tempfile.TemporaryDirectory(prefix='haarness-linux-release-') as scratch:
        scratch = Path(scratch)
        subprocess.run(['dpkg-deb', '-x', str(deb), str(scratch / 'deb')], check=True)
        bundled = scratch / 'deb/opt/Haarness/resources/bin/haarness'
        assert digest(bundled) == expected, 'the .deb carries a different CLI than the release binary'
        # /usr/bin/haarness belongs to the CLI; the app must not take its name.
        assert not (scratch / 'deb/usr/bin/haarness').exists()
        launchers = list((scratch / 'deb/usr/share/applications').glob('*.desktop'))
        assert len(launchers) == 1, launchers
        desktop_file(launchers[0].read_text())
        extracted, appimage_runs = extract_appimage(appimage, scratch)
        assert digest(extracted / 'resources/bin/haarness') == expected, 'the AppImage carries a different CLI'
        desktop_file(next(extracted.glob('*.desktop')).read_text())
        # Ubuntu 22.04's hicolor theme has no 1024x1024 folder; menus need a listed size.
        icons = {path.parent.parent.name for path in (scratch / 'deb/usr/share/icons/hicolor').glob('*/apps/haarness-desktop.png')}
        assert {'48x48', '256x256', '512x512'} <= icons, icons
    print(f'PASS {deb.name} and {appimage.name} carry the release CLI, a launcher for haarness:// links, icons and package metadata')
    check_installer(binary, appimage, version, appimage_runs)

def check_distros(binary):
    if not DISTROS or os.environ.get('HAARNESS_SKIP_DOCKER') == '1':
        print('SKIP distribution checks (HAARNESS_SKIP_DOCKER=1 or no distributions)')
        return
    if not shutil.which('docker'):
        print('SKIP distribution checks (no Docker)')
        return
    # Debian and Ubuntu images leave out ca-certificates: Haarness must say so
    # plainly (not exit during startup), then work once SSL_CERT_FILE names a bundle.
    script = ('set -e; mkdir -p /tmp/home /tmp/project; cd /tmp/project; ! command -v git;'
              ' haarness --version; haarness doctor;'
              ' if haarness doctor | grep -q "certificates: none found"; then'
              '   ! haarness run --model echo/echo-1 hello 2>/tmp/error; grep -q ca-certificates /tmp/error;'
              '   echo "explains missing certificates"; export SSL_CERT_FILE=/tmp/certs.pem; fi;'
              ' haarness run --model echo/echo-1 "linux release smoke"')
    bundle = next(path for path in ('/etc/ssl/certs/ca-certificates.crt', '/etc/pki/tls/certs/ca-bundle.crt',
                                    '/etc/ssl/cert.pem') if Path(path).is_file())
    for image in DISTROS:
        result = subprocess.run(['docker', 'run', '--rm', '-v', f'{Path(binary).resolve()}:/usr/local/bin/haarness:ro',
                                 '-v', f'{bundle}:/tmp/certs.pem:ro',
                                 '-e', 'HOME=/tmp/home', '-e', 'HAARNESS_TEST_PROVIDER=1', image, 'sh', '-c', script],
                                capture_output=True, text=True, timeout=600)
        output = result.stdout + result.stderr
        assert result.returncode == 0, f'{image}:\n{output[-3000:]}'
        assert 'OpenTUI' in output and 'symbols: createRenderer' in output, f'{image}: OpenTUI did not load\n{output[-3000:]}'
        assert 'linux release smoke' in output, f'{image}: no answer\n{output[-3000:]}'
        note = ', explains missing certificates' if 'explains missing certificates' in output else ''
        print(f'PASS {image}: loads OpenTUI and answers a turn without Git{note}')

def main():
    binary = Path(os.environ.get('HAARNESS_BIN') or ROOT / 'target/release/haarness').resolve()
    version = json.loads((ROOT / 'apps/desktop/package.json').read_text())['version']
    check_binary(binary)
    if '--cli-only' not in sys.argv:
        check_desktop(binary, version)
    check_distros(binary)

if __name__ == '__main__':
    main()
