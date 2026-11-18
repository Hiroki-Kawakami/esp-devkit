/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use super::audio::{self, Player};
use crate::protocol::{
    ENV_POWER, ENV_PSRAM_PATH, ENV_RTC, ENV_PSRAM_SIZE, ENV_SOCKET_FD, EXIT_RESTART, Event, MAX_TOUCH_POINTS, PixelFormat,
    RECORD_SIZE, TouchPoint, VERSION,
};

/// What the simulated device runs on; kept across firmware restarts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Supply {
    pub level: u8,
    pub external: bool,
    pub battery: bool,
}

pub const DEFAULT_SUPPLY: Supply = Supply { level: 76, external: true, battery: true };

// Gravity on the panel axes at each quarter turn clockwise, +1 g pointing up.
const REST_ACCEL: [[f32; 3]; 4] = [[0.0, -1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Running,
    Exited(Option<i32>),
    Off,
}

#[derive(Clone, Copy, Debug)]
pub struct Present {
    pub offset: u64,
    pub width: u16,
    pub height: u16,
    pub stride: u32,
    pub format: PixelFormat,
}

pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// RGB, 3 bytes per pixel.
    pub rgb: Vec<u8>,
}

/// The memory of a panel that keeps the picture itself, unturned.
struct Gram {
    width: usize,
    height: usize,
    format: PixelFormat,
    rotation: u8,
    bytes_per_second: u32,
    pixels: Vec<u8>,
}

impl Gram {
    /// `false` when the rectangle leaves the turned picture or `payload` is
    /// not its size.
    fn put(&mut self, (x, y, w, h): (usize, usize, usize, usize), payload: &[u8]) -> bool {
        let bpp = self.format.bytes();
        let (width, height) = if self.rotation % 2 == 1 { (self.height, self.width) } else { (self.width, self.height) };
        if w == 0 || h == 0 || x + w > width || y + h > height || payload.len() != w * h * bpp {
            return false;
        }
        let (last_x, last_y) = (self.width - 1, self.height - 1);
        for (j, row) in payload.chunks_exact(w * bpp).enumerate() {
            for (i, px) in row.chunks_exact(bpp).enumerate() {
                let (lx, ly) = (x + i, y + j);
                let (sx, sy) = match self.rotation % 4 {
                    0 => (lx, ly),
                    1 => (ly, last_y - lx),
                    2 => (last_x - lx, last_y - ly),
                    _ => (last_x - ly, lx),
                };
                let at = (sy * self.width + sx) * bpp;
                self.pixels[at..at + bpp].copy_from_slice(px);
            }
        }
        true
    }
}

struct State {
    status: Status,
    ready: bool,
    present: Option<Present>,
    gram: Option<Gram>,
    brightness: u8,
    name: Option<String>,
}

struct Run {
    pid: libc::pid_t,
    stdin: ChildStdin,
    sock: RawFd,
    net_stop: Arc<AtomicBool>,
    usb: RawFd,
}

struct Psram {
    ptr: *mut u8,
    len: usize,
    path: PathBuf,
    _file: File,
}

unsafe impl Send for Psram {}
unsafe impl Sync for Psram {}

impl Drop for Psram {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
        let _ = std::fs::remove_file(&self.path);
    }
}

pub struct Device {
    fw: PathBuf,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    psram: Psram,
    state: Mutex<State>,
    run: Mutex<Option<Run>>,
    generation: AtomicU64,
    contacts: Mutex<BTreeMap<u8, (u16, u16)>>,
    sink: Mutex<Option<super::Sink>>,
    player: Mutex<Option<Player>>,
    muted: AtomicBool,
    silent: bool,
    rotation: AtomicU8,
    supply: Mutex<Supply>,
    clock: Mutex<Clock>,
    usb_session: Mutex<Option<OwnedFd>>,
}

// The simulated RTC as an offset from the host clock.
struct Clock {
    offset: i64,
    valid: bool,
}

fn host_seconds() -> i64 {
    unsafe { libc::time(std::ptr::null_mut()) }
}

impl Device {
    pub fn new(fw: PathBuf, args: Vec<OsString>, env: Vec<(OsString, OsString)>, psram_size: usize) -> io::Result<Arc<Self>> {
        let silent = env.iter().any(|(key, _)| key == "SIMULATOR_HEADLESS");
        Ok(Arc::new(Self {
            fw,
            args,
            env,
            psram: map_psram(psram_size)?,
            state: Mutex::new(State {
                status: Status::Off,
                ready: false,
                present: None,
                gram: None,
                brightness: 0,
                name: None,
            }),
            run: Mutex::new(None),
            generation: AtomicU64::new(0),
            contacts: Mutex::new(BTreeMap::new()),
            sink: Mutex::new(None),
            player: Mutex::new(None),
            muted: AtomicBool::new(true),
            silent,
            rotation: AtomicU8::new(0),
            supply: Mutex::new(DEFAULT_SUPPLY),
            clock: Mutex::new(Clock { offset: 0, valid: std::env::var_os("SIMULATOR_RTC_INVALID").is_none_or(|v| v == "0") }),
            usb_session: Mutex::new(None),
        }))
    }

    /// Every line the firmware prints, stdout and stderr in order.
    pub fn set_sink(&self, sink: super::Sink) {
        *self.sink.lock().unwrap() = Some(sink);
    }


    pub fn start(self: &Arc<Self>) -> io::Result<()> {
        self.stop();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut fds = [0; 2];
        if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let (host_fd, fw_fd) = (fds[0], fds[1]);
        unsafe { libc::fcntl(host_fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        // One pipe for both, so log and harness lines keep the order a device
        // console would show.
        let mut out = [0; 2];
        if unsafe { libc::pipe(out.as_mut_ptr()) } != 0 {
            unsafe { libc::close(host_fd) };
            unsafe { libc::close(fw_fd) };
            return Err(io::Error::last_os_error());
        }
        unsafe { libc::fcntl(out[0], libc::F_SETFD, libc::FD_CLOEXEC) };
        let (out_read, out_write) = unsafe { (File::from_raw_fd(out[0]), OwnedFd::from_raw_fd(out[1])) };
        let err_write = out_write.try_clone()?;
        let (net_fd, fw_net_fd) = match super::net::socket_pair() {
            Ok(fds) => fds,
            Err(err) => {
                unsafe { libc::close(host_fd) };
                unsafe { libc::close(fw_fd) };
                return Err(err);
            }
        };
        let (usb_fd, fw_usb_fd) = match super::usb::socket_pair() {
            Ok(fds) => fds,
            Err(err) => {
                for fd in [host_fd, fw_fd, net_fd, fw_net_fd] {
                    unsafe { libc::close(fd) };
                }
                return Err(err);
            }
        };

        let spawned = Command::new(&self.fw)
            .args(&self.args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .env(ENV_SOCKET_FD.to_str().unwrap(), fw_fd.to_string())
            .env(super::net::ENV_NET_FD, fw_net_fd.to_string())
            .env(super::usb::ENV_USB_FD, fw_usb_fd.to_string())
            .env(ENV_PSRAM_PATH.to_str().unwrap(), &self.psram.path)
            .env(ENV_PSRAM_SIZE.to_str().unwrap(), self.psram.len.to_string())
            .env(ENV_RTC.to_str().unwrap(), {
                let clock = self.clock.lock().unwrap();
                format!("{} {}", host_seconds() + clock.offset, u8::from(clock.valid))
            })
            .env(ENV_POWER.to_str().unwrap(), {
                let Supply { level, external, battery } = self.supply();
                format!("{level} {} {}", u8::from(external), u8::from(battery))
            })
            .stdin(Stdio::piped())
            .stdout(Stdio::from(out_write))
            .stderr(Stdio::from(err_write))
            .spawn();
        unsafe { libc::close(fw_fd) };
        unsafe { libc::close(fw_net_fd) };
        unsafe { libc::close(fw_usb_fd) };
        let mut child = match spawned {
            Ok(child) => child,
            Err(err) => {
                for fd in [host_fd, net_fd, usb_fd] {
                    unsafe { libc::close(fd) };
                }
                return Err(err);
            }
        };
        let net_stop = Arc::new(AtomicBool::new(false));
        {
            let stop = net_stop.clone();
            thread::spawn(move || super::net::run(net_fd, stop));
        }

        {
            let mut state = self.state.lock().unwrap();
            state.status = Status::Running;
            state.ready = false;
            state.present = None;
            state.gram = None;
            state.brightness = 0;
        }
        let stdin = child.stdin.take().unwrap();
        *self.run.lock().unwrap() = Some(Run { pid: child.id() as libc::pid_t, stdin, sock: host_fd, net_stop, usb: usb_fd });

        let me = self.clone();
        thread::spawn(move || {
            for line in BufReader::new(out_read).lines().map_while(Result::ok) {
                if let Some(sink) = me.sink.lock().unwrap().clone() {
                    sink(line);
                }
            }
        });
        let me = self.clone();
        thread::spawn(move || me.receive(host_fd));
        let me = self.clone();
        thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code());
            if me.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            me.close_run();
            if code == Some(EXIT_RESTART) {
                if let Err(err) = me.start() {
                    eprintln!("[devkit] restart: {err}");
                }
                return;
            }
            me.state.lock().unwrap().status = Status::Exited(code);
        });
        Ok(())
    }

    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(run) = self.close_run() {
            unsafe { libc::kill(run.pid, libc::SIGKILL) };
        }
        let mut state = self.state.lock().unwrap();
        state.status = Status::Off;
        state.ready = false;
        state.present = None;
        state.gram = None;
    }

    fn close_run(&self) -> Option<Run> {
        *self.player.lock().unwrap() = None;
        self.muted.store(true, Ordering::Relaxed);
        let mut run = self.run.lock().unwrap();
        if let Some(session) = self.usb_session.lock().unwrap().take() {
            super::usb::unplug(session);
        }
        let run = run.take()?;
        run.net_stop.store(true, Ordering::Relaxed);
        unsafe { libc::shutdown(run.sock, libc::SHUT_RDWR) };
        unsafe { libc::close(run.usb) };
        Some(run)
    }

    fn receive(self: &Arc<Self>, fd: RawFd) {
        let (done, sent) = mpsc::channel::<(u32, Instant)>();
        let me = Arc::downgrade(self);
        thread::spawn(move || {
            for (ticket, due) in sent {
                thread::sleep(due.saturating_duration_since(Instant::now()));
                let Some(device) = me.upgrade() else { return };
                device.send(&Event::GramDone { ticket });
            }
        });
        let mut busy_until = Instant::now();
        let mut payload = Vec::new();
        let mut record = [0u8; RECORD_SIZE];
        while read_record(fd, &mut record) {
            let mut state = self.state.lock().unwrap();
            match Event::decode(&record) {
                Some(Event::Hello { version }) => {
                    if version != VERSION {
                        eprintln!("[devkit] firmware speaks protocol {version}, host {VERSION}");
                    }
                    state.ready = true;
                    drop(state);
                    self.send_pose();
                    self.send_supply();
                    continue;
                }
                Some(Event::Present { offset, width, height, stride, format }) => {
                    state.present = Some(Present { offset, width, height, stride, format })
                }
                Some(Event::Brightness { level }) => state.brightness = level,
                Some(Event::Device { name }) => {
                    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                    state.name = Some(String::from_utf8_lossy(&name[..end]).into_owned());
                }
                Some(Event::AudioOpen { stream, offset, size, sample_rate, sample_bytes, channels }) => {
                    let format = audio::Format { sample_rate, sample_bytes, channels };
                    self.open_audio(stream, offset, size, format);
                }
                Some(Event::AudioWrite { stream, end }) => {
                    if let Some(player) = self.player.lock().unwrap().as_ref().filter(|p| p.stream == stream) {
                        player.write(end);
                    }
                }
                Some(Event::AudioClose { stream }) => {
                    let mut player = self.player.lock().unwrap();
                    if player.as_ref().is_some_and(|p| p.stream == stream) {
                        *player = None;
                    }
                }
                Some(Event::Rtc { seconds, valid }) => {
                    *self.clock.lock().unwrap() = Clock { offset: seconds - host_seconds(), valid };
                }
                Some(Event::AudioMute { mute }) => {
                    self.muted.store(mute, Ordering::Relaxed);
                    if let Some(player) = self.player.lock().unwrap().as_ref() {
                        player.set_muted(mute);
                    }
                }
                Some(Event::GramOpen { width, height, format, rotation, bytes_per_second }) => {
                    let (width, height) = (usize::from(width), usize::from(height));
                    let pixels = vec![0; width * height * format.bytes()];
                    state.gram = Some(Gram { width, height, format, rotation, bytes_per_second, pixels });
                }
                Some(Event::GramRotate { rotation }) => {
                    if let Some(gram) = state.gram.as_mut() {
                        gram.rotation = rotation;
                    }
                }
                Some(Event::GramWrite { ticket, x, y, w, h, bytes }) => {
                    payload.resize(bytes as usize, 0);
                    if !read_bytes(fd, &mut payload) {
                        break;
                    }
                    let rect = (usize::from(x), usize::from(y), usize::from(w), usize::from(h));
                    let rate = match state.gram.as_mut() {
                        Some(gram) => {
                            if !gram.put(rect, &payload) {
                                eprintln!("[devkit] gram write {rect:?} of {bytes} bytes outside the panel");
                            }
                            gram.bytes_per_second
                        }
                        None => {
                            eprintln!("[devkit] gram write before the panel opened");
                            0
                        }
                    };
                    let start = busy_until.max(Instant::now());
                    busy_until = start + Duration::from_secs_f64(f64::from(bytes) / f64::from(rate.max(1)));
                    let _ = done.send((ticket, if rate == 0 { start } else { busy_until }));
                }
                Some(
                    Event::Touch { .. } | Event::AudioPlayed { .. } | Event::Imu { .. } | Event::Power { .. } | Event::GramDone { .. },
                )
                | None => {}
            }
        }
        unsafe { libc::close(fd) };
    }

    fn open_audio(self: &Arc<Self>, stream: u32, offset: u64, size: u32, format: audio::Format) {
        let (start, len) = (offset as usize, size as usize);
        let valid = len > 0 && format.sample_rate > 0 && matches!(format.sample_bytes, 2 | 4) && matches!(format.channels, 1 | 2);
        if !valid || start.checked_add(len).is_none_or(|end| end > self.psram.len) {
            eprintln!("[devkit] bad audio stream from the firmware");
            return;
        }
        let ring = audio::Ring { ptr: unsafe { self.psram.ptr.add(start) }, size: u64::from(size) };
        let device: Weak<Self> = Arc::downgrade(self);
        let report = move |bytes| {
            if let Some(device) = device.upgrade() {
                device.send(&Event::AudioPlayed { stream, bytes });
            }
        };
        let muted = self.muted.load(Ordering::Relaxed);
        *self.player.lock().unwrap() = Some(Player::start(stream, ring, format, muted, self.silent, report));
    }

    pub fn send(&self, event: &Event) {
        if let Some(run) = self.run.lock().unwrap().as_ref() {
            write_record(run.sock, &event.encode());
        }
    }

    pub fn send_line(&self, line: &str) {
        if let Some(run) = self.run.lock().unwrap().as_mut() {
            let _ = writeln!(run.stdin, "{line}").and_then(|_| run.stdin.flush());
        }
    }

    /// `None` releases the contact.
    pub fn touch(&self, id: u8, at: Option<(u16, u16)>) {
        let mut contacts = self.contacts.lock().unwrap();
        match at {
            Some(p) => contacts.insert(id, p),
            None => contacts.remove(&id),
        };
        let mut points = [TouchPoint::default(); MAX_TOUCH_POINTS];
        for (p, (&id, &(x, y))) in points.iter_mut().zip(contacts.iter()) {
            *p = TouchPoint { x, y, id };
        }
        let count = contacts.len().min(MAX_TOUCH_POINTS) as u8;
        drop(contacts);
        self.send(&Event::Touch { count, points });
    }

    /// Turns the simulated device clockwise by `rotation` quarter turns.
    pub fn set_rotation(&self, rotation: u8) {
        if self.rotation.swap(rotation, Ordering::Relaxed) != rotation {
            self.send_pose();
        }
    }

    fn send_pose(&self) {
        let accel = REST_ACCEL[usize::from(self.rotation.load(Ordering::Relaxed) % 4)];
        self.send(&Event::Imu { accel, gyro: [0.0; 3] });
    }

    pub fn supply(&self) -> Supply {
        *self.supply.lock().unwrap()
    }

    pub fn set_supply(&self, supply: Supply) {
        if std::mem::replace(&mut *self.supply.lock().unwrap(), supply) != supply {
            self.send_supply();
        }
    }

    fn send_supply(&self) {
        let Supply { level, external, battery } = self.supply();
        self.send(&Event::Power { level, external, battery });
    }

    /// Replaces the device plugged in before, if any.
    pub fn usb_plug(&self, session: OwnedFd, busid: &str) -> io::Result<()> {
        let run = self.run.lock().unwrap();
        let control = run.as_ref().ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "firmware not running"))?.usb;
        let mut current = self.usb_session.lock().unwrap();
        if let Some(old) = current.take() {
            super::usb::unplug(old);
        }
        super::usb::plug(control, &session, busid)?;
        *current = Some(session);
        Ok(())
    }

    /// `false` when nothing was plugged in.
    pub fn usb_unplug(&self) -> bool {
        let session = self.usb_session.lock().unwrap().take();
        session.map(super::usb::unplug).is_some()
    }

    pub fn status(&self) -> Status {
        self.state.lock().unwrap().status
    }

    pub fn ready(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.status == Status::Running && state.ready
    }

    pub fn brightness(&self) -> u8 {
        self.state.lock().unwrap().brightness
    }

    /// The board the firmware reported last, kept across restarts.
    pub fn name(&self) -> Option<String> {
        self.state.lock().unwrap().name.clone()
    }

    pub fn present(&self) -> Option<Present> {
        self.state.lock().unwrap().present
    }

    /// Whether the firmware shows a picture, from a framebuffer or a panel's
    /// own memory.
    pub fn has_panel(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.present.is_some() || state.gram.is_some()
    }

    /// The picture on the panel as RGB, unturned; the firmware may be drawing
    /// into a presented frame meanwhile.
    pub fn frame(&self) -> Option<Frame> {
        {
            let state = self.state.lock().unwrap();
            if let Some(gram) = &state.gram {
                return Some(to_rgb(&gram.pixels, gram.width, gram.height, gram.width * gram.format.bytes(), gram.format));
            }
        }
        let p = self.present()?;
        let (w, h, stride) = (usize::from(p.width), usize::from(p.height), p.stride as usize);
        let start = usize::try_from(p.offset).ok()?;
        if stride < w * p.format.bytes() || start.checked_add(stride * h)? > self.psram.len {
            return None;
        }
        let src = unsafe { std::slice::from_raw_parts(self.psram.ptr.add(start), stride * h) };
        Some(to_rgb(src, w, h, stride, p.format))
    }
}

fn to_rgb(src: &[u8], w: usize, h: usize, stride: usize, format: PixelFormat) -> Frame {
    let bpp = format.bytes();
    let mut rgb = Vec::with_capacity(w * h * 3);
    let from565 = |v: u16| {
        let (r, g, b) = ((v >> 11) & 0x1F, (v >> 5) & 0x3F, v & 0x1F);
        [(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (b << 3 | b >> 2) as u8]
    };
    for row in src.chunks_exact(stride).take(h) {
        for px in row[..w * bpp].chunks_exact(bpp) {
            match format {
                PixelFormat::L8 => rgb.extend_from_slice(&[px[0], px[0], px[0]]),
                PixelFormat::Rgb565 => rgb.extend_from_slice(&from565(u16::from_le_bytes([px[0], px[1]]))),
                PixelFormat::Rgb565Be => rgb.extend_from_slice(&from565(u16::from_be_bytes([px[0], px[1]]))),
                PixelFormat::Bgr888 => rgb.extend_from_slice(&[px[2], px[1], px[0]]),
            }
        }
    }
    Frame { width: w, height: h, rgb }
}

fn read_record(fd: RawFd, record: &mut [u8; RECORD_SIZE]) -> bool {
    read_bytes(fd, record)
}

fn read_bytes(fd: RawFd, buf: &mut [u8]) -> bool {
    let mut at = 0;
    while at < buf.len() {
        let n = unsafe { libc::recv(fd, buf[at..].as_mut_ptr().cast(), buf.len() - at, 0) };
        if n <= 0 {
            return false;
        }
        at += n as usize;
    }
    true
}

fn write_record(fd: RawFd, record: &[u8; RECORD_SIZE]) {
    let mut at = 0;
    while at < RECORD_SIZE {
        let n = unsafe { libc::send(fd, record[at..].as_ptr().cast(), RECORD_SIZE - at, 0) };
        if n <= 0 {
            return;
        }
        at += n as usize;
    }
}

fn map_psram(len: usize) -> io::Result<Psram> {
    let path = std::env::temp_dir().join(format!("devkit-psram-{}", std::process::id()));
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path)?;
    file.set_len(len as u64)?;
    let ptr = unsafe {
        libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, file.as_raw_fd(), 0)
    };
    if ptr == libc::MAP_FAILED {
        let err = io::Error::last_os_error();
        let _ = std::fs::remove_file(&path);
        return Err(err);
    }
    Ok(Psram { ptr: ptr.cast(), len, path, _file: file })
}

