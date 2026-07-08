#include "stickc_plus_hello.hpp"
#include "sim_harness.h"
#include "lvgl.hpp"
#include <cstdlib>

extern "C" int main(void) {
    app_entry();

    sim_harness_start(getenv("SIMULATOR_SCRIPT"));
    lvgl_sim_loop(sim_harness_frame);
    return sim_harness_exit_code();
}
