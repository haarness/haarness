#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
native="$root/native"
zig=${1:-zig}
version=0.5.9

if [ "$("$zig" version)" != "0.16.0" ]; then
  echo "OpenTUI v$version requires Zig 0.16.0" >&2
  exit 1
fi

(cd "$native" && sh scripts/prepare-zig-deps.sh)
(cd "$native" && "$zig" build -Dall=true -Doptimize=ReleaseFast --summary all)

temporary=$(mktemp -d "${TMPDIR:-/tmp}/haarness-opentui.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
mkdir -p "$root/artifacts"

package() {
  name=$1
  source=$2
  stage="$temporary/opentui-native-v$version-$name"
  mkdir -p "$stage"
  cp "$source" "$stage/"
  cp "$root/LICENSE" "$stage/LICENSE-OPENTUI"
  cp "$native/src/vendor/wuffs/LICENSE" "$stage/LICENSE-WUFFS"
  cp "$native/src/vendor/stb/LICENSE" "$stage/LICENSE-STB"
  cp "$native/src/vendor/libwebp/COPYING" "$stage/LICENSE-LIBWEBP"
  cp "$native/src/vendor/libwebp/PATENTS" "$stage/PATENTS-LIBWEBP"
  cp "$native/src/vendor/libwebp/AUTHORS" "$stage/AUTHORS-LIBWEBP"
  cp "$native/src/vendor/lcms2/LICENSE" "$stage/LICENSE-LCMS2"
  tar -xOf "$native/src/vendor/zig-deps.tar.gz" ghostty/LICENSE > "$stage/LICENSE-GHOSTTY"
  find "$stage" -exec touch -t 198001010000 {} +
  archive="$root/artifacts/opentui-native-v$version-$name.zip"
  rm -f "$archive"
  (cd "$temporary" && zip -X -q -r "$archive" "$(basename "$stage")")
}

package darwin-arm64 "$native/lib/aarch64-macos/libopentui.dylib"
package darwin-x64 "$native/lib/x86_64-macos/libopentui.dylib"
package linux-arm64 "$native/lib/aarch64-linux/libopentui.so"
package linux-x64 "$native/lib/x86_64-linux/libopentui.so"
package linux-arm64-musl "$native/lib/aarch64-linux-musl/libopentui.so"
package linux-x64-musl "$native/lib/x86_64-linux-musl/libopentui.so"
package windows-arm64 "$native/lib/aarch64-windows/opentui.dll"
package windows-x64 "$native/lib/x86_64-windows/opentui.dll"

shasum -a 256 "$root"/artifacts/opentui-native-v"$version"-*.zip
