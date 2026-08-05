# M5Stack Tab5 — ESP32-P4 + ILI9881C MIPI-DSI LCD + GT911 I2C touch + PI4IOE5V6408 IO expanders.
# Consumed by bsp/CMakeLists.txt when BSP_BOARD == tab5. Paths are relative to
# the bsp component directory.

set(BOARD_TARGET esp32p4)

set(BOARD_DEVICE_SRCS
    "boards/tab5/tab5.c"
    "boards/tab5/tab5_panel.c"
    "boards/tab5/tab5_audio.c"
    "devices/ili9881c/ili9881c.c"
    "devices/st712x/st7121_lcd.c"
    "devices/st712x/st7123_lcd.c"
    "devices/st712x/st712x_touch.c"
    "devices/gt911/gt911.c"
    "devices/pi4io/pi4io.c"
    "devices/es8388/es8388.c"
    "driver/mipi_dsi/mipi_dsi.c"
    "driver/sd_mmc/sd_mmc.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "devices" "devices/ili9881c" "devices/st712x" "devices/gt911" "devices/pi4io"
    "devices/es8388" "driver/mipi_dsi" "driver/sd_mmc")
set(BOARD_DEVICE_PRIV_REQUIRES
    driver esp_driver_ledc esp_driver_i2s esp_lcd vfs fatfs sdmmc esp_driver_sdmmc)

set(BOARD_SIM_SRCS "boards/tab5/tab5_sim.c")
