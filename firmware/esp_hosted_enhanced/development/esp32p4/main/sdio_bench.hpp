/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * SDIO link measurements over the test channel, for sdiobench.py:
 *   sdio caps                         -> #OK sdio <ext caps hex>
 *   sdio log on|off
 *   sdio sink <size> <count> [delay]  -> #OK sdio sink <bytes> <us>
 *   sdio source <size> <count>        -> #OK sdio source <bytes> <us> <lost>
 *   sdio echo <size> <count> [gap]    -> #OK sdio echo <p50> <p99> <max> <lost>   (us)
 *   sdio share <count>                -> #OK sdio share <ok> <err> <bytes> <us>
 *   sdio sd <kb>                      -> #OK sdio sd <bytes> <write us> <read us> <bad chunks> <sink bytes> <sink us>
 * `delay` holds each packet on the coprocessor; `gap` spaces the echo probes.
 */

#pragma once

void sdio_bench_start();
