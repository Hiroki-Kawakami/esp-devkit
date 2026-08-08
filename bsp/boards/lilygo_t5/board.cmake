# LilyGo T5 ePaper S3 — ESP32-S3 + ED047TC1 direct-drive grayscale EPD.

set(BOARD_TARGET esp32s3)

set(BOARD_DEVICE_SRCS
    "boards/lilygo_t5/lilygo_t5.c"
    "boards/lilygo_t5/lilygo_t5_panel.c"
    "devices/gt911/gt911.c"
    "devices/ed047tc1/ed047tc1.c"
    "driver/epd/epd_ll.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "devices" "devices/ed047tc1" "devices/gt911" "driver/epd")
set(BOARD_DEVICE_PRIV_REQUIRES
    driver esp_lcd esp_timer)

set(BOARD_SIM_SRCS "")
