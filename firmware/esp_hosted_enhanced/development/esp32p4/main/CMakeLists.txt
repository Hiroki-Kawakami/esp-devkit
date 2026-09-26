idf_component_register(
    SRCS "main.cpp" "netbench.cpp" "c6_flash.cpp"
    INCLUDE_DIRS "."
    PRIV_REQUIRES bsp wifi harness lwip esp_timer esp_hosted esp_hosted_enhanced mbedtls esp_driver_gpio)
