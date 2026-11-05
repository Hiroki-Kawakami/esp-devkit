# Network protocol stack.
set(srcs "/dev/null")
set(requires devkit_crypto idf rust_rt)
set(headers "")
set(options "")
if(ESP_PLATFORM)
    list(APPEND requires esp_wifi)
    if(CONFIG_DEVKIT_NET_WIFI)
        set(srcs "src/wifi_native.c")
        set(headers "esp_event_base.h" "esp_event.h" "esp_mac.h" "esp_wifi_types_generic.h" "esp_wifi.h"
                    "esp_private/wifi.h" "wifi_native.h")
        set(options "--blocklist-type=wifi_(sta_)?config_t|wifi_event_dpp_config_received_t"
                    "--blocklist-function=esp_wifi_(set|get)_config")
    endif()
else()
    set(headers "stdlib.h" "sys/socket.h")
endif()
idf_component_register(SRCS ${srcs} PRIV_INCLUDE_DIRS "src" PRIV_REQUIRES ${requires})
devkit_rust_crate(ROOT "src/lib.rs" TESTS OPT_LEVEL 2 BINDGEN_HEADERS ${headers} BINDGEN_OPTIONS ${options})
