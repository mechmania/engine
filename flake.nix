{
  description = "official mechmania engine";

  inputs = {
    flake-parts.url = "github:hercules-ci/flake-parts";
    claude.url = "github:sadjow/claude-code-nix";
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
          program = "${config.packages.default}/bin/mm-engine";
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            inputs.claude.packages.${system}.default
            # The test suite does not fit in one `cargo test`. `engine` and `client` are
            # mutually exclusive -- `src/lib.rs` `compile_error!`s on both -- and `ffi`
            # implies `client`, so `ffi.rs` and the layout registry's tests are invisible
            # to a default-feature run. Two invocations, and this is the one that runs
            # both of them.
            (writeShellScriptBin "test-all" ''
              set -e
              root="$(git rev-parse --show-toplevel)"
              cd "$root"
              echo "== default features (engine) =="
              cargo test "$@"
              echo
              echo "== ffi (client) =="
              cargo test --no-default-features --features ffi "$@"
            '')
          ];
        };
      };
      flake = { };
    };
}
