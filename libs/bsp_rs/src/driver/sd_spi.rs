/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! A card slot on an SPI bus, which other devices may share.

use core::ffi::CStr;

use idf::fat::{FatConfig, FatMount};
use idf::log::error;
use idf::spi::Bus;
use idf::{Error, Result, gpio, sdspi};

use crate::sd::{Driver, MountConfig, Space};

const TAG: &CStr = c"sd_spi";

const DEFAULT_MAX_FILES: u8 = 5;

pub(crate) struct Config {
    pub bus: Bus,
    pub cs: i32,
    pub max_freq_khz: i32,
}

pub(crate) struct SdSpi {
    config: Config,
    fat: Option<FatMount>,
}

// A card whose select floats may answer what the bus carries for others, and
// unmounting leaves the select an input.
fn deselect(cs: i32) -> Result<()> {
    gpio::configure(&[cs], gpio::Mode::Output)?;
    gpio::set_level(cs, true)
}

impl SdSpi {
    pub(crate) fn new(config: Config) -> Result<Self> {
        deselect(config.cs)?;
        Ok(Self { config, fat: None })
    }
}

impl Driver for SdSpi {
    fn mount(&mut self, path: &CStr, config: &MountConfig) -> Result<()> {
        if config.psram_bounce_buffer {
            return Err(Error::NOT_SUPPORTED);
        }
        let fat = FatConfig {
            format_if_mount_failed: config.format_if_mount_failed,
            max_files: if config.max_files == 0 { DEFAULT_MAX_FILES } else { config.max_files },
            allocation_unit_size: 0,
        };
        let khz = config.max_freq_khz.map_or(self.config.max_freq_khz, |khz| khz as i32);
        // The bus lives in `config` as long as the mount.
        let fat = unsafe { sdspi::mount_fat(path, &self.config.bus, self.config.cs, khz, &fat) }.inspect_err(|err| {
            error!(TAG, "mount: {err}");
            let _ = deselect(self.config.cs);
        })?;
        self.fat = Some(fat);
        Ok(())
    }

    fn unmount(&mut self) -> Result<()> {
        let result = self.fat.take().ok_or(Error::INVALID_STATE)?.unmount();
        deselect(self.config.cs)?;
        result
    }

    fn is_mounted(&self) -> bool {
        self.fat.is_some()
    }

    fn space(&self) -> Result<Space> {
        let (total, free) = self.fat.as_ref().ok_or(Error::INVALID_STATE)?.info()?;
        Ok(Space { total, free })
    }
}
