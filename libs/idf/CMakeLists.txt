# Rust bindings and safe wrappers for ESP-IDF APIs.
idf_component_register(
    SRCS "/dev/null"
    REQUIRES esp_common esp_system esp_timer freertos heap log esp_driver_i2c esp_driver_gpio
             esp_driver_ledc esp_driver_i2s esp_driver_ppa esp_hw_support esp_lcd
    PRIV_REQUIRES devkit rust_rt)
if(ESP_PLATFORM)
    set(log_headers "esp_log.h" "esp_log_config.h" "esp_log_level.h" "esp_log_timestamp.h"
                    "esp_log_write.h")
    set(driver_headers "driver/i2c_master.h" "driver/gpio.h" "driver/ledc.h"
                       "esp_ldo_regulator.h" "esp_lcd_types.h" "esp_lcd_panel_io.h"
                       "esp_lcd_panel_ops.h" "esp_lcd_panel_commands.h")
    if(CONFIG_SOC_MIPI_DSI_SUPPORTED)
        list(APPEND driver_headers "esp_lcd_mipi_dsi.h")
    endif()
    if(CONFIG_SOC_PPA_SUPPORTED)
        list(APPEND driver_headers "driver/ppa.h")
    endif()
    if(CONFIG_SOC_I2S_SUPPORTED)
        list(APPEND driver_headers "driver/i2s_std.h" "driver/i2s_common.h" "driver/i2s_types.h" "hal/i2s_types.h")
    endif()
else()
    set(log_headers "esp_log.h")
    set(driver_headers "driver/i2c_master.h" "driver/ppa.h")
endif()
devkit_rust_crate(ROOT "src/lib.rs" TESTS
    BINDGEN_HEADERS "esp_err.h" "esp_heap_caps.h" "multi_heap.h" "esp_system.h" "esp_timer.h"
                    "freertos/FreeRTOS.h" "freertos/portmacro.h" "freertos/task.h" "freertos/queue.h"
                    "freertos/semphr.h" "freertos/idf_additions.h" "unistd.h" "sys/stat.h"
                    ${log_headers} ${driver_headers})
