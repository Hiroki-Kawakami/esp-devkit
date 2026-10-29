#include "esp_system.h"

#include <stdlib.h>

// Overridden to hand the restart to the simulator host, which starts the
// firmware process again.
__attribute__((weak)) void sim_restart_hook(void) {
}

void esp_restart(void) {
    sim_restart_hook();
    exit(0);
}
