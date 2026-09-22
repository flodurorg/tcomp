{
  description = "tcomp — terminal companion: read-only terminal broadcast";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let pkgs = nixpkgs.legacyPackages.${system};
      in {
        packages.default = pkgs.callPackage ./nix/package.nix { };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            rust-analyzer
            pkg-config
            websocat

            # docs/record-demo.sh
            bashInteractive
            chromium
            ffmpeg-full
            xvfb-run
            gifsicle
            xdotool
            xterm
            xsetroot
            util-linux
            dejavu_fonts
            jq

            # tests/floating_windows.rs
            chromedriver
          ];
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
              echo "  docs/record-demo.sh                        regenerate the demo GIF"
              echo "  cargo test --test floating_windows -- --ignored"
              echo "                                              run the browser e2e test"
              echo
            fi
          '';
        };
      });
}
