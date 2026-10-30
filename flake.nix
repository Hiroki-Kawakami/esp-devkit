{
  description = "ESP32 development environment";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-26.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;

        toolchains = import ./nix/toolchains.nix { inherit pkgs system; };
        rust = import ./nix/rust.nix { inherit pkgs system; inherit (toolchains) clangLibs; };
        pythonEnv = (import ./nix/python-env.nix { inherit pkgs; }).pythonEnv;
        # tools/resgen; kept off PATH so it never shadows the IDF python3.
        resgenPython = pkgs.python3.withPackages (ps: [ ps.freetype-py ps.pillow ps.resvg-py ps.fonttools ]);

        # Flake git inputs fetch every submodule with its full history (gigabytes); fetchgit takes depth 1.
        esp-idf-src = pkgs.fetchgit {
          name = "esp-idf-v6.1-src";
          url = "https://github.com/espressif/esp-idf";
          rev = "fff9895c82d744c7237be8847347bdd1b07c6643";
          fetchSubmodules = true;
          hash = "sha256-6+mPhof81Yrre8CFar33ufzmPHedSnVfTG5G9A6MbYM=";
        };

        # Copy the IDF checkout into a writable store path (idf_tools writes to it).
        # Each patch's header says what it changes and where it comes from.
        idfStore = pkgs.runCommandLocal "esp-idf-v6.1" { } ''
          mkdir -p $out
          cp -R ${esp-idf-src}/. $out/
          chmod -R u+w $out
          patch -p1 -d $out < ${./nix/patches/ppa-srm-rotation-dig734.patch}
          patch -p1 -d $out < ${./nix/patches/ppa-srm-dig734-threshold.patch}
          patch -p1 -d $out < ${./nix/patches/p4-rev1-cpu-400mhz.patch}
          patch -p1 -d $out < ${./nix/patches/p4-rev1-psram-220mhz.patch}
          patch -p1 -d $out < ${./nix/patches/fatfs-exfat.patch}
        '';

        idfToolchains = [
          toolchains.xtensa
          toolchains.riscv32
          toolchains.xtensaGdb
          toolchains.riscv32Gdb
        ];

        idfPackages = [ pythonEnv ] ++ idfToolchains ++ [
          rust.rustc
          rust.bindgen
          pkgs.cmake
          pkgs.ninja
          pkgs.gperf
          pkgs.git
        ];

        idfPath = "${idfStore}/tools:${lib.makeBinPath idfToolchains}";

        idfEnv = {
          IDF_PATH = "${idfStore}";
          IDF_PYTHON_ENV_PATH = "${pythonEnv}";
          # Python env is fully Nix-managed; skip idf_tools.py's online
          # constraints fetch + version check.
          IDF_PYTHON_CHECK_CONSTRAINTS = "no";
          IDF_COMPONENT_MANAGER = "1";
          ESP_IDF_VERSION = "6.1";
          ESP_ROM_ELF_DIR = "${toolchains.romElfs}/";  # trailing slash required
          RESGEN_PYTHON = "${resgenPython}/bin/python3";
        };

        mkFwImage = { name ? "esp-devkit-fw", extraPackages ? [ ], extraEnv ? { } }:
          let
            packages = idfPackages ++ [
              pkgs.bashInteractive
              pkgs.coreutils
              pkgs.findutils
              pkgs.gnugrep
              pkgs.gnused
              pkgs.gawk
              devkit
            ] ++ extraPackages;
            env = idfEnv // {
              PATH = "${idfPath}:${lib.makeBinPath packages}";
              HOME = "/tmp";
              SSL_CERT_FILE = "/etc/ssl/certs/ca-bundle.crt";
              # The checkout is owned by the CI runner user, not by root in the container.
              GIT_CONFIG_COUNT = "1";
              GIT_CONFIG_KEY_0 = "safe.directory";
              GIT_CONFIG_VALUE_0 = "*";
            } // extraEnv;
          in pkgs.dockerTools.streamLayeredImage {
            inherit name;
            tag = "latest";
            contents = [
              pkgs.dockerTools.binSh
              pkgs.dockerTools.usrBinEnv
              pkgs.dockerTools.caCertificates
              pkgs.dockerTools.fakeNss
            ];
            extraCommands = "mkdir -m 1777 tmp";
            config = {
              Env = lib.mapAttrsToList (k: v: "${k}=${v}") env;
              WorkingDir = "/work";
            };
          };
        # Host tool, built with nixpkgs' Rust rather than the esp toolchain.
        devkit = pkgs.rustPlatform.buildRustPackage {
          pname = "devkit";
          version = "0.1.0";
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./tools/devkit/Cargo.toml
              ./tools/devkit/Cargo.lock
              ./tools/devkit/src
              ./tools/devkit/assets
              ./libs/bsp_rs/src/sim/protocol.rs
              ./libs/devkit_gfx/src/format.rs
            ];
          };
          cargoRoot = "tools/devkit";
          buildAndTestSubdir = "tools/devkit";
          cargoLock.lockFile = ./tools/devkit/Cargo.lock;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.freetype ];
        };
      in {
        lib = { inherit mkFwImage; };

        packages.devkit = devkit;

        devShells.default = pkgs.mkShell {
          packages = idfPackages ++ [
            # Host simulator toolchain (simulator/)
            pkgs.gcc
            pkgs.ccache
            pkgs.SDL2
            pkgs.libjpeg
            pkgs.zlib
            pkgs.pkg-config
            # esp-devkit CLI (docs/devkit-cli.md); cargo for working on it
            devkit
            pkgs.cargo
            # Editor language server for the Rust crates (docs/rust.md)
            pkgs.rust-analyzer
          ];
          shellHook = ''
            ${lib.concatStringsSep "\n" (lib.mapAttrsToList (k: v: "export ${k}=${lib.escapeShellArg v}") idfEnv)}
            export HOST_GCC="${pkgs.gcc}"
            export PATH=${idfPath}:$PATH
          '';
        };
      }
    );
}
