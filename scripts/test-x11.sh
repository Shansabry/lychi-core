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
# Needs:  Xvfb, openbox, xterm, xprop, xdpyinfo
set -euo pipefail

cd "$(dirname "$0")/.."

for tool in Xvfb openbox xterm xprop xdpyinfo; do
  if ! command -v "$tool" >/dev/null; then
    echo "$tool not installed — skipping X11 tests" >&2
    echo "  Fedora: sudo dnf install xorg-x11-server-Xvfb openbox xterm xorg-x11-utils" >&2
    echo "  Debian: sudo apt-get install xvfb openbox xterm x11-utils" >&2
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
  # "Is anything answering on this display?" — again, ask rather than guess
  # from a filename.
  if ! DISPLAY=":$n" xdpyinfo >/dev/null 2>&1; then
    DISP=":$n"
    break
  fi
done
: "${DISP:?no free X display}"

echo "Starting Xvfb on $DISP..."
Xvfb "$DISP" -screen 0 1280x720x24 >/dev/null 2>&1 &
XVFB_PID=$!

# Wait until the server ANSWERS, not until a socket file appears.
#
# The socket path is not a reliable signal: with `-listen unix` defaults and
# abstract sockets, /tmp/.X11-unix/X<n> may never be created even though the
# display works — this loop silently timed out on Fedora and everything
# downstream then ran against a display that was never checked, surfacing much
# later as "no EWMH _NET_ACTIVE_WINDOW". Querying the server tests the thing
# that has to be true.
for _ in $(seq 1 60); do
  if DISPLAY="$DISP" xprop -root _NET_SUPPORTED >/dev/null 2>&1 \
     || DISPLAY="$DISP" xdpyinfo >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done
if ! DISPLAY="$DISP" xdpyinfo >/dev/null 2>&1; then
  echo "Xvfb did not come up on $DISP within 15s" >&2
  exit 1
fi

# The window manager is what makes this a real test: it owns the EWMH
# properties the detector reads.
echo "Starting openbox..."
DISPLAY="$DISP" openbox >/dev/null 2>&1 &
WM_PID=$!

echo "Opening a window..."
DISPLAY="$DISP" xterm >/dev/null 2>&1 &
APP_PID=$!

# Wait for the CONDITION the tests need, not for a guessed duration.
#
# This used to be `sleep 1` after openbox and `sleep 2` after xterm. Those are
# fine on an idle laptop and race on a loaded CI runner: the job failed with
# "no EWMH _NET_ACTIVE_WINDOW" while the identical commit had passed minutes
# earlier. A fixed sleep encodes a guess about machine speed; polling encodes
# what actually has to be true.
#
# `_NET_ACTIVE_WINDOW` present and non-zero means the WM is up AND has focused
# a window — exactly the precondition, and precisely what the tests read.
echo "Waiting for the window manager to focus a window..."
# `xprop` EXITS 0 even when the atom is absent, printing one of:
#   "_NET_ACTIVE_WINDOW:  not found."            (WM not up yet)
#   "_NET_ACTIVE_WINDOW:  no such atom on any window."
#   "_NET_ACTIVE_WINDOW(WINDOW): window id # 0x0"     (up, nothing focused)
#   "_NET_ACTIVE_WINDOW(WINDOW): window id # 0x40000c" (a real window)
# so the exit status says nothing and only the window id distinguishes them.
active=""
for _ in $(seq 1 80); do   # up to 20s
  id=$(DISPLAY="$DISP" xprop -root _NET_ACTIVE_WINDOW 2>/dev/null \
       | sed -n 's/.*window id # \(0x[0-9a-fA-F]*\).*/\1/p')
  if [ -n "$id" ] && [ "$id" != "0x0" ]; then
    active="$id"
    break
  fi
  sleep 0.25
done
if [ -z "$active" ]; then
  echo "No window became active within 20s on $DISP." >&2
  echo "Last xprop: $(DISPLAY="$DISP" xprop -root _NET_ACTIVE_WINDOW 2>&1 || true)" >&2
  echo "Windows known to the WM: $(DISPLAY="$DISP" xprop -root _NET_CLIENT_LIST 2>&1 || true)" >&2
  exit 1
fi

echo "Running tests..."
DISPLAY="$DISP" \
  cargo test -p lychi-core --test x11_active_window_live -- --ignored --nocapture
