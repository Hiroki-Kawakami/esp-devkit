/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::mem;

use crate::Result;
use crate::fat::{FatConfig, FatMount};
use crate::spi::Bus;
use crate::sys;

const HOST_FLAG_SPI: u32 = 1 << 3;
const HOST_FLAG_DEINIT_ARG: u32 = 1 << 5;

/// The card's driver on `bus`, as `SDSPI_HOST_DEFAULT()` sets it up.
pub fn default_host(bus: &Bus, max_freq_khz: i32) -> sys::sdmmc_host_t {
    let mut host: sys::sdmmc_host_t = unsafe { mem::zeroed() };
    host.flags = HOST_FLAG_SPI | HOST_FLAG_DEINIT_ARG;
    host.slot = bus.host();
    host.max_freq_khz = max_freq_khz;
    host.io_voltage = 3.3;
    host.driver_strength = sys::sdmmc_driver_strength_t_SDMMC_DRIVER_STRENGTH_B;
    host.current_limit = sys::sdmmc_current_limit_t_SDMMC_CURRENT_LIMIT_200MA;
    host.init = Some(sys::sdspi_host_init);
    host.set_card_clk = Some(sys::sdspi_host_set_card_clk);
    host.do_transaction = Some(sys::sdspi_host_do_transaction);
    host.__bindgen_anon_1.deinit_p = Some(sys::sdspi_host_remove_device);
    host.io_int_enable = Some(sys::sdspi_host_io_int_enable);
    host.io_int_wait = Some(sys::sdspi_host_io_int_wait);
    host.get_real_freq = Some(sys::sdspi_host_get_real_freq);
    host.input_delay_phase = sys::sdmmc_delay_phase_t_SDMMC_DELAY_PHASE_0;
    host.unaligned_multi_block_rw_max_chunk_size = 16;
    host.check_buffer_alignment = Some(sys::sdspi_host_check_buffer_alignment);
    host
}

/// Mounts the card behind chip select `cs` on `bus` as FAT. `bus` must stay
/// up until the mount is dropped.
pub unsafe fn mount_fat(path: &CStr, bus: &Bus, cs: i32, max_freq_khz: i32, config: &FatConfig) -> Result<FatMount> {
    let host = default_host(bus, max_freq_khz);
    let mut device: sys::sdspi_device_config_t = unsafe { mem::zeroed() };
    device.host_id = bus.host() as _;
    device.gpio_cs = cs;
    device.gpio_cd = -1;
    device.gpio_wp = -1;
    device.gpio_int = -1;
    unsafe { FatMount::mount(path, config, |path, mount, card| sys::esp_vfs_fat_sdspi_mount(path, &host, &device, mount, card)) }
}
