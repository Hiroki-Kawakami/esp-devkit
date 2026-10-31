/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[cfg(CONFIG_BSP_RS_DEVICE_BMI270)]
pub(crate) mod bmi270;
#[cfg(CONFIG_BSP_RS_DEVICE_BMI270)]
mod bmi270_config;
#[cfg(CONFIG_BSP_RS_DEVICE_ES8388)]
pub(crate) mod es8388;
#[cfg(CONFIG_BSP_RS_DEVICE_GT911)]
pub(crate) mod gt911;
#[cfg(CONFIG_BSP_RS_DEVICE_ILI9881C)]
pub(crate) mod ili9881c;
#[cfg(CONFIG_BSP_RS_DEVICE_PI4IO)]
pub(crate) mod pi4io;
#[cfg(CONFIG_BSP_RS_DEVICE_ST712X_TOUCH)]
pub(crate) mod st712x_touch;
#[cfg(CONFIG_BSP_RS_DEVICE_ST7121)]
pub(crate) mod st7121;
#[cfg(CONFIG_BSP_RS_DEVICE_ST7123)]
pub(crate) mod st7123;
