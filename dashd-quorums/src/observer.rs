//! Core's observer-mode `NetDKG` (`-watchquorums` without a masternode key) as a registry
//! handler over QM1b-5's pure answers (`dash_llmq::qdata::observer`).

use std::sync::Arc;

use dash_llmq::qdata::observer::{
    QCOMPLAINT, QCONTRIB, QJUSTIFICATION, QPCOMMITMENT, QWATCH, observer_already_have, observer_dkg,
};
use dash_p2p::CommandString;
use dash_p2p::policy::Throw;
use dash_p2p_core::Inventory;
use dashd_p2p::processing::registry::CoreHandler;
use dashd_p2p::{HandlerCx, NetHandler};
use tracing::debug;

const COMMANDS: [CommandString; 5] = [
    CommandString::QCONTRIB,
    CommandString::QCOMPLAINT,
    CommandString::QJUSTIFY,
    CommandString::QPCOMMIT,
    CommandString::QWATCH,
];

/// The observer's `NetDKG`. `spork17` is `IsQuorumDKGEnabled(m_sporkman)`.
pub struct ObserverNetDkg {
    spork17: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl ObserverNetDkg {
    pub fn new(spork17: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self {
            spork17,
        }
    }
}

impl NetHandler for ObserverNetDkg {
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    fn core_handler(&self) -> Option<CoreHandler> {
        Some(CoreHandler::NetDkg)
    }

    // Core: v24.0.0 src/llmq/net_dkg.cpp:505-531 NetDKG::ProcessMessage
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        let name = [QCONTRIB, QCOMPLAINT, QJUSTIFICATION, QPCOMMITMENT, QWATCH]
            .into_iter()
            .find(|n| cmd.as_str() == *n)
            .unwrap_or_default();
        let out = observer_dkg((self.spork17)(), name);
        if let Some((score, reason)) = out.score {
            cx.misbehaving(score, reason);
        }
        if let Some(line) = out.log {
            debug!(target: "llmq-dkg", "{line}");
        }
        Ok(())
    }

    // Core: v24.0.0 src/llmq/net_dkg.cpp:681-704 NetDKG::AlreadyHave
    fn already_have(&self, inv: &Inventory) -> bool {
        observer_already_have(inv.inv_type())
    }
}
