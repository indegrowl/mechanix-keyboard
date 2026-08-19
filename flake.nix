{
  description = "mechanix-keyboard";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url       = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, fenix, crane, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs      = import nixpkgs { inherit system; };
        pkgsCross = pkgs.pkgsCross.aarch64-multiplatform;
        lib       = pkgs.lib;
        fn        = fenix.packages.${system};

        # Prebuilt std cross-build: pull in the aarch64 rust-std rather than
        # recompiling the standard library (no build-std). rust-src is dev-only,
        # for rust-analyzer.
        buildToolchain = fn.combine [
          fn.latest.cargo
          fn.latest.rustc
          fn.latest.clippy
          fn.latest.rustfmt
          fn.targets.aarch64-unknown-linux-gnu.latest.rust-std
        ];

        devToolchain = fn.combine [
          fn.latest.cargo
          fn.latest.rustc
          fn.latest.rust-src
          fn.latest.clippy
          fn.latest.rustfmt
          fn.targets.aarch64-unknown-linux-gnu.latest.rust-std
        ];

        craneLib = (crane.mkLib pkgs).overrideToolchain buildToolchain;

        src = craneLib.cleanCargoSource ./.;

        crossArgs = {
          inherit src;
          strictDeps = true;

          # Cross target: the test binaries are aarch64 and cannot execute on
          # the x86_64 build host, so skip the check phase.
          doCheck = false;

          CARGO_BUILD_TARGET = "aarch64-unknown-linux-gnu";

          CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER =
            "${pkgsCross.stdenv.cc}/bin/${pkgsCross.stdenv.cc.targetPrefix}cc";

          CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS =
            "-C target-cpu=cortex-a53";

          PKG_CONFIG_ALLOW_CROSS = "1";
          PKG_CONFIG_PATH = lib.makeSearchPathOutput "dev" "lib/pkgconfig"
            (with pkgsCross; [ libgbm libdrm ]);

          # libEGL/libGLESv2 are dlopen'd at runtime (khronos-egl `dynamic`
          # mode), so the only build-time cross link dep is libgbm, which pulls
          # libdrm through pkg-config.
          nativeBuildInputs = with pkgs; [ pkg-config clang pkgsCross.stdenv.cc ];
          buildInputs       = with pkgsCross; [ libgbm libdrm ];
        };

        keyboardAarch64 = craneLib.buildPackage (crossArgs // {
          pname   = "mechanix-keyboard";
          version = "0.1.0";

          nativeBuildInputs = (crossArgs.nativeBuildInputs or []) ++ [ pkgs.patchelf ];

          # The binary runs on the Mecha Comet (Fedora aarch64), not Nix. Point
          # its interpreter at the device's glibc loader and drop the /nix/store
          # rpath so glibc's default search path (/usr/lib64) resolves
          # libgbm/libEGL/libGLESv2/libdrm on the device.
          postInstall = ''
            patchelf \
              --set-interpreter /lib/ld-linux-aarch64.so.1 \
              --remove-rpath \
              $out/bin/mechanix-keyboard
          '';
        });

      in {
        packages = {
          mechanix-keyboard-aarch64 = keyboardAarch64;
          default                   = keyboardAarch64;
        };

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = [
            devToolchain
            pkgs.pkg-config
            pkgs.mold
            pkgs.clang
            pkgs.rust-analyzer
            pkgs.cargo-watch
          ];

          buildInputs = with pkgs; [ mesa libdrm libgbm libGL wayland wayland-protocols libxkbcommon ];

          LD_LIBRARY_PATH = lib.makeLibraryPath (with pkgs; [ mesa libdrm libgbm libGL ]);

          RUST_SRC_PATH  = "${devToolchain}/lib/rustlib/src/rust/library";
          RUST_BACKTRACE = "1";
        };
      }
    );
}
