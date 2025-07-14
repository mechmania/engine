{
  description = "mechmania 31 game engine";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self
    , nixpkgs
    , ...
    } @ inputs:
    inputs.flake-utils.lib.eachDefaultSystem (system:
    let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ (import inputs.rust-overlay) ];
      };

      craneLib = (inputs.crane.mkLib pkgs).overrideToolchain (
        p:
        p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml
      );

      # Common arguments can be set here to avoid repeating them later
      # Note: changes here will rebuild all dependency crates
      commonArgs = {
        src = craneLib.cleanCargoSource ./.;
        strictDeps = true;

        buildInputs =
          [
            # Add additional build inputs here
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            # Additional darwin specific inputs can be set here
            pkgs.libiconv
          ];
      };

      bin = craneLib.buildPackage (commonArgs
        // {
            cargoArtifacts = craneLib.buildDepsOnly commonArgs;
      });
    in
    {
      checks = {

        inherit bin;
      };
      packages.default = bin;
      apps.default = inputs.flake-utils.lib.mkApp {
        drv = bin;
      };
      devShells.default = craneLib.devShell {
        checks = self.checks.${system};
        packages = [
          (pkgs.python3.withPackages (ps:
            with ps; [
              pygame
              toml
            ]))
          pkgs.gradle
          pkgs.jdk24
        ];
      };
    });
}
