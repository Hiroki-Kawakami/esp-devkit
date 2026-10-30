idf_component_register(
    SRCS "/dev/null"
    PRIV_REQUIRES app rust_rt)
devkit_rust_crate(ROOT "main.rs")

idf_component_set_property(${COMPONENT_NAME} WHOLE_ARCHIVE ON)
