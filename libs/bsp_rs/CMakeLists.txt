# Rust board support package.
set(srcs "/dev/null")
set(bindgen_headers "")
set(bindgen_options "")
set(priv_include_dirs "")
if(ESP_PLATFORM)
    set(target_requires esp_psram)
    if(CONFIG_BSP_RS_DRIVER_ESP_HOSTED)
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
        list(REMOVE_ITEM srcs "/dev/null")
        list(APPEND srcs "src/driver/esp_hosted/wifi_default.c")
        set(priv_include_dirs "src/driver/esp_hosted")
        set(bindgen_headers "esp_event.h" "esp_wifi_types_generic.h" "esp_wifi.h" "esp_private/wifi.h" "wifi_default.h"
                            "soc/sdmmc_struct.h" "hal/sdmmc_ll.h" "esp_private/sd_host_private.h")
        # The clock functions are shadowed by macros that only work inside
        # PERIPH_RCC_ATOMIC(), which breaks bindgen's wrappers for them.
        set(bindgen_options "--blocklist-type=wifi_sta_config_t"
            "--blocklist-function=sdmmc_ll_(enable_bus_clock|reset_register|enable_sdio_pll|select_clk_source|set_clock_div|init_phase_delay|set_din_delay_phase)")
    endif()
else()
    set(bindgen_headers "simulator/sim_psram.h" "simulator/path_redirect.h" "stdlib.h" "unistd.h" "sys/socket.h" "sys/statvfs.h")
endif()
idf_component_register(
    SRCS ${srcs}
    PRIV_INCLUDE_DIRS ${priv_include_dirs}
    REQUIRES devkit_audio devkit_gfx idf
    PRIV_REQUIRES devkit devkit_net rust_rt ${target_requires})

set(depends "")
if(CONFIG_BSP_RS_DRIVER_ESP_HOSTED)
    # A firmware using the driver brings esp_wifi (and bt for NimBLE) in;
    # requiring them here would add them to every board build.
    idf_component_optional_requires(PRIVATE esp_wifi esp_event wpa_supplicant)
    # The esp_wifi remote glue and NimBLE call the hooks defined here, and the
    # strong ones must not lose to the weak objects archived next to them.
    idf_component_set_property(${COMPONENT_NAME} WHOLE_ARCHIVE ON)
    devkit_rust_out_dir(out)
    protogen_generate(rpc LANG rust OUT_DIR "${out}/protogen"
        PROTO "${CMAKE_CURRENT_LIST_DIR}/../esp_hosted_enhanced/third_party/esp_hosted_rpc/esp_hosted_rpc.proto")
    set(depends ${rpc_SOURCES})
endif()
devkit_rust_crate(ROOT "src/lib.rs" TESTS
    BINDGEN_HEADERS ${bindgen_headers}
    BINDGEN_OPTIONS ${bindgen_options}
    DEPENDS ${depends})
