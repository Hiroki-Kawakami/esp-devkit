/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Sim harness — scripted, headless UI verification driver for the simulator.
 *
 * Portable core: this translation unit has NO coupling to SDL, LVGL, the BSP, or
 * any app module — those are injected. The simulator platform registers how to
 * inject touch and capture a frame (sdl_panel does this for itself in
 * sdl_panel_create); the app registers its own script commands via
 * sim_harness_register(); and the main loop drives stepping by calling
 * sim_harness_frame() once per iteration. The script interpreter runs on its own
 * thread and advances in lockstep with those frames (a pthread cond frame
 * counter), so the host's single main loop stays the only thing that pumps the
 * UI and presents — the harness never pumps anything itself.
 *
 * Selected by the SIMULATOR_SCRIPT env var in the simulator entry (main); the
 * SDL backend honours SIMULATOR_HEADLESS independently.
 *
 * Script commands (one per line; '#' and blank lines ignored):
 *   wait <ms>            advance for <ms> of main-loop frames
 *   settle [<max_ms>]    advance until idle for a few frames (default 5000)
 *   capture <path>       write the latest frame via the capture callback
 *   tap <x> <y> [id]     synthetic press+release at panel coords (id default 0)
 *   down <x> <y> [id]    press/hold (no release) — start of a drag
 *   move <x> <y> [id]    move the held pointer (drag/swipe)
 *   up [id]              release the held pointer (id default 0)
 *   quit                 stop the run (implicit at end of script)
 *   <name> ...           any command registered with sim_harness_register()
 */

#pragma once
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- platform capability registration (injected by the simulator) ---- */

/* Synthetic multi-touch injection. Each id is an independent finger. */
typedef void (*sim_harness_touch_down_fn)(int id, int x, int y);
typedef void (*sim_harness_touch_up_fn)(int id);
void sim_harness_set_input_callback(sim_harness_touch_down_fn down,
                                    sim_harness_touch_up_fn   up);

/* Write the current frame to path; return false on failure. */
typedef bool (*sim_harness_capture_fn)(const char *path);
void sim_harness_set_capture_callback(sim_harness_capture_fn capture);

/* ---- app-specific command registration ---- */

/* Handler for a custom script command. argv[0] is the command name; return false
 * to STOP the run (like quit). user is the pointer passed at registration.
 * Returns false if the table is full. */
typedef bool (*sim_cmd_fn)(int argc, const char *const *argv, void *user);
bool sim_harness_register(const char *name, sim_cmd_fn fn, void *user);

/* ---- run control ---- */

/* Begin a scripted run: spawn the interpreter for the script at script_path
 * ("-" = stdin) and return immediately. NULL or "" is a no-op (interactive run).
 * Call after the app/board init so capability + command registration is done. */
void sim_harness_start(const char *script_path);

/* Call once per main-loop iteration (after present). Releases one frame to the
 * interpreter; idle is whether the UI is at rest this frame (used by settle).
 * Returns false once the run has finished — break the loop and return
 * sim_harness_exit_code(). Always returns true for an interactive run. */
bool sim_harness_frame(bool idle);

/* Process exit code for the finished run (0 on success). */
int sim_harness_exit_code(void);

#ifdef __cplusplus
}
#endif
