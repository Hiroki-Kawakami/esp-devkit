{ pkgs, system }:

let
  # crosstool-NG release tag (matches the GitHub release page) and the
  # in-tarball version string (no esp- prefix). Bumped together for IDF.
  releaseTag = "esp-14.2.0_20250730";
  version = pkgs.lib.removePrefix "esp-" releaseTag;

  tarballUrl = arch: hostTriple:
    "https://github.com/espressif/crosstool-NG/releases/download/${releaseTag}/${arch}-esp-elf-${version}-${hostTriple}.tar.xz";

  # Pinned from esp-idf v5.4.3 tools/tools.json.
  sources = {
    xtensa-esp-elf = {
      "aarch64-darwin" = { url = tarballUrl "xtensa" "aarch64-apple-darwin"; sha256 = "954d88961660e51599a98855cf9ed8550801e27ee10c2184a258b93c38a1edcc"; };
      "x86_64-darwin"  = { url = tarballUrl "xtensa" "x86_64-apple-darwin";  sha256 = "96d6d8388ba0710b99a0659c1dee29a16dbd0c6c5cc49a5baf91dca634167205"; };
      "x86_64-linux"   = { url = tarballUrl "xtensa" "x86_64-linux-gnu";     sha256 = "4fd6d2517f55161056b735cc53c7ccfa59c30a574a0f4decfad77cae4ca5f711"; };
      "aarch64-linux"  = { url = tarballUrl "xtensa" "aarch64-linux-gnu";    sha256 = "916a3007a75c6e4b252cb1857a00657cd0c90ebc60fc265cfa0f4cd7d18ace5c"; };
    };
    riscv32-esp-elf = {
      "aarch64-darwin" = { url = tarballUrl "riscv32" "aarch64-apple-darwin"; sha256 = "f605c426966e58cfddd5bb86967dc63e15812dbf0118a28c97c7fef8178937e5"; };
      "x86_64-darwin"  = { url = tarballUrl "riscv32" "x86_64-apple-darwin";  sha256 = "6dbb7718d332f6a300fc85b2bdd87c7decae0739bdb1a73bfe57ad034053fbb3"; };
      "x86_64-linux"   = { url = tarballUrl "riscv32" "x86_64-linux-gnu";     sha256 = "5c467d91a0ee58c2c08ce3950e00f512d93330fbc89d2a290fb37405fe805942"; };
      "aarch64-linux"  = { url = tarballUrl "riscv32" "aarch64-linux-gnu";    sha256 = "aaa865a2d9a6b7a042af814d14c0d28f0b17dc30d83c2a5b32b96e7c2ba3bacb"; };
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
