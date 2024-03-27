{
  inputs = {
    flake-utils.url = "github:numtide/flake-utils";
    cargo2nix.url = "github:cargo2nix/cargo2nix";
    rust-overlay.follows = "cargo2nix/rust-overlay";
  };
  outputs = { self, nixpkgs, flake-utils, cargo2nix, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            rust-overlay.overlays.default
            cargo2nix.overlays.default
          ];
        };

        rustVersion = "1.75.0";

      in {
        devShell =
          pkgs.mkShell {
            buildInputs = [
              (pkgs.rust-bin.stable.${rustVersion}.default.override {
                extensions = [
                  "cargo"
                  "clippy"
                  "rustc"
                  "rust-src"
                  "rustfmt"
                  "rust-analyzer"
                ];
              })
              cargo2nix.packages.${system}.default
            ];
          };

        packages = rec {
          git-format-staged =
            let
              rustPkgs = pkgs.rustBuilder.makePackageSet {
                inherit rustVersion;
                packageFun = import ./Cargo.nix;  
              };
            in
              rustPkgs.workspace.git-format-staged {};
          default = git-format-staged;
        };
      }
    );
}
