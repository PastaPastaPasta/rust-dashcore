//! P2a's M2 stubs (P2a §3.11): what a registry command does until its owner registers a handler.
//!
//! Each arm is dashd's outcome in the M2 state (a wallet-less node in IBD with
//! the tip at genesis, no sporks), with the gate and the decode where Core reads, or the interim
//! deviation the row names. The arms are deleted as owners register (the registry stops routing
//! a command here once it has an owner).

use std::sync::{Arc, LazyLock};

use dash_evo::locks::{ISLOCK_MAX_INPUTS, RecoveredSig, isdlock_hash};
use dash_num::Hash256;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, Permissions, ServiceFlags};
use dash_p2p_core::{InvType, Inventory, IsdLock, Ssc};
use dash_primitives::Commitment;
use dash_types::{Hashable, Numeric};

use super::registry::{HandlerCx, NetHandler, ROWS, Route};
use super::solicit::{GetDataResponse, UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE};

/// Core's `INVALID_ISLOCK_MISBEHAVIOR_SCORE`.
// Core: instantsend/net_instantsend.cpp:34
const INVALID_ISLOCK_MISBEHAVIOR_SCORE: i32 = 100;

/// Core's `MAX_LOCATOR_SZ`.
// Core: net_processing.cpp:150
pub const MAX_LOCATOR_SZ: usize = 101;

/// The commands the stubs serve: every registry row but `getmnlistd` (P6b's `EvoServer`) and
/// `qgetdata`/`qdata` (QM1b-6's `NetQuorum`).
static STUBBED: LazyLock<Vec<CommandString>> = LazyLock::new(|| {
    const OWNED: [CommandString; 3] =
        [CommandString::GETMNLISTD, CommandString::QGETDATA, CommandString::QDATA];
    ROWS.iter()
        .filter(|r| r.route == Route::Registry && !OWNED.contains(&r.cmd))
        .map(|r| r.cmd)
        .collect()
});

/// The M2 handler of every registry command.
#[derive(Debug, Default)]
pub struct M2Stubs;

impl NetHandler for M2Stubs {
    fn commands(&self) -> &'static [CommandString] {
        &STUBBED
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        let mut data = payload;
        let id = cx.id();
        match cmd.as_str() {
            // Core: v24.0.2 np:5444-5472 (MEMPOOL). No `-maxuploadtarget` at M2, so the
            // bandwidth check never fires. Section 8 replies on the next trickle.
            "mempool" => {
                if !cx.state.our_services.has(ServiceFlags::NODE_BLOOM)
                    && !cx.peer.has_permission(Permissions::MEMPOOL)
                {
                    if !cx.peer.has_permission(Permissions::NO_BAN) {
                        tracing::debug!(target: "net", "mempool request with bloom filters disabled, disconnect peer={id}");
                        cx.disconnect("mempool request with bloom filters disabled");
                    }
                } else if cx.state.tx_relay {
                    cx.state.send_mempool = true;
                }
            }
            // Core: np:5654-5658, np:5739-5743: never requested.
            "mnlistdiff" => {
                cx.misbehaving(100, &format!("received not-requested mnlistdiff. peer={id}"));
            }
            "qrinfo" => {
                cx.misbehaving(
                    100,
                    &format!("received not-requested quorumrotationinfo. peer={id}"),
                );
            }
            // Core: llmq/blockprocessor.cpp:98-119, called for every known command
            // (np:5783-5790): the solicitation gate (P2P-12) on `PeerConsumeGetDataResponse`.
            "qfcommit" => {
                let qc: Commitment = cx.read(&mut data)?;
                // Core's `::SerializeHash(qc)`.
                let inv = Inventory::new(InvType::QuorumFinalCommitment, qc.hash());
                if cx.consume_getdata_response(&inv) == GetDataResponse::Unrequested {
                    tracing::debug!(target: "llmq", "CQuorumBlockProcessor::ProcessMessage -- unrequested commitment from peer={id}");
                    cx.misbehaving(
                        UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE,
                        "unrequested quorum commitment",
                    );
                }
                // A requested or late commitment goes on to Core's checks
                // (blockprocessor.cpp:121-211): the node's `NetQuorumBlockProcessor` (QF1),
                // which owns `qfcommit` where it is registered.
            }
            // Core: v24.0.2 src/llmq/net_signing.cpp:47-62 NetSigning::ProcessMessage, :307-320
            // BanNode(nodeId, false)
            "qsigrec" => {
                let rec = RecoveredSig::decode(&mut data)
                    .ok_or_else(|| Throw::ios_failure("DataStream::read(): end of data"))?;
                cx.received_response(Inventory::new(
                    InvType::QuorumRecoveredSig,
                    Hash256::from_lendian(rec.hash()),
                ));
                let seam = Arc::clone(&cx.signing);
                if !seam.enabled() {
                    return Ok(());
                }
                if !seam.llmq_configured(rec.llmq_type) {
                    cx.misbehaving(100, "");
                    seam.remove_node(id);
                    return Ok(());
                }
                seam.verify_and_process(id, rec, cx.ctx.bls().is_legacy());
            }
            // Core: instantsend/net_instantsend.cpp:244-261: the enabled gate, the read, the
            // input cap and `PeerEraseObjectRequest`, for a processor without `NetInstantSend`
            // (dashd-node registers it).
            "isdlock" => {
                if !cx.overlay.instantsend_enabled() {
                    return Ok(());
                }
                let islock: IsdLock = cx.read(&mut data)?;
                if islock.inputs().len() > ISLOCK_MAX_INPUTS {
                    cx.misbehaving(INVALID_ISLOCK_MISBEHAVIOR_SCORE, "");
                    return Ok(());
                }
                cx.received_response(Inventory::new(
                    InvType::IsdLock,
                    Hash256::from_lendian(isdlock_hash(&islock)),
                ));
            }
            "qsigshare" | "qsigsesann" | "qsigsinv" | "qgetsigs" | "qbsigs" => {
                return super::sigshares::process_sig_shares(cx, cmd, payload);
            }
            // Core: llmq/net_dkg.cpp:514-521: an active masternode marks a `qwatch` sender
            // unscored; nothing here reads the mark yet.
            "qwatch" if cx.core.config().active_masternode => {}
            // Core: llmq/net_dkg.cpp:938-944 (NetDKGStub): no DKG role.
            "qcontrib" | "qcomplaint" | "qjustify" | "qpcommit" | "qwatch" => {
                cx.misbehaving(10, "");
            }
            // Core: node/sync_manager.cpp:330-343 SyncManager::ProcessMessage. Unsynced at M2,
            // so Core reads the counts and logs them; a short payload throws.
            "ssc" => {
                let ssc: Ssc = cx.read(&mut data)?;
                tracing::debug!(target: "mnsync", "SYNCSTATUSCOUNT -- got inventory count: nItemID={}  nCount={}  peer={id}", ssc.item_id(), ssc.count());
            }
            // Silent at M2 (exact): no handler at all (`merkleblock`, `cfilter`, `cfheaders`,
            // `cfcheckpt`); nothing stored (`getsporks`); unsynced returns before the read (`dsq`,
            // governance, `platformban`); no masternode role, so no sig-shares manager
            // (the sig-share rows, llmq/net_signing.cpp:64) and no CoinJoin server (`dsa`, `dsi`,
            // `dss`, init.cpp:2238-2242); no wallet, so no CoinJoin client (`dsf`, `dsc`, `dssu`,
            // coinjoin/walletman.cpp:234).
            // Ignored (deviations, P2a §3.11): `spork` until SP0 registers. `getqrinfo` is served
            // by P6b's handler; `getmnlistd` is P6b's `EvoServer` (a deviation: checks exact;
            // reply pending RP2a-P2).
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stubs_cover_every_registry_row_but_the_owned_ones() {
        let n = ROWS.iter().filter(|r| r.route == Route::Registry).count();
        assert_eq!(M2Stubs.commands().len(), n - 3);
        for cmd in [CommandString::QGETDATA, CommandString::QDATA] {
            assert!(!M2Stubs.commands().contains(&cmd), "{cmd:?}");
        }
        // 25 inline rows, including `clsig` and transaction intake.
        assert_eq!(n, 74 - 25);
    }
}
