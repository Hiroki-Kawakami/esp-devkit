set(WIFI_COMMON_SRCS
    "src/wifi_manager.cpp")

if(ESP_PLATFORM)
    set(WIFI_NATIVE_TARGETS
        esp32
        esp32c2
        esp32c3
        esp32c5
        esp32c6
        esp32c61
        esp32s2
        esp32s3)

    if(IDF_TARGET STREQUAL "esp32p4")
        set(WIFI_NETIF_SRC "src/wifi_netif_remote.cpp")
    elseif(IDF_TARGET IN_LIST WIFI_NATIVE_TARGETS)
        set(WIFI_NETIF_SRC "src/wifi_netif_native.cpp")
    else()
        message(FATAL_ERROR
            "wifi requires native Wi-Fi or CONFIG_ESP_WIFI_REMOTE_ENABLED")
    endif()

    set(WIFI_PLATFORM_REQUIRES esp_wifi)
    if(IDF_TARGET STREQUAL "esp32p4")
        list(APPEND WIFI_PLATFORM_REQUIRES esp_wifi_remote)
    endif()

    idf_component_register(
        SRCS
            ${WIFI_COMMON_SRCS}
            "src/backend_espwifi.cpp"
            ${WIFI_NETIF_SRC}
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "src"
        PRIV_REQUIRES
            ${WIFI_PLATFORM_REQUIRES}
            esp_event
            esp_netif
            nvs_flash
            freertos)
else()
    idf_component_register(
        SRCS
            ${WIFI_COMMON_SRCS}
            "src/backend_sim.cpp"
        INCLUDE_DIRS "inc"
        PRIV_INCLUDE_DIRS "src")
endif()
