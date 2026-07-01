# ESP32-S31-Korvo — ESP32-S31 + 800x480 RGB565 parallel LCD.
# Consumed by bsp/CMakeLists.txt when BSP_BOARD == s31_korvo. Paths are relative
# to the bsp component directory. Touch (GT1151) is not wired yet; this is the
# minimum bring-up to light the panel.

set(BOARD_DEVICE_SRCS
    "boards/s31_korvo/s31_korvo.c"
    "boards/s31_korvo/s31_korvo_panel.c"
    "driver/rgb_lcd/rgb_lcd.c"
    "devices/gt1151/gt1151.c"
    "devices/ws2812/ws2812.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "driver/rgb_lcd" "devices/gt1151" "devices/ws2812")
set(BOARD_DEVICE_PRIV_REQUIRES
    esp_driver_gpio esp_driver_i2c esp_driver_ledc esp_driver_rmt esp_lcd)

set(BOARD_SIM_SRCS "boards/s31_korvo/s31_korvo_sim.c")
