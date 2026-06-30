{ pkgs, system }:

let
  # crosstool-NG release tag (matches the GitHub release page) and the
  # in-tarball version string (no esp- prefix). Bumped together for IDF.
  releaseTag = "esp-15.2.0_20251204";
  version = pkgs.lib.removePrefix "esp-" releaseTag;

  tarballUrl = arch: hostTriple:
    "https://github.com/espressif/crosstool-NG/releases/download/${releaseTag}/${arch}-esp-elf-${version}-${hostTriple}.tar.xz";

  # Pinned from esp-idf v6.0.2 tools/tools.json.
  sources = {
    xtensa-esp-elf = {
      "aarch64-darwin" = { url = tarballUrl "xtensa" "aarch64-apple-darwin"; sha256 = "68d3fb1e75c6bb1b88c6a2c74977abd51efd09b560a99149bafdcf403cb21941"; };
      "x86_64-darwin"  = { url = tarballUrl "xtensa" "x86_64-apple-darwin";  sha256 = "96da1fcf01e2ac89819d1e336ca9e27762c35ea120627b89de8fd482f42c54f8"; };
      "x86_64-linux"   = { url = tarballUrl "xtensa" "x86_64-linux-gnu";     sha256 = "3d50f5cd5f173acfd524e07c1cd69bc99585731a415ca2e5bce879997fe602b8"; };
      "aarch64-linux"  = { url = tarballUrl "xtensa" "aarch64-linux-gnu";    sha256 = "c8a8255009803036ba3def98a97a7134ee5a8ac5db048425e126fcf07f27ce1c"; };
    };
    riscv32-esp-elf = {
      "aarch64-darwin" = { url = tarballUrl "riscv32" "aarch64-apple-darwin"; sha256 = "0869d1083532c631808543dd802885f02dbe1bb3bd640be0dee827e82ded768d"; };
      "x86_64-darwin"  = { url = tarballUrl "riscv32" "x86_64-apple-darwin";  sha256 = "6d4709eadf4c66aecb51c0ff9c7b068eefa6ecec37aa7817f172c9f735318e73"; };
      "x86_64-linux"   = { url = tarballUrl "riscv32" "x86_64-linux-gnu";     sha256 = "ace5aae6afe98f754947be043d40173e2e22ace57754b11a394b7238eefa01cf"; };
      "aarch64-linux"  = { url = tarballUrl "riscv32" "aarch64-linux-gnu";    sha256 = "90cccb3ef035f016836dd7c292528b27333a716d42b9361a68005d178c0f70bf"; };
    };
  };

  pickSrc = name:
    let s = sources.${name}.${system} or (throw "esp toolchain ${name}: unsupported system ${system}");
    in pkgs.fetchurl { inherit (s) url sha256; };

  mkToolchain = name:
    pkgs.stdenv.mkDerivation {
      pname = name;
      inherit version;
      src = pickSrc name;
      sourceRoot = ".";

      nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [
        pkgs.autoPatchelfHook
      ];
      buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [
        pkgs.stdenv.cc.cc.lib
        pkgs.zlib
        pkgs.libxcrypt-legacy
      ];

      dontBuild = true;
      dontConfigure = true;
      dontStrip = true;

      installPhase = ''
        runHook preInstall
        mkdir -p $out
        cp -R ${name}/. $out/
        runHook postInstall
      '';
    };
in {
  xtensa = mkToolchain "xtensa-esp-elf";
  riscv32 = mkToolchain "riscv32-esp-elf";
}
