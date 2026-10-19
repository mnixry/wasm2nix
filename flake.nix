{
  # spell-checker: disable
  inputs = {
    self.submodules = true;
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    determinate.url = "https://flakehub.com/f/DeterminateSystems/nix-src/*";
    crane.url = "github:ipetkov/crane";
    iets = {
      url = "github:numtide/iets";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.crane.follows = "crane";
    };
  };

  outputs =
    { flake-parts, ... }@inputs:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = inputs.nixpkgs.lib.systems.flakeExposed;
      imports = [ inputs.treefmt-nix.flakeModule ];
      perSystem =
        {
          config,
          lib,
          system,
          ...
        }:
        let
          pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ (import inputs.rust-overlay) ];
          };
          inherit (pkgs.stdenv) hostPlatform;

          rust = pkgs.rust-bin.stable.latest.default.override {
            targets = [
              "wasm32-unknown-unknown"
            ];
            extensions = [
              "rust-src"
              "llvm-tools-preview"
            ];
          };
          rustfmt = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.rustfmt);
          craneLib = (inputs.crane.mkLib pkgs).overrideToolchain (_: rust);
          craneCommonArgs = rec {
            src = ./.;
            inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;
            strictDeps = true;
            cargoVendorDir = craneLib.vendorMultipleCargoDeps {
              inherit (craneLib.findCargoFiles src) cargoConfigs;
              cargoLockList = [
                ./Cargo.lock
                "${rust}/lib/rustlib/src/rust/library/Cargo.lock"
              ];
            };
          };
          cargoArtifacts = craneLib.buildDepsOnly craneCommonArgs;
        in
        {
          _module.args = { inherit inputs pkgs rust; };
          treefmt =
            { ... }:
            {
              projectRootFile = ".git/config";
              settings.global.excludes = [
                "crates/wast/tests/spec/**"
              ];
              programs.nixfmt.enable = true;
              programs.taplo.enable = true;
              programs.rustfmt = {
                enable = true;
                package = rustfmt;
              };
              programs.yamlfmt.enable = true;
              programs.actionlint.enable = true;
            };
          devShells.default = pkgs.mkShell {
            inputsFrom = [ config.treefmt.build.devShell ];
            buildInputs = [
              (lib.hiPrio rustfmt)
              rust
            ]
            ++ (
              with pkgs;
              [
                cargo-edit
                cargo-nextest
              ]
              ++ lib.optionals hostPlatform.isLinux [
                llvmPackages.bolt
                perf
              ]
              ++ lib.optionals hostPlatform.isDarwin [
                darwin.libiconv
                darwin.libresolv
              ]
            );
            env = lib.optionalAttrs hostPlatform.isDarwin { SDKROOT = pkgs.apple-sdk.sdkroot; };
          };
          checks = (
            {
              clippy = craneLib.cargoClippy (
                craneCommonArgs
                // {
                  inherit cargoArtifacts;
                  cargoClippyExtraArgs = "--all-targets --all-features";
                }
              );
            }
            //
              lib.mapAttrs'
                (
                  name: args:
                  lib.nameValuePair "tests@${name}" (
                    (craneLib.cargoNextest (craneCommonArgs // { inherit cargoArtifacts; } // args)).overrideAttrs (
                      { pname, ... }: {
                        pname = "${pname}-${name}";
                      }
                    )
                  )
                )
                {
                  "NixOS/nix".nativeBuildInputs = [ pkgs.nixVersions.latest ];
                  "lix-project/lix".nativeBuildInputs = [ pkgs.lixPackageSets.latest.lix ];
                  "DeterminateSystems/nix-src".nativeBuildInputs = [ inputs.determinate.packages.${system}.default ];
                  "numtide/iets" = {
                    nativeBuildInputs = [ inputs.iets.packages.${system}.default ];
                    env.WASM2NIX_TEST_EVAL_CMD = "iets nix --extra-experimental-features 'nix-command flakes' eval --store unix:///nonexistent";
                  };
                }
          );
          packages.default = craneLib.buildPackage (
            craneCommonArgs
            // {
              inherit cargoArtifacts;
              doCheck = false;
            }
          );
        };
    };
}
