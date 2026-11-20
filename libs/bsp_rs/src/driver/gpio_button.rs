/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use idf::{Result, gpio};

use crate::button::{self, Driver};

#[derive(Clone, Copy)]
pub(crate) struct Pin {
    pub gpio: i32,
    pub active_low: bool,
    /// The internal pull toward the released level.
    pub pull: bool,
}

pub(crate) struct GpioButtons {
    pins: &'static [Pin],
}

impl GpioButtons {
    pub(crate) fn new(pins: &'static [Pin]) -> Result<Self> {
        gpio::install_isr_service()?;
        for pin in pins {
            gpio::set_direction(pin.gpio, gpio::Mode::Input)?;
            let pull = match (pin.pull, pin.active_low) {
                (false, _) => gpio::Pull::None,
                (true, true) => gpio::Pull::Up,
                (true, false) => gpio::Pull::Down,
            };
            gpio::set_pull(pin.gpio, pull)?;
            gpio::set_interrupt(pin.gpio, gpio::Interrupt::AnyEdge)?;
            unsafe { gpio::add_isr_handler(pin.gpio, on_edge, core::ptr::null_mut())? };
        }
        Ok(Self { pins })
    }
}

unsafe extern "C" fn on_edge(_arg: *mut core::ffi::c_void) {
    button::notify_from_isr();
}

impl Driver for GpioButtons {
    fn count(&self) -> usize {
        self.pins.len()
    }

    fn sample(&mut self, pressed: &mut [bool]) -> Result<()> {
        for (p, pin) in pressed.iter_mut().zip(self.pins) {
            *p = gpio::level(pin.gpio) != pin.active_low;
        }
        Ok(())
    }

    fn notifies(&self) -> bool {
        true
    }
}
