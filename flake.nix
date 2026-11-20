{
  description = "Sumi, a painting app: the desktop build, a dev shell and a runnable package";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;

      # The two Linux systems the desktop app builds for; no flake-utils for this little.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: lib.genAttrs systems f;

      # The app's version lives in the workspace Cargo.toml's [workspace.package] (the one number a
      # release is tagged with; desktop/build.rs reads CARGO_PKG_VERSION, inherited from it), so the
      # flake parses it rather than repeating it here.
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

      # libxkbcommon carries the -x11 variant too; libX11-xcb comes with libx11.
      runtimeLibs =
        pkgs: with pkgs; [
          vulkan-loader
          libxkbcommon
          wayland
          libx11
          libxcb
          libxcursor
          libxi
          # The EGL/GL loader, for the OpenGL fallback where a GPU has no Vulkan.
          libglvnd
        ];

      # The binary dlopen's its windowing, xkbcommon, Vulkan and EGL libraries at run time, so
      # nothing puts them in its RUNPATH; the wrapper passes them in LD_LIBRARY_PATH. Mesa is last
      # there for the EGL/GL fallback. On non-NixOS libglvnd finds the system mesa's EGL vendor
      # config through its standard search paths; on NixOS the /run/opengl-driver directory
      # (present with hardware.graphics) provides both the real GPU and the software fallback.
      libraryPath =
        pkgs: "${lib.makeLibraryPath (runtimeLibs pkgs)}:/run/opengl-driver/lib:${pkgs.mesa}/lib";

      # Tools the app runs when it exports a time-lapse or animation (ffmpeg) or imports a PDF's
      # pages (pdftoppm): in PATH so those work out of the box. ffmpeg-headless is the same ffmpeg
      # minus the X11/SDL/SDL2 front ends the app never uses; the app says what to install instead
      # of failing when either is missing, so this stays a convenience, not a dependency.
      runtimeTools =
        pkgs: with pkgs; [
          ffmpeg-headless
          poppler-utils
        ];

      mkSumi =
        pkgs:
        let
          inherit (pkgs) lib;
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "sumi";
          inherit version;

          # The whole tree: the desktop app embeds android/app/src/main/assets/ (fonts, brushes,
          # environments) and crates/clip-core ships assets/blank.clip at compile time. Build
          # output and the git database are the only things left out.
          src = lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              let
                base = baseNameOf path;
              in
              !(builtins.elem base [
                ".git"
                "target"
                "dist"
              ])
              && !(type == "regular" && lib.hasSuffix ".rs.bk" base);
          };

          cargoLock.lockFile = ./Cargo.lock;

          # Only the desktop crate: the Android bindgen and the other members are not packaged.
          cargoBuildFlags = [
            "-p"
            "sumi-desktop"
          ];

          # The engine's GPU tests want a GPU and skip themselves without one; testing is CI's job
          # (`cargo test --workspace`), not the package's.
          doCheck = false;

          # nixpkgs' cargo setup hook prefilters every Rust build for system libraries
          # (`cargoUseSystemLibs`), and its `LIBSQLITE3_SYS_USE_PKG_CONFIG=1` outranks the
          # `bundled` feature in libsqlite3-sys's build script: it would then link a dynamic
          # `-lsqlite3` that the sandbox does not have, failing at link time. clip-core asks for
          # the bundled amalgamation (a plain `cargo build -p sumi-desktop` builds it, and the
          # nfpm package lists no libsqlite3 dependency), so build native libraries the way the
          # plain build does instead.
          dontCargoUseSystemLibs = true;

          nativeBuildInputs = [ pkgs.makeWrapper ];

          # Ship the same launcher entry and icons as the .deb, so a profile install shows up in
          # desktop launchers.
          postInstall = ''
            install -Dm644 packaging/linux/sumi.desktop $out/share/applications/sumi.desktop
            mkdir -p $out/share/icons
            cp -R assets/app-icon/hicolor $out/share/icons/hicolor
          '';

          postFixup = ''
            wrapProgram $out/bin/sumi \
              --prefix LD_LIBRARY_PATH : "${libraryPath pkgs}" \
              --prefix PATH : "${lib.makeBinPath (runtimeTools pkgs)}"
          '';

          meta = {
            description = "Paint, draw and sketch";
            homepage = "https://github.com/mafuzyk/sumi";
            license = lib.licenses.gpl3Plus;
            platforms = lib.platforms.linux;
            mainProgram = "sumi";
          };
        };

      mkDevShell =
        pkgs:
        let
          inherit (pkgs) lib;
        in
        pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
          ];

          # `cargo run -p sumi-desktop` loads its windowing and GPU libraries by name, the same way
          # the wrapped package does, so the shell exports the same path. No Android SDK: that
          # build has its own tooling and is out of scope here.
          shellHook = ''
            export LD_LIBRARY_PATH="${libraryPath pkgs}"''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
          '';
        };
    in
    {
      packages = forAllSystems (
        system: {
          default = mkSumi nixpkgs.legacyPackages.${system};
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = lib.getExe self.packages.${system}.default;
          meta = {
            description = "Sumi, a painting app";
          };
        };
      });

      devShells = forAllSystems (
        system: {
          default = mkDevShell nixpkgs.legacyPackages.${system};
        }
      );
    };
}
