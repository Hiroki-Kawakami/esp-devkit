/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Virtual I2C bus behind the i2c_master host shim (driver/i2c_master.h). One
 * mutex per bus serializes transfers, matching the real driver's bus lock.
 * Fidelity notes: transfers are transaction-granular (no per-byte ACK model),
 * timeouts never fire (emulator callbacks are synchronous), and 10-bit
 * addressing is unsupported.
 */

#include "driver/i2c_master.h"
#include "simulator/i2c_master_sim.h"
#include <pthread.h>
#include <stdbool.h>
#include <stdlib.h>

#define I2C_SIM_MAX_PERIPHERALS 16

typedef struct {
    bool                 used;
    uint16_t             address;
    i2c_sim_device_ops_t ops;
    void                *ctx;
} i2c_sim_peripheral_t;

struct i2c_master_bus_t {
    pthread_mutex_t      lock;
    i2c_sim_peripheral_t peripherals[I2C_SIM_MAX_PERIPHERALS];
    int                  device_count;
};

struct i2c_master_dev_t {
    struct i2c_master_bus_t *bus;
    uint16_t                 address;
};

static i2c_sim_peripheral_t *find_peripheral(i2c_master_bus_handle_t bus, uint16_t address) {
    for (int i = 0; i < I2C_SIM_MAX_PERIPHERALS; i++) {
        if (bus->peripherals[i].used && bus->peripherals[i].address == address) {
            return &bus->peripherals[i];
        }
    }
    return NULL;
}

esp_err_t i2c_new_master_bus(const i2c_master_bus_config_t *bus_config,
                             i2c_master_bus_handle_t *ret_bus_handle) {
    if (!bus_config || !ret_bus_handle) return ESP_ERR_INVALID_ARG;
    struct i2c_master_bus_t *bus = calloc(1, sizeof(*bus));
    if (!bus) return ESP_ERR_NO_MEM;
    pthread_mutex_init(&bus->lock, NULL);
    *ret_bus_handle = bus;
    return ESP_OK;
}

esp_err_t i2c_del_master_bus(i2c_master_bus_handle_t bus_handle) {
    if (!bus_handle) return ESP_ERR_INVALID_ARG;
    if (bus_handle->device_count != 0) return ESP_ERR_INVALID_STATE;
    pthread_mutex_destroy(&bus_handle->lock);
    free(bus_handle);
    return ESP_OK;
}

esp_err_t i2c_master_bus_add_device(i2c_master_bus_handle_t bus_handle,
                                    const i2c_device_config_t *dev_config,
                                    i2c_master_dev_handle_t *ret_handle) {
    if (!bus_handle || !dev_config || !ret_handle) return ESP_ERR_INVALID_ARG;
    if (dev_config->dev_addr_length != I2C_ADDR_BIT_LEN_7) return ESP_ERR_NOT_SUPPORTED;
    struct i2c_master_dev_t *dev = calloc(1, sizeof(*dev));
    if (!dev) return ESP_ERR_NO_MEM;
    dev->bus = bus_handle;
    dev->address = dev_config->device_address;
    pthread_mutex_lock(&bus_handle->lock);
    bus_handle->device_count++;
    pthread_mutex_unlock(&bus_handle->lock);
    *ret_handle = dev;
    return ESP_OK;
}

esp_err_t i2c_master_bus_rm_device(i2c_master_dev_handle_t handle) {
    if (!handle) return ESP_ERR_INVALID_ARG;
    pthread_mutex_lock(&handle->bus->lock);
    handle->bus->device_count--;
    pthread_mutex_unlock(&handle->bus->lock);
    free(handle);
    return ESP_OK;
}

static esp_err_t locked_write(i2c_master_bus_handle_t bus, uint16_t address,
                              const uint8_t *data, size_t len) {
    i2c_sim_peripheral_t *p = find_peripheral(bus, address);
    if (!p || !p->ops.on_write) return ESP_ERR_INVALID_STATE;
    return p->ops.on_write(p->ctx, data, len) == ESP_OK ? ESP_OK : ESP_ERR_INVALID_STATE;
}

static esp_err_t locked_read(i2c_master_bus_handle_t bus, uint16_t address,
                             uint8_t *data, size_t len) {
    i2c_sim_peripheral_t *p = find_peripheral(bus, address);
    if (!p || !p->ops.on_read) return ESP_ERR_INVALID_STATE;
    return p->ops.on_read(p->ctx, data, len) == ESP_OK ? ESP_OK : ESP_ERR_INVALID_STATE;
}

esp_err_t i2c_master_transmit(i2c_master_dev_handle_t i2c_dev, const uint8_t *write_buffer,
                              size_t write_size, int xfer_timeout_ms) {
    (void)xfer_timeout_ms;
    if (!i2c_dev || (!write_buffer && write_size > 0)) return ESP_ERR_INVALID_ARG;
    pthread_mutex_lock(&i2c_dev->bus->lock);
    esp_err_t err = locked_write(i2c_dev->bus, i2c_dev->address, write_buffer, write_size);
    pthread_mutex_unlock(&i2c_dev->bus->lock);
    return err;
}

esp_err_t i2c_master_receive(i2c_master_dev_handle_t i2c_dev, uint8_t *read_buffer,
                             size_t read_size, int xfer_timeout_ms) {
    (void)xfer_timeout_ms;
    if (!i2c_dev || !read_buffer) return ESP_ERR_INVALID_ARG;
    pthread_mutex_lock(&i2c_dev->bus->lock);
    esp_err_t err = locked_read(i2c_dev->bus, i2c_dev->address, read_buffer, read_size);
    pthread_mutex_unlock(&i2c_dev->bus->lock);
    return err;
}

esp_err_t i2c_master_transmit_receive(i2c_master_dev_handle_t i2c_dev,
                                      const uint8_t *write_buffer, size_t write_size,
                                      uint8_t *read_buffer, size_t read_size,
                                      int xfer_timeout_ms) {
    (void)xfer_timeout_ms;
    if (!i2c_dev || !write_buffer || !read_buffer) return ESP_ERR_INVALID_ARG;
    pthread_mutex_lock(&i2c_dev->bus->lock);
    esp_err_t err = locked_write(i2c_dev->bus, i2c_dev->address, write_buffer, write_size);
    if (err == ESP_OK) err = locked_read(i2c_dev->bus, i2c_dev->address, read_buffer, read_size);
    pthread_mutex_unlock(&i2c_dev->bus->lock);
    return err;
}

esp_err_t i2c_master_probe(i2c_master_bus_handle_t bus_handle, uint16_t address,
                           int xfer_timeout_ms) {
    (void)xfer_timeout_ms;
    if (!bus_handle) return ESP_ERR_INVALID_ARG;
    pthread_mutex_lock(&bus_handle->lock);
    bool found = find_peripheral(bus_handle, address) != NULL;
    pthread_mutex_unlock(&bus_handle->lock);
    return found ? ESP_OK : ESP_ERR_NOT_FOUND;
}

esp_err_t i2c_sim_attach(i2c_master_bus_handle_t bus, uint16_t address,
                         const i2c_sim_device_ops_t *ops, void *ctx) {
    if (!bus || !ops) return ESP_ERR_INVALID_ARG;
    pthread_mutex_lock(&bus->lock);
    esp_err_t err = ESP_ERR_NO_MEM;
    if (find_peripheral(bus, address)) {
        err = ESP_ERR_INVALID_STATE;
    } else {
        for (int i = 0; i < I2C_SIM_MAX_PERIPHERALS; i++) {
            if (!bus->peripherals[i].used) {
                bus->peripherals[i] = (i2c_sim_peripheral_t){
                    .used = true, .address = address, .ops = *ops, .ctx = ctx,
                };
                err = ESP_OK;
                break;
            }
        }
    }
    pthread_mutex_unlock(&bus->lock);
    return err;
}

esp_err_t i2c_sim_detach(i2c_master_bus_handle_t bus, uint16_t address) {
    if (!bus) return ESP_ERR_INVALID_ARG;
    pthread_mutex_lock(&bus->lock);
    i2c_sim_peripheral_t *p = find_peripheral(bus, address);
    if (p) p->used = false;
    pthread_mutex_unlock(&bus->lock);
    return p ? ESP_OK : ESP_ERR_NOT_FOUND;
}
