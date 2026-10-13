/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * BLE measurements between two boards running this firmware, for blebench.py:
 *   ble init                 -> #OK ble <addr type> <addr> <ms>
 *   ble adv                  peripheral: advertise the bench service
 *   ble connect <type> <addr> -> #OK ble connect <ms> <mtu>    central, 2M PHY + data length
 *   ble write <bytes>        -> #OK ble write <bytes> <us>      central -> peripheral, no response
 *   ble notify <bytes>       -> #OK ble notify <bytes> <us>     peripheral -> central
 *   ble stats                -> #OK ble stats <bytes> <us>      received since the last stats
 *   ble rtt <count>          -> #OK ble rtt <p50> <p99> <max>   HCI command round trip (us)
 *   ble scan <ms>            -> #OK ble scan <reports>
 *   ble disconnect
 */

#pragma once

void ble_bench_start();
