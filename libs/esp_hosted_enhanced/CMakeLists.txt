set(srcs
    "third_party/esp_hosted_rpc/esp_hosted_rpc.pb-c.c"
    "third_party/protobuf-c/protobuf-c/protobuf-c.c")

if(CONFIG_ESP_HOSTED_ENHANCED_HOST)
    list(APPEND srcs
        "src/host/api.c"
        "src/host/sdio.c"
        "src/host/transport.c"
        "src/host/rpc.c"
        "src/host/wifi.c")
    set(weak_srcs "esp_wifi_remote_weak.c")
    if(CONFIG_ESP_WIFI_REMOTE_EAP_ENABLED)
        list(APPEND weak_srcs "esp_eap_client_remote_weak.c")
    endif()
    # Building them in place trips IDF's check for sources owned by another component.
    foreach(src ${weak_srcs})
        configure_file("${IDF_PATH}/components/esp_wifi/remote/${src}"
                       "${CMAKE_CURRENT_BINARY_DIR}/${src}" COPYONLY)
        list(APPEND srcs "${CMAKE_CURRENT_BINARY_DIR}/${src}")
    endforeach()
endif()

idf_component_register(
    SRCS ${srcs}
    INCLUDE_DIRS
        "inc"
        "third_party/esp_hosted_rpc"
        "third_party/protobuf-c"
    PRIV_INCLUDE_DIRS "src"
    REQUIRES esp_driver_sdmmc
    PRIV_REQUIRES sdmmc esp_driver_gpio esp_wifi esp_event esp_timer wpa_supplicant esp_mm esp_hal_sd)

if(CONFIG_ESP_HOSTED_ENHANCED_HOST)
    # Strong overrides of the esp_wifi weak hooks must not lose to the weak
    # objects archived next to them.
    idf_component_set_property(${COMPONENT_NAME} WHOLE_ARCHIVE ON)
    # esp_wifi forwards esp_wifi_* to the esp_wifi_remote_* defined here.
    idf_component_get_property(wifi_lib esp_wifi COMPONENT_LIB)
    target_link_libraries(${wifi_lib} PUBLIC ${COMPONENT_LIB})
endif()
