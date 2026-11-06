/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * A device plugged into this computer, served over USB/IP to a simulated
 * USB host through nusb (docs/simulator.md).
 */

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak, mpsc};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::Duration;

use nusb::transfer::{Buffer, Bulk, Completion, ControlIn, ControlOut, ControlType, In, Interrupt, Out, Recipient, TransferError};
use nusb::{DeviceId, DeviceInfo, MaybeFuture, Speed};

use crate::usbip::{self, Basic, DeviceRecord, Header, OpHeader};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
/// The session serving each device, so a new one takes over cleanly.
static SESSIONS: Mutex<Vec<(DeviceId, Weak<Session>)>> = Mutex::new(Vec::new());
const PRESENCE_POLL: Duration = Duration::from_millis(500);
const GONE_CHECKS: usize = 5;
const GONE_CHECK_INTERVAL: Duration = Duration::from_millis(50);
const SETUP_IN: u8 = 0x80;
const REQ_CLEAR_FEATURE: u8 = 0x01;
const REQ_SET_CONFIGURATION: u8 = 0x09;
const REQ_SET_INTERFACE: u8 = 0x0b;
const RECIPIENT_INTERFACE: u8 = 0x01;
const RECIPIENT_ENDPOINT: u8 = 0x02;
const FEATURE_ENDPOINT_HALT: u16 = 0;

fn describe(info: &DeviceInfo) -> String {
    let speed = match info.speed() {
        Some(Speed::Low) => "low",
        Some(Speed::Full) => "full",
        Some(Speed::High) => "high",
        Some(_) => "super",
        None => "?",
    };
    format!(
        "{:04x}:{:04x} serial={} \"{}\" {} speed",
        info.vendor_id(),
        info.product_id(),
        info.serial_number().unwrap_or("-"),
        info.product_string().unwrap_or(""),
        speed
    )
}

/// One line per device on this computer.
pub fn list() -> io::Result<Vec<String>> {
    Ok(nusb::list_devices().wait()?.map(|info| describe(&info)).collect())
}

/// The first device with `vendor:product`, and `serial` when given.
pub fn find(vendor: u16, product: u16, serial: Option<&str>) -> io::Result<DeviceInfo> {
    nusb::list_devices()
        .wait()?
        .find(|i| i.vendor_id() == vendor && i.product_id() == product && serial.is_none_or(|s| i.serial_number() == Some(s)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no device {vendor:04x}:{product:04x}")))
}

/// Opens `info` and serves it as `busid` on the returned socket, until either
/// end closes or the device goes away.
pub fn serve(info: DeviceInfo, busid: &str) -> io::Result<OwnedFd> {
    let previous = {
        let mut sessions = SESSIONS.lock().unwrap();
        sessions.retain(|(_, s)| s.strong_count() > 0);
        sessions.iter().find(|(id, _)| *id == info.id()).and_then(|(_, s)| s.upgrade())
    };
    if let Some(previous) = previous {
        previous.stop();
    }
    let device = info.open().wait()?;
    let mut fds = [0; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let ours = unsafe { UnixStream::from_raw_fd(fds[0]) };
    unsafe { libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC) };
    let theirs = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    #[cfg(target_os = "macos")]
    {
        let one: libc::c_int = 1;
        unsafe {
            libc::setsockopt(fds[0], libc::SOL_SOCKET, libc::SO_NOSIGPIPE, (&raw const one).cast(), 4);
        }
    }
    let session = Arc::new(Session {
        writer: Mutex::new(ours.try_clone()?),
        stream: ours.try_clone()?,
        device,
        info,
        busid: busid.to_string(),
        stopped: AtomicBool::new(false),
        interfaces: Mutex::new(HashMap::new()),
        workers: Mutex::new(HashMap::new()),
        control: Control { jobs: Mutex::new(ControlQueue::default()), ready: Condvar::new() },
    });
    SESSIONS.lock().unwrap().push((session.info.id(), Arc::downgrade(&session)));
    {
        let s = session.clone();
        thread::Builder::new().name("usb-relay".into()).spawn(move || s.run(ours))?;
    }
    {
        let s = session.clone();
        thread::Builder::new().name("usb-relay-ctrl".into()).spawn(move || s.control_loop())?;
    }
    {
        let s = session.clone();
        thread::Builder::new().name("usb-relay-watch".into()).spawn(move || s.watch())?;
    }
    Ok(theirs)
}

struct ControlJob {
    seqnum: u32,
    setup: [u8; 8],
    length: usize,
    data: Vec<u8>,
}

#[derive(Default)]
struct ControlQueue {
    queue: VecDeque<ControlJob>,
    closed: bool,
}

struct Control {
    jobs: Mutex<ControlQueue>,
    ready: Condvar,
}

struct Session {
    writer: Mutex<UnixStream>,
    stream: UnixStream,
    device: nusb::Device,
    info: DeviceInfo,
    busid: String,
    stopped: AtomicBool,
    interfaces: Mutex<HashMap<u8, nusb::Interface>>,
    workers: Mutex<HashMap<u8, Arc<Worker>>>,
    control: Control,
}

fn status_of(error: TransferError) -> i32 {
    match error {
        TransferError::Stall => usbip::EPIPE,
        TransferError::Cancelled => usbip::ECONNRESET,
        TransferError::Disconnected => usbip::ENODEV,
        TransferError::InvalidArgument => usbip::EINVAL,
        _ => usbip::EPROTO,
    }
}

fn read_exact(stream: &mut UnixStream, buf: &mut [u8]) -> bool {
    stream.read_exact(buf).is_ok()
}

impl Session {
    /// Returns with the device's interfaces released, but from an endpoint
    /// thread its own endpoint goes when that thread ends.
    fn stop(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        {
            let mut jobs = self.control.jobs.lock().unwrap();
            jobs.closed = true;
            self.control.ready.notify_all();
        }
        self.drop_workers(None);
        self.interfaces.lock().unwrap().clear();
    }

    fn send(&self, header: Header, data: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        if writer.write_all(&header.encode()).and_then(|()| writer.write_all(data)).is_err() {
            drop(writer);
            self.stop();
        }
    }

    fn ret_submit(&self, seqnum: u32, status: i32, actual: usize, data: &[u8]) {
        let header = Header::RetSubmit {
            basic: Basic { seqnum, ..Basic::default() },
            status,
            actual: actual as u32,
            start_frame: 0,
            packets: 0,
            errors: 0,
        };
        self.send(header, data);
    }

    fn ret_unlink(&self, seqnum: u32, status: i32) {
        self.send(Header::RetUnlink { basic: Basic { seqnum, ..Basic::default() }, status }, &[]);
    }

    fn record(&self) -> DeviceRecord {
        let info = &self.info;
        let mut path = [0u8; usbip::PATH_BYTES];
        let location = format!("/nusb/{}/{:?}", info.bus_id(), info.port_chain());
        let n = location.len().min(usbip::PATH_BYTES - 1);
        path[..n].copy_from_slice(&location.as_bytes()[..n]);
        let mut busid = [0u8; usbip::BUSID_BYTES];
        busid[..self.busid.len()].copy_from_slice(self.busid.as_bytes());
        let config = self.device.active_configuration().ok();
        DeviceRecord {
            path,
            busid,
            busnum: 1,
            devnum: u32::from(info.device_address()),
            speed: match info.speed() {
                Some(Speed::Low) => usbip::SPEED_LOW,
                Some(Speed::Full) => usbip::SPEED_FULL,
                _ => usbip::SPEED_HIGH,
            },
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            bcd_device: info.device_version(),
            class: info.class(),
            subclass: info.subclass(),
            protocol: info.protocol(),
            configuration_value: config.as_ref().map_or(0, |c| c.configuration_value()),
            configurations: self.device.device_descriptor().num_configurations(),
            interfaces: config.as_ref().map_or(0, |c| c.num_interfaces()),
        }
    }

    fn import(&self, stream: &mut UnixStream) -> bool {
        let mut op = [0u8; usbip::OP_HEADER_BYTES];
        let mut busid = [0u8; usbip::BUSID_BYTES];
        if !read_exact(stream, &mut op) || !read_exact(stream, &mut busid) {
            return false;
        }
        if OpHeader::decode(&op).map(|o| o.code) != Some(usbip::OP_REQ_IMPORT) {
            eprintln!("[devkit] usb relay: expected an import request");
            return false;
        }
        let name = busid.split(|&b| b == 0).next().unwrap_or(&[]);
        let matches = name == self.busid.as_bytes();
        let reply = OpHeader { code: usbip::OP_REP_IMPORT, status: u32::from(!matches) }.encode();
        let mut writer = self.writer.lock().unwrap();
        let mut ok = writer.write_all(&reply).is_ok();
        if matches {
            ok &= writer.write_all(&self.record().encode()).is_ok();
        }
        ok && matches
    }

    fn run(self: Arc<Self>, mut stream: UnixStream) {
        if self.import(&mut stream) {
            let mut header = [0u8; usbip::HEADER_BYTES];
            while !self.stopped.load(Ordering::Acquire) && read_exact(&mut stream, &mut header) {
                match Header::decode(&header) {
                    Some(Header::Submit { basic, flags, length, packets, setup, .. }) => {
                        if !self.submit(&mut stream, basic, flags, length as usize, packets, setup) {
                            break;
                        }
                    }
                    Some(Header::Unlink { basic, target }) => self.unlink(basic, target),
                    _ => {
                        eprintln!("[devkit] usb relay: unexpected USB/IP header");
                        break;
                    }
                }
            }
        }
        self.stop();
    }

    fn submit(self: &Arc<Self>, stream: &mut UnixStream, basic: Basic, flags: u32, length: usize, packets: u32, setup: [u8; 8]) -> bool {
        let data_in = basic.direction == usbip::DIR_IN;
        let mut data = Vec::new();
        if !data_in && length > 0 {
            data.resize(length, 0);
            if !read_exact(stream, &mut data) {
                return false;
            }
        }
        if packets != usbip::NO_ISO_PACKETS && packets != 0 {
            let mut skip = vec![0u8; usbip::ISO_PACKET_BYTES * packets as usize];
            if !read_exact(stream, &mut skip) {
                return false;
            }
            self.ret_submit(basic.seqnum, usbip::EPIPE, 0, &[]);
            return true;
        }
        if basic.endpoint == 0 {
            let mut jobs = self.control.jobs.lock().unwrap();
            jobs.queue.push_back(ControlJob { seqnum: basic.seqnum, setup, length, data });
            self.control.ready.notify_one();
            return true;
        }
        let address = basic.endpoint as u8 | if data_in { 0x80 } else { 0 };
        match self.worker(address) {
            Ok(worker) => worker.push(Cmd::Submit {
                seqnum: basic.seqnum,
                length,
                zlp: flags & usbip::URB_ZERO_PACKET != 0,
                data,
            }),
            Err(err) => {
                eprintln!("[devkit] usb relay: endpoint {address:02x}: {err}");
                self.ret_submit(basic.seqnum, usbip::EPIPE, 0, &[]);
            }
        }
        true
    }

    fn unlink(&self, basic: Basic, target: u32) {
        {
            let mut jobs = self.control.jobs.lock().unwrap();
            if let Some(at) = jobs.queue.iter().position(|j| j.seqnum == target) {
                jobs.queue.remove(at);
                drop(jobs);
                self.ret_unlink(basic.seqnum, usbip::ECONNRESET);
                return;
            }
        }
        if basic.endpoint != 0 {
            let address = basic.endpoint as u8 | if basic.direction == usbip::DIR_IN { 0x80 } else { 0 };
            let worker = self.workers.lock().unwrap().get(&address).cloned();
            if let Some(worker) = worker {
                worker.push(Cmd::Unlink { seqnum: basic.seqnum, target });
                return;
            }
        }
        self.ret_unlink(basic.seqnum, 0);
    }

    /// Claims the endpoint's interface on first use.
    fn worker(self: &Arc<Self>, address: u8) -> io::Result<Arc<Worker>> {
        if let Some(worker) = self.workers.lock().unwrap().get(&address) {
            return Ok(worker.clone());
        }
        let not_found = || io::Error::new(io::ErrorKind::NotFound, "not in the active configuration");
        let config = self.device.active_configuration().map_err(|_| not_found())?;
        let interfaces = self.interfaces.lock().unwrap();
        let (number, kind) = config
            .interfaces()
            .find_map(|group| {
                let alternate = interfaces.get(&group.interface_number()).map_or(0, |i| i.get_alt_setting());
                let alt = group.alt_settings().find(|a| a.alternate_setting() == alternate)?;
                let ep = alt.endpoints().find(|e| e.address() == address)?;
                Some((group.interface_number(), ep.transfer_type()))
            })
            .ok_or_else(not_found)?;
        drop(interfaces);
        let interface = self.interface(number)?;
        let endpoint = match (kind, address & 0x80 != 0) {
            (nusb::descriptors::TransferType::Bulk, true) => Ep::BulkIn(interface.endpoint::<Bulk, In>(address)?),
            (nusb::descriptors::TransferType::Bulk, false) => Ep::BulkOut(interface.endpoint::<Bulk, Out>(address)?),
            (nusb::descriptors::TransferType::Interrupt, true) => {
                Ep::InterruptIn(interface.endpoint::<Interrupt, In>(address)?)
            }
            (nusb::descriptors::TransferType::Interrupt, false) => {
                Ep::InterruptOut(interface.endpoint::<Interrupt, Out>(address)?)
            }
            _ => return Err(io::Error::new(io::ErrorKind::Unsupported, "isochronous or control endpoint")),
        };
        let worker = Worker::start(self.clone(), number, endpoint)?;
        self.workers.lock().unwrap().insert(address, worker.clone());
        Ok(worker)
    }

    fn interface(&self, number: u8) -> io::Result<nusb::Interface> {
        let mut interfaces = self.interfaces.lock().unwrap();
        if let Some(interface) = interfaces.get(&number) {
            return Ok(interface.clone());
        }
        let interface = self.device.claim_interface(number).wait()?;
        interfaces.insert(number, interface.clone());
        Ok(interface)
    }

    fn drop_workers(&self, interface: Option<u8>) {
        let stopped: Vec<_> = {
            let mut workers = self.workers.lock().unwrap();
            let gone: Vec<u8> =
                workers.iter().filter(|(_, w)| interface.is_none_or(|n| w.interface == n)).map(|(&a, _)| a).collect();
            gone.into_iter().filter_map(|a| workers.remove(&a)).collect()
        };
        for worker in stopped {
            worker.stop_and_join();
        }
    }

    fn control_loop(self: Arc<Self>) {
        loop {
            let job = {
                let mut jobs = self.control.jobs.lock().unwrap();
                loop {
                    if jobs.closed {
                        return;
                    }
                    if let Some(job) = jobs.queue.pop_front() {
                        break job;
                    }
                    jobs = self.control.ready.wait(jobs).unwrap();
                }
            };
            self.control(job);
        }
    }

    fn control(&self, job: ControlJob) {
        let [request_type, request, v0, v1, i0, i1, ..] = job.setup;
        let value = u16::from_le_bytes([v0, v1]);
        let index = u16::from_le_bytes([i0, i1]);
        let active = self.device.active_configuration().ok().map(|c| c.configuration_value());
        let result = if request_type == 0 && request == REQ_SET_CONFIGURATION && active == Some(value as u8) {
            Ok(Vec::new())
        } else if request_type == 0 && request == REQ_SET_CONFIGURATION {
            self.drop_workers(None);
            self.interfaces.lock().unwrap().clear();
            self.device.set_configuration(value as u8).wait().map(|()| Vec::new()).map_err(|_| usbip::EPIPE)
        } else if request_type == RECIPIENT_INTERFACE && request == REQ_SET_INTERFACE {
            self.drop_workers(Some(index as u8));
            match self.interface(index as u8) {
                Ok(interface) => interface.set_alt_setting(value as u8).wait().map(|()| Vec::new()).map_err(|_| usbip::EPIPE),
                Err(_) => Err(usbip::EPIPE),
            }
        } else if request_type == RECIPIENT_ENDPOINT && request == REQ_CLEAR_FEATURE && value == FEATURE_ENDPOINT_HALT
            && let Some(worker) = self.workers.lock().unwrap().get(&(index as u8)).cloned()
        {
            let (tx, rx) = mpsc::channel();
            worker.push(Cmd::ClearHalt(tx));
            rx.recv().unwrap_or(Err(usbip::ENODEV)).map(|()| Vec::new())
        } else {
            self.raw_control(&job, request_type, request, value, index)
        };
        match result {
            Ok(data) => self.ret_submit(job.seqnum, 0, data.len(), &data),
            Err(status) => {
                if status == usbip::ENODEV {
                    self.stop();
                }
                self.ret_submit(job.seqnum, status, 0, &[]);
            }
        }
    }

    fn raw_control(&self, job: &ControlJob, request_type: u8, request: u8, value: u16, index: u16) -> Result<Vec<u8>, i32> {
        let control_type = match (request_type >> 5) & 0x03 {
            0 => ControlType::Standard,
            1 => ControlType::Class,
            2 => ControlType::Vendor,
            _ => return Err(usbip::EPIPE),
        };
        let recipient = match request_type & 0x1f {
            0 => Recipient::Device,
            1 => Recipient::Interface,
            2 => Recipient::Endpoint,
            _ => Recipient::Other,
        };
        if request_type & SETUP_IN != 0 {
            let length = job.length.min(usize::from(u16::MAX)) as u16;
            let control = ControlIn { control_type, recipient, request, value, index, length };
            self.device.control_in(control, CONTROL_TIMEOUT).wait().map_err(|e| self.failure(e))
        } else {
            let control = ControlOut { control_type, recipient, request, value, index, data: &job.data };
            self.device.control_out(control, CONTROL_TIMEOUT).wait().map(|()| Vec::new()).map_err(|e| self.failure(e))
        }
    }

    fn present(&self) -> bool {
        let id = self.info.id();
        nusb::list_devices().wait().map(|mut l| l.any(|i| i.id() == id)).unwrap_or(true)
    }

    /// What a failed transfer answers. macOS reports an unplug as some other
    /// error than `Disconnected`, while it still lists the device for a while.
    fn failure(&self, error: TransferError) -> i32 {
        match error {
            TransferError::Stall => return usbip::EPIPE,
            TransferError::Disconnected => return usbip::ENODEV,
            _ => {}
        }
        for _ in 0..GONE_CHECKS {
            if !self.present() {
                return usbip::ENODEV;
            }
            thread::sleep(GONE_CHECK_INTERVAL);
        }
        status_of(error)
    }

    /// Ends the session once the device is no longer listed.
    fn watch(self: Arc<Self>) {
        while !self.stopped.load(Ordering::Acquire) {
            thread::sleep(PRESENCE_POLL);
            if !self.present() {
                eprintln!("[devkit] usb relay: device gone");
                self.stop();
            }
        }
    }
}

enum Cmd {
    Submit { seqnum: u32, length: usize, zlp: bool, data: Vec<u8> },
    Unlink { seqnum: u32, target: u32 },
    ClearHalt(mpsc::Sender<Result<(), i32>>),
}

enum Ep {
    BulkIn(nusb::Endpoint<Bulk, In>),
    BulkOut(nusb::Endpoint<Bulk, Out>),
    InterruptIn(nusb::Endpoint<Interrupt, In>),
    InterruptOut(nusb::Endpoint<Interrupt, Out>),
}

macro_rules! each {
    ($ep:expr, $e:ident => $body:expr) => {
        match $ep {
            Ep::BulkIn($e) => $body,
            Ep::BulkOut($e) => $body,
            Ep::InterruptIn($e) => $body,
            Ep::InterruptOut($e) => $body,
        }
    };
}

impl Ep {
    fn is_in(&self) -> bool {
        matches!(self, Ep::BulkIn(_) | Ep::InterruptIn(_))
    }

    fn max_packet_size(&self) -> usize {
        each!(self, e => e.max_packet_size()).max(1)
    }

    fn pending(&self) -> usize {
        each!(self, e => e.pending())
    }

    fn submit(&mut self, buffer: Buffer) {
        each!(self, e => e.submit(buffer))
    }

    fn cancel_all(&mut self) {
        each!(self, e => e.cancel_all())
    }

    fn poll_next_complete(&mut self, cx: &mut Context<'_>) -> Poll<Completion> {
        each!(self, e => e.poll_next_complete(cx))
    }

    fn clear_halt(&mut self) -> Result<(), nusb::Error> {
        each!(self, e => e.clear_halt().wait())
    }
}

/// A queued transfer, in submission order.
struct Entry {
    seqnum: u32,
    length: usize,
    /// The zero-length packet after a transfer that asked for one.
    trailer: bool,
    /// OUT data, kept so a transfer canceled by another's unlink can go again.
    data: Vec<u8>,
    zlp: bool,
    /// The unlink that canceled it.
    unlinked_by: Option<u32>,
}

struct ParkWaker(Thread);

impl Wake for ParkWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// One endpoint: its transfers run on one thread, which parks between
/// commands and completions.
struct Worker {
    interface: u8,
    commands: Mutex<VecDeque<Cmd>>,
    stop: AtomicBool,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Worker {
    fn start(session: Arc<Session>, interface: u8, endpoint: Ep) -> io::Result<Arc<Self>> {
        let worker = Arc::new(Self {
            interface,
            commands: Mutex::new(VecDeque::new()),
            stop: AtomicBool::new(false),
            thread: Mutex::new(None),
        });
        let w = worker.clone();
        let handle = thread::Builder::new().name("usb-relay-ep".into()).spawn(move || w.run(session, endpoint))?;
        *worker.thread.lock().unwrap() = Some(handle);
        Ok(worker)
    }

    fn push(&self, cmd: Cmd) {
        self.commands.lock().unwrap().push_back(cmd);
        if let Some(handle) = self.thread.lock().unwrap().as_ref() {
            handle.thread().unpark();
        }
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.lock().unwrap().as_ref() {
            handle.thread().unpark();
        }
    }

    fn stop_and_join(&self) {
        self.stop();
        let handle = self.thread.lock().unwrap().take();
        if let Some(handle) = handle
            && handle.thread().id() != thread::current().id()
        {
            let _ = handle.join();
        }
    }

    fn run(&self, session: Arc<Session>, mut ep: Ep) {
        let waker = Waker::from(Arc::new(ParkWaker(thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut entries: VecDeque<Entry> = VecDeque::new();
        let mut held: VecDeque<Cmd> = VecDeque::new();
        let mut again: Vec<Entry> = Vec::new();
        let mut draining = false;
        while !self.stop.load(Ordering::Acquire) && !session.stopped.load(Ordering::Acquire) {
            let mut commands = core::mem::take(&mut *self.commands.lock().unwrap());
            if !draining {
                while let Some(cmd) = held.pop_front() {
                    commands.push_front(cmd);
                }
            }
            let mut worked = !commands.is_empty();
            while let Some(cmd) = commands.pop_front() {
                match cmd {
                    cmd @ (Cmd::Submit { .. } | Cmd::ClearHalt(_)) if draining => held.push_back(cmd),
                    Cmd::ClearHalt(reply) if ep.pending() > 0 => held.push_back(Cmd::ClearHalt(reply)),
                    Cmd::ClearHalt(reply) => drop(reply.send(ep.clear_halt().map_err(|_| usbip::EPIPE))),
                    Cmd::Submit { seqnum, length, zlp, data } => {
                        Self::begin(&mut ep, &mut entries, Entry { seqnum, length, trailer: false, data, zlp, unlinked_by: None });
                    }
                    Cmd::Unlink { seqnum, target } => {
                        match entries.iter_mut().find(|e| e.seqnum == target && !e.trailer) {
                            Some(entry) => {
                                entry.unlinked_by = Some(seqnum);
                                if !draining {
                                    draining = true;
                                    ep.cancel_all();
                                }
                            }
                            None => session.ret_unlink(seqnum, 0),
                        }
                    }
                }
            }
            while ep.pending() > 0 {
                let Poll::Ready(done) = ep.poll_next_complete(&mut cx) else { break };
                worked = true;
                let Some(entry) = entries.pop_front() else { continue };
                if entry.trailer {
                    continue;
                }
                if draining {
                    if let Some(unlink) = entry.unlinked_by {
                        session.ret_unlink(unlink, usbip::ECONNRESET);
                    } else if done.status.is_ok() || done.actual_len > 0 {
                        Self::reply(&session, &ep, &entry, &done, done.status.is_err());
                    } else {
                        again.push(entry);
                    }
                } else {
                    Self::reply(&session, &ep, &entry, &done, false);
                }
            }
            if draining && ep.pending() == 0 {
                draining = false;
                for entry in again.drain(..) {
                    Self::begin(&mut ep, &mut entries, entry);
                }
                worked = true;
            }
            if !worked {
                thread::park();
            }
        }
    }

    fn begin(ep: &mut Ep, entries: &mut VecDeque<Entry>, entry: Entry) {
        let mps = ep.max_packet_size();
        if ep.is_in() {
            ep.submit(Buffer::new(entry.length.next_multiple_of(mps).max(mps)));
        } else {
            let mut buffer = Buffer::new(entry.data.len());
            buffer.extend_from_slice(&entry.data);
            ep.submit(buffer);
        }
        let trailer = !ep.is_in() && entry.zlp && entry.length > 0 && entry.length % mps == 0;
        entries.push_back(entry);
        if trailer {
            ep.submit(Buffer::new(0));
            entries.push_back(Entry { seqnum: 0, length: 0, trailer: true, data: Vec::new(), zlp: false, unlinked_by: None });
        }
    }

    /// `partial`: a transfer cut short by another's unlink, given back as
    /// done with what it moved.
    fn reply(session: &Session, ep: &Ep, entry: &Entry, done: &Completion, partial: bool) {
        let status = match done.status {
            Ok(()) => 0,
            Err(_) if partial => 0,
            Err(e) => session.failure(e),
        };
        if status == usbip::ENODEV {
            session.stop();
        }
        if !ep.is_in() {
            session.ret_submit(entry.seqnum, status, done.actual_len, &[]);
            return;
        }
        let got = done.actual_len.min(done.buffer.len());
        let (status, actual) = if got > entry.length { (usbip::EOVERFLOW, entry.length) } else { (status, got) };
        session.ret_submit(entry.seqnum, status, actual, &done.buffer[..actual]);
    }
}
