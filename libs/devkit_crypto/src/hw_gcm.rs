/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * AES-GCM on the AES accelerator, its DMA reading and writing the caller's
 * buffers in place.
 */

use core::alloc::{Allocator, Layout};
use core::ptr::{self, NonNull};
use core::sync::atomic::{Ordering, fence};

use idf::cache::{self, Flags};
use idf::heap::Caps;

use crate::aead::{NONCE, TAG};
use crate::psa::sys;
use crate::{Error, Result};

const LINE: usize = 64;
const BLOCK: usize = 16;
/// What one descriptor carries of the middle of a buffer, whole cache lines.
const CHUNK: usize = 4032;
/// Per direction: 64 KB of data, its two ends, the AAD and the lengths.
const DESCS: usize = (64 * 1024usize).div_ceil(CHUNK) + 4;
pub(crate) const AAD_MAX: usize = 64;

const OWNER_DMA: u32 = 1 << 31;
const SUC_EOF: u32 = 1 << 30;

#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct Desc {
    dw0: u32,
    buffer: *mut u8,
    next: *mut Desc,
}

const _: () = assert!(size_of::<Desc>() == size_of::<sys::crypto_dma_desc_t>());

/// What the DMA reads and writes besides the caller's buffers; the CPU goes
/// through the uncached alias only.
#[repr(C, align(64))]
struct Mem {
    input: [Desc; DESCS],
    output: [Desc; DESCS],
    aad: [u8; AAD_MAX],
    lengths: [u8; LINE],
    in_ends: [[u8; 2 * LINE]; 2],
    out_ends: [[u8; 2 * LINE]; 2],
}

pub(crate) struct Gcm {
    key: [u8; 32],
    key_len: usize,
    dma: NonNull<Mem>,
    cpu: *mut Mem,
}

// `Mem` is touched only while the accelerator is held.
unsafe impl Send for Gcm {}
unsafe impl Sync for Gcm {}

impl Gcm {
    pub fn new(key: &[u8]) -> Result<Self> {
        if !matches!(key.len(), 16 | 32) {
            return Err(Error::InvalidKey);
        }
        let block = (Caps::DMA | Caps::INTERNAL).allocate_zeroed(Layout::new::<Mem>()).map_err(|_| Error::NoMemory)?;
        let dma = block.cast::<Mem>();
        // Nothing cached of it may be written back over what the CPU writes
        // through the uncached alias from here on.
        unsafe { cache::msync(dma.as_ptr().cast(), size_of::<Mem>(), Flags::C2M | Flags::INVALIDATE) }.map_err(|_| Error::NoMemory)?;
        let mut stored = [0; 32];
        stored[..key.len()].copy_from_slice(key);
        Ok(Self { key: stored, key_len: key.len(), dma, cpu: cache::uncached(dma.as_ptr()) })
    }

    /// Encrypts the first `len` bytes of `buf` in place and writes the tag
    /// after them; `None` when the accelerator cannot take them.
    pub fn seal(&self, nonce: &[u8; NONCE], aad: &[u8], buf: &mut [u8], len: usize) -> Option<Result<()>> {
        let tag = match self.crypt(true, nonce, aad, buf.as_mut_ptr(), len)? {
            Ok(tag) => tag,
            Err(err) => return Some(Err(err)),
        };
        buf[len..len + TAG].copy_from_slice(&tag);
        Some(Ok(()))
    }

    /// Decrypts the ciphertext before the tag at the end of `buf` in place;
    /// the plaintext's length, or `None` when the accelerator cannot take it.
    pub fn open(&self, nonce: &[u8; NONCE], aad: &[u8], buf: &mut [u8]) -> Option<Result<usize>> {
        let len = buf.len() - TAG;
        let mut expected = [0; TAG];
        expected.copy_from_slice(&buf[len..]);
        let tag = match self.crypt(false, nonce, aad, buf.as_mut_ptr(), len)? {
            Ok(tag) => tag,
            Err(err) => return Some(Err(err)),
        };
        if !crate::ct::eq(&tag, &expected) {
            buf[..len].fill(0);
            return Some(Err(Error::Verify));
        }
        Some(Ok(len))
    }

    fn crypt(&self, encrypt: bool, nonce: &[u8; NONCE], aad: &[u8], data: *mut u8, len: usize) -> Option<Result<[u8; TAG]>> {
        let capable = |p: *const u8| unsafe { sys::esp_ptr_dma_capable(p.cast()) };
        if len == 0 || aad.len() > AAD_MAX || !capable(data) || !capable(data.wrapping_add(len - 1)) {
            return None;
        }
        unsafe { sys::esp_aes_acquire_hardware() };
        let _held = devkit::guard((), |()| unsafe { sys::esp_aes_release_hardware() });
        let (m, d) = unsafe { (&mut *self.cpu, self.dma.as_ptr()) };
        let pad = (BLOCK - len % BLOCK) % BLOCK;

        let aad_len = aad.len().next_multiple_of(BLOCK);
        m.aad[..aad.len()].copy_from_slice(aad);
        m.aad[aad.len()..aad_len].fill(0);
        m.lengths[..8].copy_from_slice(&(aad.len() as u64 * 8).to_be_bytes());
        m.lengths[8..16].copy_from_slice(&(len as u64 * 8).to_be_bytes());

        let mut input = Chain::new(m.input.as_mut_ptr(), unsafe { &raw mut (*d).input }.cast());
        let mut output = Chain::new(m.output.as_mut_ptr(), unsafe { &raw mut (*d).output }.cast());
        let in_ends = ends(&mut m.in_ends, unsafe { &raw mut (*d).in_ends });
        let out_ends = ends(&mut m.out_ends, unsafe { &raw mut (*d).out_ends });
        let built = unsafe {
            (aad.is_empty() || input.push(&raw mut (*d).aad as *mut u8, aad_len))
                && input.data(data, len, pad, in_ends, true).is_some()
                && input.push(&raw mut (*d).lengths as *mut u8, BLOCK)
        };
        let split = unsafe { output.data(data, len, pad, out_ends, false) };
        let (true, Some(split)) = (built, split) else { return None };
        input.finish();
        output.finish();
        fence(Ordering::SeqCst);

        if let Err(err) = unsafe { cache::msync(data, len, Flags::C2M | Flags::UNALIGNED) } {
            return Some(Err(Error::Psa(err.code())));
        }
        let mut tag = [0; TAG];
        unsafe {
            if usize::from(sys::aes_hal_setkey(self.key.as_ptr(), self.key_len, i32::from(encrypt))) != self.key_len {
                return Some(Err(Error::InvalidKey));
            }
            sys::aes_hal_mode_init(sys::esp_aes_mode_t_ESP_AES_BLOCK_MODE_GCM);
            sys::aes_ll_interrupt_enable(false);
            sys::aes_hal_gcm_init(aad.len().div_ceil(BLOCK), (len * 8) % 128);
            let mut h = [0; BLOCK];
            sys::aes_hal_gcm_calc_hash(h.as_mut_ptr());
            let mut j0 = [0; BLOCK];
            j0[..NONCE].copy_from_slice(nonce);
            j0[BLOCK - 1] = 1;
            sys::aes_ll_gcm_set_j0(j0.as_ptr());
            let started = sys::esp_crypto_shared_gdma_start_axi_ahb(
                input.dma.cast(),
                output.dma.cast(),
                sys::crypto_dma_user_t_CRYPTO_DMA_USER_AES,
            );
            if started != 0 {
                return Some(Err(Error::Psa(started)));
            }
            sys::aes_hal_transform_dma_gcm_start(len.div_ceil(BLOCK));
            sys::aes_hal_wait_done();
            sys::esp_crypto_shared_gdma_done();
            let _ = cache::msync(data.add(split.head), split.mid, Flags::M2C);
            sys::aes_hal_transform_dma_finish();
            sys::aes_hal_gcm_read_tag(tag.as_mut_ptr(), TAG);
            ptr::copy_nonoverlapping(m.out_ends[0].as_ptr(), data, split.head);
            ptr::copy_nonoverlapping(m.out_ends[1].as_ptr(), data.add(split.head + split.mid), split.tail);
        }
        Some(Ok(tag))
    }
}

impl Drop for Gcm {
    fn drop(&mut self) {
        crate::ct::zeroize(&mut self.key);
        unsafe { (Caps::DMA | Caps::INTERNAL).deallocate(self.dma.cast(), Layout::new::<Mem>()) };
    }
}

type Ends = [(*mut u8, *mut u8); 2];

fn ends(cpu: &mut [[u8; 2 * LINE]; 2], dma: *mut [[u8; 2 * LINE]; 2]) -> Ends {
    let dma = dma.cast::<[u8; 2 * LINE]>();
    [(cpu[0].as_mut_ptr(), dma.cast()), (cpu[1].as_mut_ptr(), dma.wrapping_add(1).cast())]
}

/// How a buffer meets cache lines: an unaligned start, whole lines, and the
/// rest.
struct Split {
    head: usize,
    mid: usize,
    tail: usize,
}

struct Chain {
    cpu: *mut Desc,
    dma: *mut Desc,
    count: usize,
}

impl Chain {
    fn new(cpu: *mut Desc, dma: *mut Desc) -> Self {
        Self { cpu, dma, count: 0 }
    }

    unsafe fn push(&mut self, buffer: *mut u8, len: usize) -> bool {
        if self.count == DESCS {
            return false;
        }
        unsafe {
            if let Some(last) = self.count.checked_sub(1) {
                (*self.cpu.add(last)).next = self.dma.add(self.count);
            }
            *self.cpu.add(self.count) = Desc { dw0: len as u32 | (len as u32) << 12 | OWNER_DMA, buffer, next: ptr::null_mut() };
        }
        self.count += 1;
        true
    }

    fn finish(&mut self) {
        if let Some(last) = self.count.checked_sub(1) {
            unsafe { (*self.cpu.add(last)).dw0 |= SUC_EOF };
        }
    }

    /// `len` bytes at `data` and `pad` zero bytes after them, what is not on
    /// whole cache lines through `ends`; `fill` copies those ends in first.
    unsafe fn data(&mut self, data: *mut u8, len: usize, pad: usize, ends: Ends, fill: bool) -> Option<Split> {
        let head = ((LINE - data as usize % LINE) % LINE).min(len);
        let mid = (len - head) / LINE * LINE;
        let split = Split { head, mid, tail: len - head - mid };
        let (head_dma, tail_dma) = if mid + split.tail == 0 { (head + pad, 0) } else { (head, split.tail + pad) };
        unsafe {
            if fill {
                ptr::copy_nonoverlapping(data, ends[0].0, head);
                ptr::write_bytes(ends[0].0.add(head), 0, head_dma - head);
                ptr::copy_nonoverlapping(data.add(head + mid), ends[1].0, split.tail);
                ptr::write_bytes(ends[1].0.add(split.tail), 0, tail_dma.saturating_sub(split.tail));
            }
            if head_dma > 0 && !self.push(ends[0].1, head_dma) {
                return None;
            }
            let mut at = head;
            while at < head + mid {
                let n = CHUNK.min(head + mid - at);
                if !self.push(data.add(at), n) {
                    return None;
                }
                at += n;
            }
            if tail_dma > 0 && !self.push(ends[1].1, tail_dma) {
                return None;
            }
        }
        Some(split)
    }
}
