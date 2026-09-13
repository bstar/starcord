{
  description = "STAR/CORD — a Winamp-feel terminal Discord client";

  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    let
      # Home-manager module, so STAR/CORD can be installed and configured
      # declaratively the way the rest of a NixOS setup is.
      #
      # There is deliberately no option for the token. It belongs in the
      # keyring, or in a mode-0600 file; the Nix store is world-readable and a
      # secret written from a module would land there in plain text.
      hmModule = { config, lib, pkgs, ... }:
        let cfg = config.programs.starcord;
        in {
          options.programs.starcord = {
            enable = lib.mkEnableOption "STAR/CORD terminal Discord client";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
              description = "The starcord package to use.";
            };
            theme = lib.mkOption {
              type = lib.types.str;
              default = "catppuccin-mocha";
              description = ''
                A built-in theme id, or the id of a file in
                ~/.local/starcord/themes. Ignored when stylix.enable is set.
              '';
            };
            stylix.enable = lib.mkEnableOption ''
              deriving a STAR/CORD theme from the active Stylix base16 scheme,
              so the client matches the rest of the desktop automatically
            '';
            settings = lib.mkOption {
              type = lib.types.attrs;
              default = { };
              example = { chat.max_image_rows = 16; notify.dms_only = true; };
              description = ''
                Extra config.toml settings, merged last and table by table, so
                setting one key in [chat] leaves the rest of [chat] alone.
              '';
            };
          };

          config = lib.mkIf cfg.enable (lib.mkMerge [
            {
              home.packages = [ cfg.package ];
              # STAR/CORD keeps everything under one directory rather than
              # spreading it across the XDG roots, so this is not
              # xdg.configFile.
              home.file.".local/starcord/config.toml".source =
                (pkgs.formats.toml { }).generate "starcord-config.toml" (
                  lib.recursiveUpdate
                    { ui.theme = if cfg.stylix.enable then "stylix" else cfg.theme; }
                    cfg.settings
                );
            }
            (lib.mkIf cfg.stylix.enable {
              home.file.".local/starcord/themes/stylix.toml".text =
                let c = config.lib.stylix.colors;
                in ''
                  # Generated from the active Stylix scheme.
                  [meta]
                  name = "Stylix"
                  id = "stylix"
                  variant = "${config.stylix.polarity}"

                  [base16]
                '' + lib.concatMapStringsSep "\n"
                  (n: ''base${n} = "#${c."base${n}"}"'')
                  [ "00" "01" "02" "03" "04" "05" "06" "07"
                    "08" "09" "0A" "0B" "0C" "0D" "0E" "0F" ]
                  + "\n";
            })
          ]);
        };
    in
    {
      homeManagerModules.starcord = hmModule;
      homeManagerModules.default = hmModule;
      overlays.default = final: prev: {
        starcord = self.packages.${final.stdenv.hostPlatform.system}.default;
      };
    }
    # Explicit rather than eachDefaultSystem, which would also claim systems
    # nobody has built this on. aarch64-darwin only, as in STAR/AMP: nixpkgs
    # 26.11 dropped x86_64-darwin outright, and naming it fails *evaluation*
    # with a release note rather than merely failing to build.
    // flake-utils.lib.eachSystem [
      "x86_64-linux"
      "aarch64-linux"
      "aarch64-darwin"
    ] (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # One version, read rather than repeated. scripts/check-version.sh
        # asserts the copies that cannot be derived (Cargo.lock, PKGBUILD).
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);

        # There are no buildInputs and no nativeBuildInputs, which is worth
        # saying out loud because the obvious guesses are all wrong here:
        # TLS is rustls, so there is no openssl; the keyring reaches
        # secret-service through zbus and the Apple Keychain through the SDK,
        # so there is no libdbus; notifications default to zbus for the same
        # reason. Nothing in the tree runs bindgen. If that changes -- a
        # dependency switching to a `-sys` crate, most likely -- this is where
        # pkg-config and the library go, and CI needs the matching apt line.
        mkStarcord = { pkgsFor ? pkgs }:
          pkgsFor.rustPlatform.buildRustPackage {
            pname = "starcord";
            version = cargoToml.package.version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            # STAR/KIT comes from a git tag rather than from crates.io, and
            # `cargoLock.lockFile` alone cannot fetch it: nix wants a hash for
            # every source it downloads. Two ways to give it one.
            #
            # `outputHashes` is the reproducible one, and it means a new hash
            # to compute and commit on every STAR/KIT tag -- a second place the
            # version lives, which is exactly the kind of copy that goes stale
            # between the bump and the person who notices.
            #
            # This asks nix's builtin `fetchGit` for it instead. The tag is
            # immutable and `Cargo.lock` records the revision it resolved to,
            # so what is fetched is still pinned; what is given up is the
            # fixed-output hash, which means this fetch happens outside the
            # sandbox and a build with no network cannot do it. That is the
            # right trade here: the lockfile is the pin, and a stale hash
            # nobody bumped is a worse failure than a build that needs the
            # network it was already going to use.
            cargoLock.allowBuiltinFetchGit = true;

            # freedesktop assets, which mean nothing on macOS.
            postInstall = pkgsFor.lib.optionalString pkgsFor.stdenv.hostPlatform.isLinux ''
              install -Dm644 packaging/starcord.desktop \
                $out/share/applications/starcord.desktop
              install -Dm644 packaging/starcord.png \
                $out/share/icons/hicolor/256x256/apps/starcord.png
              install -Dm644 packaging/starcord.svg \
                $out/share/icons/hicolor/scalable/apps/starcord.svg
            '';

            meta = with pkgsFor.lib; {
              description = "A Winamp-feel terminal Discord client";
              homepage = "https://github.com/bstar/starcord";
              license = licenses.mit;
              mainProgram = "starcord";
              platforms = platforms.linux ++ platforms.darwin;
            };
          };
      in
      {
        packages.default = mkStarcord { };
        packages.starcord = mkStarcord { };

        # buildRustPackage runs `cargo test` as part of building the package,
        # so naming it here makes `nix flake check` cover the test suite too.
        checks = {
          inherit (self.packages.${system}) default;

          fmt = pkgs.runCommand "cargo-fmt"
            { nativeBuildInputs = [ pkgs.rustfmt ]; }
            ''
              cd ${./.}
              find src tests -name '*.rs' -print0 \
                | xargs -0 rustfmt --check --edition 2021
              touch $out
            '';
        };

        formatter = pkgs.nixpkgs-fmt;

        apps.default = flake-utils.lib.mkApp {
          drv = self.packages.${system}.default;
        };

        devShells.default = pkgs.mkShell {
          packages = (with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
            # scripts/check-version.sh reads `cargo metadata`.
            jq
            # The licence and advisory gate, so it is run before CI runs it.
            # It matters more here than it looks: `deny.toml` has one accepted
            # advisory and will have one allowed git source, and the check that
            # each list still has exactly what it should is this command.
            cargo-deny
          ])
          # Only ever used to build a .deb, which only happens on Linux.
          ++ pkgs.lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.cargo-deb;

          shellHook = ''
            echo "STAR/CORD devshell · rustc $(rustc --version | cut -d' ' -f2)"
          '';
        };
      });
}
