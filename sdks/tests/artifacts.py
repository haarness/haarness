"""Install packed artifacts in clean consumer environments, then rerun coverage."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import zipfile
from server import ROOT

wheel = ROOT / "sdks/python/dist/haarness_sdk-0.1.0-py3-none-any.whl"
tarball = ROOT / "sdks/artifacts/haarness-sdk-0.1.0.tgz"
with zipfile.ZipFile(wheel) as archive:
    assert "haarness_sdk/py.typed" in archive.namelist()
    metadata = archive.read("haarness_sdk-0.1.0.dist-info/METADATA").decode()
    assert "Private :: Do Not Upload" in metadata
assert (
    json.loads((ROOT / "sdks/typescript/package.json").read_text())["private"] is True
)
with tempfile.TemporaryDirectory(prefix="haarness-sdk-artifacts-") as temporary:
    root = Path(temporary)
    (root / "package.json").write_text('{"private":true,"type":"module"}')
    subprocess.run(
        ["npm", "install", "--ignore-scripts", "--prefix", str(root), str(tarball)],
        check=True,
    )
    subprocess.run(["uv", "venv", "--python", "3.10", str(root / "venv")], check=True)
    python = root / "venv/bin/python"
    subprocess.run(
        ["uv", "pip", "install", "--python", str(python), str(wheel)], check=True
    )
    environment = {
        **os.environ,
        "SDK_PYTHON": str(python),
        "SDK_MODULE": (root / "node_modules/@haarness/sdk/dist/index.js").as_uri(),
    }
    subprocess.run(
        [str(ROOT / "sdks/python/.venv/bin/python"), str(ROOT / "sdks/tests/e2e.py")],
        env=environment,
        check=True,
    )
    # Compile a consuming program against the installed public TypeScript declarations.
    (
        root / "consumer.ts"
    ).write_text("""import { Haarness, type Thread, type ChatOptions } from '@haarness/sdk';
const client = new Haarness({baseUrl:'http://127.0.0.1:4096'});
const thread: Promise<Thread> = client.createThread({title:'consumer'});
client.chat('id','hello',{attachments:[]});
// @ts-expect-error: title cannot be a number.
client.createThread({title:42});
// @ts-expect-error: invalid lifecycle action.
client.setGoal('id',{action:'invented'});
void thread;
""")
    subprocess.run(
        [
            str(ROOT / "sdks/typescript/node_modules/.bin/tsc"),
            "--noEmit",
            "--strict",
            "--module",
            "NodeNext",
            "--moduleResolution",
            "NodeNext",
            "--target",
            "ES2022",
            str(root / "consumer.ts"),
        ],
        check=True,
    )
print(
    "Packed npm and Python wheel: clean installation, consumer typing, and full operation coverage passed (Python 3.10)"
)
