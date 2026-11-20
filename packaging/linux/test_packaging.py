#!/usr/bin/env python3
"""Regression tests for Linux Sumi packaging metadata, icon assets, and AppRun XDG behavior."""

import configparser
import os
import shutil
import stat
import struct
import subprocess
import tempfile
import unittest
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LINUX_DIR = ROOT / "packaging" / "linux"
ICON_DIR = ROOT / "assets" / "app-icon"
HICOLOR_SIZES = (16, 24, 32, 48, 64, 128, 256, 512)
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def decode_rgba_png(path: Path) -> tuple[int, int, int, int, bytearray]:
    data = path.read_bytes()
    if data[:8] != PNG_MAGIC:
        raise AssertionError(f"{path} does not start with PNG magic")
    pos = 8
    ihdr = None
    idat = bytearray()
    while pos + 8 <= len(data):
        length, kind = struct.unpack(">I4s", data[pos : pos + 8])
        body = data[pos + 8 : pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            ihdr = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat.extend(body)
        elif kind == b"IEND":
            break
    if ihdr is None:
        raise AssertionError(f"{path} missing IHDR")
    width, height, bit_depth, color_type, compression, filter_method, interlace = ihdr
    if (bit_depth, color_type, compression, filter_method, interlace) != (8, 6, 0, 0, 0):
        raise AssertionError(
            f"{path} expected 8-bit non-interlaced RGBA (color_type=6), got "
            f"depth={bit_depth} color_type={color_type} interlace={interlace}"
        )
    raw = zlib.decompress(bytes(idat))
    stride = width * 4
    expected_len = height * (1 + stride)
    if len(raw) != expected_len:
        raise AssertionError(f"{path} decompressed length {len(raw)} != {expected_len}")
    pixels = bytearray(height * stride)
    prev = bytearray(stride)
    for y in range(height):
        row_start = y * (1 + stride)
        ftype = raw[row_start]
        src = raw[row_start + 1 : row_start + 1 + stride]
        cur = bytearray(stride)
        if ftype == 0:
            cur[:] = src
        elif ftype == 1:
            for x in range(stride):
                left = cur[x - 4] if x >= 4 else 0
                cur[x] = (src[x] + left) & 0xFF
        elif ftype == 2:
            for x in range(stride):
                cur[x] = (src[x] + prev[x]) & 0xFF
        elif ftype == 3:
            for x in range(stride):
                left = cur[x - 4] if x >= 4 else 0
                cur[x] = (src[x] + ((left + prev[x]) >> 1)) & 0xFF
        elif ftype == 4:
            for x in range(stride):
                a = cur[x - 4] if x >= 4 else 0
                b = prev[x]
                c = prev[x - 4] if x >= 4 else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                cur[x] = (src[x] + pr) & 0xFF
        else:
            raise AssertionError(f"{path} unsupported PNG filter {ftype}")
        pixels[y * stride : (y + 1) * stride] = cur
        prev = cur
    return width, height, bit_depth, color_type, pixels


class LinuxPackagingTests(unittest.TestCase):
    def test_sumi_icons_have_png_magic_rgba_and_expected_tile_pixels(self) -> None:
        master = ICON_DIR / "sumi-1024.png"
        w, h, depth, ctype, px = decode_rgba_png(master)
        self.assertEqual((w, h, depth, ctype), (1024, 1024, 8, 6))

        def rgba_at(x: int, y: int) -> tuple[int, int, int, int]:
            i = (y * w + x) * 4
            return (px[i], px[i + 1], px[i + 2], px[i + 3])

        self.assertEqual(rgba_at(0, 0)[3], 0, "outer corner must be transparent")
        self.assertEqual(rgba_at(1023, 1023)[3], 0, "bottom-right corner must be transparent")
        self.assertEqual(rgba_at(512, 512), (255, 255, 255, 255), "tile center must be opaque white")
        self.assertEqual(rgba_at(512, 10), (255, 255, 255, 255), "top tile interior must be opaque white")
        enso = rgba_at(195, 512)
        self.assertEqual(enso[3], 255)
        self.assertLess(max(enso[:3]), 60, f"expected dark enso stroke pixel, got {enso}")

        for size in HICOLOR_SIZES:
            p = ICON_DIR / "hicolor" / f"{size}x{size}" / "apps" / "sumi.png"
            self.assertTrue(p.is_file(), f"missing {p}")
            iw, ih, idepth, ictype, ipx = decode_rgba_png(p)
            self.assertEqual((iw, ih, idepth, ictype), (size, size, 8, 6))
            self.assertEqual(ipx[3], 0, f"{p} top-left corner must stay transparent")
            mid = ((size // 2) * size + (size // 2)) * 4
            self.assertGreaterEqual(ipx[mid], 245, f"{p} center must be white")
            self.assertEqual(ipx[mid + 3], 255, f"{p} center must be opaque")

        self.assertTrue((ICON_DIR / "sumi-512.png").is_file())
        self.assertTrue((ICON_DIR / "sumi-256.png").is_file())
        self.assertTrue((ICON_DIR / "sumi.ico").is_file())

    def test_desktop_entry_nfpm_and_scripts_match_sumi_identity(self) -> None:
        parser = configparser.ConfigParser(interpolation=None)
        parser.optionxform = str
        desktop_path = LINUX_DIR / "sumi.desktop"
        parser.read(desktop_path, encoding="utf-8")
        entry = parser["Desktop Entry"]
        self.assertEqual(entry["Name"], "Sumi")
        self.assertEqual(entry["Icon"], "sumi")
        self.assertEqual(entry["StartupWMClass"], "Sumi")
        self.assertEqual(entry["Exec"], "sumi %F")

        nfpm = (LINUX_DIR / "nfpm.yaml").read_text(encoding="utf-8")
        self.assertIn("name: sumi", nfpm)
        self.assertIn("dst: /usr/bin/sumi", nfpm)
        self.assertIn("homepage: https://github.com/mafuzyk/sumi", nfpm)
        self.assertIn("license: GPL-3.0-or-later", nfpm)
        self.assertIn("Sumi is a painting app", nfpm)

        pkg = (LINUX_DIR / "package.sh").read_text(encoding="utf-8")
        self.assertIn('BASENAME="sumi-$VERSION-linux-$ARCH"', pkg)
        self.assertIn('echo "==> Sumi $VERSION for Linux $ARCH ($FORMATS)"', pkg)
        self.assertIn('"$STAGE/usr/share/applications/sumi.desktop"', pkg)
        self.assertIn('desktop-file-validate "$STAGE/usr/share/applications/sumi.desktop"', pkg)
        self.assertIn('APPDIR="$WORK/Sumi.AppDir"', pkg)
        self.assertIn('cp "$HERE/sumi.desktop" "$APPDIR/sumi.desktop"', pkg)
        self.assertIn('cp "$ROOT/assets/app-icon/hicolor/256x256/apps/sumi.png" "$APPDIR/sumi.png"', pkg)
        self.assertIn('ln -s sumi.png "$APPDIR/.DirIcon"', pkg)
        self.assertIn('for f in LICENSE-GPL-3.0 LICENSE-MIT LICENSE-APACHE NOTICE ATTRIBUTION.md; do', pkg)
        self.assertIn('"$STAGE/usr/share/doc/sumi/$f"', pkg)
        # The AppImage copies the staged tree, so it carries LICENSE-GPL-3.0 from that loop;
        # stripping usr/share/doc afterwards would drop every licence text from the binary.
        self.assertIn('cp -R "$STAGE" "$APPDIR"', pkg)
        self.assertNotIn('rm -rf "$APPDIR/usr/share/doc"', pkg)

        for script in ("package.sh", "AppRun", "postinst.sh"):
            subprocess.run(["bash", "-n", str(LINUX_DIR / script)], check=True)

    def test_apprun_integrates_sumi_desktop_and_icons_and_forwards_args(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sumi-apprun-") as tmp:
            root = Path(tmp)
            appdir = root / "Sumi.AppDir"
            (appdir / "usr" / "bin").mkdir(parents=True)
            (appdir / "usr" / "share" / "icons").mkdir(parents=True)

            shutil.copy2(LINUX_DIR / "AppRun", appdir / "AppRun")
            (appdir / "AppRun").chmod((appdir / "AppRun").stat().st_mode | stat.S_IXUSR)
            shutil.copy2(LINUX_DIR / "sumi.desktop", appdir / "sumi.desktop")
            shutil.copytree(ICON_DIR / "hicolor", appdir / "usr" / "share" / "icons" / "hicolor")

            stub = appdir / "usr" / "bin" / "sumi"
            stub.write_text("#!/usr/bin/env bash\nprintf '%s\\n' \"$@\"\n", encoding="utf-8")
            stub.chmod(0o755)

            fake_home = root / "home"
            xdg_data = root / "xdg data"
            fake_home.mkdir()
            xdg_data.mkdir()
            appimage_path = root / "My Apps" / "Sumi 0.0.6-beta.AppImage"

            env = os.environ.copy()
            env["HOME"] = str(fake_home)
            env["XDG_DATA_HOME"] = str(xdg_data)
            env["APPIMAGE"] = str(appimage_path)

            args = ["--open", "my painting.sumi"]
            res = subprocess.run(
                [str(appdir / "AppRun"), *args],
                env=env,
                capture_output=True,
                text=True,
                check=True,
            )
            self.assertEqual(res.stdout.splitlines(), args)

            installed_desktop = xdg_data / "applications" / "sumi.desktop"
            self.assertTrue(installed_desktop.is_file(), "AppRun failed to install sumi.desktop")

            parser = configparser.ConfigParser(interpolation=None)
            parser.optionxform = str
            parser.read(installed_desktop, encoding="utf-8")
            entry = parser["Desktop Entry"]
            self.assertEqual(entry["Name"], "Sumi")
            self.assertEqual(entry["Icon"], "sumi")
            self.assertEqual(entry["StartupWMClass"], "Sumi")
            self.assertEqual(entry["Exec"], f'"{appimage_path}" %F')

            for size in HICOLOR_SIZES:
                src_icon = ICON_DIR / "hicolor" / f"{size}x{size}" / "apps" / "sumi.png"
                dst_icon = xdg_data / "icons" / "hicolor" / f"{size}x{size}" / "apps" / "sumi.png"
                self.assertTrue(dst_icon.is_file(), f"missing installed {dst_icon}")
                self.assertEqual(dst_icon.read_bytes(), src_icon.read_bytes(), f"bitwise mismatch at {size}px")


if __name__ == "__main__":
    unittest.main()
