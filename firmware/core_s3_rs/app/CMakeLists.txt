# Shared core_s3_rs app component.
idf_component_register(
    SRCS "/dev/null"
    PRIV_REQUIRES bsp_rs devkit_gfx idf rust_rt)
devkit_rust_crate(ROOT "lib.rs")
