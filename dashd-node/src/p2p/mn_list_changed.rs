//! Core's `CDSNotificationInterface::NotifyMasternodeListChanged` for `CMNAuth`: verified
//! masternode peers whose entry was removed or whose operator key changed are disconnected.
// Core: dsnotificationinterface.cpp:90-92; evo/mnauth.cpp:179-210 at v24.0.0

use std::sync::Arc;

use dash_dml::{MnList, MnListDiff, ProTxHash};
use dash_p2p::{NetCore, Why};
use dashd_chainstate::chain::ValidationInterface;
use dashd_masternode::mnauth::{Peer, list_changed};

/// Runs inline in the chain attempt that changed the list. It reads the peer table and sets
/// `fDisconnect` flags only: both take the table's leaf lock, which no holder keeps across a
/// chain command or a wait.
pub struct MnAuthListChanged {
    core: Arc<NetCore>,
}

impl MnAuthListChanged {
    pub fn new(core: Arc<NetCore>) -> Self {
        Self {
            core,
        }
    }
}

impl ValidationInterface for MnAuthListChanged {
    // Core: dsnotificationinterface.cpp:90-92; evo/mnauth.cpp:179 at v24.0.0
    fn notify_masternode_list_changed(
        &self,
        _undo: bool,
        old: &Arc<MnList>,
        diff: &Arc<MnListDiff>,
    ) {
        disconnect_changed(&self.core, old, diff);
    }
}

// Core: net.h:1432; net.cpp:4874; evo/mnauth.cpp:186-209 at v24.0.0
fn disconnect_changed(core: &NetCore, old: &MnList, diff: &MnListDiff) {
    let peers: Vec<Peer> = core
        .peers()
        .iter()
        .filter(|p| p.successfully_connected() && !p.disconnect_requested())
        .map(|p| {
            let pro = p.verified_proreg_hash();
            Peer {
                id: p.id(),
                verified_pro: (pro != [0; 32]).then(|| ProTxHash::from(pro)),
                verified_pubkey_hash: p.verified_pubkey_hash(),
            }
        })
        .collect();
    let changed = list_changed(old, diff, &peers);
    for (id, line) in changed.ids.into_iter().zip(&changed.log) {
        tracing::debug!(target: "net|netconn", "{line}");
        core.disconnect(id, Why::Processor("mnauth list changed"));
    }
}

#[cfg(test)]
mod tests;
