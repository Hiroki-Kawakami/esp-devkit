#include <stdint.h>
#include "driver/gptimer.h"
#include "esp_attr.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

#define SAMPLES 1024

static uint32_t samples[SAMPLES];
static volatile int count = SAMPLES;
static gptimer_handle_t timer;

static bool IRAM_ATTR on_alarm(gptimer_handle_t t, const gptimer_alarm_event_data_t *e, void *arg)
{
    if (count < SAMPLES) {
        uint32_t **tcb = (uint32_t **)xTaskGetCurrentTaskHandleForCore(1);
        samples[count++] = tcb ? (*tcb)[1] : 0;
    }
    return false;
}

static void setup(void *arg)
{
    gptimer_config_t config = {.clk_src = GPTIMER_CLK_SRC_DEFAULT, .direction = GPTIMER_COUNT_UP, .resolution_hz = 1000000};
    ESP_ERROR_CHECK(gptimer_new_timer(&config, &timer));
    gptimer_event_callbacks_t cbs = {.on_alarm = on_alarm};
    ESP_ERROR_CHECK(gptimer_register_event_callbacks(timer, &cbs, NULL));
    gptimer_alarm_config_t alarm = {.alarm_count = 997, .flags.auto_reload_on_alarm = true};
    ESP_ERROR_CHECK(gptimer_set_alarm_action(timer, &alarm));
    ESP_ERROR_CHECK(gptimer_enable(timer));
    ESP_ERROR_CHECK(gptimer_start(timer));
    vTaskDelete(NULL);
}

void prof_start(void)
{
    if (!timer) {
        xTaskCreatePinnedToCore(setup, "prof", 3072, NULL, 5, NULL, 1);
    }
    count = 0;
}

void prof_dump(void)
{
    while (count < SAMPLES) {
        vTaskDelay(10);
    }
    for (int i = 0; i < SAMPLES; i += 16) {
        char line[16 * 9 + 1];
        for (int j = 0; j < 16; j++) {
            sprintf(line + j * 9, "%08lx ", (unsigned long)samples[i + j]);
        }
        ESP_LOGI("prof", "%s", line);
    }
}
