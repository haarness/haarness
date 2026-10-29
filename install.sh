#!/bin/sh
# Install checksum-verified Haarness from Haarness Cloud: the CLI, then Desktop. No sudo required.
#   curl -fsSL https://haarness.ai/install.sh | sh                      CLI and Desktop
#   curl -fsSL https://haarness.ai/install.sh | sh -s -- --no-desktop   CLI only (or HAARNESS_NO_DESKTOP=1)
#   curl -fsSL https://haarness.ai/install-router.sh | sh               Haarness Router
# HAARNESS_PRODUCT=cli, desktop or router installs only that one (`haarness upgrade` installs the CLI this way).
# install-router.sh is this script with router as the default product: the Cloud changes only the next line.
set -eu
default_product=all
base=${HAARNESS_RELEASE_URL:-https://haarness.ai}
channel=${HAARNESS_CHANNEL:-stable}
version=${HAARNESS_VERSION:-latest}
product=${HAARNESS_PRODUCT:-$default_product}
case "${HAARNESS_NO_DESKTOP:-0}" in ''|0|false|no) no_desktop=0;; *) no_desktop=1;; esac
usage() {
  if [ "$default_product" = router ]; then
    cat <<'USAGE'
Install Haarness Router, the OpenAI-compatible model router you run yourself.

  curl -fsSL https://haarness.ai/install-router.sh | sh

HAARNESS_CHANNEL        stable (default) or preview
HAARNESS_VERSION        a published version (default: latest)
HAARNESS_INSTALL_DIR    where haarness-router goes (default ~/.local/bin)
USAGE
    return
  fi
  cat <<'USAGE'
Install Haarness: the CLI, then Desktop (macOS, and Linux with a graphical session).

  curl -fsSL https://haarness.ai/install.sh | sh -s -- [--no-desktop]

  --no-desktop   Install only the CLI (or set HAARNESS_NO_DESKTOP=1)

HAARNESS_PRODUCT        cli, desktop or router: install only that one
HAARNESS_CHANNEL        stable (default) or preview
HAARNESS_VERSION        a published version (default: latest)
HAARNESS_INSTALL_DIR    the CLI and router folder (default ~/.local/bin), or Desktop's with HAARNESS_PRODUCT=desktop
HAARNESS_DESKTOP_DIR    the Desktop folder (macOS: /Applications when writable, else ~/Applications;
                        Linux: ~/.local/bin)
USAGE
}
for argument in "$@"; do
  case "$argument" in
    --no-desktop) no_desktop=1;;
    -h|--help) usage; exit 0;;
    *) echo "Unknown option: $argument" >&2; usage >&2; exit 1;;
  esac
done
# A user name would move the host: curl sends `http://localhost:@example.com` to example.com.
case "$base" in *@*|*\\*) echo 'Release URL cannot hold a user name, a password or a backslash.' >&2; exit 1;; esac
case "$base" in https://*|http://127.0.0.1:*|http://localhost:*) ;; *) echo 'Release URL requires HTTPS (HTTP is allowed only on loopback).' >&2; exit 1;; esac
case "$channel" in stable|preview) ;; *) echo 'Invalid release channel' >&2; exit 1;; esac
case "$version" in *[!a-zA-Z0-9._+-]*|'') echo 'Invalid release version' >&2; exit 1;; esac
case "$product" in all|cli|desktop|router) ;; *) echo 'Invalid product' >&2; exit 1;; esac
case "$(uname -s)" in Darwin) os=macos;; Linux) os=linux;; *) echo 'Use install.ps1 on Windows' >&2; exit 1;; esac
case "$(uname -m)" in arm64|aarch64) arch=arm64;; x86_64|amd64) arch=x64;; *) echo 'Unsupported architecture' >&2; exit 1;; esac

default_desktop_dir() {
  if [ "$os" = linux ]; then
    printf '%s\n' "$HOME/.local/bin"
  # An app already in ~/Applications is updated where it is; otherwise /Applications when this user may write there.
  elif [ ! -d "$HOME/Applications/Haarness.app" ] && [ -d /Applications ] && [ -w /Applications ]; then
    printf '/Applications\n'
  else
    printf '%s\n' "$HOME/Applications"
  fi
}

# FUSE 2 mounts an AppImage; Ubuntu has not installed it by default since 22.04.
has_fuse2() {
  { ldconfig -p || /sbin/ldconfig -p; } 2>/dev/null | grep -q 'libfuse\.so\.2'
}

# Installs one product into one folder. A subshell, so `exit` and the staging cleanup stay inside it: a failed
# Desktop install leaves the CLI that was just installed.
install_product() (
  set -e
  product=$1 destination=$2
  # name: the file installed; title: what messages call it.
  case "$product:$os" in
    cli:*) kind=binary; name=haarness; title=Haarness;;
    router:*) kind=binary; name=haarness-router; title='Haarness Router';;
    desktop:macos) kind=zip; name=Haarness.app; title='Haarness Desktop';;
    desktop:linux) kind=AppImage; name=Haarness.AppImage; title='Haarness Desktop';;
  esac
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
  # A progress bar on a terminal (stderr stays one even under `curl ... | sh`); quiet in scripts and logs.
  progress=--silent
  if [ -t 2 ]; then
    progress=--progress-bar
    case "$product" in cli) label='Haarness CLI';; *) label=$title;; esac
    printf 'Downloading %s for %s %s...\n' "$label" "$os" "$arch" >&2
  fi
  # Downloads $1 to $stage/payload (headers in $stage/headers, HTTP status in $status) in up to three tries; false
  # when all three were interrupted. A try after an interruption asks for only the rest (-C -); If-Range with the
  # file's ETag has the server send its current file whole instead if that changed meanwhile, which curl refuses
  # (33), so the try after that starts over. A response with no ETag cannot be continued: its partial file is dropped.
  download() {
    etag='' try=1
    while :; do
      if status=$(curl "$progress" --show-error --connect-timeout 20 --max-time 900 --proto '=https,http' -C - -H "If-Range: $etag" "$1" -D "$stage/headers" -o "$stage/payload" -w '%{http_code}'); then
        case "$status" in 5??) rm -f "$stage/payload" "$stage/headers"; etag='';; *) return 0;; esac
      elif [ $? -eq 33 ]; then
        rm -f "$stage/payload" "$stage/headers"; etag=''
      else
        etag=$(awk 'tolower($1)=="etag:" {gsub("\r", "", $2); print $2}' "$stage/headers" 2>/dev/null) || etag=''
        [ -n "$etag" ] || rm -f "$stage/payload"
      fi
      [ "$try" -lt 3 ] || return 1
      try=$((try + 1))
      echo "The download was interrupted; trying again ($try of 3)..." >&2
      sleep 2
    done
  }
  # Asked with HEAD, the service names the file's checksum, version and, in Content-Location, its address on the
  # release domain, which serves it from Cloudflare's edge. Without that address (an older service, one that
  # refuses HEAD) or when the edge does not deliver, the file comes from the service itself.
  edge=''
  if curl --silent --fail --head --connect-timeout 20 --max-time 60 --proto '=https,http' "$url" -D "$stage/resolved" -o /dev/null; then
    edge=$(awk 'tolower($1)=="content-location:" {gsub("\r", "", $2); print $2}' "$stage/resolved")
    case "$edge" in
      https://*) ;;
      # Plain HTTP only from a loopback service, which is all the check at the top lets through (tests).
      http://127.0.0.1:*|http://localhost:*) case "$base" in http://*) ;; *) edge='';; esac;;
      *) edge='';;
    esac
  fi
  headers="$stage/resolved" status=''
  [ -z "$edge" ] || download "$edge" || status=''
  case "$status" in
    200|206|416) ;;
    *)
      [ -z "$edge" ] || echo 'The release download site did not deliver; downloading from the release service instead.' >&2
      rm -f "$stage/payload" "$stage/headers"
      headers="$stage/headers"
      download "$url" || { echo 'Could not reach the Haarness release service; installation left unchanged.' >&2; exit 1; };;
  esac
  case "$status" in
    # 206 and 416 answer a try that continued; the checksum below covers the whole file.
    200|206|416) ;;
    404)
      if [ "$version" = latest ]; then echo "No $channel release of Haarness $product for $os $arch yet; installation left unchanged." >&2
      else echo "Haarness $product $version is not on the $channel channel for $os $arch; installation left unchanged." >&2; fi
      exit 1;;
    *) echo "The Haarness release service answered HTTP $status; installation left unchanged." >&2; exit 1;;
  esac
  # The checksum and version come from the service, whichever server sent the bytes.
  expected=$(awk 'tolower($1)=="x-content-sha256:" {gsub("\r", "", $2); print $2}' "$headers")
  released=$(awk 'tolower($1)=="x-haarness-version:" {gsub("\r", "", $2); print $2}' "$headers")
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
  # Without quarantine Gatekeeper does not ask about a developer signature; what macOS still needs (on Apple
  # silicon) is a valid signature of any kind, so a copy whose signature does not verify is signed here, ad hoc,
  # keeping its entitlements and hardened runtime.
  ensure_signature() {
    if [ "$os" = macos ] && ! codesign --verify --deep --strict "$1" >/dev/null 2>&1; then
      codesign --force --deep --sign - --preserve-metadata=entitlements,requirements,flags,runtime "$1" >/dev/null 2>&1 &&
        codesign --verify --deep --strict "$1" >/dev/null 2>&1 ||
        { echo 'The download has no valid signature and could not be signed here; installation left unchanged.' >&2; exit 1; }
    fi
  }
  if [ "$product:$os" = desktop:macos ]; then
    ditto -x -k "$stage/payload" "$stage/app"
    [ -d "$stage/app/Haarness.app" ] || { echo 'Invalid Desktop archive' >&2; exit 1; }
    clear_quarantine "$stage/app/Haarness.app"
    ensure_signature "$stage/app/Haarness.app"
    # Keep the old app recoverable until the replacement is in place.
    [ ! -e "$destination/$name" ] || mv "$destination/$name" "$stage/previous.app"
    if ! mv "$stage/app/Haarness.app" "$destination/$name"; then
      [ ! -e "$stage/previous.app" ] || mv "$stage/previous.app" "$destination/$name"
      exit 1
    fi
  else
    chmod 755 "$stage/payload"
    clear_quarantine "$stage/payload"
    # The CLI and the router are programs: signed if need be, then run once to show they start here.
    if [ "$kind" = binary ]; then
      ensure_signature "$stage/payload"
      if ! actual_version=$("$stage/payload" --version 2>"$stage/error"); then
        echo "The downloaded $title does not run on this system; installation left unchanged." >&2
        cat "$stage/error" >&2
        [ "$os" != linux ] || echo "$title for Linux needs glibc 2.17 or newer; musl systems such as Alpine are not supported yet." >&2
        exit 1
      fi
      [ "$actual_version" = "$name $released" ] || { echo "Binary version mismatch: $actual_version" >&2; exit 1; }
    fi
    mv -f "$stage/payload" "$destination/$name"
  fi
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
  case "$product" in router) installed=$title;; *) installed=Haarness;; esac
  printf 'Installed %s %s: %s/%s\n' "$installed" "$released" "$destination" "$name"
  if [ "$kind" = binary ]; then
    run=$name
    case ":$PATH:" in *":$destination:"*) ;; *) run="$destination/$name"; printf 'Add to your shell profile: export PATH="%s:$PATH"\n' "$destination";; esac
    [ "$product" != router ] ||
      printf 'Run %s to start it with the provider keys in your environment; its dashboard is at http://127.0.0.1:8080/ui\n' "$run"
  elif [ "$os" = linux ]; then
    printf 'Start Haarness from your applications menu, or run %s/haarness-desktop.\n' "$destination"
    has_fuse2 || echo 'Installing FUSE 2 (libfuse2; libfuse2t64 on Ubuntu 24.04 and later) lets it start faster.'
  else
    printf 'Open %s/%s to launch Desktop.\n' "$destination" "$name"
  fi
)

case "$product" in
  cli|router) install_product "$product" "${HAARNESS_INSTALL_DIR:-$HOME/.local/bin}";;
  desktop) install_product desktop "${HAARNESS_INSTALL_DIR:-$(default_desktop_dir)}";;
  all)
    install_product cli "${HAARNESS_INSTALL_DIR:-$HOME/.local/bin}"
    retry="curl -fsSL ${base%/}/install.sh | HAARNESS_PRODUCT=desktop sh"
    if [ "$no_desktop" = 1 ]; then
      :
    elif [ "$os" = linux ] && [ -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
      echo "No graphical session, so Haarness Desktop was not installed. To add it on a desktop: $retry"
    else
      # The CLI is installed whatever happens to Desktop; a Desktop failure is reported, not fatal.
      set +e
      install_product desktop "${HAARNESS_DESKTOP_DIR:-$(default_desktop_dir)}"
      desktop_status=$?
      set -e
      [ "$desktop_status" -eq 0 ] || echo "Haarness Desktop was not installed (the CLI is). To retry: $retry" >&2
    fi;;
esac
