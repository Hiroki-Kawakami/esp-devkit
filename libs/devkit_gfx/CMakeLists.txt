# IDF-independent 2D rendering.
idf_component_register(SRCS "/dev/null" PRIV_REQUIRES rust_rt)
devkit_rust_crate(ROOT "src/lib.rs" TESTS)
