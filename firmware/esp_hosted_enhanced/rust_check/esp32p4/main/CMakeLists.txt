set(dev "${CMAKE_CURRENT_LIST_DIR}/../../../development/esp32p4/main")
idf_component_register(
    SRCS "check.cpp" "harness_shim.c" "${dev}/netbench.cpp" "${dev}/ble_bench.cpp"
    INCLUDE_DIRS "."
    PRIV_INCLUDE_DIRS "${dev}"
    PRIV_REQUIRES bsp_rs bt esp_timer harness_rs idf lwip rust_rt wifi)
# ble_bench.cpp brings the stock esp-hosted controller up itself unless the
# enhanced host, which this Rust host stands in for, is configured.
set_source_files_properties("${dev}/ble_bench.cpp" PROPERTIES COMPILE_DEFINITIONS "CONFIG_ESP_HOSTED_ENHANCED_HOST=1")
devkit_rust_crate(ROOT "main.rs" BINDGEN_HEADERS "check.h")
idf_component_set_property(${COMPONENT_NAME} WHOLE_ARCHIVE ON)
