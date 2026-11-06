/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * USB/IP messages, big-endian on the wire; compiled into both this crate and
 * tools/devkit.
 */

pub const VERSION: u16 = 0x0111;
pub const OP_REQ_DEVLIST: u16 = 0x8005;
pub const OP_REP_DEVLIST: u16 = 0x0005;
pub const OP_REQ_IMPORT: u16 = 0x8003;
pub const OP_REP_IMPORT: u16 = 0x0003;

pub const CMD_SUBMIT: u32 = 1;
pub const CMD_UNLINK: u32 = 2;
pub const RET_SUBMIT: u32 = 3;
pub const RET_UNLINK: u32 = 4;

pub const DIR_OUT: u32 = 0;
pub const DIR_IN: u32 = 1;

pub const OP_HEADER_BYTES: usize = 8;
pub const BUSID_BYTES: usize = 32;
pub const PATH_BYTES: usize = 256;
pub const DEVICE_BYTES: usize = PATH_BYTES + BUSID_BYTES + 24;
pub const HEADER_BYTES: usize = 48;
pub const ISO_PACKET_BYTES: usize = 16;

pub const URB_SHORT_NOT_OK: u32 = 0x0001;
pub const URB_ZERO_PACKET: u32 = 0x0040;
/// `packets` of a transfer that is not isochronous.
pub const NO_ISO_PACKETS: u32 = 0xffff_ffff;

pub const SPEED_LOW: u32 = 1;
pub const SPEED_FULL: u32 = 2;
pub const SPEED_HIGH: u32 = 3;

/// Linux errno values the status fields carry, negated.
pub const ENODEV: i32 = -19;
pub const EINVAL: i32 = -22;
pub const EPIPE: i32 = -32;
pub const EPROTO: i32 = -71;
pub const EOVERFLOW: i32 = -75;
pub const ECONNRESET: i32 = -104;
pub const ESHUTDOWN: i32 = -108;

fn get16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn get32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_be_bytes());
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OpHeader {
    pub code: u16,
    pub status: u32,
}

impl OpHeader {
    pub fn encode(&self) -> [u8; OP_HEADER_BYTES] {
        let mut b = [0; OP_HEADER_BYTES];
        put16(&mut b, 0, VERSION);
        put16(&mut b, 2, self.code);
        put32(&mut b, 4, self.status);
        b
    }

    /// `None` for another protocol version.
    pub fn decode(b: &[u8; OP_HEADER_BYTES]) -> Option<Self> {
        (get16(b, 0) == VERSION).then(|| Self { code: get16(b, 2), status: get32(b, 4) })
    }
}

/// `None` when `busid` does not fit.
pub fn import_request(busid: &[u8]) -> Option<[u8; OP_HEADER_BYTES + BUSID_BYTES]> {
    if busid.len() >= BUSID_BYTES {
        return None;
    }
    let mut b = [0; OP_HEADER_BYTES + BUSID_BYTES];
    b[..OP_HEADER_BYTES].copy_from_slice(&OpHeader { code: OP_REQ_IMPORT, status: 0 }.encode());
    b[OP_HEADER_BYTES..OP_HEADER_BYTES + busid.len()].copy_from_slice(busid);
    Some(b)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceRecord {
    pub path: [u8; PATH_BYTES],
    pub busid: [u8; BUSID_BYTES],
    pub busnum: u32,
    pub devnum: u32,
    pub speed: u32,
    pub vendor_id: u16,
    pub product_id: u16,
    pub bcd_device: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub configuration_value: u8,
    pub configurations: u8,
    pub interfaces: u8,
}

impl DeviceRecord {
    /// What a submitted URB names the device by.
    pub fn devid(&self) -> u32 {
        self.busnum << 16 | (self.devnum & 0xffff)
    }

    pub fn encode(&self) -> [u8; DEVICE_BYTES] {
        let mut b = [0; DEVICE_BYTES];
        b[..PATH_BYTES].copy_from_slice(&self.path);
        b[PATH_BYTES..PATH_BYTES + BUSID_BYTES].copy_from_slice(&self.busid);
        let at = PATH_BYTES + BUSID_BYTES;
        put32(&mut b, at, self.busnum);
        put32(&mut b, at + 4, self.devnum);
        put32(&mut b, at + 8, self.speed);
        put16(&mut b, at + 12, self.vendor_id);
        put16(&mut b, at + 14, self.product_id);
        put16(&mut b, at + 16, self.bcd_device);
        b[at + 18..at + 24].copy_from_slice(&[
            self.class,
            self.subclass,
            self.protocol,
            self.configuration_value,
            self.configurations,
            self.interfaces,
        ]);
        b
    }

    pub fn decode(b: &[u8; DEVICE_BYTES]) -> Self {
        let at = PATH_BYTES + BUSID_BYTES;
        Self {
            path: b[..PATH_BYTES].try_into().unwrap(),
            busid: b[PATH_BYTES..at].try_into().unwrap(),
            busnum: get32(b, at),
            devnum: get32(b, at + 4),
            speed: get32(b, at + 8),
            vendor_id: get16(b, at + 12),
            product_id: get16(b, at + 14),
            bcd_device: get16(b, at + 16),
            class: b[at + 18],
            subclass: b[at + 19],
            protocol: b[at + 20],
            configuration_value: b[at + 21],
            configurations: b[at + 22],
            interfaces: b[at + 23],
        }
    }
}

/// The fields every command and reply starts with; replies carry zeros in
/// all but `seqnum`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Basic {
    pub seqnum: u32,
    pub devid: u32,
    pub direction: u32,
    pub endpoint: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Header {
    /// OUT data follows, then `packets` isochronous descriptors.
    Submit { basic: Basic, flags: u32, length: u32, start_frame: i32, packets: u32, interval: u32, setup: [u8; 8] },
    Unlink { basic: Basic, target: u32 },
    /// IN data follows, then `packets` isochronous descriptors.
    RetSubmit { basic: Basic, status: i32, actual: u32, start_frame: i32, packets: u32, errors: u32 },
    RetUnlink { basic: Basic, status: i32 },
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut b = [0; HEADER_BYTES];
        let (command, basic) = match self {
            Self::Submit { basic, .. } => (CMD_SUBMIT, basic),
            Self::Unlink { basic, .. } => (CMD_UNLINK, basic),
            Self::RetSubmit { basic, .. } => (RET_SUBMIT, basic),
            Self::RetUnlink { basic, .. } => (RET_UNLINK, basic),
        };
        put32(&mut b, 0, command);
        put32(&mut b, 4, basic.seqnum);
        put32(&mut b, 8, basic.devid);
        put32(&mut b, 12, basic.direction);
        put32(&mut b, 16, basic.endpoint);
        match *self {
            Self::Submit { flags, length, start_frame, packets, interval, setup, .. } => {
                put32(&mut b, 20, flags);
                put32(&mut b, 24, length);
                put32(&mut b, 28, start_frame as u32);
                put32(&mut b, 32, packets);
                put32(&mut b, 36, interval);
                b[40..48].copy_from_slice(&setup);
            }
            Self::Unlink { target, .. } => put32(&mut b, 20, target),
            Self::RetSubmit { status, actual, start_frame, packets, errors, .. } => {
                put32(&mut b, 20, status as u32);
                put32(&mut b, 24, actual);
                put32(&mut b, 28, start_frame as u32);
                put32(&mut b, 32, packets);
                put32(&mut b, 36, errors);
            }
            Self::RetUnlink { status, .. } => put32(&mut b, 20, status as u32),
        }
        b
    }

    pub fn decode(b: &[u8; HEADER_BYTES]) -> Option<Self> {
        let basic = Basic { seqnum: get32(b, 4), devid: get32(b, 8), direction: get32(b, 12), endpoint: get32(b, 16) };
        Some(match get32(b, 0) {
            CMD_SUBMIT => Self::Submit {
                basic,
                flags: get32(b, 20),
                length: get32(b, 24),
                start_frame: get32(b, 28) as i32,
                packets: get32(b, 32),
                interval: get32(b, 36),
                setup: b[40..48].try_into().unwrap(),
            },
            CMD_UNLINK => Self::Unlink { basic, target: get32(b, 20) },
            RET_SUBMIT => Self::RetSubmit {
                basic,
                status: get32(b, 20) as i32,
                actual: get32(b, 24),
                start_frame: get32(b, 28) as i32,
                packets: get32(b, 32),
                errors: get32(b, 36),
            },
            RET_UNLINK => Self::RetUnlink { basic, status: get32(b, 20) as i32 },
            _ => return None,
        })
    }
}
