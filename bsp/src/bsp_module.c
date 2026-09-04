/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Module attach seam. An app declares which module it attached where and the
 * board brings it up its own way — the pin map, rails and reset order of a
 * module differ per board, so there is nothing portable to share here beyond
 * the entry point and the provider it hands back.
 *
 * These are the weak defaults every board inherits; a board that hosts the
 * module defines the same symbol and overrides them. The override has to live
 * in a translation unit the link already pulls in for other reasons (the
 * board's bsp_init file): a strong definition sitting alone in an object no one
 * references is never extracted from the component's archive, and this weak
 * default silently wins instead.
 */

#include "bsp.h"

__attribute__((weak))
esp_err_t bsp_module_attach_display(bsp_module_port_t port,
                                    const bsp_display_module_config_t *config,
                                    bsp_panel_id_t *out_panel) {
    (void)port;
    (void)config;
    (void)out_panel;
    return ESP_ERR_NOT_SUPPORTED;
}
