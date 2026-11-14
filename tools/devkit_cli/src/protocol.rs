// Shared with the firmware side, which uses parts the host does not.
#[allow(dead_code)]
#[path = "../../../libs/bsp_rs/src/sim/protocol.rs"]
mod shared;

pub use shared::*;
