#include "ui_test.hpp"
#include "lvgl.hpp"

extern "C" int main(void) {
    app_entry();
    lvgl_sim_loop();
    return 0;
}
