//! P2a's M2 stubs (P2a §3.11): what a registry command does until its owner registers a handler.
//!
//! Each arm is dashd's outcome in the M2 state (a non-masternode, wallet-less node in IBD with
//! the tip at genesis, no sporks), with the gate and the decode where Core reads, or the interim
//! deviation the row names. The arms are deleted as owners register (the registry stops routing
//! a command here once it has an owner).

use std::sync::LazyLock;

use dash_evo::locks::RecoveredSig;
use dash_num::Hash256;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, Permissions, ServiceFlags};
use dash_p2p_core::{GetCFCheckpt, GetCFHeaders, GetCFilters, InvType, Inventory, Ssc};
use dash_primitives::Commitment;
use dash_types::{Hashable, Numeric};

use super::registry::{HandlerCx, NetHandler, ROWS, Route};
use super::solicit::{GetDataResponse, UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE};

/// Core's `MAX_LOCATOR_SZ`.
// Core: net_processing.cpp:150
pub const MAX_LOCATOR_SZ: usize = 101;

/// Core's `BlockFilterType::BASIC_FILTER`.
const BASIC_FILTER: u8 = 0;

/// The commands the stubs serve: every registry row.
static STUBBED: LazyLock<Vec<CommandString>> =
    LazyLock::new(|| ROWS.iter().filter(|r| r.route == Route::Registry).map(|r| r.cmd).collect());

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
            // Core: np:4861-4865 (TX, DSTX): returns unread in IBD. Outside IBD it is P5's and
            // CJ1's: until then a requested transaction stays in flight until it expires (P5a
            // D20).
            "tx" | "dstx" => {}
            // Core: np:5426-5454 (MEMPOOL). No `-maxuploadtarget` at M2, so the bandwidth check
            // never fires; the mempool is empty, so the flag sends nothing.
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
            // Core: np:5638-5652; PrepareBlockFilterRequest np:3430-3444. Exact at M2, where
            // NODE_COMPACT_FILTERS is off; serving with `-peerblockfilters` is P6b's.
            "getcfilters" | "getcfheaders" | "getcfcheckpt" => {
                let filter_type = match cmd.as_str() {
                    "getcfilters" => cx.read::<GetCFilters>(&mut data)?.filter_type,
                    "getcfheaders" => cx.read::<GetCFHeaders>(&mut data)?.filter_type,
                    _ => cx.read::<GetCFCheckpt>(&mut data)?.filter_type,
                };
                let supported = filter_type == BASIC_FILTER
                    && cx.state.our_services.has(ServiceFlags::NODE_COMPACT_FILTERS);
                if !supported {
                    tracing::debug!(target: "net", "peer {id} requested unsupported block filter type: {filter_type}");
                    cx.disconnect("unsupported block filter type");
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
                let inv = Inventory {
                    inv_type: InvType::QuorumFinalCommitment,
                    hash: qc.hash(),
                };
                if cx.consume_getdata_response(&inv) == GetDataResponse::Unrequested {
                    tracing::debug!(target: "llmq", "CQuorumBlockProcessor::ProcessMessage -- unrequested commitment from peer={id}");
                    cx.misbehaving(
                        UNREQUESTED_OBJECT_MISBEHAVIOR_SCORE,
                        "unrequested quorum commitment",
                    );
                }
                // A requested or late commitment goes on to Core's null and type checks
                // (blockprocessor.cpp:121-135) and the rest of the handler: LQ2's (deviation).
            }
            // Core: llmq/net_signing.cpp:49-54: the read and `PeerEraseObjectRequest`. The type
            // check and `VerifyAndProcessRecoveredSig` are SG1a's (deviation).
            "qsigrec" => {
                let rec = RecoveredSig::decode(&mut data)
                    .ok_or_else(|| Throw::ios_failure("DataStream::read(): end of data"))?;
                cx.received_response(Inventory {
                    inv_type: InvType::QuorumRecoveredSig,
                    hash: Hash256::from_lendian(rec.hash()),
                });
            }
            // Core: llmq/net_dkg.cpp:938-944 (NetDKGStub): no DKG role.
            "qcontrib" | "qcomplaint" | "qjustify" | "qpcommit" | "qwatch" => {
                cx.misbehaving(10, "");
            }
            // Core: llmq/net_quorum.cpp:79-83, 204-208: not a masternode, no `-watchquorums`.
            "qgetdata" => cx.misbehaving(10, "not a verified masternode or a qwatch connection"),
            "qdata" => {
                cx.misbehaving(10, "not a verified masternode and -watchquorums is not enabled")
            }
            // Core: node/sync_manager.cpp:330-343 SyncManager::ProcessMessage. Unsynced at M2,
            // so Core reads the counts and logs them; a short payload throws.
            "ssc" => {
                let ssc: Ssc = cx.read(&mut data)?;
                tracing::debug!(target: "mnsync", "SYNCSTATUSCOUNT -- got inventory count: nItemID={}  nCount={}  peer={id}", ssc.item_id, ssc.count);
            }
            // Silent at M2 (exact): no handler at all (`merkleblock`, `cfilter`, `cfheaders`,
            // `cfcheckpt`); nothing stored (`getsporks`); unsynced returns before the read (`dsq`,
            // governance, `mnauth`, `platformban`); no masternode role, so no sig-shares manager
            // (the sig-share rows, llmq/net_signing.cpp:64) and no CoinJoin server (`dsa`, `dsi`,
            // `dss`, init.cpp:2238-2242); no wallet, so no CoinJoin client (`dsf`, `dsc`, `dssu`,
            // coinjoin/walletman.cpp:234); InstantSend off (`isdlock` while `SPORK_2` is off, as it
            // is on test chains until a spork turns it on).
            // Ignored (deviations, P2a §3.11): `spork` until SP0 registers, `getmnlistd`,
            // `getqrinfo`, and `isdlock` once `SPORK_2` is on (always on mainnet): a requested
            // one stays in flight until it expires (LK1b, P5a D20).
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stubs_cover_every_registry_row() {
        let n = ROWS.iter().filter(|r| r.route == Route::Registry).count();
        assert_eq!(M2Stubs.commands().len(), n);
        // 23 inline rows: P2a-core's 22 and `clsig` (P5a D3).
        assert_eq!(n, 74 - 23);
    }
}
