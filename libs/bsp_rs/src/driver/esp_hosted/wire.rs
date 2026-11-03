/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * esp-hosted 2.12 SDIO wire format (libs/esp_hosted_enhanced/inc/hosted_wire.h).
 */

pub(super) const HEADER_LEN: usize = 12;
pub(super) const BUF_SIZE: usize = 1536;
pub(super) const MAX_PAYLOAD: usize = BUF_SIZE - HEADER_LEN;
pub(super) const MAX_SERIAL: usize = 8192;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(super) enum If {
    Sta = 1,
    Serial = 3,
    Hci = 4,
    Priv = 5,
    Test = 6,
    Log = 8,
}

impl If {
    pub(super) fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Sta,
            3 => Self::Serial,
            4 => Self::Hci,
            5 => Self::Priv,
            6 => Self::Test,
            8 => Self::Log,
            _ => return None,
        })
    }
}

pub(super) const FLAG_MORE_FRAGMENT: u8 = 1 << 0;

pub(super) const H2S_OPEN_DATA_PATH: u8 = 0;

pub(super) const PRIV_EVENT_INIT: u8 = 0x22;
pub(super) const PRIV_EVENT_EXT_CTRL: u8 = 0x23;

pub(super) const TLV_CAPABILITY: u8 = 0x11;
pub(super) const TLV_CHIP_ID: u8 = 0x12;
pub(super) const TLV_FW_VERSION: u8 = 0x17;
pub(super) const TLV_EXT_CAPS: u8 = 0x30;

pub(super) const TLV_HOST_CAPABILITIES: u8 = 0x44;
pub(super) const TLV_RCVD_CHIP_ID: u8 = 0x45;
pub(super) const TLV_SLV_TEST_RAW_TP: u8 = 0x46;
pub(super) const TLV_THROTTLE_HIGH: u8 = 0x47;
pub(super) const TLV_THROTTLE_LOW: u8 = 0x48;

pub(super) const TLV_CTRL_LOG: u8 = 0x01;

pub(super) const CAP_BT_SDIO: u8 = 1 << 2;

pub(super) const EXT_CAP_LOG: u32 = 1 << 0;
pub(super) const EXT_CAP_TEST: u32 = 1 << 1;
pub(super) const EXT_CAP_RX_AGGR: u32 = 1 << 2;

pub(super) const HCI_CMD: u8 = 0x01;
pub(super) const HCI_ACL: u8 = 0x02;
pub(super) const HCI_EVT: u8 = 0x04;

pub(super) const SERIAL_TLV_EP: u8 = 0x01;
pub(super) const SERIAL_TLV_DATA: u8 = 0x02;
pub(super) const SERIAL_EP_RSP: &[u8; 6] = b"RPCRsp";

pub(super) const CHIP_ID_C6: u8 = 0x0D;

#[derive(Clone, Copy, Debug)]
pub(super) struct Header {
    pub if_type: u8,
    pub flags: u8,
    pub len: u16,
    pub offset: u16,
    pub pkt_type: u8,
}

impl Header {
    pub(super) fn write(&self, out: &mut [u8]) {
        out[..HEADER_LEN].fill(0);
        out[0] = self.if_type & 0x0f;
        out[1] = self.flags;
        out[2..4].copy_from_slice(&self.len.to_le_bytes());
        out[4..6].copy_from_slice(&self.offset.to_le_bytes());
        out[11] = self.pkt_type;
    }

    pub(super) fn read(data: &[u8]) -> Self {
        Self {
            if_type: data[0] & 0x0f,
            flags: data[1],
            len: u16::from_le_bytes([data[2], data[3]]),
            offset: u16::from_le_bytes([data[4], data[5]]),
            pkt_type: data[11],
        }
    }
}
