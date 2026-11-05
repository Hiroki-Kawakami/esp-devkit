/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

pub mod request_type {
    pub const IN: u8 = 0x80;
    pub const STANDARD: u8 = 0x00;
    pub const CLASS: u8 = 0x20;
    pub const VENDOR: u8 = 0x40;
    pub const DEVICE: u8 = 0x00;
    pub const INTERFACE: u8 = 0x01;
    pub const ENDPOINT: u8 = 0x02;
}

pub mod request {
    pub const GET_STATUS: u8 = 0x00;
    pub const CLEAR_FEATURE: u8 = 0x01;
    pub const SET_ADDRESS: u8 = 0x05;
    pub const GET_DESCRIPTOR: u8 = 0x06;
    pub const SET_CONFIGURATION: u8 = 0x09;
    pub const SET_INTERFACE: u8 = 0x0b;
}

const FEATURE_ENDPOINT_HALT: u16 = 0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    pub fn bytes(&self) -> [u8; 8] {
        let [v0, v1] = self.value.to_le_bytes();
        let [i0, i1] = self.index.to_le_bytes();
        let [l0, l1] = self.length.to_le_bytes();
        [self.request_type, self.request, v0, v1, i0, i1, l0, l1]
    }

    pub fn is_in(&self) -> bool {
        self.request_type & request_type::IN != 0
    }

    pub const fn get_descriptor(kind: u8, index: u8, language: u16, length: u16) -> Self {
        Self {
            request_type: request_type::IN,
            request: request::GET_DESCRIPTOR,
            value: (kind as u16) << 8 | index as u16,
            index: language,
            length,
        }
    }

    pub const fn set_interface(interface: u8, alternate: u8) -> Self {
        Self {
            request_type: request_type::INTERFACE,
            request: request::SET_INTERFACE,
            value: alternate as u16,
            index: interface as u16,
            length: 0,
        }
    }

    pub(crate) const fn set_address(address: u8) -> Self {
        Self { request_type: 0, request: request::SET_ADDRESS, value: address as u16, index: 0, length: 0 }
    }

    pub(crate) const fn set_configuration(value: u8) -> Self {
        Self { request_type: 0, request: request::SET_CONFIGURATION, value: value as u16, index: 0, length: 0 }
    }

    pub(crate) const fn clear_halt(endpoint: u8) -> Self {
        Self {
            request_type: request_type::ENDPOINT,
            request: request::CLEAR_FEATURE,
            value: FEATURE_ENDPOINT_HALT,
            index: endpoint as u16,
            length: 0,
        }
    }
}
