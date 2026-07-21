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

echo "==> Stripping bundled libs the host already provides"
moved=0
for f in "$APPDIR"/usr/lib/*.so*; do
  [ -e "$f" ] || continue
  base="$(basename "$f")"
  # Keep only libs the host lacks; the host's copy of anything else is known-good.
  if [ -e "/usr/lib64/$base" ] || [ -e "/lib64/$base" ] \
     || [ -e "/usr/lib/x86_64-linux-gnu/$base" ] \
     || ldconfig -p 2>/dev/null | grep -qF " $base "; then
    rm -f "$f"; moved=$((moved + 1))
  fi
done
# Pulse ships under a versioned host subdir, so name-match above misses it.
rm -f "$APPDIR"/usr/lib/libpulse*.so* 2>/dev/null || true
echo "    removed $moved host-provided libs"

echo "==> Restoring the un-patchelf'd application binary"
# linuxdeploy patchelf-mangles the copied binary (changes size, can corrupt it).
# Replace with the pristine release binary, then set ONLY the rpath needed to
# find the few GTK plugin .so's that legitimately remain bundled.
cp -f "$GOOD_BINARY" "$APPDIR/usr/bin/lychi-app"
patchelf --set-rpath '$ORIGIN/../lib' "$APPDIR/usr/bin/lychi-app"

echo "==> Repacking the AppImage"
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
mv -f "$packed" "$BUNDLE_DIR/Lychi_0.1.0_amd64.AppImage"
chmod +x "$BUNDLE_DIR/Lychi_0.1.0_amd64.AppImage"

echo "==> Done: $BUNDLE_DIR/Lychi_0.1.0_amd64.AppImage"
ls -lh "$BUNDLE_DIR/Lychi_0.1.0_amd64.AppImage"
