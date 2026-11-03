/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use core::ffi::{CStr, c_void};
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use idf::heap::Caps;
use idf::log::error;
use idf::sdmmc::{self, Command, FatConfig, FatMount, LdoPower, SlotPins};
use idf::sync::{OnceLock, Semaphore};
use idf::{Error, Result, sys};

use crate::sd::{Driver, MountConfig};

const TAG: &CStr = c"sdmmc";

const DEFAULT_MAX_FILES: u8 = 5;
const SECTOR: usize = 512;

/// The SDMMC controller. The board owns it for good; each slot's user brackets
/// what touches it with `acquire`.
pub(crate) struct Controller {
    lock: Semaphore,
    owner: AtomicPtr<c_void>,
    depth: AtomicU32,
}

pub(crate) struct Guard<'a>(&'a Controller);

impl Controller {
    /// Nests within one task.
    pub(crate) fn acquire(&self) -> Guard<'_> {
        let me = unsafe { sys::xTaskGetCurrentTaskHandle() }.cast::<c_void>();
        if self.owner.load(Ordering::Acquire) != me {
            self.lock.take();
            self.owner.store(me, Ordering::Release);
        }
        self.depth.fetch_add(1, Ordering::AcqRel);
        Guard(self)
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        if self.0.depth.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.owner.store(ptr::null_mut(), Ordering::Release);
            self.0.lock.give();
        }
    }
}

static CONTROLLER: OnceLock<Controller> = OnceLock::new();

pub(crate) fn controller() -> Result<&'static Controller> {
    CONTROLLER.get_or_try_init(|| {
        ensure_host()?;
        Ok(Controller { lock: Semaphore::try_new(1, 1)?, owner: AtomicPtr::new(ptr::null_mut()), depth: AtomicU32::new(0) })
    })
}

fn ensure_host() -> Result<()> {
    if sdmmc::host_initialized() { Ok(()) } else { sdmmc::host_init() }
}

unsafe extern "C" fn host_init() -> sys::esp_err_t {
    ensure_host().err().map_or(0, Error::code)
}

unsafe extern "C" fn do_transaction(slot: i32, cmd: *mut Command) -> sys::esp_err_t {
    let Some(controller) = CONTROLLER.get() else { return Error::INVALID_STATE.code() };
    let _guard = controller.acquire();
    unsafe { sys::sdmmc_host_do_transaction(slot, cmd) }
}

// IDF deletes the controller along with its last slot; it stays the board's,
// so it comes straight back.
unsafe extern "C" fn deinit_slot(slot: i32) -> sys::esp_err_t {
    let Some(controller) = CONTROLLER.get() else { return Error::INVALID_STATE.code() };
    let _guard = controller.acquire();
    let err = unsafe { sys::sdmmc_host_deinit_slot(slot) };
    if let Err(restore) = ensure_host() {
        error!(TAG, "controller restore: {restore}");
    }
    err
}

pub(crate) struct Config {
    pub slot: i32,
    pub pins: SlotPins,
    pub width: u8,
    pub max_freq_khz: i32,
    /// The on-chip LDO channel feeding the card's I/O, if any.
    pub ldo_channel: Option<i32>,
    pub allocation_unit_size: usize,
}

struct Mounted {
    fat: FatMount,
    _bounce: Option<Box<[u8], Caps>>,
}

/// A card on one slot of the shared controller.
pub(crate) struct SdmmcCard {
    controller: &'static Controller,
    config: Config,
    power: Option<LdoPower>,
    mounted: Option<Mounted>,
}

impl SdmmcCard {
    pub(crate) fn new(controller: &'static Controller, config: Config) -> Self {
        Self { controller, config, power: None, mounted: None }
    }

    fn bounce_buffer(host: &sys::sdmmc_host_t) -> Result<Box<[u8], Caps>> {
        if cfg!(not(CONFIG_SOC_SDMMC_PSRAM_DMA_CAPABLE)) {
            return Err(Error::NOT_SUPPORTED);
        }
        let blocks = host.unaligned_multi_block_rw_max_chunk_size.max(1);
        idf::heap::zeroed_slice(blocks * SECTOR, Caps::SPIRAM | Caps::CACHE_ALIGNED)
    }
}

impl Driver for SdmmcCard {
    fn mount(&mut self, path: &CStr, config: &MountConfig) -> Result<()> {
        let mut host = sdmmc::default_host(self.config.slot);
        host.init = Some(host_init);
        host.do_transaction = Some(do_transaction);
        host.__bindgen_anon_1.deinit_p = Some(deinit_slot);
        host.max_freq_khz = config.max_freq_khz.map_or(self.config.max_freq_khz, |khz| khz as i32);

        let bounce = config.psram_bounce_buffer.then(|| Self::bounce_buffer(&host)).transpose()?;
        if let Some(bounce) = &bounce {
            host.dma_aligned_buffer = bounce.as_ptr().cast_mut().cast();
        }
        if self.power.is_none()
            && let Some(channel) = self.config.ldo_channel
        {
            self.power = Some(LdoPower::new(channel).inspect_err(|err| error!(TAG, "card power: {err}"))?);
        }
        if let Some(power) = &self.power {
            host.pwr_ctrl_handle = power.handle();
        }

        let slot = sdmmc::slot_config(&self.config.pins, self.config.width, true);
        let fat = FatConfig {
            format_if_mount_failed: config.format_if_mount_failed,
            max_files: if config.max_files == 0 { DEFAULT_MAX_FILES } else { config.max_files },
            allocation_unit_size: self.config.allocation_unit_size,
        };
        let _guard = self.controller.acquire();
        let fat = unsafe { FatMount::mount(path, &host, &slot, &fat) }.inspect_err(|err| error!(TAG, "mount: {err}"))?;
        self.mounted = Some(Mounted { fat, _bounce: bounce });
        Ok(())
    }

    fn unmount(&mut self) -> Result<()> {
        let mounted = self.mounted.take().ok_or(Error::INVALID_STATE)?;
        let _guard = self.controller.acquire();
        mounted.fat.unmount()
    }

    fn is_mounted(&self) -> bool {
        self.mounted.is_some()
    }
}
