#!/usr/bin/env python3
"""Run the CLI and bundled Bun from the actual Desktop package on its native runner, and
check that the standalone CLI carries Haarness for Chrome."""
import json, os, platform, shutil, subprocess, tempfile
from pathlib import Path
from e2e_env import clean_env
root=Path(__file__).resolve().parents[1]
name='haarness.exe' if platform.system()=='Windows' else 'haarness'
versions=json.loads((root/'apps/desktop/package.json').read_text())
paths=list((root/'apps/desktop/release').glob('*/Haarness.app/Contents/Resources/bin/'+name)) if platform.system()=='Darwin' else list((root/'apps/desktop/release').glob('*/resources/bin/'+name))
assert len(paths)==1,paths
binary=paths[0]
assert subprocess.check_output([str(binary),'--version'],text=True).strip()=='haarness '+versions['version']
bun=binary.parent/('bun.exe' if platform.system()=='Windows' else 'bun')
assert subprocess.check_output([str(bun),'--version'],text=True).strip()=='1.3.14'
subprocess.run([str(binary),'--help'],check=True,stdout=subprocess.DEVNULL)
if platform.system()=='Darwin':
    # Every program in bin/ starts wherever the app does: a Mac packages the other architecture's app too, and
    # 0.1.3's x64 app carried an arm64-only voice helper, so dictation could not start on Intel Macs.
    app_archs=set(subprocess.check_output(['lipo','-archs',str(binary.parents[2]/'MacOS'/'Haarness')],text=True).split())
    for helper in sorted(binary.parent.iterdir()):
        if helper.is_file() and subprocess.check_output(['file','-b',str(helper)],text=True).startswith('Mach-O'):
            archs=set(subprocess.check_output(['lipo','-archs',str(helper)],text=True).split())
            assert app_archs<=archs,f'{helper.name} runs on {sorted(archs)}, the app on {sorted(app_archs)}'
# Haarness for Chrome sits beside bin/, where `haarness browser setup` looks for it.
manifest=json.loads((binary.parent.parent/'browser-extension'/'chrome'/'manifest.json').read_text())
release=versions['version'].split('-')[0]
preview=versions['version'].split('.')[-1] if '-' in versions['version'] else '1000'
assert manifest['version_name']==versions['version'],manifest['version_name']
assert manifest['version']==f'{release}.{preview}',manifest['version']
assert manifest['key'] and 'nativeMessaging' in manifest['permissions']
assert (binary.parent.parent/'browser-extension'/'chrome'/'background.js').is_file()
# The standalone CLI (what release.py collect publishes) carries Haarness for Chrome
# itself: copied away from the checkout and any Desktop bundle, setup must find the
# built-in copy and write it out, touching only throwaway folders.
cli=Path(os.environ.get('HAARNESS_BIN') or root/'target'/'release'/name)
with tempfile.TemporaryDirectory(prefix='haarness-cli-chrome-') as scratch:
    scratch=Path(scratch)
    alone=scratch/'bin'/name;alone.parent.mkdir();shutil.copy2(cli,alone)
    env=clean_env('XDG_')
    env.update(HOME=str(scratch/'home'),USERPROFILE=str(scratch/'home'),HAARNESS_DATA_DIR=str(scratch/'data'),HAARNESS_BROWSER_MANIFEST_DIR=str(scratch/'profile'/'NativeMessagingHosts'))
    (scratch/'home').mkdir()
    status=json.loads(subprocess.check_output([str(alone),'browser','status','--json'],env=env,text=True))
    assert status['extensionSource']=={'kind':'embedded','path':None,'version':versions['version']},status['extensionSource']
    report=json.loads(subprocess.check_output([str(alone),'browser','setup','--json'],env=env,text=True))
    assert report['extension'] and report['extension']['version']==versions['version'],report
    copy=Path(report['extension']['path'])
    assert copy.is_relative_to(scratch/'data') and (copy/'background.js').is_file(),copy
    assert json.loads((copy/'manifest.json').read_text())==manifest
    assert all(browser['registered'] for browser in report['browsers']) and report['warnings']==[],report
print('Packaged backend, bundled JavaScript runtime and Chrome extension check out, and the CLI carries the extension')
