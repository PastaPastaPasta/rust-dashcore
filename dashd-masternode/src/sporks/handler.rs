//! The `spork` and `getsporks` branches of Core's `ProcessMessage`, and `MSG_SPORK` in
//! `AlreadyHave` and `ProcessGetData`, as P2a-core's registry handler for SP0's rows.

use std::sync::Arc;

use dash_evo::spork::{SporkHash, spork_hash};
use dash_num::Hash256;
use dash_p2p::CommandString;
use dash_p2p::policy::Throw;
use dash_p2p_core::{InvType, Inventory, Spork};
use dash_types::Numeric;
use dashd_p2p::processing::logic::out_msg;
use dashd_p2p::{HandlerCx, NetHandler};

use super::SporkManager;

const COMMANDS: [CommandString; 2] = [CommandString::SPORK, CommandString::GETSPORKS];

/// SP0's registry handler. The object-request completions and the relay go through the
/// handler context, which P2a-core applies when the handler returns (P5a D13).
pub struct SporkHandler {
    sporks: Arc<SporkManager>,
}

fn spork_inv(hash: SporkHash) -> Inventory {
    Inventory {
        inv_type: InvType::Spork,
        hash: Hash256::from_lendian(hash),
    }
}

impl SporkHandler {
    pub fn new(sporks: Arc<SporkManager>) -> SporkHandler {
        SporkHandler {
            sporks,
        }
    }

    // Core: net_processing.cpp:5688-5711
    fn on_spork(&mut self, cx: &mut HandlerCx<'_>, payload: &[u8]) {
        let id = cx.id();
        let mut data = payload;
        let msg: Spork = match cx.read(&mut data) {
            Ok(msg) => msg,
            Err(e) => {
                cx.misbehaving(
                    100,
                    &format!("malformed spork received. peer={id} error={}", e.what),
                );
                return;
            }
        };
        let inv = spork_inv(spork_hash(&msg));
        cx.received_response(inv);
        if self.sporks.is_valid_spork(&msg).is_err() {
            cx.misbehaving(100, &format!("invalid spork received. peer={id}"));
            return;
        }
        if self.sporks.process_spork(msg, Some(id)).relays() {
            cx.relay_inv(inv);
        }
    }

    /// v24 answers a peer's repeated `getsporks` only when the active set changed since its last
    /// reply (P2P-14).
    // Core: net_processing.cpp:5713-5737
    fn on_getsporks(&mut self, cx: &mut HandlerCx<'_>) {
        let active = self.sporks.active_sporks();
        // Core sorts the `uint256`s, which compare as their bytes.
        let mut hashes: Vec<SporkHash> = active.iter().map(spork_hash).collect();
        hashes.sort_unstable();
        if cx.state.getsporks_last_response.as_ref() == Some(&hashes) {
            tracing::debug!(target: "net", "Ignoring repeated \"getsporks\". peer={}", cx.id());
            return;
        }
        cx.state.getsporks_last_response = Some(hashes);
        for msg in &active {
            cx.push(out_msg(CommandString::SPORK, msg));
        }
    }
}

impl NetHandler for SporkHandler {
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        match *cmd {
            CommandString::SPORK => self.on_spork(cx, payload),
            CommandString::GETSPORKS => self.on_getsporks(cx),
            _ => unreachable!("the registry routes only {COMMANDS:?} here"),
        }
        Ok(())
    }

    // Core: net_processing.cpp:2421-2423 AlreadyHave
    fn already_have(&self, inv: &Inventory) -> bool {
        inv.inv_type == InvType::Spork
            && self.sporks.spork_by_hash(&inv.hash.to_lendian()).is_some()
    }

    // Core: net_processing.cpp:3017-3022 ProcessGetData
    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        if inv.inv_type != InvType::Spork {
            return false;
        }
        match self.sporks.spork_by_hash(&inv.hash.to_lendian()) {
            Some(msg) => {
                cx.push(out_msg(CommandString::SPORK, &msg));
                true
            }
            None => false,
        }
    }
}
