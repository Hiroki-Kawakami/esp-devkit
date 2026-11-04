/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The simulated firmware's network: libslirp behind a datagram socket that
 * carries one Ethernet frame per datagram (docs/simulator.md).
 */

use std::ffi::{CStr, c_char, c_int, c_void};
use std::os::fd::RawFd;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Names the firmware's end of the socket.
pub const ENV_NET_FD: &str = "SIM_NET_FD";

const SOCKET_BUFFER: c_int = 1 << 20;
const POLL_MAX_MS: u32 = 100;
const FRAME_MAX: usize = 2048;
const MTU: usize = 1500;

const fn ipv4(a: u8, b: u8, c: u8, d: u8) -> InAddr {
    InAddr { s_addr: u32::from_ne_bytes([a, b, c, d]) }
}

const NETWORK: InAddr = ipv4(10, 0, 2, 0);
const NETMASK: InAddr = ipv4(255, 255, 255, 0);
const HOST: InAddr = ipv4(10, 0, 2, 2);
const DHCP_START: InAddr = ipv4(10, 0, 2, 15);
const NAMESERVER: InAddr = ipv4(10, 0, 2, 3);

const SLIRP_POLL_IN: c_int = 1 << 0;
const SLIRP_POLL_OUT: c_int = 1 << 1;
const SLIRP_POLL_PRI: c_int = 1 << 2;
const SLIRP_POLL_ERR: c_int = 1 << 3;
const SLIRP_POLL_HUP: c_int = 1 << 4;

#[repr(C)]
#[derive(Clone, Copy)]
struct InAddr {
    s_addr: u32,
}

#[repr(C, align(4))]
#[derive(Clone, Copy)]
struct In6Addr([u8; 16]);

// libslirp.h, SlirpConfig up to version 5.
#[repr(C)]
struct SlirpConfig {
    version: u32,
    restricted: c_int,
    in_enabled: bool,
    vnetwork: InAddr,
    vnetmask: InAddr,
    vhost: InAddr,
    in6_enabled: bool,
    vprefix_addr6: In6Addr,
    vprefix_len: u8,
    vhost6: In6Addr,
    vhostname: *const c_char,
    tftp_server_name: *const c_char,
    tftp_path: *const c_char,
    bootfile: *const c_char,
    vdhcp_start: InAddr,
    vnameserver: InAddr,
    vnameserver6: In6Addr,
    vdnssearch: *const *const c_char,
    vdomainname: *const c_char,
    if_mtu: usize,
    if_mru: usize,
    disable_host_loopback: bool,
    enable_emu: bool,
    outbound_addr: *const c_void,
    outbound_addr6: *const c_void,
    disable_dns: bool,
    disable_dhcp: bool,
    mfr_id: u32,
    oob_eth_addr: [u8; 6],
}

type TimerCb = unsafe extern "C" fn(*mut c_void);

// libslirp.h, SlirpCb up to version 6.
#[repr(C)]
struct SlirpCb {
    send_packet: unsafe extern "C" fn(*const c_void, usize, *mut c_void) -> isize,
    guest_error: unsafe extern "C" fn(*const c_char, *mut c_void),
    clock_get_ns: unsafe extern "C" fn(*mut c_void) -> i64,
    timer_new: unsafe extern "C" fn(TimerCb, *mut c_void, *mut c_void) -> *mut c_void,
    timer_free: unsafe extern "C" fn(*mut c_void, *mut c_void),
    timer_mod: unsafe extern "C" fn(*mut c_void, i64, *mut c_void),
    register_poll_fd: unsafe extern "C" fn(c_int, *mut c_void),
    unregister_poll_fd: unsafe extern "C" fn(c_int, *mut c_void),
    notify: unsafe extern "C" fn(*mut c_void),
    init_completed: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    timer_new_opaque: Option<unsafe extern "C" fn(c_int, *mut c_void, *mut c_void) -> *mut c_void>,
    register_poll_socket: unsafe extern "C" fn(c_int, *mut c_void),
    unregister_poll_socket: unsafe extern "C" fn(c_int, *mut c_void),
}

#[link(name = "slirp")]
unsafe extern "C" {
    fn slirp_new(cfg: *const SlirpConfig, callbacks: *const SlirpCb, opaque: *mut c_void) -> *mut c_void;
    fn slirp_cleanup(slirp: *mut c_void);
    fn slirp_pollfds_fill_socket(
        slirp: *mut c_void,
        timeout: *mut u32,
        add_poll: unsafe extern "C" fn(c_int, c_int, *mut c_void) -> c_int,
        opaque: *mut c_void,
    );
    fn slirp_pollfds_poll(
        slirp: *mut c_void,
        select_error: c_int,
        get_revents: unsafe extern "C" fn(c_int, *mut c_void) -> c_int,
        opaque: *mut c_void,
    );
    fn slirp_input(slirp: *mut c_void, pkt: *const u8, pkt_len: c_int);
}

struct Timer {
    cb: TimerCb,
    cb_opaque: *mut c_void,
    /// Milliseconds on `State::clock`.
    expire: Option<i64>,
}

struct State {
    fd: RawFd,
    clock: Instant,
    timers: Vec<*mut Timer>,
    pollfds: Vec<libc::pollfd>,
}

impl State {
    fn now_ms(&self) -> i64 {
        self.clock.elapsed().as_millis() as i64
    }
}

unsafe fn state<'a>(opaque: *mut c_void) -> &'a mut State {
    unsafe { &mut *opaque.cast::<State>() }
}

unsafe extern "C" fn send_packet(buf: *const c_void, len: usize, opaque: *mut c_void) -> isize {
    let fd = unsafe { state(opaque) }.fd;
    // A full socket drops the frame, which TCP recovers from.
    unsafe { libc::send(fd, buf, len, libc::MSG_DONTWAIT) };
    len as isize
}

unsafe extern "C" fn guest_error(msg: *const c_char, _: *mut c_void) {
    eprintln!("[devkit] slirp: {}", unsafe { CStr::from_ptr(msg) }.to_string_lossy());
}

unsafe extern "C" fn clock_get_ns(opaque: *mut c_void) -> i64 {
    unsafe { state(opaque) }.clock.elapsed().as_nanos() as i64
}

unsafe extern "C" fn timer_new(cb: TimerCb, cb_opaque: *mut c_void, opaque: *mut c_void) -> *mut c_void {
    let timer = Box::into_raw(Box::new(Timer { cb, cb_opaque, expire: None }));
    unsafe { state(opaque) }.timers.push(timer);
    timer.cast()
}

unsafe extern "C" fn timer_free(timer: *mut c_void, opaque: *mut c_void) {
    let timer = timer.cast::<Timer>();
    unsafe { state(opaque) }.timers.retain(|&t| t != timer);
    drop(unsafe { Box::from_raw(timer) });
}

unsafe extern "C" fn timer_mod(timer: *mut c_void, expire_time: i64, _: *mut c_void) {
    unsafe { (*timer.cast::<Timer>()).expire = Some(expire_time) };
}

unsafe extern "C" fn ignore_fd(_: c_int, _: *mut c_void) {}

unsafe extern "C" fn notify(_: *mut c_void) {}

unsafe extern "C" fn add_poll(fd: c_int, events: c_int, opaque: *mut c_void) -> c_int {
    let mut flags = 0;
    for (slirp, poll) in [
        (SLIRP_POLL_IN, libc::POLLIN),
        (SLIRP_POLL_OUT, libc::POLLOUT),
        (SLIRP_POLL_PRI, libc::POLLPRI),
        (SLIRP_POLL_ERR, libc::POLLERR),
        (SLIRP_POLL_HUP, libc::POLLHUP),
    ] {
        if events & slirp != 0 {
            flags |= poll;
        }
    }
    let pollfds = &mut unsafe { state(opaque) }.pollfds;
    pollfds.push(libc::pollfd { fd, events: flags, revents: 0 });
    (pollfds.len() - 1) as c_int
}

unsafe extern "C" fn get_revents(idx: c_int, opaque: *mut c_void) -> c_int {
    let revents = unsafe { state(opaque) }.pollfds[idx as usize].revents;
    let mut flags = 0;
    for (poll, slirp) in [
        (libc::POLLIN, SLIRP_POLL_IN),
        (libc::POLLOUT, SLIRP_POLL_OUT),
        (libc::POLLPRI, SLIRP_POLL_PRI),
        (libc::POLLERR, SLIRP_POLL_ERR),
        (libc::POLLHUP, SLIRP_POLL_HUP),
    ] {
        if revents & poll != 0 {
            flags |= slirp;
        }
    }
    flags
}

static CALLBACKS: SlirpCb = SlirpCb {
    send_packet,
    guest_error,
    clock_get_ns,
    timer_new,
    timer_free,
    timer_mod,
    register_poll_fd: ignore_fd,
    unregister_poll_fd: ignore_fd,
    notify,
    init_completed: None,
    timer_new_opaque: None,
    register_poll_socket: ignore_fd,
    unregister_poll_socket: ignore_fd,
};

/// A socket pair for one firmware run: (host end, firmware end). The host end
/// is close-on-exec; the firmware end is for the child to inherit.
pub fn socket_pair() -> std::io::Result<(RawFd, RawFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let size = SOCKET_BUFFER;
    for fd in fds {
        for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
            unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    option,
                    (&raw const size).cast(),
                    size_of::<c_int>() as libc::socklen_t,
                )
            };
        }
    }
    unsafe { libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC) };
    Ok((fds[0], fds[1]))
}

/// Serves the firmware on `fd` until `stop`, then closes `fd`. Guest
/// 10.0.2.15 by DHCP, gateway 10.0.2.2 standing for the host's loopback,
/// DNS 10.0.2.3 forwarding to the host's resolver.
pub fn run(fd: RawFd, stop: Arc<AtomicBool>) {
    let config = SlirpConfig {
        version: 5,
        restricted: 0,
        in_enabled: true,
        vnetwork: NETWORK,
        vnetmask: NETMASK,
        vhost: HOST,
        in6_enabled: false,
        vprefix_addr6: In6Addr([0; 16]),
        vprefix_len: 0,
        vhost6: In6Addr([0; 16]),
        vhostname: ptr::null(),
        tftp_server_name: ptr::null(),
        tftp_path: ptr::null(),
        bootfile: ptr::null(),
        vdhcp_start: DHCP_START,
        vnameserver: NAMESERVER,
        vnameserver6: In6Addr([0; 16]),
        vdnssearch: ptr::null(),
        vdomainname: ptr::null(),
        if_mtu: MTU,
        if_mru: MTU,
        disable_host_loopback: false,
        enable_emu: false,
        outbound_addr: ptr::null(),
        outbound_addr6: ptr::null(),
        disable_dns: false,
        disable_dhcp: false,
        mfr_id: 0,
        oob_eth_addr: [0; 6],
    };
    let state = Box::into_raw(Box::new(State { fd, clock: Instant::now(), timers: Vec::new(), pollfds: Vec::new() }));
    let opaque = state.cast::<c_void>();
    let slirp = unsafe { slirp_new(&config, &CALLBACKS, opaque) };
    if slirp.is_null() {
        eprintln!("[devkit] slirp: no network");
        unsafe { libc::close(fd) };
        drop(unsafe { Box::from_raw(state) });
        return;
    }
    let mut frame = [0u8; FRAME_MAX];
    while !stop.load(Ordering::Relaxed) {
        let mut timeout = POLL_MAX_MS;
        unsafe {
            let s = &mut *state;
            s.pollfds.clear();
            s.pollfds.push(libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
        }
        unsafe { slirp_pollfds_fill_socket(slirp, &mut timeout, add_poll, opaque) };
        let now = unsafe { (*state).now_ms() };
        for &t in unsafe { &(*state).timers } {
            if let Some(expire) = unsafe { (*t).expire } {
                timeout = timeout.min((expire - now).clamp(0, i64::from(POLL_MAX_MS)) as u32);
            }
        }
        let ready = unsafe {
            let s = &mut *state;
            libc::poll(s.pollfds.as_mut_ptr(), s.pollfds.len() as libc::nfds_t, timeout as c_int)
        };
        if unsafe { (&(*state).pollfds)[0].revents } & libc::POLLIN != 0 {
            loop {
                let n = unsafe { libc::recv(fd, frame.as_mut_ptr().cast(), frame.len(), libc::MSG_DONTWAIT) };
                if n <= 0 {
                    break;
                }
                unsafe { slirp_input(slirp, frame.as_ptr(), n as c_int) };
            }
        }
        unsafe { slirp_pollfds_poll(slirp, c_int::from(ready < 0), get_revents, opaque) };
        let now = unsafe { (*state).now_ms() };
        let due: Vec<(TimerCb, *mut c_void)> = unsafe { &(*state).timers }
            .iter()
            .filter_map(|&t| unsafe {
                let timer = &mut *t;
                match timer.expire {
                    Some(expire) if expire <= now => {
                        timer.expire = None;
                        Some((timer.cb, timer.cb_opaque))
                    }
                    _ => None,
                }
            })
            .collect();
        for (cb, cb_opaque) in due {
            unsafe { cb(cb_opaque) };
        }
    }
    unsafe { slirp_cleanup(slirp) };
    unsafe { libc::close(fd) };
    let state = unsafe { Box::from_raw(state) };
    for &t in &state.timers {
        drop(unsafe { Box::from_raw(t) });
    }
}
