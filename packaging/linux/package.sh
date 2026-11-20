#!/usr/bin/env bash
# Packages the desktop app for Linux into $DIST (default dist/), <arch> being this machine's:
#
#   sumi-<version>-linux-<arch>.AppImage   any distro with glibc as new as the build host's
#   sumi-<version>-linux-<arch>.deb        Debian, Ubuntu, Mint, Pop!_OS, ...
#   sumi-<version>-linux-<arch>.tar.gz     the plain tree (bin/, share/)
#
# Usage: packaging/linux/package.sh [--skip-build] [--formats "appimage deb tar"]
#
# VERSION is the release's (default: the workspace Cargo.toml's [workspace.package] version). Needs
# cargo, nfpm for the .deb (https://nfpm.goreleaser.com) and appimagetool for the AppImage (downloaded
# into the target folder if missing). Build on an old distro (CI: Ubuntu 22.04) so the binary
# runs on newer ones.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HERE="$ROOT/packaging/linux"
SKIP_BUILD=0
FORMATS="appimage deb tar"
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --formats) FORMATS="$2"; shift 2 ;;
    -h | --help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
VERSION="${VERSION:-$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -n1)}"
DIST="$(mkdir -p "${DIST:-$ROOT/dist}" && cd "${DIST:-$ROOT/dist}" && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) DEB_ARCH=amd64 ;;
  aarch64 | arm64) ARCH=aarch64; DEB_ARCH=arm64 ;;
  *) echo "unsupported architecture $ARCH" >&2; exit 2 ;;
esac
BASENAME="sumi-$VERSION-linux-$ARCH"
has() { case " $FORMATS " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }
echo "==> Sumi $VERSION for Linux $ARCH ($FORMATS)"

if [ "$SKIP_BUILD" = 0 ]; then
  (cd "$ROOT" && cargo build --release --locked -p sumi-desktop)
fi

# ---- the tree every format holds -----------------------------------------------------------------
WORK="$TARGET_DIR/linux-package"
STAGE="$WORK/root"
rm -rf "$WORK"
install -Dm755 "$TARGET_DIR/release/sumi" "$STAGE/usr/bin/sumi"
strip "$STAGE/usr/bin/sumi" 2>/dev/null || true
install -Dm644 "$HERE/sumi.desktop" "$STAGE/usr/share/applications/sumi.desktop"
mkdir -p "$STAGE/usr/share/icons"
cp -R "$ROOT/assets/app-icon/hicolor" "$STAGE/usr/share/icons/"
for f in LICENSE-GPL-3.0 LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md; do
  install -Dm644 "$ROOT/$f" "$STAGE/usr/share/doc/sumi/$f"
done
if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$STAGE/usr/share/applications/sumi.desktop"
fi

# ---- .tar.gz -------------------------------------------------------------------------------------
if has tar; then
  mkdir -p "$WORK/tar"
  cp -R "$STAGE/usr" "$WORK/tar/$BASENAME"
  tar -C "$WORK/tar" -czf "$DIST/$BASENAME.tar.gz" "$BASENAME"
  echo "wrote $DIST/$BASENAME.tar.gz"
fi

# ---- .deb ----------------------------------------------------------------------------------------
if has deb; then
  command -v nfpm >/dev/null || { echo "error: nfpm not found (https://nfpm.goreleaser.com/install/)" >&2; exit 1; }
  export VERSION NFPM_ARCH="$DEB_ARCH"
  # nfpm expands variables in fields like `version`, but not in `contents[].src`
  sed "s|\${STAGE}|$STAGE|g" "$HERE/nfpm.yaml" >"$WORK/nfpm.yaml"
  (cd "$ROOT" && nfpm package -f "$WORK/nfpm.yaml" -p deb -t "$DIST/$BASENAME.deb")
fi

# ---- AppImage ------------------------------------------------------------------------------------
if has appimage; then
  APPDIR="$WORK/Sumi.AppDir"
  cp -R "$STAGE" "$APPDIR"
  # usr/share/doc/sumi holds the licence texts (LICENSE-GPL-3.0 among them): keep them with the binary
  install -Dm755 "$HERE/AppRun" "$APPDIR/AppRun"
  cp "$HERE/sumi.desktop" "$APPDIR/sumi.desktop"
  cp "$ROOT/assets/app-icon/hicolor/256x256/apps/sumi.png" "$APPDIR/sumi.png"
  ln -s sumi.png "$APPDIR/.DirIcon"
  TOOL="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
  if [ -z "$TOOL" ]; then
    TOOL="$TARGET_DIR/appimagetool-$ARCH.AppImage"
    if [ ! -x "$TOOL" ]; then
      curl -fsSL -o "$TOOL" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$ARCH.AppImage"
      chmod +x "$TOOL"
    fi
  fi
  # extracted and run, so it needs no FUSE (containers, CI)
  ARCH="$ARCH" APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" --no-appstream "$APPDIR" "$DIST/$BASENAME.AppImage"
  echo "wrote $DIST/$BASENAME.AppImage"
fi

echo "==> done"
ls -l "$DIST"
