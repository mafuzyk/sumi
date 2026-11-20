#!/usr/bin/env bash
# Packages the desktop app for macOS into $DIST (default dist/):
#
#   sumi-<version>-macos-universal.dmg   Sumi.app for Apple silicon and Intel Macs (macOS 11+),
#                                        beside a link to Applications to drag it onto
#
# Usage: packaging/macos/package.sh [--skip-build]
#
# VERSION is the release's (default: the workspace Cargo.toml's [workspace.package] version); the
# app's version is its numbers alone (beta suffix out, as the MSI's) and its build number comes from
# them: major*10000 + minor*100 + patch (0.1.0 -> 100). Runs on a Mac with Xcode's command line tools
# (lipo, codesign, hdiutil), Rust for aarch64-apple-darwin and x86_64-apple-darwin, and python3.
#
# Signing: ad hoc by default, which a Mac runs once the user allows it (System Settings > Privacy &
# Security > Open Anyway). APPLE_SIGN_IDENTITY names a "Developer ID Application" identity in the
# keychain to sign with instead; with APPLE_API_KEY_PATH (an App Store Connect API key, .p8),
# APPLE_API_KEY_ID and APPLE_API_ISSUER_ID as well, the DMG is notarized and its ticket stapled.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HERE="$ROOT/packaging/macos"
SKIP_BUILD=0
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    -h | --help) sed -n '2,17p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
CARGO_TOML="$ROOT/Cargo.toml"
VERSION="${VERSION:-$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\(.*\)"/\1/p' "$CARGO_TOML" | head -n1)}"
# the build number comes from the version's numbers: major*10000 + minor*100 + patch (0.1.0 -> 100)
if [ -z "${BUILD:-}" ]; then
  IFS=. read -r major minor patch <<<"${VERSION%%-*}"
  major=${major:-0}; minor=${minor:-0}; patch=${patch:-0}
  [[ $major =~ ^[0-9]+$ && $minor =~ ^[0-9]+$ && $patch =~ ^[0-9]+$ ]] || {
    echo "cannot derive a build number from version $VERSION" >&2; exit 2
  }
  BUILD=$((10#$major * 10000 + 10#$minor * 100 + 10#$patch))
fi
[ -n "$VERSION" ] && [ -n "$BUILD" ] || { echo "no version: set VERSION and BUILD or see $CARGO_TOML" >&2; exit 2; }
# the bundle's own version is numbers only
SHORT="${VERSION%%-*}"
DIST="$(mkdir -p "${DIST:-$ROOT/dist}" && cd "${DIST:-$ROOT/dist}" && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
TARGETS="aarch64-apple-darwin x86_64-apple-darwin"
DMG="$DIST/sumi-$VERSION-macos-universal.dmg"
echo "==> Sumi $VERSION ($SHORT, build $BUILD) for macOS, universal"

# the oldest macOS the app runs on, as Info.plist says
export MACOSX_DEPLOYMENT_TARGET=11.0
if [ "$SKIP_BUILD" = 0 ]; then
  for target in $TARGETS; do
    (cd "$ROOT" && cargo build --release --locked -p sumi-desktop --target "$target")
  done
fi

# ---- Sumi.app ------------------------------------------------------------------------------------
WORK="$TARGET_DIR/macos-package"
APP="$WORK/Sumi.app"
rm -rf "$WORK"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
slices=()
for target in $TARGETS; do slices+=("$TARGET_DIR/$target/release/sumi"); done
# one binary for both kinds of Mac
lipo -create -output "$APP/Contents/MacOS/sumi" "${slices[@]}"
strip -x "$APP/Contents/MacOS/sumi"
lipo -info "$APP/Contents/MacOS/sumi"
sed -e "s|@VERSION@|$SHORT|g" -e "s|@BUILD@|$BUILD|g" "$HERE/Info.plist" >"$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
printf 'APPL????' >"$APP/Contents/PkgInfo"
python3 "$HERE/icns.py" "$ROOT/assets/app-icon/sumi-1024.png" "$APP/Contents/Resources/sumi.icns"
for f in LICENSE-GPL-3.0 LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md; do
  cp "$ROOT/$f" "$APP/Contents/Resources/$f"
done

# ---- signing -------------------------------------------------------------------------------------
if [ -n "${APPLE_SIGN_IDENTITY:-}" ]; then
  # the hardened runtime and a timestamp, as notarization wants; the app needs no entitlements
  codesign --force --options runtime --timestamp --sign "$APPLE_SIGN_IDENTITY" "$APP"
else
  # ad hoc: an Apple silicon Mac runs nothing unsigned
  codesign --force --sign - "$APP"
fi
codesign --verify --strict --verbose=2 "$APP"

# ---- .dmg ----------------------------------------------------------------------------------------
STAGE="$WORK/dmg"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/Sumi.app"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
# hdiutil now and then finds its disk busy: tried again
for try in 1 2 3; do
  if hdiutil create -volname Sumi -srcfolder "$STAGE" -ov -format UDZO "$DMG"; then break; fi
  [ "$try" = 3 ] && { echo "error: hdiutil failed" >&2; exit 1; }
  sleep 5
done
if [ -n "${APPLE_SIGN_IDENTITY:-}" ]; then
  codesign --force --timestamp --sign "$APPLE_SIGN_IDENTITY" "$DMG"
fi

# ---- notarization --------------------------------------------------------------------------------
if [ -n "${APPLE_SIGN_IDENTITY:-}" ] && [ -n "${APPLE_API_KEY_PATH:-}" ]; then
  : "${APPLE_API_KEY_ID:?set APPLE_API_KEY_ID with APPLE_API_KEY_PATH}"
  : "${APPLE_API_ISSUER_ID:?set APPLE_API_ISSUER_ID with APPLE_API_KEY_PATH}"
  xcrun notarytool submit "$DMG" --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" \
    --issuer "$APPLE_API_ISSUER_ID" --wait
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
  spctl --assess --type open --context context:primary-signature --verbose=2 "$DMG"
fi

echo "wrote $DMG"
echo "==> done"
ls -l "$DIST"
