/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use core::ffi::c_void;
use core::mem;
use core::ptr;
use core::time::Duration;

use crate::sync::Semaphore;
use crate::{Error, Result, sys};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorMode {
    Rgb565,
    Rgb888,
    Argb8888,
}

impl ColorMode {
    fn raw(self) -> sys::ppa_srm_color_mode_t {
        match self {
            Self::Rgb565 => sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB565,
            Self::Rgb888 => sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_RGB888,
            Self::Argb8888 => sys::ppa_srm_color_mode_t_PPA_SRM_COLOR_MODE_ARGB8888,
        }
    }

    fn blend(self) -> sys::ppa_blend_color_mode_t {
        match self {
            Self::Rgb565 => sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_RGB565,
            Self::Rgb888 => sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_RGB888,
            Self::Argb8888 => sys::ppa_blend_color_mode_t_PPA_BLEND_COLOR_MODE_ARGB8888,
        }
    }
}

/// Counter-clockwise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rotation {
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    fn raw(self) -> sys::ppa_srm_rotation_angle_t {
        match self {
            Self::R0 => sys::ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_0,
            Self::R90 => sys::ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_90,
            Self::R180 => sys::ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_180,
            Self::R270 => sys::ppa_srm_rotation_angle_t_PPA_SRM_ROTATION_ANGLE_270,
        }
    }
}

/// A whole `src_width` x `src_height` picture copied to (`dst_x`, `dst_y`).
/// The destination's address and size must be cache-line aligned.
pub struct SrmOp {
    pub src: *const u8,
    pub src_width: u32,
    pub src_height: u32,
    pub dst: *mut u8,
    pub dst_size: u32,
    pub dst_width: u32,
    pub dst_height: u32,
    pub dst_x: u32,
    pub dst_y: u32,
    pub format: ColorMode,
    pub rotation: Rotation,
}

pub struct Srm {
    client: sys::ppa_client_handle_t,
    done: Semaphore,
    pending: u32,
}

unsafe impl Send for Srm {}

// Flash code in a 2D-DMA ISR path faults at cache-line boundaries on P4.
#[cfg_attr(target_os = "none", unsafe(link_section = ".iram1.idf_ppa_done"))]
unsafe extern "C" fn trans_done(
    _client: sys::ppa_client_handle_t,
    _data: *mut sys::ppa_event_data_t,
    user: *mut c_void,
) -> bool {
    let mut woken = 0;
    unsafe { sys::xQueueGiveFromISR(user.cast(), &mut woken) };
    woken != 0
}

impl Srm {
    /// `max_pending` transactions may be in flight at once.
    pub fn new(max_pending: u32) -> Result<Self> {
        let done = Semaphore::try_new(0, max_pending)?;
        let mut cfg: sys::ppa_client_config_t = unsafe { mem::zeroed() };
        cfg.oper_type = sys::ppa_operation_t_PPA_OPERATION_SRM;
        cfg.max_pending_trans_num = max_pending;
        let mut client = ptr::null_mut();
        Error::check(unsafe { sys::ppa_register_client(&cfg, &mut client) })?;
        let cbs = sys::ppa_event_callbacks_t { on_trans_done: Some(trans_done) };
        if let Err(err) = Error::check(unsafe { sys::ppa_client_register_event_callbacks(client, &cbs) }) {
            unsafe { sys::ppa_unregister_client(client) };
            return Err(err);
        }
        Ok(Self { client, done, pending: 0 })
    }

    /// Returns once queued. The pixels must stay valid until [`Self::wait`]
    /// has returned for this transaction.
    pub unsafe fn start(&mut self, op: &SrmOp) -> Result<()> {
        let mut cfg: sys::ppa_srm_oper_config_t = unsafe { mem::zeroed() };
        cfg.in_.buffer = op.src.cast();
        cfg.in_.pic_w = op.src_width;
        cfg.in_.pic_h = op.src_height;
        cfg.in_.block_w = op.src_width;
        cfg.in_.block_h = op.src_height;
        cfg.in_.__bindgen_anon_1.srm_cm = op.format.raw();
        cfg.out.buffer = op.dst.cast();
        cfg.out.buffer_size = op.dst_size;
        cfg.out.pic_w = op.dst_width;
        cfg.out.pic_h = op.dst_height;
        cfg.out.block_offset_x = op.dst_x;
        cfg.out.block_offset_y = op.dst_y;
        cfg.out.__bindgen_anon_1.srm_cm = op.format.raw();
        cfg.rotation_angle = op.rotation.raw();
        cfg.scale_x = 1.0;
        cfg.scale_y = 1.0;
        cfg.mode = sys::ppa_trans_mode_t_PPA_TRANS_MODE_NON_BLOCKING;
        cfg.user_data = self.done.raw().cast();
        Error::check(unsafe { sys::ppa_do_scale_rotate_mirror(self.client, &cfg) })?;
        self.pending += 1;
        Ok(())
    }

    pub fn pending(&self) -> u32 {
        self.pending
    }

    /// Blocks until the oldest transaction in flight has finished; `false` on
    /// timeout, with the transaction still counted as in flight.
    pub fn wait_timeout(&mut self, timeout: Duration) -> bool {
        if self.pending == 0 {
            return true;
        }
        if !self.done.take_timeout(timeout) {
            return false;
        }
        self.pending -= 1;
        true
    }

    /// Blocks until the oldest transaction in flight has finished.
    pub fn wait(&mut self) {
        if self.pending > 0 {
            self.done.take();
            self.pending -= 1;
        }
    }
}

/// An ARGB8888 picture laid over a block of a picture, in place. The picture's
/// address and size must be cache-line aligned.
pub struct BlendOp {
    pub foreground: *const u8,
    pub width: u32,
    pub height: u32,
    pub picture: *mut u8,
    pub picture_size: u32,
    pub picture_width: u32,
    pub picture_height: u32,
    pub x: u32,
    pub y: u32,
    pub format: ColorMode,
}

pub struct Blend {
    client: sys::ppa_client_handle_t,
    done: Semaphore,
}

unsafe impl Send for Blend {}

impl Blend {
    pub fn new() -> Result<Self> {
        let done = Semaphore::try_new(0, 1)?;
        let mut cfg: sys::ppa_client_config_t = unsafe { mem::zeroed() };
        cfg.oper_type = sys::ppa_operation_t_PPA_OPERATION_BLEND;
        cfg.max_pending_trans_num = 1;
        let mut client = ptr::null_mut();
        Error::check(unsafe { sys::ppa_register_client(&cfg, &mut client) })?;
        let cbs = sys::ppa_event_callbacks_t { on_trans_done: Some(trans_done) };
        if let Err(err) = Error::check(unsafe { sys::ppa_client_register_event_callbacks(client, &cbs) }) {
            unsafe { sys::ppa_unregister_client(client) };
            return Err(err);
        }
        Ok(Self { client, done })
    }

    /// Blocks until done, or for `timeout`, after which `false` leaves the
    /// transaction running and the pixels in use.
    pub unsafe fn run(&mut self, op: &BlendOp, timeout: Duration) -> Result<bool> {
        let mut cfg: sys::ppa_blend_oper_config_t = unsafe { mem::zeroed() };
        cfg.in_bg.buffer = op.picture.cast_const().cast();
        cfg.in_bg.pic_w = op.picture_width;
        cfg.in_bg.pic_h = op.picture_height;
        cfg.in_bg.block_w = op.width;
        cfg.in_bg.block_h = op.height;
        cfg.in_bg.block_offset_x = op.x;
        cfg.in_bg.block_offset_y = op.y;
        cfg.in_bg.__bindgen_anon_1.blend_cm = op.format.blend();
        cfg.in_fg.buffer = op.foreground.cast();
        cfg.in_fg.pic_w = op.width;
        cfg.in_fg.pic_h = op.height;
        cfg.in_fg.block_w = op.width;
        cfg.in_fg.block_h = op.height;
        cfg.in_fg.__bindgen_anon_1.blend_cm = ColorMode::Argb8888.blend();
        cfg.out.buffer = op.picture.cast();
        cfg.out.buffer_size = op.picture_size;
        cfg.out.pic_w = op.picture_width;
        cfg.out.pic_h = op.picture_height;
        cfg.out.block_offset_x = op.x;
        cfg.out.block_offset_y = op.y;
        cfg.out.__bindgen_anon_1.blend_cm = op.format.blend();
        cfg.mode = sys::ppa_trans_mode_t_PPA_TRANS_MODE_NON_BLOCKING;
        cfg.user_data = self.done.raw().cast();
        Error::check(unsafe { sys::ppa_do_blend(self.client, &cfg) })?;
        Ok(self.done.take_timeout(timeout))
    }
}

impl Drop for Blend {
    fn drop(&mut self) {
        unsafe { sys::ppa_unregister_client(self.client) };
    }
}

impl Drop for Srm {
    fn drop(&mut self) {
        while self.pending > 0 {
            self.wait();
        }
        unsafe { sys::ppa_unregister_client(self.client) };
    }
}

#[cfg(test)]
mod tests {
    use super::{ColorMode, Rotation, Srm, SrmOp};

    #[test]
    fn srm_rotates_and_reports_completion() {
        let src: [u16; 2] = [0x1234, 0xabcd];
        let mut dst = [0u16; 2];
        let mut srm = Srm::new(2).unwrap();
        let op = SrmOp {
            src: src.as_ptr().cast(),
            src_width: 2,
            src_height: 1,
            dst: dst.as_mut_ptr().cast(),
            dst_size: 4,
            dst_width: 1,
            dst_height: 2,
            dst_x: 0,
            dst_y: 0,
            format: ColorMode::Rgb565,
            rotation: Rotation::R90,
        };
        unsafe { srm.start(&op) }.unwrap();
        assert_eq!(srm.pending(), 1);
        srm.wait();
        assert_eq!(srm.pending(), 0);
        assert_eq!(dst, [0xabcd, 0x1234]);
    }
}
