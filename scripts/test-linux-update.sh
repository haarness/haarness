#!/bin/sh
# Proves Desktop's AppImage self-update in a Linux desktop session, in Docker on this machine's own architecture
# (arm64 on a Mac with Apple silicon, natively): install.sh installs the channel's current Desktop as a user would
# (on Ubuntu 24.04, so without libfuse2, through the haarness-desktop launcher), and a stand-in release service on
# loopback offers it a newer version whose file is the previous release's AppImage on the release domain, with the
# size and SHA-256 the service published for it. The driver checks for updates in Settings → About, waits for
# Restart to update, restarts, and requires the AppImage replaced in place with those bytes, a successful install
# result, the app reopened with its flags (--no-sandbox) and still running, and, opened again, the other version.
# With FROM_VERSION, the release itself is the proof: install.sh installs that earlier version, and the app updates
# from the release service to the channel's current version, with no stand-in.
#   scripts/test-linux-update.sh [out dir]
#   FROM_VERSION=0.1.6 scripts/test-linux-update.sh [out dir]
# Env: HAARNESS_RELEASE_SERVICE (https://haarness.ai), CHANNEL (stable), FROM_VERSION. Needs Docker and
# apps/desktop's node_modules.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
out=${1:-$(mktemp -d "${TMPDIR:-/tmp}/haarness-linux-update.XXXXXX")}
service=${HAARNESS_RELEASE_SERVICE:-https://haarness.ai}
channel=${CHANNEL:-stable}
mkdir -p "$out"
case "$(uname -m)" in arm64|aarch64) arch=arm64;; *) arch=x64;; esac
rm -f "$out/payload.json"
# Without FROM_VERSION: the previous release's AppImage for this architecture, as the service published it.
[ -n "${FROM_VERSION:-}" ] || curl -fsS "$service/api/v1/releases/history?channel=$channel&limit=10" | python3 -c '
import json, sys
arch, out = sys.argv[1], sys.argv[2]
versions = []
for row in json.load(sys.stdin):
    manifest = row["release"]["manifest"]
    for item in manifest["products"].get("desktop", {}).get("artifacts", []):
        if item["os"] == "linux" and item["arch"] == arch and item["kind"] == "AppImage":
            versions.append({**item, "version": manifest["products"]["desktop"].get("version", manifest["version"])})
if len(versions) < 2:
    sys.exit("The channel needs two releases with a Linux AppImage.")
json.dump(versions[1], open(out + "/payload.json", "w"))
print("current", versions[0]["version"], "- the update offered is", versions[1]["version"] + "'"'"'s file")
' "$arch" "$out"
docker build -q --target desktop -t haarness-linux-update-test:24.04 "$here/linux-update" >/dev/null
docker run --rm --init --shm-size=1g \
  -e HAARNESS_RELEASE_SERVICE="$service" -e CHANNEL="$channel" -e FROM_VERSION="${FROM_VERSION:-}" \
  -v "$root/apps/desktop/node_modules:/work/node_modules:ro" \
  -v "$here/linux-update:/work/test:ro" \
  -v "$here/linux-desktop-session.sh:/work/linux-desktop-session.sh:ro" \
  -v "$out:/work/out" \
  haarness-linux-update-test:24.04 /work/linux-desktop-session.sh /work/test/run.sh 2>&1 | grep -v 'dbus-run-session: ignoring'
echo "Screenshots and requests: $out"
