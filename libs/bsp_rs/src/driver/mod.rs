/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[cfg(CONFIG_BSP_RS_DRIVER_MIPI_DSI)]
pub(crate) mod mipi_dsi;
#[cfg(not(target_os = "none"))]
pub(crate) mod sim_panel;
#[cfg(not(target_os = "none"))]
pub(crate) mod sim_touch;
