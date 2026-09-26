/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Throughput / latency peers for netbench.py: a TCP sink, a TCP source and a
 * UDP echo, each on its own task.
 */

#pragma once

constexpr int kNetbenchSinkPort = 5001;
constexpr int kNetbenchSourcePort = 5002;
constexpr int kNetbenchEchoPort = 5003;

/* Idempotent; call once the station has an IP. */
void netbench_start();
