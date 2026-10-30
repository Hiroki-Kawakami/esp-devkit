# Streaming decompressors.
idf_component_register(SRCS "/dev/null" PRIV_REQUIRES devkit rust_rt)
devkit_rust_crate(ROOT "src/lib.rs" TESTS OPT_LEVEL 2)
