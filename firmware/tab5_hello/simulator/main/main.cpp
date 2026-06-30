#include "tab5_hello.hpp"
#include "sim_harness.h"
#include "lvgl.hpp"
#include <cstdlib>

extern "C" int main(void) {
    app_entry();

    /* Scripted UI verification: if SIMULATOR_SCRIPT names a script, the harness
     * interpreter runs on its own thread in lockstep with this loop's frames.
     * NULL (env unset) is a no-op interactive run. sdl_panel registered its
     * input/capture callbacks during bsp_init. */
    sim_harness_start(getenv("SIMULATOR_SCRIPT"));
    lvgl_sim_loop(sim_harness_frame);
    return sim_harness_exit_code();
}
