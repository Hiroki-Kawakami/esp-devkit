#include "esp_random.h"

#include <stdlib.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <sys/random.h>
#endif

uint32_t esp_random(void) {
    uint32_t value;
    if (getentropy(&value, sizeof(value)) != 0) {
        abort();
    }
    return value;
}
