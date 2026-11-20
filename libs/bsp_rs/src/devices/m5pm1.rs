/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::CStr;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use idf::i2c::{Device, MasterBus};
use idf::log::{error, info};
use idf::sync::Mutex;
use idf::{Error, Result, gpio, thread};

use crate::button::{self, Event, Judged};

const TAG: &CStr = c"m5pm1";

pub(crate) const ADDR: u16 = 0x6E;
pub(crate) const DEFAULT_HZ: u32 = 100_000;

const TIMEOUT: Duration = Duration::from_millis(50);
const WAKE_TRIES: u32 = 3;
const WAKE_POLL: Duration = Duration::from_millis(10);

const REG_ID: u8 = 0x00;
const REG_I2C_CFG: u8 = 0x09;
const REG_GPIO_FUNC0: u8 = 0x16;
const REG_IRQ_GPIO: u8 = 0x40;
const REG_IRQ_SYS: u8 = 0x41;
const REG_IRQ_BTN: u8 = 0x42;
const REG_MASK_GPIO: u8 = 0x43;
const REG_MASK_SYS: u8 = 0x44;
const REG_MASK_BTN: u8 = 0x45;
const REG_BTN_CFG: u8 = 0x49;
const REG_BTN_CFG_2: u8 = 0x4A;

const ID: [u8; 2] = [0x50, 0x20];
const FUNC_IRQ: u8 = 0b01;
const MASK_ALL_GPIO: u8 = 0x1F;
const MASK_ALL_SYS: u8 = 0x3F;
const BTN_CLICK: u8 = 1 << 0;
const BTN_WAKE: u8 = 1 << 1;
const BTN_DOUBLE: u8 = 1 << 2;
const BTN_ALL: u8 = BTN_CLICK | BTN_WAKE | BTN_DOUBLE;
const CFG_SINGLE_RESET_DIS: u8 = 1 << 0;
const CFG_DOUBLE_SHIFT: u8 = 5;
const CFG_DOUBLE_MASK: u8 = 0b11 << CFG_DOUBLE_SHIFT;
const CFG_2_DOUBLE_POWEROFF_DIS: u8 = 1 << 0;
const DOUBLE_INTERVALS_MS: [u64; 4] = [125, 250, 500, 1000];
const DOUBLE_DEFAULT: u8 = 0b01;

pub(crate) struct M5pm1<'bus> {
    device: Device<'bus>,
}

impl<'bus> M5pm1<'bus> {
    /// Keeps the chip from sleeping between transfers.
    pub(crate) fn new(bus: &'bus MasterBus) -> Result<Self> {
        let device = bus.add_device(ADDR, DEFAULT_HZ)?;
        let mut id = [0u8; 2];
        let mut tries = 0;
        // A chip an earlier firmware let sleep spends the first transfer
        // waking up.
        while let Err(err) = device.transmit_receive(&[REG_ID], &mut id, TIMEOUT) {
            tries += 1;
            if tries >= WAKE_TRIES {
                error!(TAG, "not responding on 0x{ADDR:02x}: {err}");
                return Err(err);
            }
            thread::sleep(WAKE_POLL);
        }
        if id != ID {
            error!(TAG, "id {:02x} {:02x}", id[0], id[1]);
            return Err(Error::NOT_FOUND);
        }
        let pm1 = Self { device };
        pm1.write(REG_I2C_CFG, 0x00)?;
        Ok(pm1)
    }

    fn read(&self, reg: u8) -> Result<u8> {
        let mut v = [0u8];
        self.device.transmit_receive(&[reg], &mut v, TIMEOUT)?;
        Ok(v[0])
    }

    fn write(&self, reg: u8, value: u8) -> Result<()> {
        self.device.transmit(&[reg, value], TIMEOUT)
    }
}

static IRQ_PENDING: AtomicBool = AtomicBool::new(true);

/// The power button, as the chip judges its clicks. While nothing listens, the
/// chip keeps its own uses: a click resets, a double click powers off.
pub(crate) struct PowerButton {
    pm1: &'static Mutex<M5pm1<'static>>,
}

impl PowerButton {
    /// Raises the chip's `irq_out` pin as its interrupt line, wired to
    /// `irq_pin`, with only the button's clicks unmasked.
    pub(crate) fn new(pm1: &'static Mutex<M5pm1<'static>>, irq_out: u8, irq_pin: i32) -> Result<Self> {
        {
            let pm1 = pm1.lock();
            pm1.write(REG_MASK_GPIO, MASK_ALL_GPIO)?;
            pm1.write(REG_MASK_SYS, MASK_ALL_SYS)?;
            // Stays set until the wake source is cleared.
            pm1.write(REG_MASK_BTN, BTN_WAKE)?;
            let shift = irq_out * 2;
            let func = pm1.read(REG_GPIO_FUNC0)?;
            pm1.write(REG_GPIO_FUNC0, func & !(0b11 << shift) | FUNC_IRQ << shift)?;
            for reg in [REG_IRQ_GPIO, REG_IRQ_SYS, REG_IRQ_BTN] {
                pm1.write(reg, 0x00)?;
            }
        }
        gpio::install_isr_service()?;
        gpio::set_direction(irq_pin, gpio::Mode::Input)?;
        gpio::set_pull(irq_pin, gpio::Pull::Up)?;
        gpio::set_interrupt(irq_pin, gpio::Interrupt::FallingEdge)?;
        unsafe { gpio::add_isr_handler(irq_pin, on_irq, core::ptr::null_mut())? };
        info!(TAG, "power button on GPIO{irq_pin}");
        Ok(Self { pm1 })
    }
}

unsafe extern "C" fn on_irq(_arg: *mut core::ffi::c_void) {
    IRQ_PENDING.store(true, Ordering::Release);
    button::notify_from_isr();
}

fn nearest_double(interval: Duration) -> u8 {
    let ms = interval.as_millis() as u64;
    (0..DOUBLE_INTERVALS_MS.len()).min_by_key(|&i| DOUBLE_INTERVALS_MS[i].abs_diff(ms)).unwrap_or(0) as u8
}

impl Judged for PowerButton {
    fn count(&self) -> usize {
        1
    }

    fn poll(&mut self, emit: &mut dyn FnMut(usize, Event)) -> Result<()> {
        if !IRQ_PENDING.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let pm1 = self.pm1.lock();
        let status = pm1
            .read(REG_IRQ_BTN)
            .and_then(|status| pm1.write(REG_IRQ_BTN, !status & BTN_ALL).map(|()| status))
            .inspect_err(|_| IRQ_PENDING.store(true, Ordering::Release))?;
        drop(pm1);
        if status & BTN_CLICK != 0 {
            emit(0, Event::Click);
        }
        if status & BTN_DOUBLE != 0 {
            emit(0, Event::DoubleClick);
        }
        Ok(())
    }

    fn configure(&mut self, listening: bool, double: &[Option<Duration>]) -> Result<()> {
        let double = double.first().copied().flatten().filter(|_| listening);
        let pm1 = self.pm1.lock();
        let cfg = pm1.read(REG_BTN_CFG)?;
        let window = double.map_or(DOUBLE_DEFAULT, nearest_double);
        let reset_dis = if listening { CFG_SINGLE_RESET_DIS } else { 0 };
        pm1.write(REG_BTN_CFG, cfg & !(CFG_DOUBLE_MASK | CFG_SINGLE_RESET_DIS) | window << CFG_DOUBLE_SHIFT | reset_dis)?;
        let cfg_2 = pm1.read(REG_BTN_CFG_2)?;
        let poweroff_dis = if double.is_some() { CFG_2_DOUBLE_POWEROFF_DIS } else { 0 };
        pm1.write(REG_BTN_CFG_2, cfg_2 & !CFG_2_DOUBLE_POWEROFF_DIS | poweroff_dis)
    }
}
