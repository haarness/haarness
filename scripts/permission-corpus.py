#!/usr/bin/env python3
"""Compare how two builds read and decide realistic shell lines.

`run` feeds the corpus below to haarness-core's `permission_corpus` test (an ignored unit test,
`crates/haarness-core/src/runtime/permission_corpus.rs`) in a fixture project and writes one JSON
object per line: the shell reading (patterns, readings, outside folders, deletions, the script card,
the read-only check) and the decision under three configurations (defaults; `"*": "ask"` with an
allowlist; an `rm *` deny), each with and without remembered Always answers. Nothing is run.

`diff` compares two such files and lists every line whose reading or decision changed, marking a
decision that moved from allow to ask or deny under the defaults.

Usage:
  cargo test -p haarness-core --lib --no-run      # prints the test binary's path
  python3 scripts/permission-corpus.py run <test binary> <out.jsonl> [--root <folder>]
  python3 scripts/permission-corpus.py diff <before.jsonl> <after.jsonl>
"""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

CORPUS = [
    # Git
    'git status', 'git status --short', 'git diff HEAD~1 -- src/main.rs', 'git log --oneline -20',
    'git add -A && git commit -m "fix: the thing"', 'git push origin main', 'git push --force origin main',
    'git checkout -b feature/x', 'git stash list', 'git -C ../outside status',
    'git diff --no-index ../outside/a README.md', 'git commit -F - <<\'EOF\'\nfix: thing\n\nBody line.\nEOF',
    # Package managers and builds
    'npm install', 'npm run build', 'npm test -- --watch=false', 'npx tsc --noEmit', 'pnpm -r build',
    'cargo build -p haarness-cli', 'cargo test -p haarness-core permissions',
    'cargo clippy --all-targets -- -D warnings', 'cargo fmt --all --check', 'timeout 30 npm test',
    'env NODE_ENV=test npm test', 'export PATH="$PWD/bin:$PATH" && make',
    # Pipes
    'ls -la | head -20', 'grep -rn "TODO" src | wc -l', "cat package.json | jq '.scripts'",
    'find . -name "*.rs" | xargs grep -l unsafe', 'ps aux | grep node | grep -v grep',
    'rg -n "fn main" --type rust | sort | uniq -c',
    # Redirects
    'cargo test 2>&1 | tail -50', 'npm run build > build.log 2>&1', 'echo hi > /tmp/x.txt',
    'cat < /etc/hosts', 'wc -l < src/main.rs', 'make > /dev/null 2>&1', 'cmd 2>/dev/null || true',
    'echo x >> notes.txt', 'cat /dev/null > build/out.log', 'echo x > /dev/shm/x', 'cat < /dev/shm/x',
    'echo x > /dev/stderr', 'read -r line < /dev/tty', 'exec 3< /dev/fd/0', 'echo ping > /dev/tcp/127.0.0.1/9',
    'tee out.txt < in.txt', 'grep -c x <<< "$VAR"', 'while read -r l; do echo "$l"; done < list.txt',
    'cat < ../outside/secret', 'echo x > /dev/mqueue/q', 'dd if=/dev/zero of=build/blank bs=1k count=1',
    # Here-documents
    "cat > notes.md <<'EOF'\n# Notes\nRun `npm test` first.\nEOF",
    'cat > notes.md <<EOF\n# Notes\nRun `npm test` first.\nEOF',
    'cat > README.md <<EOF\nVersion: $(git describe --tags)\nUse `cargo build` to build.\nEOF',
    'cat > n.md <<EOF\n`rm -rf victim`\nEOF',
    'cat > n.md <<EOF\nHome is `echo ~/x` and \\`literal\\` here.\nEOF',
    'cat > n.md <<EOF\n```bash\nnpm i\n```\nEOF',
    'cat > n.md <<EOF\nAn unterminated `tick here.\nEOF',
    'cat > n.md <<"EOF"\n`rm -rf victim`\nEOF',
    'cat > n.md <<\\EOF\n`rm -rf victim`\nEOF',
    'cat <<EOF > config.json\n{"home": "$HOME", "name": "x"}\nEOF',
    "python3 - <<'EOF'\nimport os\nprint(os.getcwd())\nEOF",
    "bash <<'EOF'\nset -e\nrm -rf build\nEOF",
    "bash <<'EOF'\n\\rm -rf victim\nEOF",
    "cat > paper.tex <<'EOF'\n\\documentclass{article}\n\\begin{document}\nHi\n\\end{document}\nEOF",
    'cat > paper.tex <<EOF\n\\documentclass{article}\n\\begin{document}\nEOF',
    'cat <<EOF | kubectl apply -f -\napiVersion: v1\nkind: ConfigMap\nEOF',
    'cat > win.txt <<EOF\nC:\\Users\\me\\file\nEOF',
    "cat <<'EOF'\n\\head -1 ../outside/x\nEOF",
    'cat <<EOF\n$(true\n\\rm -rf victim)\nEOF',
    # Changing directory
    'cd src && ls', 'cd .. && ls', 'cd /tmp && rm -rf build', 'cd a/l/../../.. && touch escaped.txt',
    'cd build && cmake .. && make -j8', 'pushd src && cargo build && popd', 'cd "$HOME" && ls',
    'cd ~/work && ls', 'cd deep/er/x/.. && ls', 'cd a/l/.. && ls', 'pushd a/l/../../.. && touch escaped.txt',
    'cd a/l && ls', 'pushd /tmp && cat notes.txt', 'pushd -n /tmp', 'pushd +1', 'popd',
    'cd src && cat ../README.md', 'cd src && echo x > ../out.txt', 'cd src && cat ../../outside/secret',
    'cd src; cat ../README.md', '(cd src) && cat ../README.md', 'cd "$D" && cat ../README.md',
    'cd web && npm install && npm run build && cp -r dist ../build/', 'npm ci && cd src && cat ../README.md',
    'cd src && cargo test 2>&1 | tee ../test.log', 'cd deep/er && cat ../../README.md && cd x && ls',
    'cd /tmp && cat ../etc/hosts', 'cd a/l && cat ../../../README.md', 'cd src && eval true && cat ../README.md',
    'cd nosuch && rm -rf ../build', 'cd && rm -rf Documents', 'cd -',
    'pushd src && pushd +1 && rm -rf ../x',
    # Quoting and nested shells
    'echo "hello world"', "echo 'single $x'", "printf '%s\\n' \"$PATH\"", 'grep -E "a|b" README.md',
    "sh -c 'echo $0' x", 'bash -c "npm run build && npm test"', 'node -e "console.log(1)"',
    "python3 -c 'print(1)'",
    # Line continuations
    'docker run --rm \\\n  -v "$PWD":/app \\\n  -w /app node:22 npm test',
    "curl -fsS -X POST \\\n  -H 'Content-Type: application/json' \\\n  -d '{\"a\":1}' http://localhost:3000/api",
    'cargo test \\\n  -p haarness-core', 'true \\\nhead -1 ../outside/x',
    # Lines that start with a backslash, and other backslash forms
    '\\cp -f a b', '\\rm -rf build', 'ls\n\\rm -rf victim', 'true\n\\rm -rf victim',
    'npm run build\n\\rm -rf victim', 'git status --short\n\\rm -rf victim', 'mkdir -p d\n\\echo ../outside/x',
    'cd src\n\\cp -f a b', 'true &&\n\\rm -rf victim', 'true |\n\\rm -rf victim', 'true;\n\\rm -rf victim',
    'true &\n\\rm -rf victim', 'true\r\n\\rm -rf victim', 'true \\\r\n\\rm -rf victim',
    'true\n\\\nrm -rf victim', 'A=1 B=2\n\\rm -rf victim', 'export A=1\n\\rm -rf victim',
    '# note\n\\rm -rf victim', 'true # c\n\\rm -rf victim', '(true\n\\rm -rf victim)', '{ true\n\\rm -rf victim; }',
    'echo $(true\n\\rm -rf victim)', 'true\n  \t\\rm -rf victim', 'true\n\n\\rm -rf victim',
    'true 2>err.txt\n\\rm -rf victim', 'true\n\\cat ../outside/x', 'true\n\\cd ../outside',
    'rm -f a\n\\rm -f ../outside/x', 'r\\\nm -rf victim', 'git pu\\\nsh --force', "echo 'a\n\\rm x'",
    'echo "a\n\\rm x"', 'true\n\\$x', '\\r\\m -f victim', 'r\\m -f victim', "'rm' -f victim",
    # Multi-line scripts
    'set -e\nnpm ci\nnpm run build\nnpm test', 'for f in src/*.rs; do wc -l "$f"; done',
    'if [ -f .env ]; then echo yes; fi', 'npm test\r\nnpm run build',
    # Wrappers, the variables they set and the folders they choose
    'time cargo build', 'time npm test', 'time -p make', 'env RUST_LOG=1 cargo test',
    'env NODE_ENV=production npm run build', 'env -i PATH=/usr/bin make', 'sudo -E make',
    'sudo -u postgres psql -c "select 1"', 'NODE_ENV=test npm test', 'RUST_LOG=debug cargo run -- serve',
    'nohup npm start > server.log 2>&1 &', 'env -C src ls', 'env -C src cat ../README.md', 'env -Csrc cat main.rs',
    'env --chdir=src cat main.rs', 'env -C src cat ../../outside/secret', 'env --chdir=/tmp cat x',
    'sudo -D /tmp cat x', 'env -C ~/.ssh cat id_rsa', 'env -C "$D" cat x', 'env -C~/x cat y',
    'cd src && env -C ../deep cat ../README.md', 'time A=1 rm -rf ~/Documents', 'time -p A=$(pwd) B+=x rm -rf ~/x',
    'env A+=1 rm -rf ~/x', "env 'A B=1' rm -rf ~/x", 'env A=$(pwd) rm -rf victim', 'sudo A=1 -u root rm -rf victim',
    'env - rm -rf victim', 'env --chdir="$D" rm -rf victim', 'coproc rm -rf ~/x', 'coproc NAME { rm -rf victim; }',
    '! { rm -rf victim; }',
    # Lines of assignments
    'A=1 B=2\nnpm test', 'PORT=3000 HOST=0.0.0.0\nnpm start', 'A=1\n! cat ../outside/x', 'A=1 B=2\n! rm -rf victim',
    'A=1\nif true; then rm -rf victim; fi', 'A=1 B=2\n{ rm -rf victim; }', 'A=1 B=2 # note\nrm -rf victim',
    'A=1 B=2 \\\n  npm test', 'export A=1 B=2\nnpm test', 'A=1 B=2 npm test',
    # Function definitions
    'cat() { echo x; } > notes.md; cat', 'f() { git status; }; f',
    # Files
    'rm -rf node_modules && npm install', 'rm -rf ~/Library/Caches/foo', 'cat ~/.zshrc', 'cat ../outside/secret',
    'cp .env.example .env', 'mv build dist', 'chmod +x run.sh && ./run.sh', 'bash run.sh', 'sudo rm -rf /tmp/x',
    'xargs -n1 echo < files.txt', "find . -name '*.tmp' -delete", 'git clean -fdx', 'kill -9 1234',
    'open http://localhost:3000', 'ln -s ../deep link', 'touch ~/x', 'rm -rf victim',
]


def fixture(root):
    project = root / 'project'
    outside = root / 'outside'
    for folder in ['victim', 'src', 'build', 'deep/er/x', 'a', 'node_modules/pkg', 'd']:
        (project / folder).mkdir(parents=True, exist_ok=True)
    outside.mkdir(parents=True, exist_ok=True)
    for name, text in [('victim/file', 'x'), ('src/main.rs', 'fn main() {}\n'), ('build/out.log', ''),
                       ('node_modules/pkg/index.js', ''), ('.env', 'SECRET=1\n'), ('.env.example', 'SECRET=\n'),
                       ('run.sh', '#!/bin/bash\necho hi\n'), ('list.txt', 'a\n'), ('files.txt', 'a\n'),
                       ('in.txt', 'a\n'), ('notes.txt', ''), ('package.json', '{}\n'), ('README.md', '# x\n')]:
        (project / name).write_text(text)
    (project / 'run.sh').chmod(0o755)
    link = project / 'a' / 'l'
    if not link.is_symlink():
        link.symlink_to('../deep/er/x')
    for name in ['secret', 'a', 'x']:
        (outside / name).write_text('outside\n')
    return project


def run(binary, out, root=None):
    holder = None if root else tempfile.TemporaryDirectory(prefix='haarness-permission-corpus-')
    root = Path(root or holder.name)
    root.mkdir(parents=True, exist_ok=True)
    root = root.resolve()
    project = fixture(root)
    corpus = root / 'corpus.json'
    corpus.write_text(json.dumps(CORPUS))
    raw = root / 'raw.jsonl'
    env = {**os.environ, 'HAARNESS_PERMISSION_CORPUS': str(corpus),
           'HAARNESS_PERMISSION_CORPUS_PROJECT': str(project), 'HAARNESS_PERMISSION_CORPUS_OUT': str(raw)}
    subprocess.run([binary, '--exact', 'runtime::permission_corpus::permission_corpus', '--ignored',
                    '--test-threads', '1'], env=env, check=True, capture_output=True)
    text = raw.read_text()
    # Paths differ between runs: name them by what they are.
    for path, name in [(str(root), '<root>'), (str(Path.home()), '<home>')]:
        text = text.replace(json.dumps(path)[1:-1], name)
    Path(out).write_text(text)
    print(f'{len(CORPUS)} lines read into {out}')


def changes(before, after, path=''):
    if isinstance(before, dict) and isinstance(after, dict):
        found = []
        for key in sorted(set(before) | set(after)):
            found += changes(before.get(key), after.get(key), f'{path}.{key}' if path else key)
        return found
    return [] if before == after else [(path, before, after)]


def diff(before_file, after_file):
    before = [json.loads(line) for line in Path(before_file).read_text().splitlines()]
    after = {entry['line']: entry for entry in (json.loads(line) for line in Path(after_file).read_text().splitlines())}
    changed = 0
    loosened = []
    stricter_default = []
    for entry in before:
        other = after.get(entry['line'])
        if other is None:
            continue
        found = changes(entry, other)
        if not found:
            continue
        changed += 1
        print('=' * 72)
        print(json.dumps(entry['line']))
        for path, old, new in found:
            print(f'  {path}:\n    before {json.dumps(old)}\n    after  {json.dumps(new)}')
        for key, decision in entry['decisions'].items():
            old, new = decision['decision'], other['decisions'][key]['decision']
            order = {'allow': 0, 'ask': 1, 'deny': 2}
            if order[new] < order[old]:
                loosened.append((entry['line'], key, old, new))
            if key in ('default', 'default+always') and order[new] > order[old]:
                stricter_default.append((entry['line'], key, old, new))
    print('=' * 72)
    print(f'{changed} of {len(before)} lines changed')
    for line, key, old, new in stricter_default:
        print(f'DEFAULT STRICTER {key}: {old} -> {new}: {json.dumps(line)}')
    for line, key, old, new in loosened:
        print(f'LOOSER {key}: {old} -> {new}: {json.dumps(line)}')


if __name__ == '__main__':
    if len(sys.argv) >= 4 and sys.argv[1] == 'run':
        root = sys.argv[sys.argv.index('--root') + 1] if '--root' in sys.argv else None
        run(sys.argv[2], sys.argv[3], root)
    elif len(sys.argv) == 4 and sys.argv[1] == 'diff':
        diff(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)
