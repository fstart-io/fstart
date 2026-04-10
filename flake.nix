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
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = [
              rust

              # QEMU — all system emulators
              pkgs.qemu

              # Cross-compilers for building Linux / OpenSBI / TF-A
              pkgs.pkgsCross.riscv64.stdenv.cc
              pkgs.pkgsCross.aarch64-multiplatform.stdenv.cc
              pkgs.pkgsCross.armv7l-hf-multiplatform.stdenv.cc

              # Kernel / firmware build tools
              pkgs.gnumake
              pkgs.flex
              pkgs.bison
              pkgs.bc
              pkgs.perl
              pkgs.elfutils

              # Device tree / FIT image
              pkgs.dtc
              pkgs.ubootTools

              # Go (u-root initramfs)
              pkgs.go

              # Utilities
              pkgs.git
              pkgs.curl
              pkgs.xz
            ];
          };
        }
      );
    };
}
