/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The debug host: owns the link to one firmware (a simulator process or a
 * board's serial port), keeps its log, and runs harness script commands for
 * any number of clients (docs/devkit-cli.md).
 */

pub mod decode;
pub mod serial;
pub mod sim;

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use jpeg_encoder::{ColorType, Encoder};

use decode::Decoder;
use serial::Serial;
use sim::Device;

pub type Sink = Arc<dyn Fn(String) + Send + Sync>;

const LOG_LINES: usize = 2000;
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const SNAP_TIMEOUT: Duration = Duration::from_secs(30);
pub const READY_TIMEOUT: Duration = Duration::from_secs(20);
const TOUCH_HOLD: Duration = Duration::from_millis(80);
const DEFAULT_QUALITY: u8 = 90;
const IMU_POSES: [(&str, &str); 6] = [
    ("rot0", "0 -1 0"),
    ("rot90", "-1 0 0"),
    ("rot180", "0 1 0"),
    ("rot270", "1 0 0"),
    ("face-up", "0 0 -1"),
    ("face-down", "0 0 1"),
];

pub enum Link {
    Sim(Arc<Device>),
    Serial(Arc<Serial>),
}

impl Link {
    fn send_line(&self, line: &str) {
        match self {
            Self::Sim(d) => d.send_line(line),
            Self::Serial(s) => s.send_line(line),
        }
    }

    fn kind(&self) -> String {
        match self {
            Self::Sim(_) => "sim".into(),
            Self::Serial(s) => format!("serial:{}", s.path().display()),
        }
    }
}

pub struct Session {
    out: Mutex<Box<dyn Write + Send>>,
    log: AtomicBool,
    color: bool,
}

impl Session {
    pub fn new(out: Box<dyn Write + Send>, log: bool, color: bool) -> Arc<Self> {
        Arc::new(Self { out: Mutex::new(out), log: AtomicBool::new(log), color })
    }

    fn line(&self, line: &str) -> bool {
        let line = if self.color { colorize(line) } else { line.into() };
        let mut out = self.out.lock().unwrap();
        writeln!(out, "{line}").and_then(|_| out.flush()).is_ok()
    }

    fn reply(&self, line: impl AsRef<str>) {
        self.line(&format!("#{}", line.as_ref()));
    }
}

#[derive(PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

pub struct Host {
    link: Link,
    label: String,
    log: Mutex<VecDeque<String>>,
    sessions: Mutex<Vec<Arc<Session>>>,
    firmware: Mutex<()>,
    waiter: Mutex<Option<Sender<String>>>,
    decoder: Mutex<Option<Decoder>>,
    socket: Mutex<Option<PathBuf>>,
    me: Mutex<Weak<Host>>,
    held: Mutex<Option<Vec<String>>>,
}

impl Host {
    pub fn new(link: Link, label: String, decoder: Option<Decoder>) -> Arc<Self> {
        let host = Arc::new(Self {
            link,
            label,
            log: Mutex::new(VecDeque::new()),
            sessions: Mutex::new(Vec::new()),
            firmware: Mutex::new(()),
            waiter: Mutex::new(None),
            decoder: Mutex::new(decoder),
            socket: Mutex::new(None),
            me: Mutex::new(Weak::new()),
            held: Mutex::new(None),
        });
        *host.me.lock().unwrap() = Arc::downgrade(&host);
        let me = host.clone();
        let sink: Sink = Arc::new(move |line| me.on_firmware_line(line));
        match &host.link {
            Link::Sim(d) => d.set_sink(sink),
            Link::Serial(s) => s.set_sink(sink),
        }
        host
    }

    pub fn device(&self) -> Option<&Arc<Device>> {
        match &self.link {
            Link::Sim(d) => Some(d),
            Link::Serial(_) => None,
        }
    }

    pub fn log_lines(&self) -> Vec<String> {
        self.log.lock().unwrap().iter().cloned().collect()
    }

    fn on_firmware_line(&self, line: String) {
        if line.starts_with('#') {
            if let Some(tx) = self.waiter.lock().unwrap().as_ref() {
                let _ = tx.send(line);
                return;
            }
        }
        let feed = self.decoder.lock().unwrap().as_mut().map(|d| d.feed(&line));
        let Some(feed) = feed else {
            self.show(line);
            return;
        };
        if feed.show {
            self.show(line);
        }
        for line in feed.extra {
            self.show(line);
        }
        if feed.answer {
            self.link.send_line("");
        }
        if let Some(job) = feed.job {
            let Some(me) = self.me.lock().unwrap().upgrade() else { return };
            *self.held.lock().unwrap() = Some(Vec::new());
            thread::spawn(move || {
                for line in job() {
                    me.log_line(line);
                }
                let mut held = me.held.lock().unwrap();
                for line in held.take().unwrap_or_default() {
                    me.log_line(line);
                }
            });
        }
    }

    // Lines arriving during a decode wait for its result, which belongs before
    // them: the board has usually rebooted by the time gdb answers.
    fn show(&self, line: String) {
        let mut held = self.held.lock().unwrap();
        match held.as_mut() {
            Some(lines) => lines.push(line),
            None => {
                drop(held);
                self.log_line(line);
            }
        }
    }

    fn log_line(&self, line: String) {
        {
            let mut log = self.log.lock().unwrap();
            if log.len() == LOG_LINES {
                log.pop_front();
            }
            log.push_back(line.clone());
        }
        self.sessions.lock().unwrap().retain(|s| !s.log.load(Ordering::Relaxed) || s.line(&line));
    }

    pub fn attach(&self, session: Arc<Session>) {
        self.sessions.lock().unwrap().push(session);
    }

    pub fn detach(&self, session: &Arc<Session>) {
        self.sessions.lock().unwrap().retain(|s| !Arc::ptr_eq(s, session));
    }

    /// Sends `line` to the firmware and returns its `#` reply lines (several
    /// for `snap`), without the `#`.
    fn firmware(&self, line: &str, timeout: Duration) -> Result<Vec<String>, String> {
        let _turn = self.firmware.lock().unwrap();
        let (tx, rx) = mpsc::channel();
        *self.waiter.lock().unwrap() = Some(tx);
        self.link.send_line(line);
        let name = line.split_whitespace().next().unwrap_or("");
        let deadline = Instant::now() + timeout;
        let mut lines = Vec::new();
        let result = loop {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break Err(format!("{name}: no answer"));
            };
            let Ok(reply) = rx.recv_timeout(left) else { continue };
            let body = reply[1..].to_string();
            let done = body.starts_with("ERR")
                || body.starts_with("END")
                || (body.split_whitespace().take(2).eq(["OK", name]) && name != "snap");
            let ours = done || body.starts_with("D ") || body.split_whitespace().take(2).eq(["OK", name]);
            if ours {
                lines.push(body);
            } else {
                self.log_line(reply);
            }
            if done {
                break Ok(lines);
            }
        };
        *self.waiter.lock().unwrap() = None;
        result
    }

    pub fn wait_ready(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut token = 0;
        while Instant::now() < deadline {
            if self.device().is_some_and(|d| d.ready()) {
                return Ok(());
            }
            token += 1;
            if self.firmware(&format!("ping {token}"), Duration::from_secs(1)).is_ok() {
                return Ok(());
            }
        }
        Err("firmware did not come up".into())
    }

    fn reset(&self) -> Result<(), String> {
        match &self.link {
            Link::Sim(d) => d.start().map_err(|e| e.to_string())?,
            Link::Serial(s) => s.reset().map_err(|e| e.to_string())?,
        }
        thread::sleep(Duration::from_millis(200));
        self.wait_ready(READY_TIMEOUT)
    }

    /// Runs one script line for `session`; the reply is always a single
    /// `#OK`/`#ERR` line, after any data it produces.
    pub fn execute(&self, line: &str, session: &Session) -> Flow {
        let args: Vec<&str> = line.split_whitespace().collect();
        let Some(&cmd) = args.first() else { return Flow::Continue };
        let rest = &args[1..];
        let result = match cmd {
            "wait" => match rest.first().and_then(|a| a.parse().ok()) {
                Some(ms) => {
                    thread::sleep(Duration::from_millis(ms));
                    Ok("OK wait".to_string())
                }
                None => Err("usage: wait <ms>".to_string()),
            },
            "settle" => self.settle(rest.first().and_then(|a| a.parse().ok()).unwrap_or(5000)),
            "capture" => match rest.first() {
                Some(path) => self.capture(Path::new(path), rest.get(1).and_then(|q| q.parse().ok()).unwrap_or(0)),
                None => Err("usage: capture <path> [quality]".to_string()),
            },
            "tap" | "down" | "move" => match parse_point(rest) {
                Some((x, y, id)) if cmd == "tap" => self
                    .touch(id, Some((x, y)))
                    .and_then(|_| {
                        thread::sleep(TOUCH_HOLD);
                        self.touch(id, None)
                    })
                    .map(|_| {
                        thread::sleep(TOUCH_HOLD);
                        "OK tap".to_string()
                    }),
                Some((x, y, id)) => self.touch(id, Some((x, y))).map(|_| format!("OK {cmd}")),
                None => Err(format!("usage: {cmd} <x> <y> [id]")),
            },
            "up" => self.touch(rest.first().and_then(|a| a.parse().ok()).unwrap_or(0), None).map(|_| "OK up".into()),
            "btn" => self.button(rest),
            "imu" => {
                let pose = (rest.len() == 1).then(|| IMU_POSES.iter().find(|(n, _)| *n == rest[0])).flatten();
                let args = pose.map(|(_, v)| v.to_string()).unwrap_or_else(|| rest.join(" "));
                self.forward(&format!("imu {args}"))
            }
            "reset" => self.reset().map(|_| "OK reset".into()),
            "log" => match rest.first() {
                Some(&"on") | Some(&"off") => {
                    session.log.store(rest[0] == "on", Ordering::Relaxed);
                    Ok("OK log".into())
                }
                _ => Err("usage: log on|off".into()),
            },
            "backlog" => {
                let lines = self.log_lines();
                let n = rest.first().and_then(|a| a.parse().ok()).unwrap_or(lines.len()).min(lines.len());
                for line in &lines[lines.len() - n..] {
                    session.line(line);
                }
                Ok("OK backlog".into())
            }
            "host" => Ok(format!("OK host {} {} {}", std::process::id(), self.link.kind(), self.label)),
            "port" => self.port(rest),
            "quit" => {
                session.reply("OK quit");
                return Flow::Quit;
            }
            _ => self.forward(line),
        };
        match result {
            Ok(reply) => session.reply(reply),
            Err(err) => session.reply(format!("ERR {cmd} {err}")),
        }
        Flow::Continue
    }

    fn forward(&self, line: &str) -> Result<String, String> {
        let replies = self.firmware(line, REPLY_TIMEOUT)?;
        let last = replies.last().cloned().unwrap_or_default();
        if last.starts_with("ERR") {
            return Err(last.trim_start_matches("ERR").trim().to_string());
        }
        Ok(last)
    }

    fn touch(&self, id: u8, at: Option<(u16, u16)>) -> Result<(), String> {
        match &self.link {
            Link::Sim(d) if d.present().is_some() => {
                d.touch(id, at);
                Ok(())
            }
            _ => {
                let line = match at {
                    Some((x, y)) => format!("down {id} {x} {y}"),
                    None => format!("up {id}"),
                };
                self.forward(&line).map(|_| ())
            }
        }
    }

    fn button(&self, args: &[&str]) -> Result<String, String> {
        let Some(id) = args.first() else { return Err("usage: btn <id> [click|down|up]".into()) };
        match args.get(1).copied().unwrap_or("click") {
            "click" => {
                self.forward(&format!("btn {id} down"))?;
                thread::sleep(TOUCH_HOLD);
                self.forward(&format!("btn {id} up"))?;
                thread::sleep(TOUCH_HOLD);
                Ok("OK btn".into())
            }
            action => self.forward(&format!("btn {id} {action}")).map(|_| "OK btn".into()),
        }
    }

    fn settle(&self, max_ms: u64) -> Result<String, String> {
        let deadline = Instant::now() + Duration::from_millis(max_ms);
        let mut quiet = 0;
        while Instant::now() < deadline {
            let idle = self.forward("idle")?;
            quiet = if idle.ends_with(" 1") { quiet + 1 } else { 0 };
            if quiet >= 3 {
                return Ok("OK settle 1".into());
            }
            thread::sleep(Duration::from_millis(50));
        }
        Ok("OK settle 0".into())
    }

    fn capture(&self, path: &Path, quality: u8) -> Result<String, String> {
        let quality = if quality == 0 { DEFAULT_QUALITY } else { quality.min(100) };
        let (width, height, jpeg) = match self.device().and_then(|d| d.frame()) {
            Some(frame) => {
                let mut jpeg = Vec::new();
                Encoder::new(&mut jpeg, quality)
                    .encode(&frame.rgb, frame.width as u16, frame.height as u16, ColorType::Rgb)
                    .map_err(|e| e.to_string())?;
                (frame.width, frame.height, jpeg)
            }
            None => self.firmware_snap(quality)?,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, &jpeg).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(format!("OK capture {width} {height} {}", jpeg.len()))
    }

    fn firmware_snap(&self, quality: u8) -> Result<(usize, usize, Vec<u8>), String> {
        let lines = self.firmware(&format!("snap {quality}"), SNAP_TIMEOUT)?;
        let mut size = (0, 0);
        let mut data = String::new();
        for line in &lines {
            let parts: Vec<&str> = line.split_whitespace().collect();
            match parts.as_slice() {
                ["OK", "snap", w, h] => size = (w.parse().unwrap_or(0), h.parse().unwrap_or(0)),
                ["D", chunk] => data.push_str(chunk),
                ["END", _] => {}
                _ => return Err(line.clone()),
            }
        }
        let jpeg = base64_decode(&data).ok_or("bad snap data")?;
        Ok((size.0, size.1, jpeg))
    }

    fn port(&self, args: &[&str]) -> Result<String, String> {
        let Link::Serial(s) = &self.link else { return Err("not a serial host".into()) };
        match args.first() {
            Some(&"release") => {
                s.release();
                Ok("OK port".into())
            }
            Some(&"acquire") => s.acquire().map(|_| "OK port".into()).map_err(|e| e.to_string()),
            _ => Err(format!("usage: port release|acquire (now {})", if s.held() { "held" } else { "released" })),
        }
    }

    pub fn socket(&self) -> Option<PathBuf> {
        self.socket.lock().unwrap().clone()
    }

    /// Accepts clients on `<dir>/<pid>.sock` until the process exits.
    pub fn serve(self: &Arc<Self>) -> io::Result<()> {
        let dir = socket_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        *self.socket.lock().unwrap() = Some(path);
        let me = self.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let host = me.clone();
                thread::spawn(move || host.client(stream));
            }
        });
        Ok(())
    }

    fn client(&self, stream: UnixStream) {
        let Ok(out) = stream.try_clone() else { return };
        let session = Session::new(Box::new(out), false, false);
        self.attach(session.clone());
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if self.execute(&line, &session) == Flow::Quit {
                break;
            }
        }
        self.detach(&session);
    }

    /// Reads commands from this process's stdin into `session`; `Quit` for
    /// `quit`, `Continue` at EOF.
    pub fn terminal(&self, session: &Arc<Session>) -> Flow {
        let attached = self.sessions.lock().unwrap().iter().any(|s| Arc::ptr_eq(s, session));
        if !attached {
            self.attach(session.clone());
        }
        let mut flow = Flow::Continue;
        for line in io::stdin().lock().lines().map_while(Result::ok) {
            if self.execute(&line, session) == Flow::Quit {
                flow = Flow::Quit;
                break;
            }
        }
        flow
    }

    pub fn shutdown(&self) {
        if let Some(path) = self.socket.lock().unwrap().take() {
            let _ = std::fs::remove_file(path);
        }
        if let Link::Sim(d) = &self.link {
            d.stop();
        }
    }
}

// Not TMPDIR: nix develop changes it, and clients inside and outside a dev
// shell must find the same hosts.
/// Colors a log line by its level, as `idf.py monitor` does.
pub fn colorize(line: &str) -> Cow<'_, str> {
    let color = match line.as_bytes().first() {
        Some(b'I') => "\x1b[0;32m",
        Some(b'W') => "\x1b[0;33m",
        Some(b'E') => "\x1b[0;31m",
        _ => return line.into(),
    };
    let stamp = line[1..].strip_prefix(" (").and_then(|r| r.split_once(')')).map(|(s, _)| s);
    if !stamp.is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || ":. -".contains(c))) {
        return line.into();
    }
    format!("{color}{line}\x1b[0m").into()
}

pub fn socket_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("esp-devkit"),
        None => PathBuf::from(format!("/tmp/esp-devkit-{}", unsafe { libc::getuid() })),
    }
}

fn parse_point(args: &[&str]) -> Option<(u16, u16, u8)> {
    let x = args.first()?.parse().ok()?;
    let y = args.get(1)?.parse().ok()?;
    let id = match args.get(2) {
        Some(a) => a.parse().ok()?,
        None => 0,
    };
    Some((x, y, id))
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let bytes = s.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        let mut v = 0u32;
        for &c in &chunk[..4 - pad] {
            v = v << 6 | value(c)?;
        }
        v <<= 6 * pad as u32;
        out.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8][..3 - pad]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{base64_decode, colorize, parse_point};

    #[test]
    fn colors_lines_with_a_log_prefix_only() {
        assert_eq!(colorize("E (12) tag: x"), "\x1b[0;31mE (12) tag: x\x1b[0m");
        assert_eq!(colorize("W (00:00:01.000) tag: x"), "\x1b[0;33mW (00:00:01.000) tag: x\x1b[0m");
        assert_eq!(colorize("I am fine (really)"), "I am fine (really)");
        assert_eq!(colorize("#OK tap"), "#OK tap");
    }

    #[test]
    fn base64_round_trips_padding() {
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
        assert!(base64_decode("Zm9").is_none());
    }

    #[test]
    fn points_take_an_optional_id() {
        assert_eq!(parse_point(&["10", "20"]), Some((10, 20, 0)));
        assert_eq!(parse_point(&["10", "20", "2"]), Some((10, 20, 2)));
        assert_eq!(parse_point(&["10"]), None);
    }
}
