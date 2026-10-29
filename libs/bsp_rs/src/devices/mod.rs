/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[cfg(CONFIG_BSP_RS_DEVICE_ILI9881C)]
pub(crate) mod ili9881c;
#[cfg(CONFIG_BSP_RS_DEVICE_PI4IO)]
pub(crate) mod pi4io;
#[cfg(CONFIG_BSP_RS_DEVICE_ST7121)]
pub(crate) mod st7121;
#[cfg(CONFIG_BSP_RS_DEVICE_ST7123)]
pub(crate) mod st7123;
