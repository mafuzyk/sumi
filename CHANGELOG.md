# Changelog

## [0.1.2] - 2026-11-21

### Fixed

- Restored real-time time-lapse replay: 1× now advances at the recorded 30 frames per second of wall-clock time, and faster speeds account for delayed redraws without drifting.
- Replaced blocking Linux portal/Zenity-dependent file and folder dialogs with in-window choosers throughout the desktop application, including brush sources, fonts, profiles, materials, plugins, Save As, exports, and gallery sharing.
- Prevented accidental overwrites by confirming the final destination filename and its extension before replacing an existing file. Save dialogs now keep the filename while browsing folders and never mistake a folder for a file destination.
- Kept file dialogs above editor overlays, prevented exporting into a different artwork after switching canvases, and kept temporary videos safe when saving to their own generated path.
- Moved Linux error notifications into the application instead of blocking on external dialogs; video copies remain asynchronous and clean up their temporary files after completion or cancellation.

### Platforms

- Linux x86_64: AppImage, Debian package and portable tarball.
- Windows x64 and ARM64: MSI installer and portable ZIP (availability depends on CI results).
- Nix: `github:mafuzyk/sumi/v0.1.2`.
- No macOS package in this release.

## [0.1.1] - 2026-11-20

### Changed

- Redesigned the desktop Start Deck and added functional time-lapse transport controls, including timeline seeking and playback speed.
- Removed dormant Android and iOS frontend code from the desktop release while retaining shared artwork assets and compatibility paths.
- Improved time-lapse player frame-window reuse, forward and backward seeking, and cancellation of superseded video readers.

### Fixed

- Prevented paused previews from blocking export and import: operations with `Required` decoder priority can preempt a replay, and a new replay may preempt another only after sustained backpressure. Active replays can now coexist through slow decoder startup without cancelling one another.
- Kept at most one native video decoder/FFmpeg reader active while ensuring cancelled readers are stopped and their subprocesses reaped.
- Made time-lapse export and export-card cancellation interrupt blocked readers, including when they are waiting for the decoder permit.
- Isolated decoder lifetime regressions across the video and time-lapse test modules so parallel tests do not interfere with one another.

### Platforms

- Linux x86_64: AppImage, Debian package and portable tarball.
- Windows x64 and ARM64: MSI installer and portable ZIP.
- Nix: versioned flake package and application.
- macOS builds are not included in this release.

## [0.1.0] - 2026-11-20

- Initial public desktop release for Linux and Windows, with drawing, layers, gallery, import/export, animation, time-lapse, plugins and 3D painting.

[0.1.2]: https://github.com/mafuzyk/sumi/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/mafuzyk/sumi/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/mafuzyk/sumi/releases/tag/v0.1.0
