/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::ffi::CString;
use core::ffi::CStr;
use core::mem;
use core::ptr;

use crate::{Error, Result, sys};

pub const FREQ_DEFAULT_KHZ: i32 = sys::SDMMC_FREQ_DEFAULT as i32;
pub const FREQ_HIGHSPEED_KHZ: i32 = sys::SDMMC_FREQ_HIGHSPEED as i32;

pub type Command = sys::sdmmc_command_t;

const HOST_FLAG_1BIT: u32 = 1 << 0;
const HOST_FLAG_4BIT: u32 = 1 << 1;
const HOST_FLAG_8BIT: u32 = 1 << 2;
const HOST_FLAG_DDR: u32 = 1 << 4;
const HOST_FLAG_DEINIT_ARG: u32 = 1 << 5;
const SLOT_FLAG_INTERNAL_PULLUP: u32 = 1 << 0;

/// The controller's driver, as `SDMMC_HOST_DEFAULT()` sets it up for `slot`.
/// The hooks are the IDF host functions; a caller sharing the controller
/// replaces them.
pub fn default_host(slot: i32) -> sys::sdmmc_host_t {
    let mut host: sys::sdmmc_host_t = unsafe { mem::zeroed() };
    host.flags = HOST_FLAG_8BIT | HOST_FLAG_4BIT | HOST_FLAG_1BIT | HOST_FLAG_DDR | HOST_FLAG_DEINIT_ARG;
    host.slot = slot;
    host.max_freq_khz = FREQ_DEFAULT_KHZ;
    host.io_voltage = 3.3;
    host.driver_strength = sys::sdmmc_driver_strength_t_SDMMC_DRIVER_STRENGTH_B;
    host.current_limit = sys::sdmmc_current_limit_t_SDMMC_CURRENT_LIMIT_200MA;
    host.init = Some(sys::sdmmc_host_init);
    host.set_bus_width = Some(sys::sdmmc_host_set_bus_width);
    host.get_bus_width = Some(sys::sdmmc_host_get_slot_width);
    host.set_bus_ddr_mode = Some(sys::sdmmc_host_set_bus_ddr_mode);
    host.set_card_clk = Some(sys::sdmmc_host_set_card_clk);
    host.set_cclk_always_on = Some(sys::sdmmc_host_set_cclk_always_on);
    host.do_transaction = Some(sys::sdmmc_host_do_transaction);
    host.__bindgen_anon_1.deinit_p = Some(sys::sdmmc_host_deinit_slot);
    host.io_int_enable = Some(sys::sdmmc_host_io_int_enable);
    host.io_int_wait = Some(sys::sdmmc_host_io_int_wait);
    host.get_real_freq = Some(sys::sdmmc_host_get_real_freq);
    host.input_delay_phase = sys::sdmmc_delay_phase_t_SDMMC_DELAY_PHASE_0;
    host.set_input_delay = Some(sys::sdmmc_host_set_input_delay);
    host.set_input_delayline = Some(sys::sdmmc_host_set_input_delayline);
    host.unaligned_multi_block_rw_max_chunk_size = 16;
    host.check_buffer_alignment = Some(sys::sdmmc_host_check_buffer_alignment);
    host.is_slot_set_to_uhs1 = Some(sys::sdmmc_host_is_slot_set_to_uhs1);
    host
}

pub fn host_init() -> Result<()> {
    Error::check(unsafe { sys::sdmmc_host_init() })
}

pub fn host_initialized() -> bool {
    let mut state: sys::sdmmc_host_state_t = unsafe { mem::zeroed() };
    (unsafe { sys::sdmmc_host_get_state(&mut state) }) == 0 && state.host_initialized
}

/// -1 leaves a pin unconnected.
#[derive(Clone, Copy, Debug)]
pub struct SlotPins {
    pub clk: i32,
    pub cmd: i32,
    pub d0: i32,
    pub d1: i32,
    pub d2: i32,
    pub d3: i32,
}

pub fn slot_config(pins: &SlotPins, width: u8, internal_pullup: bool) -> sys::sdmmc_slot_config_t {
    let mut slot: sys::sdmmc_slot_config_t = unsafe { mem::zeroed() };
    slot.clk = pins.clk;
    slot.cmd = pins.cmd;
    slot.d0 = pins.d0;
    slot.d1 = pins.d1;
    slot.d2 = pins.d2;
    slot.d3 = pins.d3;
    slot.d4 = -1;
    slot.d5 = -1;
    slot.d6 = -1;
    slot.d7 = -1;
    slot.__bindgen_anon_1.cd = -1;
    slot.__bindgen_anon_2.wp = -1;
    slot.width = width;
    slot.flags = if internal_pullup { SLOT_FLAG_INTERNAL_PULLUP } else { 0 };
    slot
}

/// An on-chip LDO channel powering the card's I/O.
pub struct LdoPower(sys::sd_pwr_ctrl_handle_t);

unsafe impl Send for LdoPower {}

impl LdoPower {
    pub fn new(channel: i32) -> Result<Self> {
        let mut config: sys::sd_pwr_ctrl_ldo_config_t = unsafe { mem::zeroed() };
        config.ldo_chan_id = channel;
        let mut handle = ptr::null_mut();
        Error::check(unsafe { sys::sd_pwr_ctrl_new_on_chip_ldo(&config, &mut handle) })?;
        Ok(Self(handle))
    }

    pub fn handle(&self) -> sys::sd_pwr_ctrl_handle_t {
        self.0
    }
}

impl Drop for LdoPower {
    fn drop(&mut self) {
        unsafe { sys::sd_pwr_ctrl_del_on_chip_ldo(self.0) };
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FatConfig {
    pub format_if_mount_failed: bool,
    pub max_files: u8,
    pub allocation_unit_size: usize,
}

/// A card mounted as FAT at a VFS path; unmounted on drop.
pub struct FatMount {
    path: CString,
    card: *mut sys::sdmmc_card_t,
}

unsafe impl Send for FatMount {}

impl FatMount {
    /// The card and its host must stay set up until the mount is dropped.
    pub unsafe fn mount(
        path: &CStr,
        host: &sys::sdmmc_host_t,
        slot: &sys::sdmmc_slot_config_t,
        config: &FatConfig,
    ) -> Result<Self> {
        let mut mount: sys::esp_vfs_fat_mount_config_t = unsafe { mem::zeroed() };
        mount.format_if_mount_failed = config.format_if_mount_failed;
        mount.max_files = i32::from(config.max_files);
        mount.allocation_unit_size = config.allocation_unit_size;
        let mut card = ptr::null_mut();
        Error::check(unsafe {
            sys::esp_vfs_fat_sdmmc_mount(path.as_ptr(), host, ptr::from_ref(slot).cast(), &mount, &mut card)
        })?;
        Ok(Self { path: path.into(), card })
    }

    /// Total and free bytes.
    pub fn info(&self) -> Result<(u64, u64)> {
        let (mut total, mut free) = (0, 0);
        Error::check(unsafe { sys::esp_vfs_fat_info(self.path.as_ptr(), &mut total, &mut free) })?;
        Ok((total, free))
    }

    pub fn unmount(self) -> Result<()> {
        let mount = mem::ManuallyDrop::new(self);
        let path = unsafe { ptr::read(&mount.path) };
        Error::check(unsafe { sys::esp_vfs_fat_sdcard_unmount(path.as_ptr(), mount.card) })
    }
}

impl Drop for FatMount {
    fn drop(&mut self) {
        unsafe { sys::esp_vfs_fat_sdcard_unmount(self.path.as_ptr(), self.card) };
    }
}
