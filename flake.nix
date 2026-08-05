{
  description = "loti (LOcal TIckets) — markdown-backed local ticketing CLI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    # Explicit system list: nixpkgs unstable (26.11) has dropped x86_64-darwin,
    # which eachDefaultSystem would still include.
    flake-utils.lib.eachSystem [
      "x86_64-linux"
      "aarch64-linux"
      "aarch64-darwin"
    ] (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        # Single source of truth: the Rust channel + components + targets come
        # from ./rust-toolchain.toml; the package version from ./Cargo.toml.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        version = cargoToml.workspace.package.version;

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # Whitelisted source: just the crates + lockfiles, so target/, the
        # prototype tree, tickets and docs never bloat the store copy.
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./crates
          ];
        };

        loti = rustPlatform.buildRustPackage {
          pname = "loti";
          inherit version src;
          cargoLock.lockFile = ./Cargo.lock;
          # The package build compiles only; the suite is run from the dev shell
          doCheck = false;
          meta = {
            description = "loti (LOcal TIckets) — markdown-backed local ticketing CLI";
            mainProgram = "loti";
          };
        };
      in {
        packages.default = loti;
        packages.loti = loti;

        devShells.default = pkgs.mkShell {
          packages = [ rustToolchain pkgs.util-linux ];
          # Static-link the CRT for the musl release targets (Linux only; the
          # vars are simply unused elsewhere).
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS = "-C target-feature=+crt-static";
          CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS = "-C target-feature=+crt-static";
        };
      });
}
