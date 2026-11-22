/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

#![no_std]

extern crate alloc;

mod text;

mod resources {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/resources.rs"));
}

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use core::ffi::CStr;
use core::time::Duration;

use bsp_rs::Board;
use bsp_rs::audio::Note;
use bsp_rs::display::{Display, Epd, EpdMode, Image, ImageFormat, Rect, Waveform};
use bsp_rs::power::{Battery, PowerState};
use bsp_rs::rtc::DateTime;
use bsp_rs::sd::MountConfig;
use bsp_rs::touch::{MAX_POINTS, Point};
use idf::fs::File;
use idf::log::{error, info};
use idf::thread;
use idf::time::Instant;

use text::{Canvas, Writer};

const TAG: &CStr = c"paper_rs";

const LEVELS: usize = 16;
const MARGIN: usize = 12;
const LEVELS_H: usize = 60;
const MARK: usize = 48;
const INFO_Y: usize = LEVELS_H + MARGIN;
const LINE_H: usize = 36;
const INFO_LINES: usize = 4;
const BUTTONS_Y: usize = INFO_Y + LINE_H * INFO_LINES + MARGIN;
const BUTTON_H: usize = 72;
const BORDER: usize = 2;
const PAD_Y: usize = BUTTONS_Y + BUTTON_H + MARGIN;
const DOT: usize = 8;
const UPDATE: Duration = Duration::from_secs(5);
const TOUCH_POLL: Duration = Duration::from_millis(10);
const OFF_HOLD: Duration = Duration::from_secs(2);
const APP_STACK: usize = 16 * 1024;

const fn note(hz: u16, ms: u16) -> Note {
    Note { hz, ms }
}

// A rising scale without rests: each note must follow the last whole.
const SCALE: [Note; 8] = [
    note(523, 120),
    note(587, 120),
    note(659, 120),
    note(698, 120),
    note(784, 120),
    note(880, 120),
    note(988, 120),
    note(1047, 240),
];
const BEEPS: [Note; 3] = [note(2000, 80), note(0, 120), note(2000, 80)];
const HOLD: [Note; 1] = [note(440, 0)];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Scale,
    Beeps,
    Hold,
    Stop,
    RtcForward,
    Sd,
    PowerOff,
}

const BUTTONS: [(Action, &str); 7] = [
    (Action::Scale, "Scale"),
    (Action::Beeps, "Beeps"),
    (Action::Hold, "440 Hz"),
    (Action::Stop, "Stop"),
    (Action::RtcForward, "RTC +1m"),
    (Action::Sd, "SD"),
    (Action::PowerOff, "Off 2 s"),
];

const SD_MOUNT: &CStr = c"/sdcard";
const SD_FILE: &CStr = c"/sdcard/paper_rs.txt";

struct App {
    board: Board,
    display: &'static Display,
    epd: Epd<'static>,
    writer: Writer,
    w: usize,
    h: usize,
    last: String,
}

// `bsp_rs::init` stays on the main task, whose default stack it must fit; the
// app draws text, which takes more.
pub fn run() {
    let board = match bsp_rs::init(&bsp_rs::Config::default()) {
        Ok(board) => board,
        Err(err) => {
            error!(TAG, "bsp_rs::init failed: {err}");
            return;
        }
    };
    if let Err(err) = thread::Builder::new().name("paper_rs").stack_size(APP_STACK).spawn(move || start(board)) {
        error!(TAG, "app task: {err}");
    }
}

fn start(board: Board) {
    let Some(display) = board.display else {
        info!(TAG, "no display");
        return;
    };
    let Some(epd) = display.epd() else {
        error!(TAG, "the display is not an EPD");
        return;
    };
    let size = display.size();
    info!(TAG, "display: {}x{} {:?}", size.width, size.height, display.pixel_format());
    let mut app = App {
        board,
        display,
        epd,
        writer: Writer::new(),
        w: usize::from(size.width),
        h: usize::from(size.height),
        last: "-".to_string(),
    };
    app.log("set_mode", epd.set_mode(EpdMode::Hold));
    if let Some(imu) = app.board.imu {
        imu.set_orientation_enabled(true);
    }
    app.show_all();
    app.act(Action::Scale);
    app.run();
}

fn rect(x: usize, y: usize, w: usize, h: usize) -> Rect {
    Rect::new(x as i16, y as i16, w as i16, h as i16)
}

fn button_rect(i: usize, w: usize) -> (usize, usize) {
    let bw = (w - MARGIN * (BUTTONS.len() + 1)) / BUTTONS.len();
    (MARGIN + i * (bw + MARGIN), bw)
}

fn power_line(state: idf::Result<PowerState>) -> String {
    match state {
        Ok(PowerState { source, battery: Battery::Present(b) }) => {
            let mv = b.voltage_mv.map_or("?".to_string(), |mv| format!("{mv} mV"));
            let charging = match b.charging {
                Some(true) => "charging",
                Some(false) => "not charging",
                None => "charging unknown",
            };
            format!("Power: {source:?}, battery {}% {mv}, {charging}", b.level)
        }
        Ok(PowerState { source, battery }) => format!("Power: {source:?}, battery {battery:?}"),
        Err(err) => format!("Power: {err}"),
    }
}

fn rtc_line(board: &Board) -> String {
    let Some(rtc) = board.rtc else { return "RTC: none".to_string() };
    match rtc.is_valid().and_then(|valid| Ok((valid, rtc.now()?))) {
        Ok((valid, t)) => format!(
            "RTC: {}-{:02}-{:02} {:02}:{:02}:{:02} UTC{}",
            t.year,
            t.month,
            t.day,
            t.hour,
            t.minute,
            t.second,
            if valid { "" } else { " (lost power)" }
        ),
        Err(err) => format!("RTC: {err}"),
    }
}

fn imu_line(board: &Board) -> String {
    let Some(imu) = board.imu else { return "IMU: none".to_string() };
    match imu.read() {
        Ok(s) => format!(
            "IMU: accel {:+.2} {:+.2} {:+.2} g, pose {:?}",
            s.accel.x,
            s.accel.y,
            s.accel.z,
            imu.orientation()
        ),
        Err(err) => format!("IMU: {err}"),
    }
}

impl App {
    fn log(&self, what: &str, result: idf::Result<()>) {
        if let Err(err) = result {
            error!(TAG, "{what}: {err}");
        }
    }

    fn put(&self, x: usize, y: usize, canvas: &Canvas) -> Rect {
        let area = rect(x, y, canvas.w, canvas.h);
        let image = Image::from_pixels(ImageFormat::L8, canvas.w as u16, canvas.h as u16, canvas.w, &canvas.px);
        self.log("draw", unsafe { self.display.draw(area, &image) });
        area
    }

    // The 16 levels from black on the left, with a black mark at the top
    // right, on white, to show which way is up.
    fn draw_levels(&self) -> Rect {
        let mut canvas = Canvas::new(self.w, LEVELS_H, 0xFF);
        for i in 0..LEVELS {
            let (x0, x1) = (self.w * i / LEVELS, self.w * (i + 1) / LEVELS);
            canvas.fill(x0, 0, x1 - x0, LEVELS_H, (i * 0x11) as u8);
        }
        canvas.fill(self.w - MARK, 0, MARK, MARK, 0x00);
        self.put(0, 0, &canvas)
    }

    fn draw_info(&mut self) -> Rect {
        let lines = [
            power_line(self.board.power.read()),
            rtc_line(&self.board),
            imu_line(&self.board),
            format!("Last: {}", self.last),
        ];
        for line in &lines {
            info!(TAG, "{line}");
        }
        let mut canvas = Canvas::new(self.w, LINE_H * INFO_LINES, 0xFF);
        for (i, line) in lines.iter().enumerate() {
            self.writer.draw(&mut canvas, MARGIN, i * LINE_H, line, 0x00);
        }
        self.put(0, INFO_Y, &canvas)
    }

    fn draw_button(&mut self, i: usize, pressed: bool) -> Rect {
        let (x, bw) = button_rect(i, self.w);
        let (paper, ink) = if pressed { (0x00, 0xFF) } else { (0xFF, 0x00) };
        let mut canvas = Canvas::new(bw, BUTTON_H, 0x00);
        canvas.fill(BORDER, BORDER, bw - BORDER * 2, BUTTON_H - BORDER * 2, paper);
        let label = BUTTONS[i].1;
        let (tw, th) = self.writer.measure(label);
        self.writer.draw(&mut canvas, bw.saturating_sub(tw) / 2, BUTTON_H.saturating_sub(th) / 2, label, ink);
        self.put(x, BUTTONS_Y, &canvas)
    }

    fn draw_pad(&self) -> Rect {
        let mut canvas = Canvas::new(self.w, self.h - PAD_Y, 0xFF);
        canvas.fill(0, 0, self.w, BORDER, 0x00);
        self.put(0, PAD_Y, &canvas)
    }

    fn show_all(&mut self) {
        self.log("clear", self.epd.clear());
        self.draw_levels();
        self.draw_info();
        for i in 0..BUTTONS.len() {
            self.draw_button(i, false);
        }
        self.draw_pad();
        let full = rect(0, 0, self.w, self.h);
        self.log("refresh", self.epd.refresh(full, Waveform::Quality, true));
    }

    fn update_info(&mut self) {
        let area = self.draw_info();
        self.log("refresh", self.epd.refresh(area, Waveform::Text, false));
    }

    fn press(&mut self, i: usize, pressed: bool) {
        let area = self.draw_button(i, pressed);
        self.log("refresh", self.epd.refresh(area, Waveform::Fast, false));
    }

    fn act(&mut self, action: Action) {
        let name = BUTTONS.iter().find(|(a, _)| *a == action).map_or("?", |(_, n)| n);
        let ok = |r: idf::Result<()>| r.map(|()| "ok".to_string()).map_err(|e| e.to_string());
        let result = match (action, self.board.audio) {
            (Action::Scale | Action::Beeps | Action::Hold | Action::Stop, None) => Err("no audio".to_string()),
            (Action::Scale, Some(audio)) => ok(audio.tone(&SCALE)),
            (Action::Beeps, Some(audio)) => ok(audio.tone(&BEEPS)),
            (Action::Hold, Some(audio)) => ok(audio.tone(&HOLD)),
            (Action::Stop, Some(audio)) => {
                audio.stop_tone();
                Ok("ok".to_string())
            }
            (Action::RtcForward, _) => self.rtc_forward(),
            (Action::Sd, _) => self.sd_check(),
            (Action::PowerOff, _) => Err(format!("still on: {}", self.board.power.off())),
        };
        self.last = match result {
            Ok(text) | Err(text) => format!("{name}: {text}"),
        };
        info!(TAG, "{}", self.last);
        self.update_info();
    }

    fn rtc_forward(&self) -> Result<String, String> {
        let rtc = self.board.rtc.ok_or("no RTC")?;
        let now = rtc.now().map_err(|e| e.to_string())?;
        rtc.set(&DateTime::from_seconds(now.seconds() + 60)).map_err(|e| e.to_string())?;
        Ok("ok".to_string())
    }

    /// Mounts the card, writes a file and reads it back, then unmounts.
    fn sd_check(&self) -> Result<String, String> {
        let sd = self.board.sd.ok_or("no SD slot")?;
        if !sd.is_mounted() {
            sd.mount(SD_MOUNT, &MountConfig::default()).map_err(|e| format!("mount: {e}"))?;
        }
        let result = (|| {
            let text = format!("paper_rs {}\n", rtc_line(&self.board));
            File::create(SD_FILE).and_then(|f| f.write_all(text.as_bytes())).map_err(|e| format!("write: {e}"))?;
            let mut back = vec![0; text.len()];
            let n = File::open(SD_FILE).and_then(|f| f.read(&mut back)).map_err(|e| format!("read: {e}"))?;
            if back[..n] != *text.as_bytes() {
                return Err("read back differs".to_string());
            }
            let space = sd.space().map_err(|e| format!("space: {e}"))?;
            Ok(format!("{} of {} MB free", space.free >> 20, space.total >> 20))
        })();
        let unmounted = sd.unmount().map_err(|e| format!("unmount: {e}"));
        let free = result?;
        unmounted?;
        Ok(format!("wrote and read back, {free}"))
    }

    fn dot(&self, p: &Point) {
        let (x, y) = (usize::from(p.x), usize::from(p.y));
        if y < PAD_Y + BORDER + DOT / 2 {
            return;
        }
        let x = x.saturating_sub(DOT / 2).min(self.w - DOT);
        let y = (y - DOT / 2).min(self.h - DOT);
        let area = self.put(x, y, &Canvas::new(DOT, DOT, 0x00));
        self.log("refresh", self.epd.refresh(area, Waveform::Fast, false));
    }

    fn button_at(&self, p: &Point) -> Option<usize> {
        let (x, y) = (usize::from(p.x), usize::from(p.y));
        if !(BUTTONS_Y..BUTTONS_Y + BUTTON_H).contains(&y) {
            return None;
        }
        (0..BUTTONS.len()).find(|&i| {
            let (bx, bw) = button_rect(i, self.w);
            (bx..bx + bw).contains(&x)
        })
    }

    // Two fingers clear the glass and draw everything again.
    fn run(&mut self) -> ! {
        let mut points = [Point::default(); MAX_POINTS];
        let mut last = 0;
        let mut held: Option<(usize, Instant)> = None;
        let mut updated = Instant::now();
        loop {
            let n = self.board.touch.map_or(0, |touch| touch.read(&mut points));
            if n >= 2 && last < 2 {
                info!(TAG, "touch: two fingers, redrawing");
                self.show_all();
            } else if n == 1 && last == 0 {
                info!(TAG, "touch: at {},{}", points[0].x, points[0].y);
                match self.button_at(&points[0]) {
                    Some(i) => {
                        self.press(i, true);
                        if BUTTONS[i].0 == Action::PowerOff {
                            held = Some((i, Instant::now()));
                        } else {
                            self.act(BUTTONS[i].0);
                            self.press(i, false);
                        }
                    }
                    None => self.dot(&points[0]),
                }
            } else if n == 1 {
                match held {
                    Some((i, since)) if since.elapsed() >= OFF_HOLD => {
                        held = None;
                        self.act(Action::PowerOff);
                        self.press(i, false);
                    }
                    Some(_) => {}
                    None => self.dot(&points[0]),
                }
            } else if n == 0 && let Some((i, _)) = held.take() {
                self.last = "Off: let go too early".to_string();
                self.press(i, false);
                self.update_info();
            }
            last = n;
            if updated.elapsed() >= UPDATE {
                updated = Instant::now();
                self.update_info();
            }
            thread::sleep(TOUCH_POLL);
        }
    }
}
