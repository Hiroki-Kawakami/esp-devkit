{
  description = "ESP32 development environment";

  # Two ESP-IDF versions are pinned side by side:
  #   default -> v6.0.2 (stable, all shipping boards)
  #   beta    -> v6.1-beta1 (needed for esp32s31 / ESP32-S31-Korvo)
  # crosstool-NG (nix/toolchains.nix) is shared: both IDFs pin esp-15.2.0_20251204.
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-26.05";
    flake-utils.url = "github:numtide/flake-utils";
    esp-idf-src = {
      url = "git+https://github.com/espressif/esp-idf?ref=refs/tags/v6.0.2&submodules=1";
      flake = false;
    };
    esp-idf-beta-src = {
      url = "git+https://github.com/espressif/esp-idf?ref=refs/tags/v6.1-beta1&submodules=1";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, esp-idf-src, esp-idf-beta-src }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

        toolchains = import ./nix/toolchains.nix { inherit pkgs system; };
        pythonEnv = (import ./nix/python-env.nix { inherit pkgs; }).pythonEnv;

        # Copy the IDF checkout into a writable store path (idf_tools writes to it).
        mkIdfStore = { src, tag }: pkgs.runCommandLocal "esp-idf-${tag}" { } ''
          mkdir -p $out
          cp -R ${src}/. $out/
          chmod -R u+w $out
        '';

        mkIdfShell = { idfStore, idfVersion }: pkgs.mkShell {
          packages = [
            pythonEnv
            toolchains.xtensa
            toolchains.riscv32
            toolchains.xtensaGdb
            toolchains.riscv32Gdb
            pkgs.cmake
            pkgs.ninja
            pkgs.gperf
            pkgs.git
            # Host simulator toolchain (simulator/)
            pkgs.gcc
            pkgs.ccache
            pkgs.cjson
            pkgs.SDL2
            pkgs.libjpeg
            pkgs.zlib
            pkgs.pkg-config
          ];
          shellHook = ''
            export IDF_PATH=${idfStore}
            export IDF_PYTHON_ENV_PATH=${pythonEnv}
            # Python env is fully Nix-managed; skip idf_tools.py's online
            # constraints fetch + version check.
            export IDF_PYTHON_CHECK_CONSTRAINTS=no
            export IDF_COMPONENT_MANAGER=1
            export ESP_IDF_VERSION="${idfVersion}"
            export ESP_ROM_ELF_DIR="${toolchains.romElfs}/"  # trailing slash required
            export HOST_GCC="${pkgs.gcc}"
            export PATH=$IDF_PATH/tools:${toolchains.xtensa}/bin:${toolchains.riscv32}/bin:${toolchains.xtensaGdb}/bin:${toolchains.riscv32Gdb}/bin:$PATH
          '';
        };
      in {
        devShells.default = mkIdfShell {
          idfStore = mkIdfStore { src = esp-idf-src; tag = "v6.0.2"; };
          idfVersion = "6.0";
        };

        # Enter with: nix develop .#beta -c <cmd>
        devShells.beta = mkIdfShell {
          idfStore = mkIdfStore { src = esp-idf-beta-src; tag = "v6.1-beta1"; };
          idfVersion = "6.1";
        };
      }
    );
}
