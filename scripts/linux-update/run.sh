#!/bin/sh
# In the container, in a desktop session: install the channel's Desktop as a user would, then drive its update.
set -eu
curl -fsSL "$HAARNESS_RELEASE_SERVICE/install.sh" | HAARNESS_PRODUCT=desktop HAARNESS_RELEASE_URL="$HAARNESS_RELEASE_SERVICE" sh 2>&1 | tail -3
file "$HOME/.local/bin/Haarness.AppImage"
ldconfig -p | grep -q 'libfuse\.so\.2' || echo "no libfuse2: the launcher runs the AppImage with APPIMAGE_EXTRACT_AND_RUN=1"
exec node /work/test/driver.mjs
