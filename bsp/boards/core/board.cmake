# M5Stack Core (Basic / Core2) — ESP32 + ILI9342C 320x240 TFT (SPI). Only the
# Basic panel path (GPIO33 reset, GPIO32 LEDC backlight) is wired today. Consumed
# by bsp/CMakeLists.txt when BSP_BOARD == core. Paths are relative to the bsp
# component directory.

set(BOARD_TARGET esp32)

set(BOARD_DEVICE_SRCS
    "boards/core/core.c"
    "boards/core/core_panel.c"
    "boards/core/core_audio.c"
    "devices/ili9342c/ili9342c.c"
    "devices/axp192/axp192.c"
    "devices/ft6336u/ft6336u.c"
    "driver/gpio_button/gpio_button.c"
    "driver/i2s_dac/i2s_dac.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "devices" "devices/ili9342c" "devices/axp192" "devices/ft6336u"
    "driver/gpio_button" "driver/i2s_dac")
set(BOARD_DEVICE_PRIV_REQUIRES
    driver esp_driver_spi esp_driver_ledc esp_driver_i2c esp_driver_i2s
    esp_hal_ana_conv esp_hal_i2s soc)

set(BOARD_SIM_SRCS "boards/core/core_sim.c")
