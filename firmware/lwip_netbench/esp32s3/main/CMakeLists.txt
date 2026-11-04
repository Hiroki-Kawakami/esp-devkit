set(dev "${CMAKE_CURRENT_LIST_DIR}/../../../esp_hosted_enhanced/development/esp32p4/main")
idf_component_register(
    SRCS "bench.cpp" "harness_shim.c" "${dev}/netbench.cpp"
    INCLUDE_DIRS "."
    PRIV_INCLUDE_DIRS "${dev}"
    PRIV_REQUIRES bsp_rs esp_timer harness_rs idf lwip rust_rt wifi)
devkit_rust_crate(ROOT "main.rs" BINDGEN_HEADERS "bench.h")
idf_component_set_property(${COMPONENT_NAME} WHOLE_ARCHIVE ON)
