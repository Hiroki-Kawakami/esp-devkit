/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

//! The per-pixel waveform engine of a direct-drive EPD, apart from the bus.
//!
//! A pixel is a `u16`: `[15:14]` waveform slot, `[13:8]` start frame mod 64,
//! `[7:4]` its DC, `[3:0]` the target gray. The high byte (`b1`) is its life
//! cycle: [`B1_IDLE`], [`B1_PENDING`] (drawn, not yet armed), or replaying
//! its slot at step `(frame - start) & 63`. Step 63 is armed but not started,
//! so a pixel armed during a scan starts on the next frame whole.
//!
//! The DC is what the drives have left on the pixel, a black frame +1 and a
//! white one -1, kept as `dc + 8` for `dc` in -8..=7. A replay reads it as it
//! was when the replay started, and leaves it where its frames take it; a
//! replay cut short leaves it where the frames so far took it, so a waveform
//! that cancels the DC it starts from keeps the glass balanced however it is
//! cut.
//!
//! A waveform is one [`Frame`] per step: 16 words indexed by `dc + 8`, each
//! holding a 2-bit action per target gray (0 hold, 1 black, 2 white). A frame
//! of [`SETTLE_WORD`]s drives nothing and may be cut short while a draw
//! waits; a frame of zeros drives nothing but always runs, and a draw never
//! stops a replay with one still ahead.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering};

use idf::heap::{self, Caps};
use idf::{Error, Result};

/// The scan reads pixels while draws write them.
pub(crate) type Pixel = AtomicU16;
pub(crate) type Frame = [u32; 16];

pub(crate) const SLOTS: usize = 4;
pub(crate) const STEP_MAX: usize = 62;
pub(crate) const B1_IDLE: u8 = 0xFF;
pub(crate) const B1_PENDING: u8 = 0xFE;
pub(crate) const SETTLE_WORD: u32 = 0xFFFF_FFFF;

/// The DC of a pixel nothing has driven yet.
pub(crate) const DC_ZERO: u16 = 8;

pub(crate) const B: u8 = 1;
pub(crate) const W: u8 = 2;

const fn dc_of(action: u32) -> i32 {
    match action {
        1 => 1,
        2 => -1,
        _ => 0,
    }
}

const fn word(actions: [u8; 16]) -> u32 {
    let mut w = 0;
    let mut to = 0;
    while to < 16 {
        w |= (actions[to] as u32) << (to * 2);
        to += 1;
    }
    w
}

/// A frame whose action depends only on the DC, -8 to 7 left to right.
pub(crate) const fn by_dc(actions: [u8; 16]) -> Frame {
    let mut frame = [0; 16];
    let mut dc = 0;
    while dc < 16 {
        frame[dc] = word([actions[dc]; 16]);
        dc += 1;
    }
    frame
}

/// A frame whose action depends only on the target gray.
pub(crate) const fn by_to(actions: [u8; 16]) -> Frame {
    [word(actions); 16]
}

/// Drives nothing and always runs.
pub(crate) const STOP: Frame = [0; 16];
/// Drives nothing and may be cut short.
pub(crate) const SETTLE: Frame = [SETTLE_WORD; 16];

pub(crate) static HOLD: Frame = [0; 16];

/// What the scan drives a `b1` with this frame. A pointer, as a table may be
/// replaced while one still points into it, unread.
pub(crate) type Rowset = *const Frame;

pub(crate) fn hold() -> Rowset {
    &HOLD
}

const fn action(frame: &Frame, dc: usize, to: usize) -> u32 {
    if frame[0] == SETTLE_WORD { 0 } else { (frame[dc] >> (to * 2)) & 3 }
}

/// Whether a table is usable: 1..=`STEP_MAX` frames, none mixing SETTLE words
/// with others, and from any DC toward any gray every step keeps the DC in
/// -8..=7.
pub(crate) const fn check(lut: &[Frame]) -> bool {
    if lut.is_empty() || lut.len() > STEP_MAX {
        return false;
    }
    let mut f = 0;
    while f < lut.len() {
        let settle = lut[f][0] == SETTLE_WORD;
        let mut i = 1;
        while i < 16 {
            if (lut[f][i] == SETTLE_WORD) != settle {
                return false;
            }
            i += 1;
        }
        f += 1;
    }
    let mut start = 0;
    while start < 16 {
        let mut to = 0;
        while to < 16 {
            let mut dc = start as i32;
            let mut f = 0;
            while f < lut.len() {
                dc += dc_of(action(&lut[f], start, to));
                if dc < 0 || dc > 15 {
                    return false;
                }
                f += 1;
            }
            to += 1;
        }
        start += 1;
    }
    true
}

/// Indexed by `dc << 4 | target`, the DC a pixel keeps.
pub(crate) type DcTab = [u8; 256];

/// Leaves the DC as it is.
pub(crate) static KEEP: DcTab = {
    let mut tab = [0; 256];
    let mut i = 0;
    while i < 256 {
        tab[i] = (i >> 4) as u8;
        i += 1;
    }
    tab
};

pub(crate) struct Waveform {
    lut: &'static [Frame],
    /// The first step of the tail of SETTLE frames; the length without one.
    first_skippable: usize,
    /// The steps run from which no STOP frame is ahead.
    cuttable_from: usize,
    /// After each count of steps, none to all, the DC of a pixel by the DC it
    /// started from and its target.
    dc: Vec<DcTab, Caps>,
}

impl Waveform {
    pub(crate) fn new(lut: &'static [Frame]) -> Result<Self> {
        if !check(lut) {
            return Err(Error::INVALID_ARG);
        }
        let mut first_skippable = lut.len();
        while first_skippable > 0 && lut[first_skippable - 1][0] == SETTLE_WORD {
            first_skippable -= 1;
        }
        let cuttable_from = lut.iter().rposition(|frame| *frame == STOP).map_or(0, |i| i + 1);
        let mut dc = heap::try_vec(lut.len() + 1, Caps::SPIRAM)?;
        dc.push(KEEP);
        for frame in lut {
            let mut next = dc[dc.len() - 1];
            for (i, value) in next.iter_mut().enumerate() {
                *value = (i32::from(*value) + dc_of(action(frame, i >> 4, i & 15))) as u8;
            }
            dc.push(next);
        }
        Ok(Self { lut, first_skippable, cuttable_from, dc })
    }

    #[allow(dead_code)]
    pub(crate) fn lut(&self) -> &'static [Frame] {
        self.lut
    }

    pub(crate) fn steps(&self) -> usize {
        self.lut.len()
    }

    /// The DCs once `steps` steps have run.
    pub(crate) fn dc_after(&self, steps: usize) -> &DcTab {
        &self.dc[steps.min(self.lut.len())]
    }
}

pub(crate) fn is_active(px: u16) -> bool {
    (px >> 8) < u16::from(B1_PENDING)
}

/// `b1` for a pixel armed while the engine is at `frame`. Slot 3 must not be
/// armed at frames 61 and 62: its `b1` would be one of the markers.
pub(crate) fn armed(slot: usize, frame: u8) -> u8 {
    (slot << 6) as u8 | (frame.wrapping_add(1) & 63)
}

/// The steps a pixel replaying as `b1` has run once the scan has sent
/// `frame`.
pub(crate) fn steps_run(frame: u8, b1: u8) -> usize {
    (usize::from(frame.wrapping_sub(b1) & 63) + 1) & 63
}

/// Whether a draw may stop a pixel replaying `replay` as `b1` once the scan
/// has sent `frame`, and the DCs it leaves; an empty slot leaves the DC as it
/// is.
pub(crate) fn cut_dc(replay: Option<&Waveform>, frame: u8, b1: u8) -> Option<&DcTab> {
    let Some(replay) = replay else { return Some(&KEEP) };
    let run = steps_run(frame, b1);
    (run >= replay.cuttable_from).then(|| replay.dc_after(run))
}

/// Stops a replaying `px` where `dc`, the DCs after the steps it has run,
/// leaves it, and draws `gray` from there.
pub(crate) fn cut_px(px: u16, gray: u8, dc: &DcTab) -> u16 {
    u16::from(B1_PENDING) << 8 | u16::from(dc[usize::from(px & 0xFF)]) << 4 | u16::from(gray)
}

/// `px` idle at `gray`, with the DC `end` leaves a drive toward it.
pub(crate) fn seed_px(px: u16, gray: u8, end: &DcTab) -> u16 {
    let dc = end[usize::from(px & 0xF0 | u16::from(gray))];
    0xFF00 | u16::from(dc) << 4 | u16::from(gray)
}

/// The grays of an L8 row.
pub(crate) fn grays_l8(row: &[u8]) -> impl Iterator<Item = u8> + '_ {
    row.iter().map(|g| g >> 4)
}

/// The grays of an L4 row, the first pixel in the high nibble.
pub(crate) fn grays_l4(row: &[u8]) -> impl Iterator<Item = u8> + '_ {
    row.iter().flat_map(|b| [b >> 4, b & 0x0F])
}

pub(crate) struct Drawn {
    pub px: u16,
    /// The pixel replays toward another gray; it is left as it was.
    pub conflict: bool,
    /// It went from idle to pending.
    pub now_nonidle: bool,
}

/// Stamps `gray` as the target of `px` unless it is already, never touching
/// one that replays.
pub(crate) fn draw_px(px: u16, gray: u8) -> Drawn {
    let gray = u16::from(gray);
    if is_active(px) {
        return Drawn { px, conflict: px & 15 != gray, now_nonidle: false };
    }
    if px & 15 == gray {
        return Drawn { px, conflict: false, now_nonidle: false };
    }
    let now_nonidle = (px >> 8) as u8 == B1_IDLE;
    Drawn { px: u16::from(B1_PENDING) << 8 | (px & 0x00F0) | gray, conflict: false, now_nonidle }
}

/// The DCs pixels finishing a frame keep, or null for those that go on. A
/// pointer, as `Rowset` is.
pub(crate) type Retire = *const DcTab;

/// For each `b1` at `frame`: the frame to drive, and how its pixels finish
/// with it. With `skip_settle`, pixels left with only SETTLE frames finish.
pub(crate) fn build_frame_tab(
    wf: &[Option<Waveform>; SLOTS],
    frame: u8,
    skip_settle: bool,
    rowset_tab: &mut [Rowset; 256],
    retire_tab: &mut [Retire; 256],
) {
    for b1 in 0..256 {
        let mut rowset = hold();
        let mut retire: Retire = core::ptr::null();
        if b1 < usize::from(B1_PENDING) {
            let step = usize::from(frame.wrapping_sub(b1 as u8) & 63);
            match &wf[b1 >> 6] {
                Some(w) if step < w.steps() => {
                    let row = &w.lut[step];
                    if row[0] != SETTLE_WORD {
                        rowset = row;
                    }
                    if step + 1 >= w.steps() || (skip_settle && step + 1 >= w.first_skippable) {
                        retire = w.dc_after(step + 1);
                    }
                }
                // An empty slot or a stale step goes back to idle.
                _ if step != 63 => retire = &KEEP,
                _ => {}
            }
        }
        rowset_tab[b1] = rowset;
        retire_tab[b1] = retire;
    }
}

fn pack(a: [u32; 4]) -> u8 {
    (a[0] << 6 | a[1] << 4 | a[2] << 2 | a[3]) as u8
}

fn get(px: &Pixel) -> u16 {
    px.load(Ordering::Relaxed)
}

/// Packs a row's actions 4 pixels a byte, the leftmost in the top bits, and
/// returns whether any drives. `dst` holds a byte per 4 pixels of `row`.
/// Unrolled by 8 so the table lookups overlap.
fn blit(row: &[Pixel], dst: &mut [u8], act: impl Fn(u16) -> u32) -> bool {
    let act = |px: &Pixel| act(get(px));
    let mut acc = 0;
    let mut chunks = row.chunks_exact(8);
    for (px, d) in (&mut chunks).zip(dst.chunks_exact_mut(2)) {
        let a = [act(&px[0]), act(&px[1]), act(&px[2]), act(&px[3]), act(&px[4]), act(&px[5]), act(&px[6]), act(&px[7])];
        d[0] = pack([a[0], a[1], a[2], a[3]]);
        d[1] = pack([a[4], a[5], a[6], a[7]]);
        acc |= a.iter().fold(0, |acc, a| acc | a);
    }
    let rest = chunks.remainder();
    if rest.len() == 4 {
        let a = [act(&rest[0]), act(&rest[1]), act(&rest[2]), act(&rest[3])];
        dst[row.len() / 4 - 1] = pack(a);
        acc |= a.iter().fold(0, |acc, a| acc | a);
    }
    acc != 0
}

/// `row.len()` is a multiple of 4, and every entry of `rowset_tab` a pixel of
/// it reaches points at a live frame.
pub(crate) unsafe fn blit_line(row: &[Pixel], rowset_tab: &[Rowset; 256], dst: &mut [u8]) -> bool {
    blit(row, dst, |v| {
        let v = usize::from(v);
        let rowset = unsafe { &*rowset_tab[v >> 8] };
        (rowset[(v >> 4) & 15] >> ((v & 15) * 2)) & 3
    })
}

/// Flattens a frame into an action per `dc << 4 | target`.
pub(crate) fn build_act256(rowset: &Frame, act: &mut [u8; 256]) {
    for (dc, &w) in rowset.iter().enumerate() {
        for to in 0..16 {
            act[dc << 4 | to] = ((w >> (to * 2)) & 3) as u8;
        }
    }
}

/// [`blit_line`] through a table from [`build_act256`] per `b1`.
pub(crate) fn blit_line_tab(row: &[Pixel], tabs: &[&[u8; 256]; 256], dst: &mut [u8]) -> bool {
    blit(row, dst, |v| u32::from(tabs[usize::from(v >> 8)][usize::from(v & 0xFF)]))
}

/// Puts the pixels that finish this frame back to idle with the DC their
/// replay left, and returns how many did. Every entry of `retire_tab` a pixel
/// of `row` reaches is null or points at a live table.
pub(crate) unsafe fn retire_row(row: &[Pixel], retire_tab: &[Retire; 256]) -> u16 {
    let mut freed = 0;
    for px in row {
        let v = get(px);
        let retire = retire_tab[usize::from(v >> 8)];
        if !retire.is_null() {
            let dc = unsafe { &*retire }[usize::from(v & 0xFF)];
            px.store(0xFF00 | u16::from(dc) << 4 | (v & 15), Ordering::Relaxed);
            freed += 1;
        }
    }
    freed
}

/// The byte of a line that drives every pixel alike, when `rowset` does.
pub(crate) fn rowset_uniform(rowset: &Frame) -> Option<u8> {
    let w = rowset[0];
    if !matches!(w, 0 | 0x5555_5555 | 0xAAAA_AAAA) || rowset.iter().any(|&r| r != w) {
        return None;
    }
    Some(w as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a4(a: u8, b: u8, c: u8, d: u8) -> u8 {
        a << 6 | b << 4 | c << 2 | d
    }

    fn px(dc: u16, target: u16, b1: u8) -> u16 {
        u16::from(b1) << 8 | dc << 4 | target
    }

    static LUT_Q: [Frame; 4] = [
        by_dc([B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W]),
        by_to([B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W]),
        SETTLE,
        SETTLE,
    ];

    static LUT_C: [Frame; 3] = [by_dc([B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), by_to([W; 16]), STOP];

    fn slots(q: Option<usize>, c: Option<usize>) -> [Option<Waveform>; SLOTS] {
        let mut wf = [const { None }; SLOTS];
        if let Some(i) = q {
            wf[i] = Some(Waveform::new(&LUT_Q).unwrap());
        }
        if let Some(i) = c {
            wf[i] = Some(Waveform::new(&LUT_C).unwrap());
        }
        wf
    }

    fn tables(wf: &[Option<Waveform>; SLOTS], frame: u8, skip: bool) -> ([Rowset; 256], [Retire; 256]) {
        let mut rowset = [hold(); 256];
        let mut retire = [core::ptr::null(); 256];
        build_frame_tab(wf, frame, skip, &mut rowset, &mut retire);
        (rowset, retire)
    }

    fn finish(row: &[Pixel], retire: &[Retire; 256]) -> u16 {
        unsafe { retire_row(row, retire) }
    }

    fn same(a: Rowset, b: &Frame) -> bool {
        core::ptr::eq(a, b)
    }

    fn line(row: &[Pixel], rowset: &[Rowset; 256], dst: &mut [u8]) -> bool {
        unsafe { blit_line(row, rowset, dst) }
    }

    fn cells<const N: usize>(v: [u16; N]) -> [Pixel; N] {
        v.map(AtomicU16::new)
    }

    fn values(row: &[Pixel]) -> Vec<u16> {
        row.iter().map(get).collect()
    }

    #[test]
    fn lut_helpers() {
        assert_eq!(word([B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, W]), 1 | 2 << 30);
        for i in 0..16 {
            assert_eq!(LUT_Q[2][i], SETTLE_WORD);
            assert_eq!(LUT_C[2][i], 0);
        }
        assert_eq!((LUT_Q[0][0] >> 30) & 3, u32::from(B));
        assert_eq!(LUT_Q[0][15] & 3, u32::from(W));
        assert_eq!(LUT_Q[1][7] & 3, u32::from(B));
        assert_eq!((LUT_Q[1][7] >> 30) & 3, u32::from(W));
        assert_eq!((LUT_Q[1][7] >> 10) & 3, 0);
    }

    #[test]
    fn grays() {
        assert_eq!(grays_l8(&[0x00, 0x1F, 0xF0, 0xFF]).collect::<Vec<_>>(), [0, 1, 15, 15]);
        assert_eq!(grays_l4(&[0x0F, 0xA5]).collect::<Vec<_>>(), [0, 15, 10, 5]);
    }

    #[test]
    fn waveform_new() {
        let q = Waveform::new(&LUT_Q).unwrap();
        assert_eq!((q.steps(), q.first_skippable), (4, 2));
        let c = Waveform::new(&LUT_C).unwrap();
        assert_eq!((c.steps(), c.first_skippable), (3, 3));
        assert!(Waveform::new(&[]).is_err());
        static LONG: [Frame; STEP_MAX + 1] = [STOP; STEP_MAX + 1];
        assert!(Waveform::new(&LONG).is_err());
        static MIXED: [Frame; 1] = [[SETTLE_WORD, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]];
        assert!(Waveform::new(&MIXED).is_err());
        static BELOW: [Frame; 1] = [by_to([W; 16])];
        assert!(Waveform::new(&BELOW).is_err());
        static ABOVE: [Frame; 2] = [by_dc([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, B]), STOP];
        assert!(!check(&ABOVE));
    }

    #[test]
    fn dc_follows_the_frames() {
        let q = Waveform::new(&LUT_Q).unwrap();
        assert_eq!(*q.dc_after(0), KEEP);
        assert_eq!(q.dc_after(1)[15 << 4], 14);
        assert_eq!(q.dc_after(1)[5], 1);
        assert_eq!(q.dc_after(1)[8 << 4 | 3], 8);
        assert_eq!(q.dc_after(2)[15 << 4], 15);
        assert_eq!(q.dc_after(2)[15 << 4 | 15], 13);
        assert_eq!(q.dc_after(4), q.dc_after(2));
        assert_eq!(q.dc_after(9), q.dc_after(4));
    }

    #[test]
    fn steps_run_count() {
        let b1 = armed(0, 10);
        assert_eq!(steps_run(10, b1), 0);
        assert_eq!(steps_run(11, b1), 1);
        assert_eq!(steps_run(12, b1), 2);
        assert_eq!(steps_run(12, 1 << 6 | b1), 2);
        let wrapped = armed(0, 61);
        assert_eq!(steps_run(62, wrapped), 1);
        assert_eq!(steps_run(0, wrapped), 3);
    }

    #[test]
    fn stop_ahead_holds_off_cuts() {
        let b1 = armed(0, 10);
        let q = Waveform::new(&LUT_Q).unwrap();
        assert!(core::ptr::eq(cut_dc(Some(&q), 10, b1).unwrap(), q.dc_after(0)));
        assert!(core::ptr::eq(cut_dc(Some(&q), 12, b1).unwrap(), q.dc_after(2)));

        let c = Waveform::new(&LUT_C).unwrap();
        assert!((10..13).all(|frame| cut_dc(Some(&c), frame, b1).is_none()));

        static LUT_S: [Frame; 3] = [by_dc([B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), STOP, by_to([W; 16])];
        let s = Waveform::new(&LUT_S).unwrap();
        assert!(cut_dc(Some(&s), 10, b1).is_none());
        assert!(cut_dc(Some(&s), 11, b1).is_none());
        assert!(core::ptr::eq(cut_dc(Some(&s), 12, b1).unwrap(), s.dc_after(2)));

        assert!(core::ptr::eq(cut_dc(None, 12, b1).unwrap(), &KEEP));
    }

    #[test]
    fn cut_and_seed() {
        let q = Waveform::new(&LUT_Q).unwrap();
        let b1 = armed(0, 10);
        let replaying = px(15, 0, b1);
        let cut = cut_px(replaying, 9, q.dc_after(steps_run(11, b1)));
        assert_eq!(cut, px(14, 9, B1_PENDING));
        assert_eq!(cut_px(replaying, 9, q.dc_after(steps_run(10, b1))), px(15, 9, B1_PENDING));

        assert_eq!(seed_px(px(DC_ZERO, 3, B1_IDLE), 0, q.dc_after(q.steps())), px(DC_ZERO + 1, 0, B1_IDLE));
        assert_eq!(seed_px(px(DC_ZERO, 3, B1_PENDING), 7, &KEEP), px(DC_ZERO, 7, B1_IDLE));
    }

    #[test]
    fn draw() {
        let idle9 = px(9, 9, B1_IDLE);
        let d = draw_px(idle9, 9);
        assert!(d.px == idle9 && !d.conflict && !d.now_nonidle);

        let d = draw_px(idle9, 4);
        assert!(d.px == px(9, 4, B1_PENDING) && !d.conflict && d.now_nonidle);

        let pend = px(9, 4, B1_PENDING);
        let d = draw_px(pend, 7);
        assert!(d.px == px(9, 7, B1_PENDING) && !d.conflict && !d.now_nonidle);
        let d = draw_px(pend, 4);
        assert!(d.px == pend && !d.conflict && !d.now_nonidle);

        let act = px(9, 4, armed(0, 10));
        assert!(is_active(act));
        let d = draw_px(act, 4);
        assert!(d.px == act && !d.conflict && !d.now_nonidle);
        let d = draw_px(act, 12);
        assert!(d.px == act && d.conflict && !d.now_nonidle);

        assert!(!is_active(idle9));
        assert!(!is_active(pend));
    }

    #[test]
    fn frame_tab() {
        let wf = slots(Some(0), Some(1));
        let (rowset, retire) = tables(&wf, 10, false);
        let idle = usize::from(B1_IDLE);
        let pending = usize::from(B1_PENDING);
        let q = wf[0].as_ref().unwrap();
        assert!(same(rowset[idle], &HOLD) && retire[idle].is_null());
        assert!(same(rowset[pending], &HOLD) && retire[pending].is_null());

        let a = usize::from(armed(0, 10));
        assert!(same(rowset[a], &HOLD) && retire[a].is_null());

        assert!(same(rowset[10], &LUT_Q[0]) && retire[10].is_null());
        assert!(same(rowset[9], &LUT_Q[1]) && retire[9].is_null());
        assert!(same(rowset[8], &HOLD) && retire[8].is_null());
        assert!(same(rowset[7], &HOLD) && core::ptr::eq(retire[7], q.dc_after(4)));

        let (rowset, retire) = tables(&wf, 10, true);
        assert!(retire[10].is_null());
        assert!(core::ptr::eq(retire[9], q.dc_after(2)));
        assert!(core::ptr::eq(retire[8], q.dc_after(3)));
        assert!(retire[1 << 6 | 10].is_null());
        assert!(same(rowset[1 << 6 | 9], &LUT_C[1]) && retire[1 << 6 | 9].is_null());
        assert!(same(rowset[1 << 6 | 8], &LUT_C[2]) && !retire[1 << 6 | 8].is_null());

        assert!(core::ptr::eq(retire[2 << 6 | 10], &KEEP) && same(rowset[2 << 6 | 10], &HOLD));
    }

    #[test]
    fn blit_packs_actions() {
        let wf = slots(Some(0), None);
        let (rowset, _) = tables(&wf, 0, false);
        let (s0, s1) = (0, 63);
        let row = cells([
            px(0, 15, s0),
            px(15, 0, s0),
            px(15, 0, s1),
            px(0, 15, s1),
            px(5, 9, s0),
            px(9, 4, B1_PENDING),
            px(9, 9, B1_IDLE),
            px(0, 15, armed(0, 0)),
        ]);
        let mut dst = [0xEE; 2];
        assert!(line(&row, &rowset, &mut dst));
        assert_eq!(dst, [a4(B, W, B, W), 0]);

        let idle = cells([px(9, 9, 0xFF), px(1, 1, 0xFF), px(15, 15, 0xFF), px(3, 3, 0xFF)]);
        assert!(!line(&idle, &rowset, &mut dst[..1]));
        assert_eq!(dst[0], 0);
    }

    #[test]
    fn blit_tab_matches_blit() {
        let wf = slots(Some(0), Some(1));
        let mut t = [0; 256];
        build_act256(&LUT_Q[0], &mut t);
        assert_eq!(t[15], B);
        assert_eq!(t[15 << 4], W);
        assert_eq!(t[5 << 4 | 9], 0);
        build_act256(&LUT_Q[1], &mut t);
        assert!(t[7 << 4] == B && t[7 << 4 | 15] == W);

        static HOLD256: [u8; 256] = [0; 256];
        let mut seed: u32 = 12345;
        for frame in 0..12 {
            let (rowset, _) = tables(&wf, frame, false);
            let mut seen: Vec<Rowset> = Vec::new();
            let mut pool: Vec<Box<[u8; 256]>> = Vec::new();
            let mut index = [usize::MAX; 256];
            for b1 in 0..256 {
                if same(rowset[b1], &HOLD) {
                    continue;
                }
                let j = match seen.iter().position(|&f| core::ptr::eq(f, rowset[b1])) {
                    Some(j) => j,
                    None => {
                        assert!(seen.len() < 16);
                        let mut act = Box::new([0; 256]);
                        build_act256(unsafe { &*rowset[b1] }, &mut act);
                        seen.push(rowset[b1]);
                        pool.push(act);
                        seen.len() - 1
                    }
                };
                index[b1] = j;
            }
            let tabs: [&[u8; 256]; 256] = core::array::from_fn(|b1| if index[b1] == usize::MAX { &HOLD256 } else { &*pool[index[b1]] });
            let row: [Pixel; 64] = core::array::from_fn(|_| {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                AtomicU16::new((seed >> 8) as u16)
            });
            let (mut d1, mut d2) = ([0; 16], [0; 16]);
            assert_eq!(line(&row, &rowset, &mut d1), blit_line_tab(&row, &tabs, &mut d2));
            assert_eq!(d1, d2);
        }
    }

    #[test]
    fn blit_tail_of_four() {
        let wf = slots(Some(0), None);
        let (rowset, _) = tables(&wf, 0, false);
        let mut row = [px(9, 9, B1_IDLE); 12];
        row[11] = px(0, 15, 0);
        let row = cells(row);
        let mut dst = [0xEE; 3];
        assert!(line(&row, &rowset, &mut dst));
        assert_eq!(dst, [0, 0, a4(0, 0, 0, B)]);
    }

    #[test]
    fn retire() {
        let wf = slots(Some(0), None);
        let (_, retire) = tables(&wf, 3, false);
        let (ending, running) = (0, 2);
        let row = cells([px(9, 9, B1_IDLE), px(15, 4, ending), px(15, 2, running), px(15, 7, ending)]);
        assert_eq!(finish(&row, &retire), 2);
        assert_eq!(values(&row), [px(9, 9, B1_IDLE), px(14, 4, B1_IDLE), px(15, 2, running), px(14, 7, B1_IDLE)]);
    }

    #[test]
    fn lifecycle_across_wrap() {
        let wf = slots(Some(0), None);
        let mut state = [px(15, 15, B1_IDLE); 8];
        let mut row_nonidle = 0;
        for p in &mut state[..4] {
            let d = draw_px(*p, 0);
            assert!(!d.conflict);
            *p = d.px;
            row_nonidle += u16::from(d.now_nonidle);
        }
        assert_eq!(row_nonidle, 4);

        let mut frame = 61u8;
        let b1 = armed(0, frame);
        assert_eq!(b1, 62);
        for p in &mut state[..4] {
            *p = (*p & 0xFF) | u16::from(b1) << 8;
        }
        let row = cells(state);

        let mut dst = [0; 2];
        let mut frames_driven = 0;
        for _ in 0..10 {
            if row_nonidle == 0 {
                break;
            }
            frame = (frame + 1) & 63;
            let (rowset, retire) = tables(&wf, frame, false);
            let driven = line(&row, &rowset, &mut dst);
            match frame {
                62 => assert!(driven && dst == [a4(W, W, W, W), 0]),
                63 => assert!(driven && dst == [a4(B, B, B, B), 0]),
                0 | 1 => assert!(!driven),
                _ => {}
            }
            frames_driven += u32::from(driven);
            row_nonidle -= finish(&row, &retire);
        }
        assert_eq!(frames_driven, 2);
        assert_eq!(row_nonidle, 0);
        let state = values(&row);
        assert!(state[..4].iter().all(|&p| p == px(15, 0, B1_IDLE)));
        assert!(state[4..].iter().all(|&p| p == px(15, 15, B1_IDLE)));

        let d = draw_px(state[0], 0);
        assert!(d.px == state[0] && !d.now_nonidle);
    }

    #[test]
    fn pixels_progress_independently() {
        let wf = slots(Some(0), None);
        let state = cells([px(15, 0, 1), px(15, 15, B1_IDLE), px(15, 15, B1_IDLE), px(15, 15, B1_IDLE)]);
        let mut dst = [0];

        let (rowset, retire) = tables(&wf, 1, false);
        assert!(line(&state, &rowset, &mut dst));
        assert_eq!(dst[0], a4(W, 0, 0, 0));
        state[2].store(px(15, 0, armed(0, 1)), Ordering::Relaxed);
        assert_eq!(finish(&state, &retire), 0);

        let (rowset, retire) = tables(&wf, 2, false);
        assert!(line(&state, &rowset, &mut dst));
        assert_eq!(dst[0], a4(B, 0, W, 0));
        finish(&state, &retire);

        let (rowset, retire) = tables(&wf, 3, false);
        assert!(!line(&state, &rowset, &mut dst) || dst[0] == a4(0, 0, B, 0));
        finish(&state, &retire);
        let (_, retire) = tables(&wf, 4, false);
        finish(&state, &retire);
        assert_eq!(get(&state[0]), px(15, 0, B1_IDLE));
        assert!(is_active(get(&state[2])));
        let (_, retire) = tables(&wf, 5, false);
        finish(&state, &retire);
        assert_eq!(get(&state[2]), px(15, 0, B1_IDLE));
    }

    #[test]
    fn settle_skip() {
        let wf = slots(Some(0), Some(1));
        let state = cells([px(15, 0, 1), px(15, 15, 1 << 6 | 1), px(15, 15, B1_IDLE), px(15, 15, B1_IDLE)]);

        let (_, retire) = tables(&wf, 1, true);
        assert_eq!(finish(&state, &retire), 0);
        let (_, retire) = tables(&wf, 2, true);
        assert_eq!(finish(&state, &retire), 1);
        assert_eq!(get(&state[0]), px(15, 0, B1_IDLE));
        assert!(is_active(get(&state[1])));
        let (_, retire) = tables(&wf, 3, true);
        assert_eq!(finish(&state, &retire), 1);
        assert_eq!(get(&state[1]), px(14, 15, B1_IDLE));

        let s2 = cells([px(15, 0, 1)]);
        let (_, retire) = tables(&wf, 2, false);
        assert_eq!(finish(&s2, &retire), 0);
    }

    #[test]
    fn uniform() {
        assert_eq!(rowset_uniform(&LUT_C[1]), Some(0xAA));
        assert_eq!(rowset_uniform(&LUT_C[2]), Some(0));
        assert_eq!(rowset_uniform(&HOLD), Some(0));
        assert_eq!(rowset_uniform(&LUT_Q[0]), None);
        assert_eq!(rowset_uniform(&LUT_Q[1]), None);
    }
}
