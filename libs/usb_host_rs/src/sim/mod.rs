/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * The simulator's host controller: a USB/IP client on sessions the devkit
 * host hands over (docs/simulator.md).
 */

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ffi::{CStr, c_int};
use core::sync::atomic::{AtomicU32, Ordering};

use idf::executor::Sender;
use idf::log::{error, info, warn};
use idf::sync::Mutex;
use idf::thread::{self, JoinHandle};

use crate::hcd::{Hcd, PipeConfig, PipeId, PortEvent, Request};
use crate::setup::request;
use crate::usbip::{self, Basic, DeviceRecord, Header, OpHeader};
use crate::{Error, Result, Speed};

#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals, unsafe_op_in_unsafe_fn)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

const TAG: &CStr = c"usb_host_sim";

/// A datagram socket from the devkit host; each datagram is a bus id with
/// a USB/IP connection to import it from.
const ENV_USB_FD: &CStr = c"SIM_USB_FD";
const THREAD_STACK: usize = 8192;

type Fd = c_int;

#[derive(Clone, Copy)]
struct Pipe {
    endpoint: u8,
}

struct Session {
    fd: Fd,
    devid: u32,
    speed: Speed,
    next_seqnum: AtomicU32,
    inflight: Mutex<BTreeMap<u32, Arc<Request>>>,
    /// Unlink seqnum to the seqnum it unlinks.
    unlinks: Mutex<BTreeMap<u32, u32>>,
    writer: Mutex<()>,
}

impl Session {
    fn seqnum(&self) -> u32 {
        self.next_seqnum.fetch_add(1, Ordering::Relaxed)
    }

    fn send(&self, parts: &[&[u8]]) -> bool {
        let _turn = self.writer.lock();
        parts.iter().all(|p| write_all(self.fd, p))
    }

    fn fail_all(&self) {
        let inflight = core::mem::take(&mut *self.inflight.lock());
        for request in inflight.into_values() {
            request.complete(Err(Error::NoDevice));
        }
    }
}

struct State {
    events: Option<Sender<PortEvent>>,
    session: Option<Arc<Session>>,
    pipes: Vec<Option<Pipe>>,
}

pub(crate) struct SimHcd {
    state: Mutex<State>,
}

impl SimHcd {
    pub(crate) const fn new() -> Self {
        Self { state: Mutex::new(State { events: None, session: None, pipes: Vec::new() }) }
    }

    fn post(&self, event: PortEvent) {
        let state = self.state.lock();
        if let Some(events) = &state.events
            && events.try_send(event).is_err()
        {
            error!(TAG, "port event {event:?} dropped");
        }
    }

    fn listen(&'static self, control: Fd) {
        let mut name = [0u8; usbip::BUSID_BYTES];
        let mut reader: Option<JoinHandle<()>> = None;
        loop {
            let mut fd = -1;
            let n = unsafe { ffi::usbh_sim_recv_fd(control, name.as_mut_ptr().cast(), name.len(), &mut fd) };
            if n < 0 {
                error!(TAG, "host USB link closed");
                return;
            }
            if let Some(old) = self.state.lock().session.clone() {
                unsafe { ffi::shutdown(old.fd, ffi::SHUT_RDWR as c_int) };
            }
            if let Some(reader) = reader.take() {
                reader.join();
            }
            if fd < 0 {
                continue;
            }
            let busid = &name[..n as usize];
            let record = match import(fd, busid) {
                Ok(record) => record,
                Err(err) => {
                    error!(TAG, "import {}: {err}", core::str::from_utf8(busid).unwrap_or("?"));
                    unsafe { ffi::close(fd) };
                    continue;
                }
            };
            let speed = match record.speed {
                usbip::SPEED_LOW => Speed::Low,
                usbip::SPEED_FULL => Speed::Full,
                _ => Speed::High,
            };
            info!(TAG, "attached {:04x}:{:04x}", record.vendor_id, record.product_id);
            let session = Arc::new(Session {
                fd,
                devid: record.devid(),
                speed,
                next_seqnum: AtomicU32::new(1),
                inflight: Mutex::new(BTreeMap::new()),
                unlinks: Mutex::new(BTreeMap::new()),
                writer: Mutex::new(()),
            });
            self.state.lock().session = Some(session.clone());
            let spawned =
                thread::Builder::new().name("usbh_sim_rx").stack_size(THREAD_STACK).spawn(move || self.receive(session));
            match spawned {
                Ok(handle) => {
                    reader = Some(handle);
                    self.post(PortEvent::Connected);
                }
                Err(err) => {
                    error!(TAG, "rx task: {err}");
                    if let Some(session) = self.state.lock().session.take() {
                        unsafe { ffi::close(session.fd) };
                    }
                }
            }
        }
    }

    fn receive(&self, session: Arc<Session>) {
        let mut header = [0u8; usbip::HEADER_BYTES];
        while read_exact(session.fd, &mut header) {
            match Header::decode(&header) {
                Some(Header::RetSubmit { basic, status, actual, .. }) => {
                    if !self.ret_submit(&session, basic.seqnum, status, actual as usize) {
                        break;
                    }
                }
                Some(Header::RetUnlink { basic, status }) => {
                    let target = session.unlinks.lock().remove(&basic.seqnum);
                    if status == usbip::ECONNRESET
                        && let Some(target) = target
                        && let Some(request) = session.inflight.lock().remove(&target)
                    {
                        request.complete(Err(Error::Canceled));
                    }
                }
                _ => {
                    error!(TAG, "unexpected USB/IP header");
                    break;
                }
            }
        }
        session.fail_all();
        unsafe { ffi::close(session.fd) };
        {
            let mut state = self.state.lock();
            if state.session.as_ref().is_some_and(|s| Arc::ptr_eq(s, &session)) {
                state.session = None;
            }
        }
        info!(TAG, "detached");
        self.post(PortEvent::Disconnected);
    }

    /// `false` once the connection is unusable.
    fn ret_submit(&self, session: &Session, seqnum: u32, status: i32, actual: usize) -> bool {
        let request = session.inflight.lock().remove(&seqnum);
        let data_in = request.as_ref().is_none_or(|r| r.is_in());
        let mut result = match status {
            0 => Ok(actual),
            usbip::EPIPE => Err(Error::Stall),
            usbip::EOVERFLOW => Err(Error::Overflow),
            usbip::ECONNRESET => Err(Error::Canceled),
            usbip::ENODEV | usbip::ESHUTDOWN => Err(Error::NoDevice),
            _ => Err(Error::Failed),
        };
        if data_in && actual > 0 {
            let room = request.as_ref().map_or(0, |r| r.len);
            let take = actual.min(room);
            if let Some(request) = &request
                && !read_exact(session.fd, unsafe { &mut request.data()[..take] })
            {
                request.complete(Err(Error::NoDevice));
                return false;
            }
            if !discard(session.fd, actual - take) {
                if let Some(request) = request {
                    request.complete(Err(Error::NoDevice));
                }
                return false;
            }
            if take < actual && result.is_ok() {
                result = Err(Error::Overflow);
            }
        }
        if let Some(request) = request {
            request.complete(result);
        }
        true
    }

    fn session(&self) -> Option<Arc<Session>> {
        self.state.lock().session.clone()
    }

    fn pipe(&self, pipe: PipeId) -> Option<Pipe> {
        *self.state.lock().pipes.get(usize::from(pipe.0))?
    }
}

impl Hcd for SimHcd {
    fn init(&'static self, events: Sender<PortEvent>) -> Result<()> {
        self.state.lock().events = Some(events);
        let Some(control) = env_fd(ENV_USB_FD) else {
            warn!(TAG, "no simulator host USB");
            return Ok(());
        };
        thread::Builder::new()
            .name("usbh_sim")
            .stack_size(THREAD_STACK)
            .spawn(move || self.listen(control))
            .map(drop)
            .map_err(|err| {
                error!(TAG, "listener task: {err}");
                Error::NoMemory
            })
    }

    async fn reset(&self) -> Result<Speed> {
        self.session().map(|s| s.speed).ok_or(Error::NoDevice)
    }

    fn pipe_alloc(&self, config: &PipeConfig) -> Result<PipeId> {
        let pipe = Pipe { endpoint: config.endpoint };
        let mut state = self.state.lock();
        let index = match state.pipes.iter().position(Option::is_none) {
            Some(index) => {
                state.pipes[index] = Some(pipe);
                index
            }
            None => {
                state.pipes.push(Some(pipe));
                state.pipes.len() - 1
            }
        };
        Ok(PipeId(index as u16))
    }

    fn pipe_update(&self, _: PipeId, _: u8, _: u16) {}

    fn pipe_free(&self, pipe: PipeId) {
        if let Some(slot) = self.state.lock().pipes.get_mut(usize::from(pipe.0)) {
            *slot = None;
        }
    }

    fn submit(&self, pipe: PipeId, request: Arc<Request>) -> Result<()> {
        let (Some(session), Some(pipe)) = (self.session(), self.pipe(pipe)) else {
            return Err(Error::NoDevice);
        };
        // The server's side already has the device addressed.
        if let Some(setup) = request.setup
            && setup.request_type == 0
            && setup.request == request::SET_ADDRESS
        {
            request.complete(Ok(0));
            return Ok(());
        }
        let seqnum = session.seqnum();
        let data_in = request.is_in();
        let header = Header::Submit {
            basic: Basic {
                seqnum,
                devid: session.devid,
                direction: if data_in { usbip::DIR_IN } else { usbip::DIR_OUT },
                endpoint: u32::from(pipe.endpoint & 0x0f),
            },
            flags: if request.zlp { usbip::URB_ZERO_PACKET } else { 0 },
            length: request.len as u32,
            start_frame: 0,
            packets: usbip::NO_ISO_PACKETS,
            interval: 0,
            setup: request.setup.map_or([0; 8], |s| s.bytes()),
        }
        .encode();
        session.inflight.lock().insert(seqnum, request.clone());
        let data: &[u8] = if data_in { &[] } else { unsafe { request.data() } };
        if session.send(&[&header, data]) {
            return Ok(());
        }
        match session.inflight.lock().remove(&seqnum) {
            Some(_) => Err(Error::NoDevice),
            None => Ok(()),
        }
    }

    fn cancel(&self, pipe: PipeId, request: &Arc<Request>) {
        let (Some(session), Some(pipe)) = (self.session(), self.pipe(pipe)) else { return };
        let target = {
            let inflight = session.inflight.lock();
            inflight.iter().find(|(_, r)| Arc::ptr_eq(r, request)).map(|(&seqnum, _)| seqnum)
        };
        let Some(target) = target else { return };
        let seqnum = session.seqnum();
        session.unlinks.lock().insert(seqnum, target);
        let header = Header::Unlink {
            basic: Basic {
                seqnum,
                devid: session.devid,
                direction: if request.is_in() { usbip::DIR_IN } else { usbip::DIR_OUT },
                endpoint: u32::from(pipe.endpoint & 0x0f),
            },
            target,
        }
        .encode();
        session.send(&[&header]);
    }

    fn clear(&self, _: PipeId) {}

    fn recover(&self) {}
}

fn env_fd(name: &CStr) -> Option<Fd> {
    let value = unsafe { ffi::getenv(name.as_ptr()) };
    if value.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(value) }.to_str().ok()?.parse().ok()
}

fn import(fd: Fd, busid: &[u8]) -> Result<DeviceRecord> {
    let request = usbip::import_request(busid).ok_or(Error::InvalidArgument)?;
    if !write_all(fd, &request) {
        return Err(Error::NoDevice);
    }
    let mut op = [0u8; usbip::OP_HEADER_BYTES];
    if !read_exact(fd, &mut op) {
        return Err(Error::NoDevice);
    }
    match OpHeader::decode(&op) {
        Some(OpHeader { code: usbip::OP_REP_IMPORT, status: 0 }) => {}
        Some(OpHeader { code: usbip::OP_REP_IMPORT, .. }) => return Err(Error::NotFound),
        _ => return Err(Error::InvalidResponse),
    }
    let mut record = [0u8; usbip::DEVICE_BYTES];
    if !read_exact(fd, &mut record) {
        return Err(Error::NoDevice);
    }
    Ok(DeviceRecord::decode(&record))
}

fn read_exact(fd: Fd, mut buf: &mut [u8]) -> bool {
    while !buf.is_empty() {
        let n = unsafe { ffi::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n <= 0 {
            return false;
        }
        buf = &mut buf[n as usize..];
    }
    true
}

fn discard(fd: Fd, mut n: usize) -> bool {
    let mut scratch = [0u8; 256];
    while n > 0 {
        let take = n.min(scratch.len());
        if !read_exact(fd, &mut scratch[..take]) {
            return false;
        }
        n -= take;
    }
    true
}

fn write_all(fd: Fd, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let n = unsafe { ffi::usbh_sim_send(fd, data.as_ptr().cast(), data.len()) };
        if n <= 0 {
            return false;
        }
        data = &data[n as usize..];
    }
    true
}
