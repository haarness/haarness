#!/bin/sh
# Install a checksum-verified CLI or Desktop from Haarness Cloud. No sudo required.
set -eu
base=${HAARNESS_RELEASE_URL:-https://app.haarness.ai}
channel=${HAARNESS_CHANNEL:-stable}
version=${HAARNESS_VERSION:-latest}
product=${HAARNESS_PRODUCT:-cli}
case "$base" in https://*|http://127.0.0.1:*|http://localhost:*) ;; *) echo 'Release URL requires HTTPS (HTTP is allowed only on loopback).' >&2; exit 1;; esac
case "$channel" in stable|preview) ;; *) echo 'Invalid release channel' >&2; exit 1;; esac
case "$version" in *[!a-zA-Z0-9._+-]*|'') echo 'Invalid release version' >&2; exit 1;; esac
case "$(uname -s)" in Darwin) os=macos;; Linux) os=linux;; *) echo 'Use install.ps1 on Windows' >&2; exit 1;; esac
case "$(uname -m)" in arm64|aarch64) arch=arm64;; x86_64|amd64) arch=x64;; *) echo 'Unsupported architecture' >&2; exit 1;; esac
case "$product:$os" in cli:*) kind=binary; destination=${HAARNESS_INSTALL_DIR:-$HOME/.local/bin}; name=haarness;; desktop:macos) kind=zip; destination=${HAARNESS_INSTALL_DIR:-$HOME/Applications}; name=Haarness.app;; desktop:linux) kind=AppImage; destination=${HAARNESS_INSTALL_DIR:-$HOME/.local/bin}; name=Haarness.AppImage;; *) echo 'Invalid product' >&2; exit 1;; esac
mkdir -p "$destination"
# Stage on the destination filesystem: final rename is atomic for CLI/AppImage.
stage=$(mktemp -d "$destination/.haarness-install.XXXXXXXX")
trap 'rm -rf "$stage"' EXIT HUP INT TERM
url="${base%/}/api/v1/releases/$channel/$product/download?os=$os&arch=$arch&kind=$kind"
if [ "$version" != latest ]; then url="$url&version=$version"; fi
curl --fail --silent --show-error --connect-timeout 20 --max-time 900 --proto '=https,http' "$url" -D "$stage/headers" -o "$stage/payload"
expected=$(awk 'tolower($1)=="x-content-sha256:" {gsub("\r", "", $2); print $2}' "$stage/headers")
released=$(awk 'tolower($1)=="x-haarness-version:" {gsub("\r", "", $2); print $2}' "$stage/headers")
case "$expected" in *[!a-f0-9]*|'') echo 'Missing or invalid SHA-256' >&2; exit 1;; esac
[ ${#expected} -eq 64 ] || { echo 'Invalid SHA-256 length' >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$stage/payload" | cut -d ' ' -f 1); else actual=$(shasum -a 256 "$stage/payload" | cut -d ' ' -f 1); fi
[ "$actual" = "$expected" ] || { echo 'Checksum mismatch; installation left unchanged.' >&2; exit 1; }
[ -n "$released" ] || { echo 'Missing release version' >&2; exit 1; }
if [ "$version" != latest ] && [ "$released" != "$version" ]; then echo 'Release version mismatch' >&2; exit 1; fi
# Clear quarantine only on the verified installation payload.
# Pin the system utility; Python packages also sometimes provide an incompatible xattr.
clear_quarantine() {
  if [ "$os" = macos ]; then
    /usr/bin/xattr -dr com.apple.quarantine "$1" || {
      echo 'Could not clear download quarantine; installation left unchanged.' >&2
      exit 1
    }
  fi
}
if [ "$product:$os" = desktop:macos ]; then
  ditto -x -k "$stage/payload" "$stage/app"
  [ -d "$stage/app/Haarness.app" ] || { echo 'Invalid Desktop archive' >&2; exit 1; }
  clear_quarantine "$stage/app/Haarness.app"
  # Keep the old app recoverable until the replacement is in place.
  [ ! -e "$destination/$name" ] || mv "$destination/$name" "$stage/previous.app"
  if ! mv "$stage/app/Haarness.app" "$destination/$name"; then
    [ ! -e "$stage/previous.app" ] || mv "$stage/previous.app" "$destination/$name"
    exit 1
  fi
else
  chmod 755 "$stage/payload"
  clear_quarantine "$stage/payload"
  if [ "$product" = cli ]; then
    actual_version=$("$stage/payload" --version)
    [ "$actual_version" = "haarness $released" ] || { echo "Binary version mismatch: $actual_version" >&2; exit 1; }
  fi
  mv -f "$stage/payload" "$destination/$name"
fi
printf 'Installed Haarness %s: %s/%s\n' "$released" "$destination" "$name"
if [ "$product" = cli ]; then
  case ":$PATH:" in *":$destination:"*) ;; *) printf 'Add to your shell profile: export PATH="%s:$PATH"\n' "$destination";; esac
else
  printf 'Open %s/%s to launch Desktop.\n' "$destination" "$name"
fi
