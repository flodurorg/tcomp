{
  description = "tcomp — terminal companion: read-only terminal broadcast";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        rust = with pkgs; [
          rustc
          cargo
          clippy
          rustfmt
          rust-analyzer
          pkg-config
        ];

        browser = with pkgs; [
          chromium
          chromedriver
          xterm
          xvfb-run
        ];

        recording = with pkgs; [
          bashInteractive
          curl
          dejavu_fonts
          ffmpeg-full
          gifsicle
          jq
          util-linux
          websocat
          xdotool
          xsetroot
        ];
      in {
        packages.default = pkgs.callPackage ./nix/package.nix { };

        devShells.default = pkgs.mkShell {
          packages = rust ++ browser;
          RUST_BACKTRACE = "1";

          shellHook = ''
            if [ -t 1 ]; then
              echo "tcomp dev shell"
              echo
              echo "  cargo build                                build the binary"
              echo "  cargo test                                 run the tests"
              echo "  cargo clippy --all-targets -- -D warnings  lint"
              echo "  cargo fmt                                  format"
              echo "  cargo run -p tcomp -- standalone -- htop   try it locally"
              echo "  nix build                                  build the package"
              echo "  cargo test --test floating_windows -- --ignored"
              echo "                                             run the browser e2e test"
              echo "  nix develop .#demo                         shell for recording the demo GIF"
              echo
            fi
          '';
        };

        devShells.demo = pkgs.mkShell {
          packages = rust ++ browser ++ recording;
          RUST_BACKTRACE = "1";

          shellHook = ''
            if [ -t 1 ]; then
              echo "tcomp demo shell"
              echo
              echo "  ./docs/record-demo.sh                      regenerate docs/demo.gif"
              echo
            fi
          '';
        };
      });
}
