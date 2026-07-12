# M5Stack Air Quality Kit — ESP32-S3 + GDEY0154D67 (SSD1681) 200x200 B/W SPI
# EPD + two front buttons (A=GPIO0, B=GPIO8) + power button (GPIO42) + passive
# buzzer (GPIO9) + BM8563 RTC on I2C (SCL=GPIO12, SDA=GPIO11). The SEN55/SCD40
# air sensors sit on the same bus, their rail on BSP_POWER_SWITCH_SENSOR.
# Consumed by bsp/CMakeLists.txt when BSP_BOARD == airq. Paths are relative to
# the bsp component directory.

set(BOARD_TARGET esp32s3)

set(BOARD_DEVICE_SRCS
    "boards/airq/airq.c"
    "driver/gpio_button/gpio_button.c"
    "driver/pwm_buzzer/pwm_buzzer.c"
    "driver/adc_battery/adc_battery.c"
    "devices/gdey0154d67/gdey0154d67.c"
    "devices/gdey0154d67/gdey0154d67_epd.c"
    "devices/bm8563/bm8563.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "driver/gpio_button" "driver/pwm_buzzer" "driver/adc_battery" "devices"
    "devices/gdey0154d67" "devices/bm8563")
set(BOARD_DEVICE_PRIV_REQUIRES
    driver esp_driver_spi esp_driver_gpio esp_driver_ledc esp_driver_i2c esp_adc)

set(BOARD_SIM_SRCS "boards/airq/airq_sim.c")
