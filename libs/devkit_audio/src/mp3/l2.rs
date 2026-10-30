/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use super::tables::{L2_ALLOC_HIGH, L2_ALLOC_LOW, L2_ALLOC_LSF, L2_CLASSES, L2_INV_STEPS_Q32, L2_SCALE_Q29, L2Class};
use super::{FRAC, MODE_JOINT, State};
use crate::bits::Bits;

fn dequant(code: u32, c: &L2Class, inv: u32, scf: u8) -> i32 {
    let num = (2 * code + 1) as i32 - i32::from(c.steps);
    let f = ((i64::from(num) * i64::from(inv)) >> 2) as i32;
    ((i64::from(f) * i64::from(L2_SCALE_Q29[usize::from(scf)])) >> (59 - FRAC)) as i32
}

impl State {
    pub(super) fn l2_decode(&mut self, pcm: &mut [i16]) {
        let f = self.f;
        let bytes = u32::from(f.h.bytes);
        let mut b = Bits::new(&self.frame, if f.crc { 48 } else { 32 }, bytes * 8);
        let nch = usize::from(f.h.channels);

        let (rows, sblimit) = if f.version != 0 {
            (&L2_ALLOC_LSF[..], 30)
        } else {
            let kbps = if nch == 2 { f.kbps / 2 } else { f.kbps };
            if kbps < 56 {
                (&L2_ALLOC_LOW[..], if f.rate_index == 2 { 12 } else { 8 })
            } else {
                let limit = if kbps >= 96 && f.rate_index != 1 { 30 } else { 27 };
                (&L2_ALLOC_HIGH[..], limit)
            }
        };
        let joint = if f.mode == MODE_JOINT { 4 + 4 * usize::from(f.mode_ext) } else { sblimit };
        let bound = joint.min(sblimit);

        let mut alloc = [[0u8; 32]; 2];
        let mut scfsi = [[0u8; 32]; 2];
        let mut scf = [[[0u8; 3]; 32]; 2];
        let mut row = 0;
        let mut row_end = usize::from(rows[0].subbands);
        for sb in 0..sblimit {
            if sb == row_end {
                row += 1;
                row_end += usize::from(rows[row].subbands);
            }
            let r = &rows[row];
            for ch in 0..(if sb < bound { nch } else { 1 }) {
                let v = b.get(u32::from(r.nbal)) as usize;
                alloc[ch][sb] = if v != 0 { r.classes[v - 1] + 1 } else { 0 };
            }
            if sb >= bound {
                alloc[1][sb] = alloc[0][sb];
            }
        }
        for sb in 0..sblimit {
            for ch in 0..nch {
                if alloc[ch][sb] != 0 {
                    scfsi[ch][sb] = b.get(2) as u8;
                }
            }
        }
        for sb in 0..sblimit {
            for ch in 0..nch {
                if alloc[ch][sb] == 0 {
                    continue;
                }
                let s = &mut scf[ch][sb];
                s[0] = b.get(6) as u8;
                match scfsi[ch][sb] {
                    0 => {
                        s[1] = b.get(6) as u8;
                        s[2] = b.get(6) as u8;
                    }
                    1 => {
                        s[1] = s[0];
                        s[2] = b.get(6) as u8;
                    }
                    2 => {
                        s[1] = s[0];
                        s[2] = s[0];
                    }
                    _ => {
                        s[1] = b.get(6) as u8;
                        s[2] = s[1];
                    }
                }
            }
        }

        let mut pos = b.pos;
        let out_frame = 32 * self.channels;
        for gr in 0..12 {
            let part = gr >> 2;
            let mut b = Bits::new(&self.frame, pos, bytes * 8);
            for ch in 0..nch {
                for s in 0..3 {
                    self.sb[ch][s][sblimit..].fill(0);
                }
            }
            for sb in 0..sblimit {
                let mut codes = [[0u32; 3]; 2];
                let coded = if sb < bound { nch } else { 1 };
                for ch in 0..coded {
                    if alloc[ch][sb] == 0 {
                        continue;
                    }
                    let c = &L2_CLASSES[usize::from(alloc[ch][sb]) - 1];
                    let steps = u32::from(c.steps);
                    if c.grouped {
                        let mut v = b.get(u32::from(c.bits));
                        for s in 0..3 {
                            let q = if s < 2 { v % steps } else { v };
                            codes[ch][s] = q.min(steps - 1);
                            v /= steps;
                        }
                    } else {
                        for s in 0..3 {
                            codes[ch][s] = b.get(u32::from(c.bits));
                        }
                    }
                }
                if coded == 1 && nch == 2 {
                    codes[1] = codes[0];
                }
                for ch in 0..nch {
                    let a = usize::from(alloc[ch][sb]);
                    for s in 0..3 {
                        self.sb[ch][s][sb] = if a != 0 {
                            dequant(codes[ch][s], &L2_CLASSES[a - 1], L2_INV_STEPS_Q32[a - 1], scf[ch][sb][part])
                        } else {
                            0
                        };
                    }
                }
            }
            pos = b.pos;
            self.emit(3, &mut pcm[gr * 3 * out_frame..]);
        }
    }
}
