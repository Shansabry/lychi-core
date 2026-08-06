#!/usr/bin/env bash
# Run the X11 active-window tests against a headless X server.
#
# `detect_x11` is the most widely used of the three window backends — every X11
# session on every desktop — and it had no test at all. It reads EWMH properties
# (`_NET_ACTIVE_WINDOW`, `_NET_WM_NAME`, `WM_CLASS`, `_NET_WM_PID`), so it needs
# both an X server AND a window manager that sets them. Xvfb alone is not
# enough: with no WM, `_NET_ACTIVE_WINDOW` is never set, `detect_x11` correctly
# returns None, and the tests would pass having asserted nothing.
#
# Verified: typo'ing the `_NET_WM_PID` atom — the classic silent-None bug, which
# compiles fine and is indistinguishable from "no window focused" — fails these
# tests.
#
# Usage:  scripts/test-x11.sh
# Needs:  Xvfb, openbox, xterm
set -euo pipefail

cd "$(dirname "$0")/.."

for tool in Xvfb openbox xterm; do
  if ! command -v "$tool" >/dev/null; then
    echo "$tool not installed — skipping X11 tests" >&2
    echo "  Fedora: sudo dnf install xorg-x11-server-Xvfb openbox xterm" >&2
    echo "  Debian: sudo apt-get install xvfb openbox xterm" >&2
    exit 127
  fi
done

XVFB_PID=""; WM_PID=""; APP_PID=""
cleanup() {
  for pid in "$APP_PID" "$WM_PID" "$XVFB_PID"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT

# Pick a display number nothing is using — attaching to the developer's real
# X server would test the wrong thing (and pop windows onto their desktop).
for n in $(seq 90 120); do
  if [ ! -e "/tmp/.X11-unix/X$n" ]; then
    DISP=":$n"
    break
  fi
done
: "${DISP:?no free X display}"

echo "Starting Xvfb on $DISP..."
Xvfb "$DISP" -screen 0 1280x720x24 >/dev/null 2>&1 &
XVFB_PID=$!
for _ in $(seq 1 40); do
  [ -e "/tmp/.X11-unix/X${DISP#:}" ] && break
  sleep 0.25
done

# The window manager is what makes this a real test: it owns the EWMH
# properties the detector reads.
echo "Starting openbox..."
DISPLAY="$DISP" openbox >/dev/null 2>&1 &
WM_PID=$!
sleep 1

echo "Opening a window..."
DISPLAY="$DISP" xterm >/dev/null 2>&1 &
APP_PID=$!
sleep 2

echo "Running tests..."
DISPLAY="$DISP" \
  cargo test -p lychi-core --test x11_active_window_live -- --ignored --nocapture
