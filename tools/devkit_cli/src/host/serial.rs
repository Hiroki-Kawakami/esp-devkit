/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::Sink;

const POLL_MS: i32 = 200;

pub struct Serial {
    path: PathBuf,
    baud: u32,
    fd: Mutex<Option<libc::c_int>>,
    wanted: AtomicBool,
    sink: Mutex<Option<Sink>>,
}

impl Serial {
    /// Opening leaves the board running.
    pub fn open(path: &Path, baud: u32) -> io::Result<Arc<Self>> {
        let serial = Arc::new(Self {
            path: path.to_path_buf(),
            baud,
            fd: Mutex::new(None),
            wanted: AtomicBool::new(false),
            sink: Mutex::new(None),
        });
        serial.acquire()?;
        let me = serial.clone();
        thread::spawn(move || me.read_loop());
        Ok(serial)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_sink(&self, sink: Sink) {
        *self.sink.lock().unwrap() = Some(sink);
    }

    pub fn acquire(&self) -> io::Result<()> {
        self.wanted.store(true, Ordering::SeqCst);
        let mut slot = self.fd.lock().unwrap();
        if slot.is_none() {
            *slot = Some(open_port(&self.path, self.baud)?);
        }
        Ok(())
    }

    pub fn release(&self) {
        self.wanted.store(false, Ordering::SeqCst);
        self.close();
    }

    fn close(&self) {
        if let Some(fd) = self.fd.lock().unwrap().take() {
            unsafe { libc::close(fd) };
        }
    }

    fn report(&self, line: String) {
        if let Some(sink) = self.sink.lock().unwrap().clone() {
            sink(line);
        }
    }

    /// A USB port vanishes across some resets and deep sleep.
    fn reopen(&self) {
        if !self.wanted.load(Ordering::SeqCst) {
            return;
        }
        let mut slot = self.fd.lock().unwrap();
        if slot.is_none() {
            if let Ok(fd) = open_port(&self.path, self.baud) {
                *slot = Some(fd);
                drop(slot);
                self.report("--- port reconnected".into());
            }
        }
    }

    pub fn held(&self) -> bool {
        self.fd.lock().unwrap().is_some()
    }

    pub fn send_line(&self, line: &str) {
        let Some(fd) = *self.fd.lock().unwrap() else { return };
        let data = format!("{line}\n");
        let mut at = 0;
        while at < data.len() {
            let n = unsafe { libc::write(fd, data[at..].as_ptr().cast(), data.len() - at) };
            if n < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
                return;
            }
            at += n as usize;
        }
    }

    /// Pulses EN through RTS.
    pub fn reset(&self) -> io::Result<()> {
        let fd = self.fd.lock().unwrap().ok_or_else(|| io::Error::other("port released"))?;
        set_lines(fd, libc::TIOCM_RTS, true);
        thread::sleep(Duration::from_millis(100));
        set_lines(fd, libc::TIOCM_RTS, false);
        Ok(())
    }

    fn read_loop(&self) {
        let mut pending = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let Some(fd) = *self.fd.lock().unwrap() else {
                pending.clear();
                thread::sleep(Duration::from_millis(POLL_MS as u64));
                self.reopen();
                continue;
            };
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut pfd, 1, POLL_MS) } <= 0 {
                continue;
            }
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                if n == 0 || io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock {
                    self.close();
                    self.report("--- port closed, waiting for it to come back".into());
                }
                continue;
            }
            pending.extend_from_slice(&buf[..n as usize]);
            while let Some(nl) = pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = pending.drain(..=nl).collect();
                let line = String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string();
                self.report(line);
            }
        }
    }
}

fn set_lines(fd: libc::c_int, bits: libc::c_int, on: bool) {
    let request = if on { libc::TIOCMBIS } else { libc::TIOCMBIC };
    unsafe { libc::ioctl(fd, request as _, &bits) };
}

fn open_port(path: &Path, baud: u32) -> io::Result<libc::c_int> {
    let c = CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // Opening asserts DTR and RTS; dropping RTS before DTR never passes through
    // DTR=0/RTS=1, which would pull EN low and reboot the board.
    set_lines(fd, libc::TIOCM_DTR | libc::TIOCM_RTS, true);
    set_lines(fd, libc::TIOCM_RTS, false);
    set_lines(fd, libc::TIOCM_DTR, false);
    if let Err(err) = configure(fd, baud) {
        unsafe { libc::close(fd) };
        return Err(err);
    }
    Ok(fd)
}

fn configure(fd: libc::c_int, baud: u32) -> io::Result<()> {
    let mut tio: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut tio) } != 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { libc::cfmakeraw(&mut tio) };
    tio.c_cflag |= libc::CLOCAL | libc::CREAD;
    tio.c_cflag &= !libc::HUPCL;
    #[cfg(target_os = "linux")]
    {
        let speed = match baud {
            9600 => libc::B9600,
            19200 => libc::B19200,
            38400 => libc::B38400,
            57600 => libc::B57600,
            115200 => libc::B115200,
            230400 => libc::B230400,
            460800 => libc::B460800,
            921600 => libc::B921600,
            1500000 => libc::B1500000,
            2000000 => libc::B2000000,
            3000000 => libc::B3000000,
            _ => return Err(io::Error::other(format!("unsupported baud {baud}"))),
        };
        unsafe { libc::cfsetspeed(&mut tio, speed) };
    }
    #[cfg(target_os = "macos")]
    unsafe {
        libc::cfsetspeed(&mut tio, libc::B9600)
    };
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &tio) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // The termios speeds stop at 230400; IOSSIOSPEED sets any rate.
    #[cfg(target_os = "macos")]
    {
        const IOSSIOSPEED: libc::c_ulong = 0x8008_5402;
        let speed: libc::speed_t = baud as libc::speed_t;
        if unsafe { libc::ioctl(fd, IOSSIOSPEED, &speed) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
