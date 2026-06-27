# M5PaperS3 — ESP32-S3 + ED047TC1 i80 grayscale EPD + GT911 I2C touch.
# Consumed by bsp/CMakeLists.txt when BSP_BOARD == paper_s3. Paths are relative
# to the bsp component directory.

set(BOARD_DEVICE_SRCS
    "boards/paper_s3/paper_s3.c"
    "boards/paper_s3/paper_s3_panel.c"
    "boards/paper_s3/paper_s3_sd.c"
    "devices/gt911/gt911.c"
    "devices/bm8563/bm8563.c"
    "devices/ed047tc1/ed047tc1.c"
    "driver/epd_ll.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "devices" "devices/ed047tc1" "devices/gt911" "devices/bm8563" "driver")
set(BOARD_DEVICE_PRIV_REQUIRES
    driver esp_lcd nvs_flash bt fatfs sdmmc esp_driver_sdmmc esp_driver_sdspi)

set(BOARD_SIM_SRCS "boards/paper_s3/paper_s3_sim.c")
