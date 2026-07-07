# M5Paper — ESP32 + IT8951E SPI EPD controller + GT911 I2C touch. The EPD and
# the microSD card share one SPI bus (brought up in paper.c). Consumed by
# bsp/CMakeLists.txt when BSP_BOARD == paper. Paths are relative to the bsp
# component directory.

set(BOARD_TARGET esp32)

set(BOARD_DEVICE_SRCS
    "boards/paper/paper.c"
    "boards/paper/paper_panel.c"
    "boards/paper/paper_sd.c"
    "devices/gt911/gt911.c"
    "devices/gt911/gt911_hotknot.c"
    "devices/gt911/gt911_hotknot_fw_loader.c"
    "devices/gt911/gt911_hotknot_fw_blob.c"
    "devices/bm8563/bm8563.c"
    "devices/it8951e/it8951e.c"
    "devices/it8951e/it8951e_epd.c"
    "driver/gpio_button/gpio_button.c"
    "driver/adc_battery/adc_battery.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "devices" "devices/it8951e" "devices/gt911" "devices/bm8563"
    "driver/gpio_button" "driver/adc_battery")
set(BOARD_DEVICE_PRIV_REQUIRES
    driver esp_driver_i2c esp_adc esp_timer nvs_flash vfs fatfs sdmmc esp_driver_sdmmc esp_driver_sdspi)

set(BOARD_SIM_SRCS "boards/paper/paper_sim.c")
