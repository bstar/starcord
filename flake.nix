{
  description = "STAR/CORD — a Winamp-feel terminal Discord client";

  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    # Explicit rather than eachDefaultSystem, which would also claim systems
    # nobody has built this on. aarch64-darwin only, as in STAR/AMP: nixpkgs
    # 26.11 dropped x86_64-darwin outright, and naming it fails *evaluation*
    # with a release note rather than merely failing to build.
    flake-utils.lib.eachSystem [
      "x86_64-linux"
      "aarch64-linux"
      "aarch64-darwin"
    ] (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # One version, read rather than repeated.
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
            # STAR/KIT is a git dependency pinned to a tag, and a lock file
            # entry for one carries no hash for Nix to check. This lets the
            # builtin fetcher take it from the revision the lock file names,
            # which is what makes `nix build` work without a second copy of
            # every git dependency's hash in this file.
            cargoLock.allowBuiltinFetchGit = true;

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
          packages = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
            # The licence and advisory gate, so it is run before CI runs it.
            cargo-deny
          ];

          shellHook = ''
            echo "STAR/CORD devshell · rustc $(rustc --version | cut -d' ' -f2)"
          '';
        };
      });
}
