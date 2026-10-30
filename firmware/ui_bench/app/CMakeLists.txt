# Shared ui_bench app component.
idf_component_register(
    SRCS "/dev/null"
    PRIV_REQUIRES bsp_rs harness_rs idf rust_rt)
devkit_rust_crate(ROOT "lib.rs")
