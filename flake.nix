{
  description = "quick-translate: select text, press a shortcut, see the translation at the cursor (Wayland)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      appId = "io.github.quicktranslate.QuickTranslate";
      cargoToml = lib.importTOML ./Cargo.toml;

      # Libraries that are dlopened (wgpu -> Vulkan/GL) or linked at run time.
      runtimeLibs =
        pkgs: with pkgs; [
          wayland
          libxkbcommon
          vulkan-loader
          libGL
        ];
    in
    {
      packages = forAllSystems (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.quick-translate;

        quick-translate = pkgs.rustPlatform.buildRustPackage {
          pname = "quick-translate";
          inherit (cargoToml.workspace.package) version;

          # Only the files cargo needs, so edits elsewhere (docs, CI, openspec)
          # do not trigger a rebuild.
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./crates
              ./patches
              ./packaging/appimage/${appId}.desktop
              (lib.fileset.maybeMissing ./packaging/icons)
            ];
          };

          cargoLock.lockFile = ./Cargo.lock;

          cargoBuildFlags = [
            "-p"
            "quick-translate"
          ];
          # Unit and integration tests run in CI (cargo test / headless sway);
          # several need a Wayland compositor or a session bus that the
          # build sandbox does not have.
          doCheck = false;

          nativeBuildInputs = with pkgs; [
            pkg-config
            makeWrapper
          ];
          buildInputs = with pkgs; [
            wayland
            libxkbcommon
          ];

          postInstall = ''
            install -Dm644 packaging/appimage/${appId}.desktop \
              $out/share/applications/${appId}.desktop
            sed -i '/^X-AppImage-Version=/d' $out/share/applications/${appId}.desktop
            if [ -f packaging/icons/${appId}.svg ]; then
              install -Dm644 packaging/icons/${appId}.svg \
                $out/share/icons/hicolor/scalable/apps/${appId}.svg
            fi
          '';

          postFixup = ''
            wrapProgram $out/bin/quick-translate \
              --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath (runtimeLibs pkgs)}
          '';

          meta = {
            description = "Wayland translation popup: select text, press a shortcut, read the translation at the cursor";
            homepage = cargoToml.workspace.package.repository;
            license = with lib.licenses; [
              mit
              asl20
            ];
            mainProgram = "quick-translate";
            platforms = lib.platforms.linux;
          };
        };
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          meta.description = "Run quick-translate";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            # Rust toolchain
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            # Build and runtime libraries
            pkg-config
            wayland
            libxkbcommon
            vulkan-loader
            libGL
            # Headless Wayland integration tests (design D12)
            sway
            wl-clipboard
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          LD_LIBRARY_PATH = lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });
    };
}
