# M5Stack CoreS3 — ESP32-S3 + AXP2101 PMIC (I2C) + AW9523B I/O expander (I2C)
# + ILI9342C 320x240 TFT (SPI) + AW88298 mono speaker amp (I2S) + BM8563 RTC
# (I2C). Consumed by
# bsp/CMakeLists.txt when BSP_BOARD == core_s3. Paths are relative to the bsp
# component directory.

set(BOARD_TARGET esp32s3)

set(BOARD_DEVICE_SRCS
    "boards/core_s3/core_s3.c"
    "boards/core_s3/core_s3_panel.c"
    "boards/core_s3/core_s3_audio.c"
    "devices/axp2101/axp2101.c"
    "devices/aw9523/aw9523.c"
    "devices/ili9342c/ili9342c.c"
    "devices/ft6336u/ft6336u.c"
    "devices/aw88298/aw88298.c"
    "devices/bm8563/bm8563.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "devices" "devices/axp2101" "devices/aw9523" "devices/ili9342c" "devices/ft6336u"
    "devices/aw88298" "devices/bm8563")
set(BOARD_DEVICE_PRIV_REQUIRES
    driver esp_driver_i2c esp_driver_spi esp_driver_i2s)

set(BOARD_SIM_SRCS "boards/core_s3/core_s3_sim.c")
