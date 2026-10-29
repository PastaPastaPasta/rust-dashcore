//! `-capturemessages`: the hook Core's `CaptureMessage` is (P4a D10). NetCore calls it for each
//! message pushed and each message taken for processing; the node's file writer is
//! `dashd_net::capture`.

use core::fmt;

use crate::netaddr::Address;

/// Where captured messages go (Core's `CaptureMessage` function object).
pub trait MessageCapture: Send + Sync + fmt::Debug {
    /// One message to or from the peer at `addr` (Core's `pnode->addr`) at the core's mockable
    /// time `time_us` (Core's `GetTime<std::chrono::microseconds>()`): its type and payload.
    /// Called on the pushing thread for sends and on the processing thread for receives, as
    /// Core does.
    fn capture(&self, time_us: i64, addr: &Address, cmd: &str, payload: &[u8], incoming: bool);
}
