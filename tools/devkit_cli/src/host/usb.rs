/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The simulated firmware's USB host port: each plug hands it a USB/IP
 * connection to import a device from (docs/simulator.md).
 */

use std::io;
use std::net::TcpStream;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

/// Names the firmware's end of the socket.
pub const ENV_USB_FD: &str = "SIM_USB_FD";
pub const DEFAULT_BUSID: &str = "1-1";
const BUSID_MAX: usize = 31;

pub fn socket_pair() -> io::Result<(RawFd, RawFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC) };
    Ok((fds[0], fds[1]))
}

pub fn connect_tcp(addr: &str) -> io::Result<OwnedFd> {
    let stream = TcpStream::connect(addr)?;
    stream.set_nodelay(true)?;
    Ok(stream.into())
}

/// Plugs `session` in as the device `busid` on its server.
pub fn plug(control: RawFd, session: &OwnedFd, busid: &str) -> io::Result<()> {
    if busid.is_empty() || busid.len() > BUSID_MAX {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bus id of 1 to 31 bytes"));
    }
    let fd = session.as_raw_fd();
    let space = unsafe { libc::CMSG_SPACE(size_of::<RawFd>() as u32) } as usize;
    let mut control_buf = vec![0u8; space];
    let mut iov = libc::iovec { iov_base: busid.as_ptr() as *mut _, iov_len: busid.len() };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control_buf.as_mut_ptr().cast();
    msg.msg_controllen = space as _;
    unsafe {
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as _;
        std::ptr::copy_nonoverlapping((&raw const fd).cast::<u8>(), libc::CMSG_DATA(c), size_of::<RawFd>());
    }
    if unsafe { libc::sendmsg(control, &msg, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Ends the connection for the firmware and the server alike, which both
/// see the device unplugged.
pub fn unplug(session: OwnedFd) {
    unsafe { libc::shutdown(session.as_raw_fd(), libc::SHUT_RDWR) };
}
