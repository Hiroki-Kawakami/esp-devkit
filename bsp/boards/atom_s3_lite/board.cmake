# M5Stack AtomS3 Lite — ESP32-S3, no display. WS2812 RGB LED (GPIO35) + GPIO
# button (GPIO41). Consumed by bsp/CMakeLists.txt when BSP_BOARD == atom_s3_lite;
# paths are relative to the bsp component directory.

set(BOARD_TARGET esp32s3)

set(BOARD_DEVICE_SRCS
    "boards/atom_s3_lite/atom_s3_lite.c"
    "driver/gpio_button/gpio_button.c"
    "devices/ws2812/ws2812.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "driver/gpio_button" "devices/ws2812")
set(BOARD_DEVICE_PRIV_REQUIRES
    esp_driver_gpio esp_driver_rmt)

set(BOARD_SIM_SRCS "boards/atom_s3_lite/atom_s3_lite_sim.c")
