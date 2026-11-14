// Shared with the firmware side, which uses parts the host does not.
#[allow(dead_code)]
#[path = "../../../libs/usb_host_rs/src/usbip.rs"]
mod shared;

pub use shared::*;
