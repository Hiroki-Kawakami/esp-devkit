/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Coprocessor flashing over harness, for c6flash.py:
 *   c6 recovery | c6 normal   IO2 held low / released, now and after the next reset
 *   c6 state                  -> #OK c6 recovery|normal
 *   c6 pin low | c6 pin release
 *   c6 begin | c6 data <base64> | c6 end | c6 activate
 *   c6 ver                    -> #OK c6 <major>.<minor>.<patch>
 */

#pragma once

/* True when the host was restarted into recovery. */
bool c6_flash_start();
