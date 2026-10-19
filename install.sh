#!/bin/sh
# Install a checksum-verified CLI or Desktop from Haarness Cloud. No sudo required.
set -eu
base=${HAARNESS_RELEASE_URL:-https://haarness.ai}
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
if [ "$version" != latest ]; then
  # All other allowed version characters are URL-unreserved. A literal + in
  # SemVer build metadata must not be decoded as a space by the query parser.
  encoded_version=$(printf '%s' "$version" | sed 's/+/%2B/g')
  url="$url&version=$encoded_version"
fi
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
    if ! actual_version=$("$stage/payload" --version 2>"$stage/error"); then
      echo 'The downloaded Haarness does not run on this system; installation left unchanged.' >&2
      cat "$stage/error" >&2
      [ "$os" != linux ] || echo 'Haarness for Linux needs glibc 2.17 or newer; musl systems such as Alpine are not supported yet.' >&2
      exit 1
    fi
    [ "$actual_version" = "haarness $released" ] || { echo "Binary version mismatch: $actual_version" >&2; exit 1; }
  fi
  mv -f "$stage/payload" "$destination/$name"
fi
# FUSE 2 mounts an AppImage; Ubuntu has not installed it by default since 22.04.
has_fuse2() {
  { ldconfig -p || /sbin/ldconfig -p; } 2>/dev/null | grep -q 'libfuse\.so\.2'
}
# The AppImage gets what the .deb gives: a haarness-desktop command, a menu
# entry with its icon, and the haarness:// link handler. Without FUSE 2 the
# command has the AppImage unpack itself to run, which starts more slowly.
install_linux_launcher() {
  data=${XDG_DATA_HOME:-$HOME/.local/share}
  launcher="$destination/haarness-desktop"
  quoted=$(printf '%s' "$destination/$name" | sed "s/'/'\\\\''/g")
  cat > "$stage/launcher" <<LAUNCHER
#!/bin/sh
if ! { { ldconfig -p || /sbin/ldconfig -p; } 2>/dev/null | grep -q 'libfuse\.so\.2'; }; then
  export APPIMAGE_EXTRACT_AND_RUN=1
fi
exec '$quoted' "\$@"
LAUNCHER
  chmod 755 "$stage/launcher"
  mv -f "$stage/launcher" "$launcher"
  icon=haarness-desktop
  # Extracting files from an AppImage needs no FUSE.
  if (cd "$stage" && "$destination/$name" --appimage-extract 'usr/share/icons/*' >/dev/null 2>&1); then
    found=$(find "$stage/squashfs-root/usr/share/icons" -name haarness-desktop.png 2>/dev/null | sort -V | tail -n 1)
    if [ -n "$found" ]; then
      mkdir -p "$data/icons"
      cp "$found" "$data/icons/haarness-desktop.png"
      icon="$data/icons/haarness-desktop.png"
    fi
  fi
  exec_path=$(printf '%s' "$launcher" | sed 's/[\\"`$]/\\&/g')
  mkdir -p "$data/applications"
  cat > "$stage/haarness-desktop.desktop" <<ENTRY
[Desktop Entry]
Type=Application
Name=Haarness
Comment=Desktop app for Haarness, a coding agent that works in your project folders.
Exec="$exec_path" %U
Icon=$icon
Terminal=false
Categories=Development;
MimeType=x-scheme-handler/haarness;
StartupWMClass=haarness-desktop
ENTRY
  mv -f "$stage/haarness-desktop.desktop" "$data/applications/haarness-desktop.desktop"
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$data/applications" >/dev/null 2>&1 || true
  fi
}
[ "$product:$os" != desktop:linux ] || install_linux_launcher
printf 'Installed Haarness %s: %s/%s\n' "$released" "$destination" "$name"
if [ "$product" = cli ]; then
  case ":$PATH:" in *":$destination:"*) ;; *) printf 'Add to your shell profile: export PATH="%s:$PATH"\n' "$destination";; esac
elif [ "$os" = linux ]; then
  printf 'Start Haarness from your applications menu, or run %s/haarness-desktop.\n' "$destination"
  has_fuse2 || echo 'Installing FUSE 2 (libfuse2; libfuse2t64 on Ubuntu 24.04 and later) lets it start faster.'
else
  printf 'Open %s/%s to launch Desktop.\n' "$destination" "$name"
fi
