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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::protocol::{
    ENV_PSRAM_PATH, ENV_PSRAM_SIZE, ENV_SOCKET_FD, EXIT_RESTART, Event, MAX_TOUCH_POINTS, PixelFormat,
    RECORD_SIZE, TouchPoint, VERSION,
};

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

struct State {
    status: Status,
    ready: bool,
    present: Option<Present>,
    brightness: u8,
    name: Option<String>,
}

struct Run {
    pid: libc::pid_t,
    stdin: ChildStdin,
    sock: RawFd,
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
}

impl Device {
    pub fn new(fw: PathBuf, args: Vec<OsString>, env: Vec<(OsString, OsString)>, psram_size: usize) -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            fw,
            args,
            env,
            psram: map_psram(psram_size)?,
            state: Mutex::new(State {
                status: Status::Off,
                ready: false,
                present: None,
                brightness: 0,
                name: None,
            }),
            run: Mutex::new(None),
            generation: AtomicU64::new(0),
            contacts: Mutex::new(BTreeMap::new()),
            sink: Mutex::new(None),
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

        let spawned = Command::new(&self.fw)
            .args(&self.args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .env(ENV_SOCKET_FD.to_str().unwrap(), fw_fd.to_string())
            .env(ENV_PSRAM_PATH.to_str().unwrap(), &self.psram.path)
            .env(ENV_PSRAM_SIZE.to_str().unwrap(), self.psram.len.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::from(out_write))
            .stderr(Stdio::from(err_write))
            .spawn();
        unsafe { libc::close(fw_fd) };
        let mut child = match spawned {
            Ok(child) => child,
            Err(err) => {
                unsafe { libc::close(host_fd) };
                return Err(err);
            }
        };

        {
            let mut state = self.state.lock().unwrap();
            state.status = Status::Running;
            state.ready = false;
            state.present = None;
            state.brightness = 0;
        }
        let stdin = child.stdin.take().unwrap();
        *self.run.lock().unwrap() = Some(Run { pid: child.id() as libc::pid_t, stdin, sock: host_fd });

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
    }

    fn close_run(&self) -> Option<Run> {
        let run = self.run.lock().unwrap().take()?;
        unsafe { libc::shutdown(run.sock, libc::SHUT_RDWR) };
        Some(run)
    }

    fn receive(&self, fd: RawFd) {
        let mut record = [0u8; RECORD_SIZE];
        while read_record(fd, &mut record) {
            let mut state = self.state.lock().unwrap();
            match Event::decode(&record) {
                Some(Event::Hello { version }) => {
                    if version != VERSION {
                        eprintln!("[devkit] firmware speaks protocol {version}, host {VERSION}");
                    }
                    state.ready = true;
                }
                Some(Event::Present { offset, width, height, stride, format }) => {
                    state.present = Some(Present { offset, width, height, stride, format })
                }
                Some(Event::Brightness { level }) => state.brightness = level,
                Some(Event::Device { name }) => {
                    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                    state.name = Some(String::from_utf8_lossy(&name[..end]).into_owned());
                }
                Some(Event::Touch { .. }) | None => {}
            }
        }
        unsafe { libc::close(fd) };
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

    /// The presented frame as RGB; the firmware may be drawing into it meanwhile.
    pub fn frame(&self) -> Option<Frame> {
        let p = self.present()?;
        let (w, h, stride) = (usize::from(p.width), usize::from(p.height), p.stride as usize);
        let bpp = p.format.bytes();
        let start = usize::try_from(p.offset).ok()?;
        if stride < w * bpp || start.checked_add(stride * h)? > self.psram.len {
            return None;
        }
        let src = unsafe { std::slice::from_raw_parts(self.psram.ptr.add(start), stride * h) };
        let mut rgb = Vec::with_capacity(w * h * 3);
        for row in src.chunks_exact(stride) {
            for px in row[..w * bpp].chunks_exact(bpp) {
                match p.format {
                    PixelFormat::L8 => rgb.extend_from_slice(&[px[0], px[0], px[0]]),
                    PixelFormat::Rgb565 => {
                        let v = u16::from_le_bytes([px[0], px[1]]);
                        let (r, g, b) = ((v >> 11) & 0x1F, (v >> 5) & 0x3F, v & 0x1F);
                        rgb.extend_from_slice(&[(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (b << 3 | b >> 2) as u8]);
                    }
                    PixelFormat::Bgr888 => rgb.extend_from_slice(&[px[2], px[1], px[0]]),
                }
            }
        }
        Some(Frame { width: w, height: h, rgb })
    }
}

fn read_record(fd: RawFd, record: &mut [u8; RECORD_SIZE]) -> bool {
    let mut at = 0;
    while at < RECORD_SIZE {
        let n = unsafe { libc::recv(fd, record[at..].as_mut_ptr().cast(), RECORD_SIZE - at, 0) };
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

