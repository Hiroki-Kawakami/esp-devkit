/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Test harness: a line protocol the PC drives (tools/harness/harness.py) to
 * poke the UI and read the screen back, identical on device and simulator.
 * Commands are one line each; every reply is one '#'-prefixed line so the
 * client can pick them out of the log stream sharing the same channel:
 *
 *   ping                    -> #OK ping
 *   info                    -> #OK info <w> <h> <pixfmt> <touch> <buttons> <capture>
 *   down <id> <x> <y>       press / drag contact <id> (panel px)   -> #OK down
 *   move <id> <x> <y>       same as down (kept for script symmetry) -> #OK move
 *   up <id>                 release contact <id>                    -> #OK up
 *   btn <id> down|up        hold / release physical button <id>     -> #OK btn
 *   idle                    -> #OK idle 0|1   (UI at rest: no LVGL animation)
 *   snap [quality]          -> #OK snap <w> <h>, then #D <base64>... , #END <bytes>
 *   quit                    -> #OK quit; the simulator process then exits
 *   <name> ...              any command added with harness_register()
 *
 * Timing (wait/settle/tap) lives on the PC side: the device only exposes
 * primitives, so the same script runs against a simulator process or a
 * board without the firmware carrying an interpreter.
 */

#pragma once
#include <stdbool.h>
#include <stddef.h>
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ---- app-specific commands ---- */

/* Handler for a custom command; argv[0] is the name. Return true when handled
 * (a "#OK <name>" is sent unless the handler already replied), false for
 * "#ERR". Runs on the harness transport thread. */
typedef bool (*harness_cmd_fn)(int argc, const char *const *argv, void *user);
bool harness_register(const char *name, harness_cmd_fn fn, void *user);

/* Send one reply line; the '#' prefix and newline are added. Handlers use
 * "OK ..." / "ERR ..." bodies like the built-ins. */
void harness_reply(const char *fmt, ...) __attribute__((format(printf, 1, 2)));

/* ---- app / port hooks ---- */

/* Idle predicate for the `idle` command (device: the LVGL port answers under
 * its lock). Without one, the simulator's per-frame flag from harness_frame()
 * is used. */
typedef bool (*harness_idle_fn)(void *user);
void harness_set_idle_cb(harness_idle_fn fn, void *user);

/* Held around a capture so the renderer cannot flush mid-read (device: the
 * LVGL port lock). Optional. */
typedef void (*harness_lock_fn)(void *user);
void harness_set_lock_cb(harness_lock_fn lock, harness_lock_fn unlock, void *user);

/* ---- run control ---- */

/* Start the transport: a console reader task on device, a stdin reader thread
 * on the host. lvgl_port_init() already does this for a UI firmware; call it
 * directly only from one that has no LVGL port, once the BSP is up. */
esp_err_t harness_start(void);

/* Host main loop hook, once per frame after present: publishes `idle` for the
 * `idle` command and returns false once `quit` (or stdin EOF under a pipe) has
 * been received. Always true on device. */
bool harness_frame(bool idle);

/* Encode the whole panel to a JPEG file (stdio path; parent dirs created).
 * Also what the simulator's `s` key uses. */
bool harness_capture_to_file(const char *path, int quality);

#ifdef __cplusplus
}
#endif
