#!/usr/bin/env python3
"""Run the CLI and bundled Bun from the actual Desktop package on its native runner."""
import json, platform, subprocess
from pathlib import Path
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
# Haarness for Chrome sits beside bin/, where `haarness browser setup` looks for it.
manifest=json.loads((binary.parent.parent/'browser-extension'/'chrome'/'manifest.json').read_text())
release=versions['version'].split('-')[0]
preview=versions['version'].split('.')[-1] if '-' in versions['version'] else '1000'
assert manifest['version_name']==versions['version'],manifest['version_name']
assert manifest['version']==f'{release}.{preview}',manifest['version']
assert manifest['key'] and 'nativeMessaging' in manifest['permissions']
assert (binary.parent.parent/'browser-extension'/'chrome'/'background.js').is_file()
print('Packaged backend, bundled JavaScript runtime and Chrome extension check out')
