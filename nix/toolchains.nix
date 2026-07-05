{ pkgs, system }:

let
  # crosstool-NG release tag (matches the GitHub release page) and the
  # in-tarball version string (no esp- prefix). Bumped together for IDF.
  releaseTag = "esp-15.2.0_20251204";
  version = pkgs.lib.removePrefix "esp-" releaseTag;

  tarballUrl = arch: hostTriple:
    "https://github.com/espressif/crosstool-NG/releases/download/${releaseTag}/${arch}-esp-elf-${version}-${hostTriple}.tar.xz";

  # gdb (from binutils-gdb, not crosstool-NG) — needed by `idf.py monitor` backtraces.
  gdbReleaseTag = "esp-gdb-v17.1_20260402";
  gdbVersion = "17.1_20260402";

  gdbUrl = arch: hostTriple:
    "https://github.com/espressif/binutils-gdb/releases/download/${gdbReleaseTag}/${arch}-esp-elf-gdb-${gdbVersion}-${hostTriple}.tar.gz";

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

  # macOS builds carry an OS-version suffix (…-darwin24.5), so pin full URLs per system.
  gdbSources = {
    xtensa-esp-elf-gdb = {
      "aarch64-darwin" = { url = gdbUrl "xtensa" "aarch64-apple-darwin24.5"; sha256 = "da97440e74a9ff36370bdb598cf421a8183c11ae6fb44431be594ad16dbe77ef"; };
      "x86_64-darwin"  = { url = gdbUrl "xtensa" "x86_64-apple-darwin24.5";  sha256 = "706d58849fd4a83244023051605b3631e835565395fa2783ed5afce1f17413ee"; };
      "x86_64-linux"   = { url = gdbUrl "xtensa" "x86_64-linux-gnu";         sha256 = "73bc6c4e50b06dceb60e94b53aded61b7769be3cf563572269d9c8d643db8e95"; };
      "aarch64-linux"  = { url = gdbUrl "xtensa" "aarch64-linux-gnu";        sha256 = "00290ffe21b2916ffd343fd28ac34fc8b93e99b992a3656fa09b4f2bdc564bea"; };
    };
    riscv32-esp-elf-gdb = {
      "aarch64-darwin" = { url = gdbUrl "riscv32" "aarch64-apple-darwin24.5"; sha256 = "f944bb6a07b03e5d74dd1f878ba05320ee7957d3a06bcc80cd0a9cf355e8ceea"; };
      "x86_64-darwin"  = { url = gdbUrl "riscv32" "x86_64-apple-darwin24.5";  sha256 = "4845ec4968207e40c9f840f29007d8e0cd7d11eb613bb9e9078a41bb0ceb6d4d"; };
      "x86_64-linux"   = { url = gdbUrl "riscv32" "x86_64-linux-gnu";         sha256 = "35f3db841338cb4f9bc60d757bc3e87dfa50ff50607bcbc3867c5b1ac28dd342"; };
      "aarch64-linux"  = { url = gdbUrl "riscv32" "aarch64-linux-gnu";        sha256 = "f185d924497750f254290a32c48163c08e3b2a29eb248d739d90990ebee17f44"; };
    };
  };

  pickFrom = set: name:
    let s = set.${name}.${system} or (throw "esp toolchain ${name}: unsupported system ${system}");
    in pkgs.fetchurl { inherit (s) url sha256; };

  mkUnpacked = { name, ver, src, extraBuildInputs ? [] }:
    pkgs.stdenv.mkDerivation {
      pname = name;
      version = ver;
      inherit src;
      sourceRoot = ".";

      nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [
        pkgs.autoPatchelfHook
      ];
      buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux ([
        pkgs.stdenv.cc.cc.lib
        pkgs.zlib
        pkgs.libxcrypt-legacy
      ] ++ extraBuildInputs);

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

  mkToolchain = name: mkUnpacked { inherit name; ver = version; src = pickFrom sources name; };

  # extraBuildInputs are the python-enabled gdb binary's shared-lib deps (Linux patchelf).
  mkGdb = name: mkUnpacked {
    inherit name;
    ver = gdbVersion;
    src = pickFrom gdbSources name;
    extraBuildInputs = [ pkgs.python3 pkgs.ncurses pkgs.expat pkgs.gmp pkgs.mpfr ];
  };

  # ROM symbol ELFs; ESP_ROM_ELF_DIR feeds the build's gdbinit for monitor backtraces.
  romElfs = pkgs.stdenvNoCC.mkDerivation {
    pname = "esp-rom-elfs";
    version = "20241011";
    src = pkgs.fetchurl {
      url = "https://github.com/espressif/esp-rom-elfs/releases/download/20241011/esp-rom-elfs-20241011.tar.gz";
      sha256 = "921f000164a421c7628fbfee55b173384aafaa51883adc65cd27bf9b0af9e9a9";
    };
    sourceRoot = ".";
    dontBuild = true;
    dontConfigure = true;
    installPhase = ''
      runHook preInstall
      mkdir -p $out
      cp *.elf $out/
      runHook postInstall
    '';
  };
in {
  xtensa = mkToolchain "xtensa-esp-elf";
  riscv32 = mkToolchain "riscv32-esp-elf";
  xtensaGdb = mkGdb "xtensa-esp-elf-gdb";
  riscv32Gdb = mkGdb "riscv32-esp-elf-gdb";
  inherit romElfs;
}
