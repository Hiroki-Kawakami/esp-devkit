# M5Stack Air Quality Kit — ESP32-S3 + GDEY0154D67 (SSD1681) 200x200 B/W SPI
# EPD + two front buttons (A=GPIO0, B=GPIO8) + power button (GPIO42) + passive
# buzzer (GPIO9). Consumed
# by bsp/CMakeLists.txt when BSP_BOARD == airq. Paths are relative to the bsp
# component directory. (Air-quality sensors are not wired yet.)

set(BOARD_TARGET esp32s3)

set(BOARD_DEVICE_SRCS
    "boards/airq/airq.c"
    "driver/gpio_button/gpio_button.c"
    "driver/pwm_buzzer/pwm_buzzer.c"
    "devices/gdey0154d67/gdey0154d67.c"
    "devices/gdey0154d67/gdey0154d67_epd.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "driver/gpio_button" "driver/pwm_buzzer" "devices" "devices/gdey0154d67")
set(BOARD_DEVICE_PRIV_REQUIRES
    driver esp_driver_spi esp_driver_gpio esp_driver_ledc)

set(BOARD_SIM_SRCS "boards/airq/airq_sim.c")
