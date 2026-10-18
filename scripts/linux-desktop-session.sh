#!/bin/sh
# Run a command the way a Linux desktop login would host Haarness Desktop:
# a D-Bus session, an unlocked keyring (Desktop refuses to keep remote-server
# credentials in plain text) and a virtual display.
#   scripts/linux-desktop-session.sh npm --prefix apps/desktop run test:e2e
set -eu
if [ -z "${HAARNESS_DESKTOP_SESSION:-}" ]; then
  HAARNESS_DESKTOP_SESSION=1 exec dbus-run-session -- "$0" "$@"
fi
printf 'haarness-test' | gnome-keyring-daemon --unlock --replace >/dev/null 2>&1
gnome-keyring-daemon --start --components=secrets >/dev/null 2>&1
# Chromium chooses its password store from the desktop: under a bare X server
# it would keep secrets in plain text, which Desktop refuses.
export XDG_CURRENT_DESKTOP="${XDG_CURRENT_DESKTOP:-GNOME}"
exec xvfb-run -a -s '-screen 0 1600x1000x24' "$@"
