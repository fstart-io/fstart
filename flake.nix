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
      mkPkgs =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
    in
    {
      # What the QEMU boot tests boot besides fstart (ci/boot-assets).
      packages.x86_64-linux = import ./ci/boot-assets { pkgs = mkPkgs "x86_64-linux"; };

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
              # iasl, for the boards' ACPI tables.
              pkgs.acpica-tools
              # Every system emulator fbuild runs, and the TPM of the
              # measured-boot tests.
              pkgs.qemu
              pkgs.swtpm
            ];
          };

          # What CI's lint job runs.
          lint = pkgs.mkShell {
            nativeBuildInputs = [
              pkgs.actionlint
              pkgs.nixfmt
              pkgs.shellcheck
              pkgs.zizmor
            ];
          };
        }
      );
    };
}
