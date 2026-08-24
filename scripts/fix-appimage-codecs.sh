#!/usr/bin/env bash
# Fix the Tauri AppImage that segfaults on launch (SIGSEGV in _dl_init, before
# main()) on newer-glibc hosts.
#
# ROOT CAUSE: Tauri runs linuxdeploy with `--plugin gtk`, which recursively
# bundles every transitive dependency of WebKitGTK's GStreamer stack — the audio/
# video codec + crypto libs (libmp3lame, libmpg123, libgsm, libogg, libopus,
# libvorbis, libFLAC, libnettle/libhogweed, libp11-kit, libpulse, ...). These are
# NOT on linuxdeploy's default excludelist (unlike libpulse-core/libjack). Bundled
# and preferred via the binary's injected RUNPATH ($ORIGIN/../lib), their ELF
# constructors crash at _dl_init on a host with a newer glibc than the build box.
# Tauri exposes no per-library exclude config, so we fix the AppImage post-build:
# remove every bundled lib the host already provides (its copy is known-good),
# keeping only libs the host genuinely lacks (GTK input-method / pixbuf / print
# plugins), then repack. Result: the loader falls through to host libs and the
# app launches. Also shrinks the AppImage massively (~114MB -> ~15MB here).
#
# Idempotent-ish: safe to re-run; it rebuilds the AppDir->AppImage from the
# already-bundled AppDir that `tauri build` leaves in place.
#
# Requires: patchelf, the linuxdeploy appimage plugin (downloaded by tauri into
# ~/.cache/tauri). Run from the crate root (core/) after `tauri build`.
set -euo pipefail

BUNDLE_DIR="target/release/bundle/appimage"
APPDIR="$BUNDLE_DIR/Lychi.AppDir"
GOOD_BINARY="target/release/lychi-app"
PLUGIN="$HOME/.cache/tauri/linuxdeploy-plugin-appimage.AppImage"

if [ ! -d "$APPDIR" ]; then
  echo "fix-appimage-codecs: no AppDir at $APPDIR — did 'tauri build' run?" >&2
  exit 1
fi
if [ ! -x "$GOOD_BINARY" ]; then
  echo "fix-appimage-codecs: missing $GOOD_BINARY" >&2
  exit 1
fi

# WHAT WE KEEP — an explicit policy, NOT a scan of this machine.
#
# The previous rule was "drop anything the build host also has". That makes the
# artifact a function of whoever built it: on a full KDE workstation the host has
# the entire GStreamer stack, so it all got stripped and the AppImage silently
# depended on the TARGET having it too. It didn't — a GNOME tester's WebProcess
# died on `appsink` and the UI went blank (I-013). The build machine cannot tell
# us what a user's machine has, so it must not be asked.
#
# Instead: keep exactly the plugin modules that are dlopen'd at runtime (no
# DT_NEEDED entry, so nothing auto-detects them) and drop every shared library,
# which we deliberately take from the host. That contract is only safe because
# the app asks the host for very little — see harden_webview() in
# platform/linux.rs, which switches off WebKit's media stack so the GStreamer
# libraries below are never loaded at all.
#
# If Lychi ever needs media, this policy must change with it: bundle the
# GStreamer runtime here AND set GST_PLUGIN_SYSTEM_PATH in AppRun.
KEEP_GLOB='im-*.so io-*.so libpixbufloader-*.so libprintbackend-*.so'

echo "==> Applying bundle policy (keep dlopen'd plugins, drop shared libs)"
kept=0; dropped=0
for f in "$APPDIR"/usr/lib/*.so*; do
  [ -e "$f" ] || continue
  base="$(basename "$f")"
  keep=0
  for pat in $KEEP_GLOB; do
    # shellcheck disable=SC2254 # $pat is a glob on purpose
    case "$base" in $pat) keep=1; break ;; esac
  done
  if [ "$keep" = 1 ]; then
    kept=$((kept + 1))
  else
    rm -f "$f"; dropped=$((dropped + 1))
  fi
done
echo "    kept $kept dlopen'd plugins, dropped $dropped host-provided libs"

# Fail loudly if the policy stops matching reality. A silent zero here would
# mean shipping an AppImage with no input-method or pixbuf plugins, which
# degrades quietly (no CJK input, missing image formats) rather than crashing.
if [ "$kept" -eq 0 ]; then
  echo "fix-appimage-codecs: KEEP_GLOB matched nothing — did linuxdeploy's" >&2
  echo "  layout change? Expected plugins like im-ibus.so in $APPDIR/usr/lib" >&2
  exit 1
fi

# IME fix (#48): linuxdeploy's GTK hook pins GTK to the BUNDLED input-method
# cache via GTK_IM_MODULE_FILE — and the build container has no fcitx/ibus, so
# that cache only lists GTK's stock modules. Result: the host's fcitx5/ibus
# module can never load and CJK input is dead, with no user-side override
# (the hook re-exports on every launch). We do not bundle libgtk — the HOST's
# GTK runs — so the host's own module cache is the correct one. Drop the pin,
# and GTK_EXE_PREFIX with it (its only effect here is re-deriving the same
# bundled path as the fallback). GDK_PIXBUF_MODULE_FILE stays: pixbuf loaders
# ARE deliberately bundled. The now-unreferenced stock immodules go too.
echo "==> Un-pinning GTK input methods from the bundle (host fcitx/ibus must load)"
HOOK="$APPDIR/apprun-hooks/linuxdeploy-plugin-gtk.sh"
if [ ! -f "$HOOK" ]; then
  echo "fix-appimage-codecs: expected GTK hook at $HOOK — did linuxdeploy change?" >&2
  exit 1
fi
sed -i '/export GTK_IM_MODULE_FILE=/d; /export GTK_EXE_PREFIX=/d' "$HOOK"
if grep -q "GTK_IM_MODULE_FILE\|GTK_EXE_PREFIX" "$HOOK"; then
  echo "fix-appimage-codecs: IM-module pin survived the sed — hook format changed?" >&2
  exit 1
fi
rm -rf "$APPDIR"/usr/lib/x86_64-linux-gnu/gtk-3.0/3.0.0/immodules \
       "$APPDIR"/usr/lib/x86_64-linux-gnu/gtk-3.0/3.0.0/immodules.cache

echo "==> Restoring the un-patchelf'd application binary"
# linuxdeploy patchelf-mangles the copied binary (changes size, can corrupt it).
# Replace with the pristine release binary, then set ONLY the rpath needed to
# find the few GTK plugin .so's that legitimately remain bundled.
cp -f "$GOOD_BINARY" "$APPDIR/usr/bin/lychi-app"
patchelf --set-rpath '$ORIGIN/../lib' "$APPDIR/usr/bin/lychi-app"

echo "==> Repacking the AppImage"
# Preserve the bundler's own filename (Lychi_<version>_amd64.AppImage) — a
# hardcoded name here shipped two releases labelled 0.1.0. Captured BEFORE the
# rm, from the file the bundler just wrote.
orig_name="$(basename "$(ls -1 "$BUNDLE_DIR"/*.AppImage | head -1)")"
if [ -z "$orig_name" ]; then
  echo "fix-appimage-codecs: no bundler AppImage found to take the name from" >&2
  exit 1
fi
rm -f "$BUNDLE_DIR"/*.AppImage
# The appimage plugin writes <Name>-<arch>.AppImage into the current directory.
APPIMAGE_EXTRACT_AND_RUN=1 NO_STRIP=1 ARCH=x86_64 \
  "$PLUGIN" --appimage-extract-and-run --appdir "$APPDIR"

# Normalize the output name to match Tauri's convention and move it into place.
packed="$(ls -1 ./*-x86_64.AppImage 2>/dev/null | head -1 || true)"
if [ -z "$packed" ]; then
  echo "fix-appimage-codecs: repack produced no AppImage" >&2
  exit 1
fi
mv -f "$packed" "$BUNDLE_DIR/$orig_name"
chmod +x "$BUNDLE_DIR/$orig_name"

echo "==> Done: $BUNDLE_DIR/$orig_name"
ls -lh "$BUNDLE_DIR/$orig_name"
