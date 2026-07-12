/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Host shim of the ESP-IDF i2c_master API, backed by a virtual bus. Pins and
 * clocks are accepted and ignored; peripherals are chip emulators attached at
 * a 7-bit address via simulator/i2c_master_sim.h. NACK (no peripheral at the
 * address, or the emulator refuses the transfer) maps to the device error
 * codes the real driver returns: ESP_ERR_INVALID_STATE for transfers,
 * ESP_ERR_NOT_FOUND for probe.
 */

#pragma once
#include <stdint.h>
#include <stddef.h>
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef int i2c_port_num_t;

#define I2C_NUM_0  0
#define I2C_NUM_1  1

#define I2C_CLK_SRC_DEFAULT  0

typedef enum {
    I2C_ADDR_BIT_LEN_7  = 0,
    I2C_ADDR_BIT_LEN_10 = 1,
} i2c_addr_bit_len_t;

typedef struct i2c_master_bus_t *i2c_master_bus_handle_t;
typedef struct i2c_master_dev_t *i2c_master_dev_handle_t;

typedef struct {
    i2c_port_num_t i2c_port;
    int            sda_io_num;
    int            scl_io_num;
    int            clk_source;
    uint8_t        glitch_ignore_cnt;
    int            intr_priority;
    size_t         trans_queue_depth;
    struct {
        uint32_t enable_internal_pullup : 1;
        uint32_t allow_pd : 1;
    } flags;
} i2c_master_bus_config_t;

typedef struct {
    i2c_addr_bit_len_t dev_addr_length;
    uint16_t           device_address;
    uint32_t           scl_speed_hz;
    uint32_t           scl_wait_us;
    struct {
        uint32_t disable_ack_check : 1;
    } flags;
} i2c_device_config_t;

esp_err_t i2c_new_master_bus(const i2c_master_bus_config_t *bus_config,
                             i2c_master_bus_handle_t *ret_bus_handle);
esp_err_t i2c_del_master_bus(i2c_master_bus_handle_t bus_handle);
esp_err_t i2c_master_bus_add_device(i2c_master_bus_handle_t bus_handle,
                                    const i2c_device_config_t *dev_config,
                                    i2c_master_dev_handle_t *ret_handle);
esp_err_t i2c_master_bus_rm_device(i2c_master_dev_handle_t handle);
esp_err_t i2c_master_transmit(i2c_master_dev_handle_t i2c_dev, const uint8_t *write_buffer,
                              size_t write_size, int xfer_timeout_ms);
esp_err_t i2c_master_receive(i2c_master_dev_handle_t i2c_dev, uint8_t *read_buffer,
                             size_t read_size, int xfer_timeout_ms);
esp_err_t i2c_master_transmit_receive(i2c_master_dev_handle_t i2c_dev,
                                      const uint8_t *write_buffer, size_t write_size,
                                      uint8_t *read_buffer, size_t read_size,
                                      int xfer_timeout_ms);
esp_err_t i2c_master_probe(i2c_master_bus_handle_t bus_handle, uint16_t address,
                           int xfer_timeout_ms);

#ifdef __cplusplus
}
#endif
