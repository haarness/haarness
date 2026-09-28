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
print('Packaged backend and bundled JavaScript runtime execute successfully')
