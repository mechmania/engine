{
  description = "official mechmania engine";

  inputs = {
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ ];
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      perSystem = { config, self', inputs', pkgs, system, ... }: {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          name = "mm-engine";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
        };

        apps.default = {
          type = "app";
          package = config.packges.default;
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
          ];
        };
      };
      flake = { };
    };
}
