# M5Stack ATOM Lite — ESP32, no display. WS2812 RGB LED (GPIO27) + GPIO button
# (GPIO39). Consumed by bsp/CMakeLists.txt when BSP_BOARD == atom_lite; paths
# are relative to the bsp component directory.

set(BOARD_TARGET esp32)

set(BOARD_DEVICE_SRCS
    "boards/atom_lite/atom_lite.c"
    "driver/gpio_button/gpio_button.c"
    "devices/ws2812/ws2812.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "driver/gpio_button" "devices/ws2812")
set(BOARD_DEVICE_PRIV_REQUIRES
    esp_driver_gpio esp_driver_rmt)

set(BOARD_SIM_SRCS "boards/atom_lite/atom_lite_sim.c")
