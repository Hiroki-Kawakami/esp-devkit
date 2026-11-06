/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * IDF's PPA driver, the reference media_engine is compared against.
 */

use core::ffi::c_void;
use core::mem;
use core::ptr;
use core::time::Duration;

use idf::{Error, Result};

use crate::ffi as sys;

const MAX_DELAY: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorMode {
    Rgb565,
    Rgb888,
    Argb8888,
}

impl ColorMode {
    fn srm(self) -> sys::ppa_srm_color_mode_t {
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

pub struct SrmOp {
    pub src: *const u8,
    pub src_width: u32,
    pub src_height: u32,
    pub src_x: u32,
    pub src_y: u32,
    pub block_width: u32,
    pub block_height: u32,
    pub src_format: ColorMode,
    pub dst: *mut u8,
    pub dst_size: u32,
    pub dst_width: u32,
    pub dst_height: u32,
    pub dst_x: u32,
    pub dst_y: u32,
    pub format: ColorMode,
    pub rotation: Rotation,
    pub scale: f32,
}

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

unsafe extern "C" fn trans_done(_client: sys::ppa_client_handle_t, _data: *mut sys::ppa_event_data_t, user: *mut c_void) -> bool {
    let mut woken = 0;
    unsafe { sys::xQueueGiveFromISR(user.cast(), &mut woken) };
    woken != 0
}

fn ticks(d: Duration) -> u32 {
    (d.as_micros() as u64 * sdkconfig::CONFIG_FREERTOS_HZ as u64 / 1_000_000 + 1) as u32
}

struct Client {
    client: sys::ppa_client_handle_t,
    done: sys::QueueHandle_t,
}

impl Client {
    fn new(oper: sys::ppa_operation_t) -> Result<Self> {
        let done = unsafe { sys::xQueueCreateCountingSemaphore(1, 0) };
        if done.is_null() {
            return Err(Error::NO_MEM);
        }
        let mut cfg: sys::ppa_client_config_t = unsafe { mem::zeroed() };
        cfg.oper_type = oper;
        cfg.max_pending_trans_num = 1;
        let mut client = ptr::null_mut();
        Error::check(unsafe { sys::ppa_register_client(&cfg, &mut client) })?;
        let cbs = sys::ppa_event_callbacks_t { on_trans_done: Some(trans_done) };
        Error::check(unsafe { sys::ppa_client_register_event_callbacks(client, &cbs) })?;
        Ok(Self { client, done })
    }

    fn take(&self, ticks: u32) -> bool {
        unsafe { sys::xQueueSemaphoreTake(self.done, ticks) != 0 }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe {
            sys::ppa_unregister_client(self.client);
            sys::vQueueDelete(self.done);
        }
    }
}

pub struct Srm {
    client: Client,
    pending: bool,
}

impl Srm {
    pub fn new() -> Result<Self> {
        Ok(Self { client: Client::new(sys::ppa_operation_t_PPA_OPERATION_SRM)?, pending: false })
    }

    pub unsafe fn start(&mut self, op: &SrmOp) -> Result<()> {
        let mut cfg: sys::ppa_srm_oper_config_t = unsafe { mem::zeroed() };
        cfg.in_.buffer = op.src.cast();
        cfg.in_.pic_w = op.src_width;
        cfg.in_.pic_h = op.src_height;
        cfg.in_.block_w = op.block_width;
        cfg.in_.block_h = op.block_height;
        cfg.in_.block_offset_x = op.src_x;
        cfg.in_.block_offset_y = op.src_y;
        cfg.in_.__bindgen_anon_1.srm_cm = op.src_format.srm();
        cfg.out.buffer = op.dst.cast();
        cfg.out.buffer_size = op.dst_size;
        cfg.out.pic_w = op.dst_width;
        cfg.out.pic_h = op.dst_height;
        cfg.out.block_offset_x = op.dst_x;
        cfg.out.block_offset_y = op.dst_y;
        cfg.out.__bindgen_anon_1.srm_cm = op.format.srm();
        cfg.rotation_angle = op.rotation.raw();
        cfg.scale_x = op.scale;
        cfg.scale_y = op.scale;
        cfg.mode = sys::ppa_trans_mode_t_PPA_TRANS_MODE_NON_BLOCKING;
        cfg.user_data = self.client.done.cast();
        Error::check(unsafe { sys::ppa_do_scale_rotate_mirror(self.client.client, &cfg) })?;
        self.pending = true;
        Ok(())
    }

    pub fn wait(&mut self) {
        if self.pending {
            self.client.take(MAX_DELAY);
            self.pending = false;
        }
    }
}

pub struct Blend {
    client: Client,
}

impl Blend {
    pub fn new() -> Result<Self> {
        Ok(Self { client: Client::new(sys::ppa_operation_t_PPA_OPERATION_BLEND)? })
    }

    /// An ARGB8888 picture laid over a block of a picture, in place.
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
        cfg.user_data = self.client.done.cast();
        Error::check(unsafe { sys::ppa_do_blend(self.client.client, &cfg) })?;
        Ok(self.client.take(ticks(timeout)))
    }
}
