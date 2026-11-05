#include "usbd_loopback.hpp"

extern "C" void app_main() {
    usbd_loopback_start();
}
