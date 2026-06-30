{
  description = "ESP32 development environment";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-26.05";
    flake-utils.url = "github:numtide/flake-utils";
    esp-idf-src = {
      url = "git+https://github.com/espressif/esp-idf?ref=refs/tags/v5.4.3&submodules=1";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, esp-idf-src }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          config.permittedInsecurePackages = [
            "python3.13-ecdsa-0.19.2"
          ];
        };

        toolchains = import ./nix/toolchains.nix { inherit pkgs system; };
        pythonEnv = (import ./nix/python-env.nix { inherit pkgs; }).pythonEnv;

        # IDF source as a Nix store path.
        esp-idf = pkgs.runCommandLocal "esp-idf-v5.4.3" { } ''
          mkdir -p $out
          cp -R ${esp-idf-src}/. $out/
          chmod -R u+w $out
        '';
      in {
        devShells.default = pkgs.mkShell {
          packages = [
            pythonEnv
            toolchains.xtensa
            toolchains.riscv32
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
          ];
          shellHook = ''
            export IDF_PATH=${esp-idf}
            export IDF_PYTHON_ENV_PATH=${pythonEnv}
            # Python env is fully Nix-managed; skip idf_tools.py's online
            # constraints fetch + version check.
            export IDF_PYTHON_CHECK_CONSTRAINTS=no
            export IDF_COMPONENT_MANAGER=1
            export ESP_IDF_VERSION="5.4"
            export HOST_GCC="${pkgs.gcc}"
            export PATH=$IDF_PATH/tools:${toolchains.xtensa}/bin:${toolchains.riscv32}/bin:$PATH
          '';
        };
      }
    );
}
