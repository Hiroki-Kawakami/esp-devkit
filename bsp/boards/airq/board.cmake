# M5Stack Air Quality Kit — ESP32-S3 + GDEY0154D67 (SSD1681) 200x200 B/W SPI
# EPD. Consumed by bsp/CMakeLists.txt when BSP_BOARD == airq. Paths are relative
# to the bsp component directory. (Air-quality sensors are not wired yet.)

set(BOARD_TARGET esp32s3)

set(BOARD_DEVICE_SRCS
    "boards/airq/airq.c"
    "devices/gdey0154d67/gdey0154d67.c"
    "devices/gdey0154d67/gdey0154d67_epd.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "devices" "devices/gdey0154d67")
set(BOARD_DEVICE_PRIV_REQUIRES
    driver esp_driver_spi esp_driver_gpio)

set(BOARD_SIM_SRCS "boards/airq/airq_sim.c")
