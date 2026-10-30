// Shared with the firmware side, which uses parts the host does not.
#[allow(dead_code)]
#[path = "../../../libs/sim_device/src/protocol.rs"]
mod shared;

pub use shared::*;
