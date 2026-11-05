/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::string::String;

pub const DEVICE: u8 = 0x01;
pub const CONFIGURATION: u8 = 0x02;
pub const STRING: u8 = 0x03;
pub const INTERFACE: u8 = 0x04;
pub const ENDPOINT: u8 = 0x05;
pub const INTERFACE_ASSOCIATION: u8 = 0x0b;

pub const DEVICE_BYTES: usize = 18;
pub const CONFIGURATION_BYTES: usize = 9;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TransferType {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceDescriptor {
    pub usb: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet_size0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device: u16,
    pub manufacturer: u8,
    pub product: u8,
    pub serial: u8,
    pub configurations: u8,
}

impl DeviceDescriptor {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < DEVICE_BYTES || b[1] != DEVICE {
            return None;
        }
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        Some(Self {
            usb: u16_at(2),
            class: b[4],
            subclass: b[5],
            protocol: b[6],
            max_packet_size0: b[7],
            vendor_id: u16_at(8),
            product_id: u16_at(10),
            device: u16_at(12),
            manufacturer: b[14],
            product: b[15],
            serial: b[16],
            configurations: b[17],
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Raw<'a> {
    pub kind: u8,
    pub bytes: &'a [u8],
}

/// Stops at the first descriptor whose length does not fit.
#[derive(Clone)]
pub struct Descriptors<'a> {
    rest: &'a [u8],
}

impl<'a> Descriptors<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }
}

impl<'a> Iterator for Descriptors<'a> {
    type Item = Raw<'a>;

    fn next(&mut self) -> Option<Raw<'a>> {
        let len = usize::from(*self.rest.first()?);
        if len < 2 || len > self.rest.len() {
            self.rest = &[];
            return None;
        }
        let (bytes, rest) = self.rest.split_at(len);
        self.rest = rest;
        Some(Raw { kind: bytes[1], bytes })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Configuration<'a> {
    bytes: &'a [u8],
}

impl<'a> Configuration<'a> {
    /// `bytes` is the whole configuration as wTotalLength gives it.
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        if bytes.len() < CONFIGURATION_BYTES || bytes[0] < 9 || bytes[1] != CONFIGURATION {
            return None;
        }
        let total = usize::from(u16::from_le_bytes([bytes[2], bytes[3]]));
        if total < CONFIGURATION_BYTES || total > bytes.len() {
            return None;
        }
        Some(Self { bytes: &bytes[..total] })
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn interfaces_count(&self) -> u8 {
        self.bytes[4]
    }

    pub fn value(&self) -> u8 {
        self.bytes[5]
    }

    pub fn attributes(&self) -> u8 {
        self.bytes[7]
    }

    pub fn max_power(&self) -> u8 {
        self.bytes[8]
    }

    /// Every descriptor after the configuration descriptor itself.
    pub fn descriptors(&self) -> Descriptors<'a> {
        Descriptors::new(&self.bytes[usize::from(self.bytes[0])..])
    }

    /// Every alternate setting of every interface, in descriptor order.
    pub fn interfaces(&self) -> Interfaces<'a> {
        Interfaces { descriptors: self.descriptors() }
    }

    pub fn interface(&self, number: u8, alternate: u8) -> Option<InterfaceDescriptor<'a>> {
        self.interfaces().find(|i| i.number == number && i.alternate == alternate)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InterfaceDescriptor<'a> {
    pub number: u8,
    pub alternate: u8,
    pub endpoints_count: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub string: u8,
    /// The descriptors that follow it up to the next interface or
    /// interface association: endpoints and class-specific ones.
    pub extra: &'a [u8],
}

impl<'a> InterfaceDescriptor<'a> {
    pub fn descriptors(&self) -> Descriptors<'a> {
        Descriptors::new(self.extra)
    }

    pub fn endpoints(&self) -> impl Iterator<Item = EndpointDescriptor> + 'a {
        self.descriptors().filter_map(|d| EndpointDescriptor::parse(d.bytes))
    }
}

pub struct Interfaces<'a> {
    descriptors: Descriptors<'a>,
}

impl<'a> Iterator for Interfaces<'a> {
    type Item = InterfaceDescriptor<'a>;

    fn next(&mut self) -> Option<InterfaceDescriptor<'a>> {
        let b = loop {
            let d = self.descriptors.next()?;
            if d.kind == INTERFACE && d.bytes.len() >= 9 {
                break d.bytes;
            }
        };
        let rest = self.descriptors.rest;
        let mut extra_len = 0;
        for d in Descriptors::new(rest) {
            if d.kind == INTERFACE || d.kind == INTERFACE_ASSOCIATION {
                break;
            }
            extra_len += d.bytes.len();
        }
        Some(InterfaceDescriptor {
            number: b[2],
            alternate: b[3],
            endpoints_count: b[4],
            class: b[5],
            subclass: b[6],
            protocol: b[7],
            string: b[8],
            extra: &rest[..extra_len],
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EndpointDescriptor {
    pub address: u8,
    pub attributes: u8,
    pub max_packet_size: u16,
    pub interval: u8,
}

impl EndpointDescriptor {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 7 || b[1] != ENDPOINT {
            return None;
        }
        Some(Self {
            address: b[2],
            attributes: b[3],
            max_packet_size: u16::from_le_bytes([b[4], b[5]]),
            interval: b[6],
        })
    }

    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }

    pub fn transfer_type(&self) -> TransferType {
        match self.attributes & 0x03 {
            0 => TransferType::Control,
            1 => TransferType::Isochronous,
            2 => TransferType::Bulk,
            _ => TransferType::Interrupt,
        }
    }

    /// Bytes per transaction, without the extra transactions per microframe.
    pub fn max_packet_bytes(&self) -> u16 {
        self.max_packet_size & 0x07ff
    }

    /// Extra transactions per microframe of a high-speed periodic endpoint.
    pub fn mult(&self) -> u8 {
        ((self.max_packet_size >> 11) & 0x03) as u8
    }
}

/// A string descriptor's UTF-16LE text, trailing spaces and NULs trimmed.
pub fn string(desc: &[u8]) -> String {
    if desc.len() < 2 || desc[1] != STRING {
        return String::new();
    }
    let end = usize::from(desc[0]).min(desc.len());
    let units = desc[2.min(end)..end].chunks_exact(2).map(|u| u16::from_le_bytes([u[0], u[1]]));
    let mut out: String = char::decode_utf16(units).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect();
    let trimmed = out.trim_end_matches([' ', '\0']).len();
    out.truncate(trimmed);
    out
}
