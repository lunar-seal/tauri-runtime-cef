{
  description = "Rust Rover environment";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        rustToolchain = pkgs.rust-bin.stable."1.88.0".default.override {
          extensions = [ "rust-src" "clippy" "rustfmt" ];
        };
      in {
        devShell = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustToolchain
          ];

          buildInputs = with pkgs; [
            gtk3
            openssl
            pkg-config
            webkitgtk_4_1

            rust-analyzer
          ];

          shellHook = ''
            # cef-dll-sys preserves read-only archive modes, then overwrites
            # those files when its build script changes.
            if [ -d target/debug ]; then
              find target/debug -maxdepth 2 -type f ! -perm -u=w -exec chmod u+w {} +
            fi

            mkdir -p ~/.rust-rover/toolchain

            ln -sfn ${rustToolchain}/lib ~/.rust-rover/toolchain
            ln -sfn ${rustToolchain}/bin ~/.rust-rover/toolchain

            export RUST_SRC_PATH="$HOME/.rust-rover/toolchain/lib/rustlib/src/rust/library"
          '';
        };
      }
    );
}
