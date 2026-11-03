# Network protocol stack.
idf_component_register(SRCS "/dev/null" PRIV_REQUIRES idf rust_rt)
devkit_rust_crate(ROOT "src/lib.rs" TESTS)
