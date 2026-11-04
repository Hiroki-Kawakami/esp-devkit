idf_component_register(
    SRCS "prof.c"
    PRIV_REQUIRES app rust_rt perfmon esp_driver_gptimer freertos log)
devkit_rust_crate(ROOT "main.rs")

# Keep app_main() (referenced across the main<->app component link group) from
# being garbage-collected.
idf_component_set_property(${COMPONENT_NAME} WHOLE_ARCHIVE ON)
