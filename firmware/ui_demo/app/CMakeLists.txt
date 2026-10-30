# Shared ui_demo app component.
idf_component_register(
    SRCS "/dev/null"
    PRIV_REQUIRES bsp_rs devkit devkit_gfx devkit_ui harness_rs idf rust_rt)
devkit_rust_crate(ROOT "lib.rs" RESOURCES ../resources/resources.json)
