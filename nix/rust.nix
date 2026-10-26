{ pkgs, system, clangLibs }:

let
  lib = pkgs.lib;

  # esp-rs/rust-build release tag; the in-tarball version drops the v prefix.
  releaseTag = "v1.97.0.0";
  version = lib.removePrefix "v" releaseTag;

  tarballUrl = name: "https://github.com/esp-rs/rust-build/releases/download/${releaseTag}/${name}-${version}";

  hosts = {
    "aarch64-darwin" = { triple = "aarch64-apple-darwin";      sha256 = "430fcbf54967e99d16debe48926a1df558ab8b67af3c060a658d25ce752bd790"; };
    "x86_64-linux"   = { triple = "x86_64-unknown-linux-gnu";  sha256 = "a99bfee69221e9ff6d86388f6811ee688cd405e6a0400a3cd1784e8d463e9d99"; };
    "aarch64-linux"  = { triple = "aarch64-unknown-linux-gnu"; sha256 = "dc6efa8906849608497c2ffb3d6a9466588835a79b5e8886c229de133640945c"; };
  };
  host = hosts.${system} or (throw "esp rust toolchain: unsupported system ${system}");

  rustTarball = pkgs.fetchurl {
    url = tarballUrl "rust" + "-${host.triple}.tar.xz";
    inherit (host) sha256;
  };
  rustSrcTarball = pkgs.fetchurl {
    url = tarballUrl "rust-src" + ".tar.xz";
    sha256 = "568d688b9f8f332ec4d04657544fad23e99ce11e9e6cf5835979e68a28c68b73";
  };

  # The release ships std for the host only; core/alloc/compiler_builtins for
  # these are built from rust-src below.
  targets = [
    "riscv32imac-unknown-none-elf"
    "riscv32imafc-unknown-none-elf"
    "xtensa-esp32-none-elf"
    "xtensa-esp32s3-none-elf"
  ];

  buildSysroot = target: ''
    out_dir=$out/lib/rustlib/${target}/lib
    mkdir -p $out_dir
    common="--target ${target} --edition 2024 --crate-type rlib -Copt-level=3 -Cpanic=abort -Cdebuginfo=1 -Cembed-bitcode=no -Zforce-unstable-if-unmarked --cap-lints allow"
    $out/bin/rustc $common --crate-name core $src_lib/core/src/lib.rs --out-dir $out_dir
    # No feature "mem": memcpy and friends come from libc/ROM. codegen-units=10000 puts
    # each intrinsic in its own archive member so libgcc's copies are never pulled alongside.
    $out/bin/rustc $common --crate-name compiler_builtins $src_lib/compiler-builtins/compiler-builtins/src/lib.rs \
      --cfg 'feature="compiler-builtins"' --cfg 'feature="unmangled-names"' --cfg 'feature="arch"' \
      --cfg intrinsics_enabled --cfg optimizations_enabled \
      -Ccodegen-units=10000 -Cdebug-assertions=no -Coverflow-checks=no \
      --extern core=$out_dir/libcore.rlib --out-dir $out_dir
    $out/bin/rustc $common --crate-name alloc $src_lib/alloc/src/lib.rs \
      --extern core=$out_dir/libcore.rlib --extern compiler_builtins=$out_dir/libcompiler_builtins.rlib \
      --out-dir $out_dir
  '';

  rustc = pkgs.stdenv.mkDerivation {
    pname = "rust-esp";
    inherit version;
    srcs = [ rustTarball rustSrcTarball ];
    sourceRoot = ".";

    nativeBuildInputs = lib.optionals pkgs.stdenv.isLinux [ pkgs.autoPatchelfHook ];
    buildInputs = lib.optionals pkgs.stdenv.isLinux [ pkgs.stdenv.cc.cc.lib pkgs.zlib ];

    dontConfigure = true;
    dontBuild = true;
    dontStrip = true;

    installPhase = ''
      runHook preInstall
      patchShebangs rust-nightly-${host.triple}/install.sh rust-src-nightly/install.sh
      rust-nightly-${host.triple}/install.sh --prefix=$out --disable-ldconfig \
        --components=rustc,rust-std-${host.triple},rustfmt-preview
      rust-src-nightly/install.sh --prefix=$out --disable-ldconfig
      ${lib.optionalString pkgs.stdenv.isLinux "autoPatchelf $out"}
      src_lib=$out/lib/rustlib/src/rust/library
      patch -p1 -d $src_lib < ${./patches/compiler-builtins-no-libm.patch}
      ${lib.concatMapStrings buildSysroot targets}
      runHook postInstall
    '';
  };

  bindgenUnwrapped = pkgs.rust-bindgen-unwrapped.overrideAttrs {
    cargoBuildFeatures = [ "logging" "runtime" ];
    doCheck = false;
  };

  bindgen = pkgs.runCommandLocal "bindgen-esp-${bindgenUnwrapped.version}" {
    nativeBuildInputs = [ pkgs.makeWrapper ];
  } ''
    # Loaded at runtime on Linux, libclang resolves its resource directory to a
    # relative lib/clang/<version> and misses its builtin headers (stddef.h).
    makeWrapper ${bindgenUnwrapped}/bin/bindgen $out/bin/bindgen \
      --set LIBCLANG_PATH ${clangLibs}/lib \
      --set BINDGEN_EXTRA_CLANG_ARGS "-resource-dir $(echo ${clangLibs}/lib/clang/*)"
  '';
in {
  inherit rustc bindgen;
}
