#!/usr/bin/env bash
# Run the wlroots foreign-toplevel tests against a headless Sway.
#
# Why this exists: the Sway SIGABRT (I-011) shipped because neither developer
# machine runs wlroots. KDE and GNOME do not advertise
# `zwlr_foreign_toplevel_manager_v1`, so the entire Dispatch state machine is
# dead code there and every test of it passes vacuously. A hand-written Wayland
# dispatch that panics inside a C callback cannot unwind — it aborts the
# process, which is total startup failure, not a degraded feature.
#
# Headless Sway advertises the protocol for real, so the same code path runs
# here as on a user's machine. Verified: removing the `event_created_child!`
# specialization reproduces the original
# "Missing event_created_child specialization for event opcode 0" abort, and
# these tests fail.
#
# Usage:  scripts/test-wlroots.sh
# Needs:  sway (WLR_BACKENDS=headless), foot
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v sway >/dev/null; then
  echo "sway not installed — skipping wlroots tests" >&2
  echo "  Fedora: sudo dnf install sway foot" >&2
  echo "  Debian: sudo apt-get install sway foot" >&2
  exit 127
fi

RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp}"
WORK="$(mktemp -d)"
trap 'cleanup' EXIT

SWAY_PID=""
cleanup() {
  [ -n "$SWAY_PID" ] && kill "$SWAY_PID" 2>/dev/null || true
  rm -rf "$WORK"
}

cat > "$WORK/sway.conf" <<'CONF'
output HEADLESS-1 resolution 1280x720
CONF

# Pick a display name that is definitely free — a developer running this on
# their own Wayland desktop already owns wayland-0, and silently attaching to
# THAT compositor would make the test assert against the wrong thing.
for n in $(seq 1 20); do
  if [ ! -e "$RUNTIME_DIR/wayland-$n" ]; then
    DISPLAY_NAME="wayland-$n"
    break
  fi
done
: "${DISPLAY_NAME:?no free wayland display slot}"

echo "Starting headless Sway on $DISPLAY_NAME..."
WLR_BACKENDS=headless \
WLR_LIBINPUT_NO_DEVICES=1 \
WAYLAND_DISPLAY="$DISPLAY_NAME" \
  sway -c "$WORK/sway.conf" > "$WORK/sway.log" 2>&1 &
SWAY_PID=$!

for _ in $(seq 1 40); do
  [ -e "$RUNTIME_DIR/$DISPLAY_NAME" ] && break
  sleep 0.25
done
if [ ! -e "$RUNTIME_DIR/$DISPLAY_NAME" ]; then
  echo "Sway failed to start:" >&2
  cat "$WORK/sway.log" >&2
  exit 1
fi

# A compositor with no windows never emits the `toplevel` event — which is the
# event that creates the child object and aborted. Without one open, these
# tests pass without executing the code they exist to test.
SWAYSOCK="$(ls -t "$RUNTIME_DIR"/sway-ipc.*."$SWAY_PID".sock 2>/dev/null | head -1 ||
            ls -t "$RUNTIME_DIR"/sway-ipc.*.sock 2>/dev/null | head -1)"
if [ -n "$SWAYSOCK" ] && command -v foot >/dev/null; then
  echo "Opening a toplevel so the child-object path is exercised..."
  SWAYSOCK="$SWAYSOCK" swaymsg exec foot >/dev/null 2>&1 || true
  # Poll sway's own view count rather than sleeping a guessed interval. The
  # X11 harness failed on CI for exactly this reason: a fixed sleep is a bet on
  # machine speed, and a loaded runner loses it. Here the compositor can be
  # asked directly whether a toplevel exists — which is the precondition these
  # tests need.
  for _ in $(seq 1 60); do   # up to 15s
    views=$(SWAYSOCK="$SWAYSOCK" swaymsg -t get_tree 2>/dev/null |
            grep -c '"type": *"con"' || true)
    [ "${views:-0}" -gt 0 ] && break
    sleep 0.25
  done
  if [ "${views:-0}" -eq 0 ]; then
    echo "WARNING: no toplevel appeared within 15s — the child-object path may" >&2
    echo "         not be exercised by this run." >&2
  fi
else
  echo "WARNING: no foot/swaysock — running with zero toplevels, which does" >&2
  echo "         NOT exercise the event_created_child path." >&2
fi

echo "Running tests..."
WAYLAND_DISPLAY="$DISPLAY_NAME" \
  cargo test -p lychi-core --test wlroots_toplevel_live -- --ignored --nocapture
