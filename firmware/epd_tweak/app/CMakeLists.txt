# Shared epd_tweak app component.
idf_component_register(
    SRCS "/dev/null"
    PRIV_REQUIRES bsp_rs devkit_core devkit_gfx devkit_image harness_rs idf rust_rt)
devkit_rust_crate(ROOT "lib.rs" RESOURCES ../resources/resources.json)
