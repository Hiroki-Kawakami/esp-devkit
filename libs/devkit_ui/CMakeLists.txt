# UI framework: framebuffer presentation on bsp_rs.
idf_component_register(
    SRCS "/dev/null"
    REQUIRES bsp_rs devkit_gfx idf
    PRIV_REQUIRES rust_rt)
devkit_rust_crate(ROOT "src/lib.rs")
