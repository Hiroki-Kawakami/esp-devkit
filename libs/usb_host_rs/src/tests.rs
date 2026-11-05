/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::Setup;
use crate::desc::{self, Configuration, DeviceDescriptor, TransferType};
use crate::usbip::{self, Basic, DeviceRecord, Header, OpHeader};

const DEVICE: [u8; 18] = [18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x09, 0x12, 0x01, 0x00, 0x00, 0x01, 1, 2, 3, 1];

// Configuration, IAD, a vendor interface with a class-specific descriptor and
// two bulk endpoints, then an interface with two alternates, the second
// with an endpoint descriptor too short to be one.
const CONFIG: [u8; 66] = [
    9, 2, 66, 0, 2, 1, 0, 0x80, 50,
    8, 0x0b, 0, 1, 0xff, 0, 0, 0,
    9, 4, 0, 0, 2, 0xff, 0x42, 0x01, 4,
    4, 0x24, 0xaa, 0xbb,
    7, 5, 0x81, 2, 0x00, 0x02, 0,
    7, 5, 0x02, 2, 0x00, 0x02, 0,
    9, 4, 1, 0, 0, 0x01, 0x02, 0, 0,
    9, 4, 1, 1, 1, 0x01, 0x02, 0, 0,
    4, 5, 0x83, 1,
];

#[test]
fn device_descriptor() {
    let d = DeviceDescriptor::parse(&DEVICE).unwrap();
    assert_eq!((d.usb, d.max_packet_size0, d.vendor_id, d.product_id), (0x0200, 64, 0x1209, 0x0001));
    assert_eq!((d.manufacturer, d.product, d.serial, d.configurations), (1, 2, 3, 1));
    assert_eq!(DeviceDescriptor::parse(&DEVICE[..17]), None);
}

#[test]
fn configuration_walk() {
    let c = Configuration::parse(&CONFIG).unwrap();
    assert_eq!((c.value(), c.interfaces_count(), c.max_power()), (1, 2, 50));
    let interfaces: Vec<_> = c.interfaces().map(|i| (i.number, i.alternate, i.class)).collect();
    assert_eq!(interfaces, [(0, 0, 0xff), (1, 0, 0x01), (1, 1, 0x01)]);

    let vendor = c.interface(0, 0).unwrap();
    assert_eq!((vendor.subclass, vendor.protocol, vendor.string), (0x42, 0x01, 4));
    let kinds: Vec<u8> = vendor.descriptors().map(|d| d.kind).collect();
    assert_eq!(kinds, [0x24, desc::ENDPOINT, desc::ENDPOINT]);
    let eps: Vec<_> = vendor.endpoints().map(|e| (e.address, e.is_in(), e.transfer_type(), e.max_packet_bytes())).collect();
    assert_eq!(eps, [(0x81, true, TransferType::Bulk, 512), (0x02, false, TransferType::Bulk, 512)]);

    let alt = c.interface(1, 1).unwrap();
    assert_eq!(alt.extra, &[4, 5, 0x83, 1]);
    assert_eq!(alt.endpoints().count(), 0);
    assert_eq!(c.interface(1, 0).unwrap().extra, &[] as &[u8]);
    assert_eq!(c.interface(2, 0), None);
}

#[test]
fn configuration_rejects_bad_length() {
    let mut short = CONFIG;
    short[2] = 200;
    assert_eq!(Configuration::parse(&short), None);
    assert_eq!(Configuration::parse(&CONFIG[..8]), None);
}

#[test]
fn endpoint_mult() {
    let ep = desc::EndpointDescriptor::parse(&[7, 5, 0x81, 1, 0x00, 0x14, 1]).unwrap();
    assert_eq!((ep.transfer_type(), ep.max_packet_bytes(), ep.mult()), (TransferType::Isochronous, 1024, 2));
}

#[test]
fn string_descriptor() {
    assert_eq!(desc::string(&[10, 3, b'A', 0, b'B', 0, b' ', 0, 0, 0]), "AB");
    assert_eq!(desc::string(&[8, 3, 0x3d, 0xd8, 0x00, 0xde, b'x', 0]), "\u{1f600}x");
    assert_eq!(desc::string(&[4, 2, b'A', 0]), "");
    assert_eq!(desc::string(&[200, 3, b'A', 0]), "A");
}

#[test]
fn setup_bytes() {
    let s = Setup::get_descriptor(desc::STRING, 2, 0x0409, 255);
    assert_eq!(s.bytes(), [0x80, 6, 2, 3, 0x09, 0x04, 255, 0]);
    assert!(s.is_in());
    assert!(!Setup::set_address(5).is_in());
}

#[test]
fn usbip_headers_round_trip() {
    let basic = Basic { seqnum: 7, devid: 0x0001_0002, direction: usbip::DIR_IN, endpoint: 1 };
    for h in [
        Header::Submit {
            basic,
            flags: usbip::URB_ZERO_PACKET,
            length: 512,
            start_frame: -1,
            packets: usbip::NO_ISO_PACKETS,
            interval: 0,
            setup: [1, 2, 3, 4, 5, 6, 7, 8],
        },
        Header::Unlink { basic, target: 6 },
        Header::RetSubmit { basic, status: usbip::EPIPE, actual: 3, start_frame: 0, packets: 0, errors: 0 },
        Header::RetUnlink { basic, status: usbip::ECONNRESET },
    ] {
        assert_eq!(Header::decode(&h.encode()), Some(h));
    }
    assert_eq!(Header::decode(&[0; usbip::HEADER_BYTES]), None);
}

#[test]
fn usbip_submit_layout() {
    let h = Header::Submit {
        basic: Basic { seqnum: 0x0102_0304, devid: 0x0001_0002, direction: usbip::DIR_OUT, endpoint: 2 },
        flags: 0,
        length: 0x200,
        start_frame: 0,
        packets: usbip::NO_ISO_PACKETS,
        interval: 0,
        setup: [0; 8],
    }
    .encode();
    assert_eq!(&h[..8], &[0, 0, 0, 1, 1, 2, 3, 4]);
    assert_eq!(&h[8..20], &[0, 1, 0, 2, 0, 0, 0, 0, 0, 0, 0, 2]);
    assert_eq!(&h[24..28], &[0, 0, 2, 0]);
    assert_eq!(&h[32..36], &[0xff; 4]);
}

#[test]
fn usbip_import() {
    let req = usbip::import_request(b"1-1").unwrap();
    assert_eq!(&req[..8], &[0x01, 0x11, 0x80, 0x03, 0, 0, 0, 0]);
    assert_eq!(&req[8..12], b"1-1\0");
    assert_eq!(usbip::import_request(&[b'x'; 32]), None);

    let mut record = DeviceRecord {
        path: [0; usbip::PATH_BYTES],
        busid: [0; usbip::BUSID_BYTES],
        busnum: 1,
        devnum: 2,
        speed: usbip::SPEED_HIGH,
        vendor_id: 0x1209,
        product_id: 0x0001,
        bcd_device: 0x0100,
        class: 0,
        subclass: 0,
        protocol: 0,
        configuration_value: 1,
        configurations: 1,
        interfaces: 1,
    };
    record.busid[..3].copy_from_slice(b"1-1");
    let bytes = record.encode();
    assert_eq!(&bytes[288..300], &[0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3]);
    assert_eq!(DeviceRecord::decode(&bytes), record);
    assert_eq!(record.devid(), 0x0001_0002);

    let op = OpHeader { code: usbip::OP_REP_IMPORT, status: 0 };
    assert_eq!(OpHeader::decode(&op.encode()), Some(op));
    assert_eq!(OpHeader::decode(&[0x01, 0x06, 0, 3, 0, 0, 0, 0]), None);
}
