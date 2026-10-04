/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#include <utility>

#include "esp_intr_alloc.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "host_internal.hpp"

namespace usb_host {

namespace {

struct ClassDriver {
    esp_err_t (*install)();
    void (*connected)(uint8_t address);
    void (*gone)(usb_device_handle_t handle);
};

constexpr ClassDriver kDrivers[] = {
#if CONFIG_USBH_MSC
    {detail::msc_install, detail::msc_connected, detail::msc_gone},
#endif
#if CONFIG_USBH_UAC
    {detail::uac_install, detail::uac_connected, detail::uac_gone},
#endif
};

struct ClientEvent {
    bool connected;
    uint8_t address;
    usb_device_handle_t handle;
};

QueueHandle_t s_events;
Callbacks s_callbacks;
usb_host_client_handle_t s_client;

void host_lib_task(void*) {
    while (true) {
        uint32_t flags = 0;
        usb_host_lib_handle_events(portMAX_DELAY, &flags);
        if (flags & USB_HOST_LIB_EVENT_FLAGS_NO_CLIENTS) usb_host_device_free_all();
    }
}

void client_event_cb(const usb_host_client_event_msg_t* message, void*) {
    ClientEvent event = {};
    if (message->event == USB_HOST_CLIENT_EVENT_NEW_DEV) {
        event.connected = true;
        event.address = message->new_dev.address;
    } else if (message->event == USB_HOST_CLIENT_EVENT_DEV_GONE) {
        event.handle = message->dev_gone.dev_hdl;
    } else {
        return;
    }
    xQueueSend(s_events, &event, 0);
}

void client_task(void*) {
    while (true) {
        usb_host_client_handle_events(s_client, portMAX_DELAY);
    }
}

void worker_task(void*) {
    ClientEvent event;
    while (true) {
        xQueueReceive(s_events, &event, portMAX_DELAY);
        for (const ClassDriver& driver : kDrivers) {
            if (event.connected) {
                driver.connected(event.address);
            } else {
                driver.gone(event.handle);
            }
        }
    }
}

}  // namespace

const Callbacks& detail::callbacks() {
    return s_callbacks;
}

usb_host_client_handle_t detail::client() {
    return s_client;
}

esp_err_t install(Callbacks callbacks) {
    if (s_events) return ESP_ERR_INVALID_STATE;
    s_callbacks = std::move(callbacks);
    s_events = xQueueCreate(4, sizeof(ClientEvent));
    if (!s_events) return ESP_ERR_NO_MEM;

    for (const ClassDriver& driver : kDrivers) {
        const esp_err_t err = driver.install();
        if (err != ESP_OK) return err;
    }

    usb_host_config_t host_config = {};
    host_config.intr_flags = ESP_INTR_FLAG_LEVEL1;
    esp_err_t err = usb_host_install(&host_config);
    if (err != ESP_OK) return err;

    usb_host_client_config_t client_config = {};
    client_config.is_synchronous = false;
    client_config.max_num_event_msg = 10;
    client_config.async.client_event_callback = client_event_cb;
    err = usb_host_client_register(&client_config, &s_client);
    if (err != ESP_OK) return err;

    if (xTaskCreate(host_lib_task, "usb_host", 3072, nullptr, 5, nullptr) != pdPASS) {
        return ESP_ERR_NO_MEM;
    }
    if (xTaskCreate(client_task, "usb_client", 4096, nullptr, 5, nullptr) != pdPASS) {
        return ESP_ERR_NO_MEM;
    }
    if (xTaskCreate(worker_task, "usbh", 4096, nullptr, 5, nullptr) != pdPASS) {
        return ESP_ERR_NO_MEM;
    }
    return ESP_OK;
}

}  // namespace usb_host
