//! Transaction intake and sender-owned orphan reconsideration.

use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use dash_consensus::{
    DecodeErrorKind, Tx, TxRef, TxValidationError, TxValidationResult, decode_tx,
};
use dash_num::Hash256;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, PeerHandle, PeerId, Permissions};
use dash_p2p_core::{DsTx, InvType, Inventory, P2pDecodeError};
use dash_primitives::TxHash;
use dash_types::Numeric;

use super::blocks::BlockStep;
use super::logic::PeerLogic;
use super::misbehavior;
use super::peer::{PeerState, assert_no_header_cell};
use super::registry::HandlerCx;
use super::seams::{AcceptResult, AcceptResultType, ChainSnapshot, DstxCheck, Interrupted};
use super::signals::DoorbellWaker;

/// Core v24.0.0 `src/version.h`, `ASSET_UNLOCK_INV_VERSION`.
const ASSET_UNLOCK_INV_VERSION: i32 = 70242;

/// A decoded transaction awaiting admission with the peer/header locks released.
pub struct PendingTx {
    pub(super) tx: TxRef,
    dstx: Option<DsTx>,
}

impl std::fmt::Debug for PendingTx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingTx").field("txid", &self.tx.txid()).finish_non_exhaustive()
    }
}

fn inventory(kind: InvType, hash: TxHash) -> Inventory {
    Inventory::new(kind, Hash256::from_lendian(hash.to_lendian()))
}

fn state_string(result: &AcceptResult) -> TxValidationError {
    TxValidationError::new(result.validation_result, result.reason.clone())
        .with_debug(result.debug_message.clone())
}

/// Core v24.0.0 `src/core_memusage.h:30-39`, for a freshly deserialized 64-bit transaction.
/// CScript keeps up to 28 bytes inline; CTxIn and CTxOut occupy 80 and 40 bytes respectively.
fn rejected_tx_usage(tx: &Tx) -> usize {
    let malloc = |n: usize| {
        if n == 0 {
            0
        } else {
            (n + 31) & !15
        }
    };
    let scripts = tx
        .inputs()
        .iter()
        .map(|i| i.script_sig().as_bytes().len())
        .chain(tx.outputs().iter().map(|o| o.script_pubkey().as_bytes().len()));
    malloc(tx.inputs().len() * 80)
        + malloc(tx.outputs().len() * 40)
        + scripts.filter(|n| *n > 28).map(malloc).sum::<usize>()
}

impl PeerLogic {
    /// Core v24.0.0 `src/net_processing.cpp:4879-4960`, TX/DSTX before admission.
    pub(super) fn on_tx(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<BlockStep, Throw> {
        if cx.chain.is_initial_block_download(&cx.snap, cx.core.clock().node_now()) {
            return Ok(BlockStep::Done);
        }
        let is_dstx = cmd.as_str() == "dstx";
        if !self.seams.txpool.enabled() || (is_dstx && !self.seams.coinjoin.enabled()) {
            return Ok(BlockStep::Done);
        }
        let mut cursor = payload;
        let dstx = if is_dstx {
            Some(cx.read::<DsTx>(&mut cursor)?)
        } else {
            None
        };
        let tx = match &dstx {
            Some(dstx) => Arc::new(Tx::new(dstx.tx().clone())),
            None => Arc::new(
                decode_tx(&mut cursor)
                    .map_err(|e| match e.kind {
                        DecodeErrorKind::Codec(c) => {
                            Throw::from_decode(&P2pDecodeError::Consensus(c))
                        }
                        other => Throw::ios_failure(&format!("{other:?}")),
                    })?
                    .into_value(),
            ),
        };
        let txid = tx.txid();
        let stable_unlock = tx.is_asset_unlock_with_stable_txid();
        let relay_hash = if stable_unlock {
            tx.instance_hash()
        } else {
            txid
        };
        let kind = if stable_unlock {
            InvType::AssetUnlock
        } else if is_dstx {
            InvType::Dstx
        } else {
            InvType::Tx
        };
        {
            let mut relay = cx.state.inv_relay.lock();
            relay.add_known(&relay_hash.to_lendian());
            if stable_unlock && cx.peer.common_version().get() < ASSET_UNLOCK_INV_VERSION {
                relay.add_known(&txid.to_lendian());
            }
        }
        {
            let mut tracker = self.object_request.lock();
            tracker.received_response(cx.id(), &inventory(InvType::Tx, txid));
            tracker.received_response(cx.id(), &inventory(InvType::Dstx, txid));
            if stable_unlock {
                tracker.received_response(cx.id(), &inventory(InvType::AssetUnlock, relay_hash));
            }
        }
        if let Some(dstx) = &dstx {
            match self.seams.coinjoin.validate_dstx(dstx, cx.id()) {
                DstxCheck::Accept {
                    priority_delta,
                } => {
                    self.seams.tx_intake.prioritize_transaction(&txid, priority_delta);
                }
                DstxCheck::Reject {
                    score,
                } => {
                    if let Some(score) = score {
                        cx.misbehaving(score, "invalid dstx");
                    }
                    return Ok(BlockStep::Done);
                }
                DstxCheck::Stop => return Ok(BlockStep::Done),
            }
        }
        if self.already_have(cx.core, &cx.snap, &inventory(kind, relay_hash)) {
            if cx.peer.has_permission(Permissions::FORCE_RELAY) {
                if self.seams.txpool.exists(&txid) {
                    tracing::info!("Force relaying tx {txid} from peer={}", cx.id());
                    self.relay_transaction(txid);
                } else {
                    tracing::info!(
                        "Not relaying non-mempool transaction {txid} from forcerelay peer={}",
                        cx.id()
                    );
                }
            }
            return Ok(BlockStep::Done);
        }
        Ok(BlockStep::Transaction(Box::new(PendingTx {
            tx,
            dstx,
        })))
    }

    pub(super) fn wait_tx(
        &mut self,
        tx: TxRef,
        from: Option<PeerId>,
    ) -> Result<AcceptResult, Interrupted> {
        assert_no_header_cell();
        let mut wait = self.seams.txpool.accept(tx, from);
        let bells = Arc::clone(&self.signals.bells);
        let waker = Waker::from(Arc::new(DoorbellWaker(Arc::clone(&bells))));
        let mut cx = Context::from_waker(&waker);
        loop {
            if self.interrupted() {
                return Err(Interrupted);
            }
            if let Poll::Ready(result) = wait.0.as_mut().poll(&mut cx) {
                return Ok(result);
            }
            bells.wait_doorbell();
            self.drain_sync_signals();
        }
    }

    /// Core v24.0.0 `src/net_processing.cpp:4962-5067`, TX/DSTX after admission.
    pub(super) fn tx_tail(
        &mut self,
        peer: &PeerHandle,
        st: &mut PeerState,
        pending: PendingTx,
        result: AcceptResult,
        snap: &ChainSnapshot,
    ) {
        let PendingTx {
            tx,
            dstx,
        } = pending;
        let txid = tx.txid();
        if result.result_type == AcceptResultType::Valid {
            if let Some(dstx) = dstx {
                tracing::debug!(
                    target: "coinjoin",
                    "DSTX -- Masternode transaction accepted, txid={txid}, peer={}",
                    peer.id()
                );
                self.seams.coinjoin.add_dstx(dstx);
            }
            self.forget_transaction(&tx);
            self.relay_transaction(txid);
            self.tx_orphans.add_children_to_work_set(&tx);
            peer.set_last_tx_time(self.core.clock().node_now().0);
            tracing::debug!(target: "mempool", "AcceptToMemoryPool: peer={}: accepted {txid} (poolsz {} txn, {} kB)",
                peer.id(), self.seams.txpool.size(), self.seams.txpool.dynamic_memory_usage() / 1000);
        } else if result.validation_result == TxValidationResult::MissingInputs {
            let mut parents: Vec<_> = tx.inputs().iter().map(|i| i.prevout().txid()).collect();
            parents.sort_unstable_by_key(TxHash::to_lendian);
            parents.dedup();
            if parents.iter().any(|p| self.recent_rejects.contains(&p.to_lendian())) {
                tracing::debug!(target: "mempool", "not keeping orphan with rejected parents {txid}");
                self.recent_rejects.insert(&txid.to_lendian());
                self.forget_transaction_hash(txid);
            } else {
                let core = Arc::clone(&self.core);
                let now_us = core.clock().node_now_micros();
                for parent in parents {
                    let inv = inventory(InvType::Dstx, parent);
                    st.inv_relay.lock().add_known(&parent.to_lendian());
                    if !self.already_have(&core, snap, &inv) {
                        self.add_object_announcement(st, &inv, now_us);
                        self.seams.objects.add_object_announcement(st.id, &inv, now_us);
                    }
                }
                if self.tx_orphans.add_tx(Arc::clone(&tx), peer.id(), core.clock().node_now().0) {
                    self.compact_extra(Arc::clone(&tx));
                }
                self.forget_transaction_hash(txid);
                self.tx_orphans.limit_orphans(
                    self.max_orphan_tx_bytes,
                    &mut |upper| core.get_rand(upper as u64) as usize,
                    core.clock().node_now().0,
                );
            }
        } else {
            let hash = if tx.is_asset_unlock_with_stable_txid() {
                tx.instance_hash()
            } else {
                txid
            };
            self.recent_rejects.insert(&hash.to_lendian());
            self.forget_transaction(&tx);
            if rejected_tx_usage(&tx) < 100_000 {
                self.compact_extra(Arc::clone(&tx));
            }
        }
        if result.validation_result != TxValidationResult::Unset {
            tracing::debug!(target: "mempoolrej", "{txid} from peer={} was not accepted: {}", peer.id(), state_string(&result));
            misbehavior::maybe_punish_node_for_tx(st, result.validation_result);
        }
    }

    /// Core v24.0.0 `src/net_processing.cpp:3393-3433`, `ProcessOrphanTx`.
    pub(super) fn process_orphan_tx(&mut self, peer: &PeerHandle) -> bool {
        self.process_orphan_tx_for(peer.id(), Some(peer))
    }

    /// Reconsiders one orphan for a peer id, including Core's block-connected `-1` work set.
    pub(super) fn process_orphan_tx_for(
        &mut self,
        peer_id: PeerId,
        peer: Option<&PeerHandle>,
    ) -> bool {
        while let Some(tx) = self.tx_orphans.get_tx_to_reconsider(peer_id) {
            let Ok(result) = self.wait_tx(Arc::clone(&tx), Some(peer_id)) else {
                return false;
            };
            self.drain_sync_signals();
            let hash = tx.txid();
            if result.result_type == AcceptResultType::Valid {
                tracing::debug!(target: "mempool", "   accepted orphan tx {hash}");
                self.relay_transaction(hash);
                self.tx_orphans.add_children_to_work_set(&tx);
                self.tx_orphans.erase_tx(&hash);
                self.forget_transaction_hash(hash);
                return true;
            }
            if result.validation_result != TxValidationResult::MissingInputs {
                if result.validation_result != TxValidationResult::Unset {
                    tracing::debug!(target: "mempool", "   invalid orphan tx {hash} from peer={peer_id}. {}", state_string(&result));
                    if let Some(state) = peer.and_then(|p| self.state_of(p.id())) {
                        misbehavior::maybe_punish_node_for_tx(
                            &mut state.lock(),
                            result.validation_result,
                        );
                    }
                }
                tracing::debug!(target: "mempool", "   removed orphan tx {hash}");
                self.recent_rejects.insert(&hash.to_lendian());
                self.tx_orphans.erase_tx(&hash);
                self.forget_transaction_hash(hash);
                return true;
            }
        }
        self.tx_orphans.have_more_work(peer_id)
    }

    /// Core v24.0.0 `src/net_processing.cpp:1666-1683`, `ForgetTx` overloads.
    pub fn forget_transaction_hash(&self, txid: TxHash) {
        let mut tracker = self.object_request.lock();
        tracker.forget_tx_hash(&inventory(InvType::Tx, txid));
        tracker.forget_tx_hash(&inventory(InvType::Dstx, txid));
    }

    /// Forget both txid announcements and a stable unlock's particular instance.
    pub fn forget_transaction(&self, tx: &Tx) {
        self.forget_transaction_hash(tx.txid());
        if tx.is_asset_unlock_with_stable_txid() {
            self.object_request
                .lock()
                .forget_tx_hash(&inventory(InvType::AssetUnlock, tx.instance_hash()));
        }
    }

    fn compact_extra(&mut self, tx: TxRef) {
        if let Some(download) = self.seams.block_sync.download() {
            download.add_to_compact_extra_transactions(tx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_state_string_preserves_reason_and_debug_message() {
        let mut result = AcceptResult {
            result_type: AcceptResultType::Invalid,
            validation_result: TxValidationResult::Consensus,
            reason: "mandatory-script-verify-flag-failed".into(),
            debug_message: "OP_RETURN was encountered".into(),
            vsize: None,
            base_fee: None,
        };
        assert_eq!(
            state_string(&result).to_string(),
            "mandatory-script-verify-flag-failed, OP_RETURN was encountered"
        );
        result.debug_message.clear();
        assert_eq!(state_string(&result).to_string(), "mandatory-script-verify-flag-failed");
    }
}
