#!/bin/sh
set -eu

repository="${HAARNESS_REPOSITORY:-haarness/haarness}"
version="${HAARNESS_VERSION:-latest}"
install_dir="${HAARNESS_INSTALL_DIR:-$HOME/.local/bin}"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  Darwin-x86_64) target="x86_64-apple-darwin" ;;
  Linux-aarch64) target="aarch64-unknown-linux-gnu" ;;
  Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
  *) echo "unsupported platform" >&2; exit 1 ;;
esac
if [ "$version" = latest ]; then
  version="$(curl -fsSL "https://api.github.com/repos/$repository/releases/latest" | sed -n 's/.*"tag_name": "\([^"]*\)".*/\1/p')"
fi
base="https://github.com/$repository/releases/download/$version"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT INT TERM
curl -fsSL "$base/haarness-$target" -o "$temporary/haarness"
curl -fsSL "$base/SHA256SUMS" -o "$temporary/SHA256SUMS"
(cd "$temporary" && grep "haarness-$target" SHA256SUMS | sed "s/haarness-$target/haarness/" | shasum -a 256 -c -)
mkdir -p "$install_dir"
chmod 755 "$temporary/haarness"
mv "$temporary/haarness" "$install_dir/haarness"
echo "Installed $install_dir/haarness"

