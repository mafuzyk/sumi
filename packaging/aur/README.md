# sumi-bin (AUR)

Binary package for [Sumi](https://github.com/mafuzyk/sumi) on Arch Linux (x86_64). It installs the prebuilt
Linux release of the desktop app — `/usr/bin/sumi`, the `.desktop` file, the hicolor icons and the
licence texts — rather than building it from source.

The release tarball's root (`sumi-<version>-linux-x86_64/`) holds `bin/` and `share/` directly
(that is what `packaging/linux/package.sh` produces with `cp -R "$STAGE/usr" "$WORK/tar/$BASENAME"`);
there is no `usr/` directory inside it. `package()` installs `bin/sumi` to `/usr/bin/sumi` and
copies `share/` into `/usr/share/`. The shipped binary is already stripped, so the PKGBUILD sets
`options=(!strip)` to keep it byte-identical to the published checksums.

## How it tracks the release

`pkgver` mirrors the GitHub release tag `vX.Y.Z` at
https://github.com/mafuzyk/sumi/releases. Each build downloads
`sumi-<version>-linux-x86_64.tar.gz` and the release's `SHA256SUMS.txt`; `prepare()` checks the
tarball against the published sum and fails on a mismatch. No hash is pinned in the PKGBUILD
because the tarball is produced by CI for every release.

## Maintainer steps at each release

1. Bump `pkgver` in `PKGBUILD` to the new version (without the leading `v`) and reset `pkgrel=1`.
2. Regenerate the metadata from this directory: `makepkg --printsrcinfo > .SRCINFO`.
3. Commit `PKGBUILD` and `.SRCINFO`, then push to the AUR package repository.

Optional dependencies mirror the `.deb` package's recommends (see `packaging/linux/nfpm.yaml`):
ffmpeg, poppler, xdg-desktop-portal and a sound playback tool. Sumi calls `pw-play`/`pw-cat`,
`paplay`/`pacat` or `aplay` for Feedback Sounds and brush sounds, so Arch lists the packages that
ship those tools (`pipewire-audio`, `libpulse`, `alsa-utils`) rather than the sound servers.

For GPU rendering, Arch's `mesa` package provides only OpenGL/EGL (the fallback when the GPU has
no Vulkan); the Vulkan ICDs are separate packages. Sumi's Vulkan use is therefore covered by
`vulkan-intel` (Intel integrated GPUs), `vulkan-radeon` (AMD Radeon GPUs), `vulkan-nouveau`
(NVIDIA GPUs via Nouveau) and `vulkan-swrast` (software rendering on the CPU), alongside `mesa`
as the OpenGL/EGL fallback.
