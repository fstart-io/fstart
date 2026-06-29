{
  description = "fstart firmware framework";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      mkPkgs = system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
      ciPayloadTools = pkgs: [
        pkgs.pkgsCross.riscv64.stdenv.cc
        pkgs.pkgsCross.aarch64-multiplatform.stdenv.cc
        pkgs.pkgsCross.armv7l-hf-multiplatform.stdenv.cc
        pkgs.gnumake
        pkgs.flex
        pkgs.bison
        pkgs.bc
        pkgs.perl
        pkgs.elfutils
        pkgs.dtc
        pkgs.ubootTools
        pkgs.git
        pkgs.curl
        pkgs.xz
      ];
    in
    {
      apps = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          ciPayloads = pkgs.writeShellApplication {
            name = "fstart-ci-build-payloads";
            runtimeInputs = ciPayloadTools pkgs;
            text = ''
              set -euo pipefail
              output_dir="''${1:-payloads}"
              exec bash ci/build-payloads.sh "$output_dir"
            '';
          };
        in
        {
          ci-build-payloads = {
            type = "app";
            program = "${ciPayloads}/bin/fstart-ci-build-payloads";
            meta.description = "Build CI payload artifacts via ci/build-payloads.sh";
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = mkPkgs system;
          rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = [
              rust

              # QEMU, all system emulators used by xtask and CI smoke tests.
              pkgs.qemu

              # CI payload build resources. Keep this in sync with
              # ci/build-payloads.sh and .github/workflows/ci.yml.
            ] ++ ciPayloadTools pkgs ++ [
              # Go (u-root/initramfs experiments and future boot assets)
              pkgs.go
            ];
          };
        }
      );
    };
}
