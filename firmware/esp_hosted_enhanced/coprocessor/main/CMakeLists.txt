idf_component_register(
    SRCS "main.c" "board.c" "transport.c" "serial.c" "rpc.c" "wifi.c" "ota.c" "recovery.c" "logfwd.c" "bench.c"
    INCLUDE_DIRS "."
    PRIV_REQUIRES esp_hosted_enhanced esp_driver_sdio esp_driver_gpio esp_wifi nvs_flash esp_event esp_app_format app_update esp_partition spi_flash)
