{
  description = "Vulcan Rust workspace";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      crane,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        src = craneLib.cleanCargoSource ./.;

        commonArgs = {
          inherit src;
          pname = "vulcan";
          version = "0.1.0";
          strictDeps = true;
          cargoExtraArgs = "-p vulcan --bin vulcan";

          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [
            pkgs.openssl
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.libiconv
            pkgs.darwin.apple_sdk.frameworks.Security
            pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
          ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        vulcan = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
          }
        );
      in
      {
        checks = {
          inherit vulcan;

          fmt = craneLib.cargoFmt {
            inherit src;
          };
        };

        packages.default = vulcan;

        apps.default = flake-utils.lib.mkApp {
          drv = vulcan;
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          inputsFrom = [ vulcan ];

          packages = with pkgs; [
            cargo-deny
            cargo-hack
            cargo-llvm-cov
            cargo-machete
            cargo-nextest
            nil
            rust-analyzer
            rustToolchain
            taplo
          ];

          RUST_BACKTRACE = "1";

          shellHook = ''
            unset RUSTC_WRAPPER
            unset CARGO_BUILD_RUSTC_WRAPPER
          '';
        };
      }
    );
}
