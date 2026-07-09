# M5StickC-Plus — ESP32-PICO-D4 + AXP192 PMIC (I2C) + ST7789V2 135x240 TFT (SPI)
# + passive buzzer (GPIO2) + red LED (GPIO10, PWM).
# Consumed by bsp/CMakeLists.txt when BSP_BOARD == stickc_plus. Paths are
# relative to the bsp component directory.

set(BOARD_TARGET esp32)

set(BOARD_DEVICE_SRCS
    "boards/stickc_plus/stickc_plus.c"
    "devices/axp192/axp192.c"
    "devices/axp192/axp192_button.c"
    "devices/st7789v2/st7789v2.c"
    "driver/gpio_button/gpio_button.c"
    "driver/pwm_buzzer/pwm_buzzer.c"
    "driver/pwm_led/pwm_led.c")
set(BOARD_DEVICE_PRIV_INCLUDE_DIRS
    "devices" "devices/axp192" "devices/st7789v2" "driver/gpio_button"
    "driver/pwm_buzzer" "driver/pwm_led")
set(BOARD_DEVICE_PRIV_REQUIRES
    driver esp_driver_i2c esp_driver_spi esp_driver_ledc)

set(BOARD_SIM_SRCS "boards/stickc_plus/stickc_plus_sim.c")
