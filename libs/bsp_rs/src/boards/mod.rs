/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#[cfg(CONFIG_BSP_RS_BOARD_ATOM_S3_LITE)]
pub(crate) mod atom_s3_lite;
#[cfg(CONFIG_BSP_RS_BOARD_CORE2)]
pub(crate) mod core2;
#[cfg(CONFIG_BSP_RS_BOARD_CORE_S3)]
pub(crate) mod core_s3;
#[cfg(CONFIG_BSP_RS_BOARD_TAB5)]
pub(crate) mod tab5;
