//! CH3a's `DisconnectBlock` and `ApplyTxInUndo` (CH3 §2.12, D15): Core's steps in Core's order
//! over one attempt's [`BlockOverlay`], with the evo side behind [`AttemptSteps`].
//!
//! The bracket around it is the connector's, as for a connect: it opens the attempt, runs
//! [`SerialCore::disconnect`], applies the attempt's effects and only then commits the overlay,
//! with the parent as its best block, and the evo transaction (D33, D36).
//!
//! Semantics are Dash Core v24.0.0-rc.3's; `v:N` is `validation.cpp` line N at that tag.

use dash_chain::IndexAccess as _;
use dash_consensus::coins::compress::is_unspendable;
use dash_consensus::coins::{BlockUndoRef, CoinRef, CoinsView, access_by_txid};
use dash_consensus::ctx::Deployment;
use dash_consensus::{Block, BlockId};
use dash_kv::AbortSink as _;
use dash_params::BuriedDeployment;
use dash_primitives::{BlockHash, OutPoint};
use dashd_store::KvEngine;
use dashd_store::flatfile::FlatFs;

use super::evo::BlockAt;
use super::guard::{g1_capability, g1_rule, refusal_text};
use super::serial::{
    AttemptSteps, EVODB_INCONSISTENCY, SerialCore, coins_failed, entry, read_failed,
};
use crate::chain::{ConnectCx, DisconnectResult, Fatal};
use crate::coins::{BlockOverlay, CoinSrc, OutPointKey};
use crate::store::{GuardedFiles, store_pos};

/// `DisconnectBlock`'s line after any failed `UndoReadFromDisk` (`v:2300`).
pub const UNDO_READ_FAILED: &str = "DisconnectBlock(): failure reading undo data";

impl SerialCore {
    // Core: validation.cpp:2282-2378 Chainstate::DisconnectBlock; :3272-3273 and :3320, the BLS
    // scheme guard of DisconnectTip
    /// `DisconnectBlock(block, pindex, view)` for the indexed block `id` over `overlay`, the
    /// attempt's view of the block's coins. `tip` says the caller is `DisconnectTip` (the active
    /// coins), whose BLS scheme guard holds around the call (H1); VerifyDB's call has none.
    ///
    /// The overlay is left for the bracket, which commits it with the parent as its best block
    /// as the caller would flush it (D36). After `Failed` it is in Core's "indeterminate state".
    ///
    /// # Errors
    ///
    /// A coins read error or a broken invariant (CH3 §2.1): no result. An undo record that does
    /// not read is `Failed`, not an error (§2.10.2).
    #[expect(
        clippy::too_many_arguments,
        reason = "CH3 §2.1's seam arguments, the attempt's steps and its view"
    )]
    pub fn disconnect<E: KvEngine, F: FlatFs>(
        &mut self,
        steps: &mut dyn AttemptSteps,
        cx: &ConnectCx<'_>,
        overlay: &mut BlockOverlay<'_, E>,
        files: &GuardedFiles<F>,
        block: &Block,
        id: BlockId,
        tip: bool,
    ) -> Result<DisconnectResult, Fatal> {
        let e = entry(cx.reader, id)?;
        // v:3256 assert(pindexDelete->pprev)
        let prev = e.prev.ok_or_else(|| Fatal::new("DisconnectBlock: genesis"))?;
        let parent = entry(cx.reader, prev)?.hash;
        let at = BlockAt {
            hash: e.hash,
            height: e.height,
            prev: e.prev,
            id: Some(id),
        };
        if block.hash() != at.hash {
            return Err(Fatal::new(format!(
                "DisconnectBlock: block {} is not entry {}",
                block.hash(),
                id.0
            )));
        }
        // v:3280 (and VerifyDB's v:5111): the view is at the block.
        if overlay.best_block() != Some(at.hash) {
            return Err(Fatal::new(format!(
                "DisconnectBlock: the view's best block {:?} is not the block {}",
                overlay.best_block(),
                at.hash
            )));
        }
        // v:3272-3273: the scheme in force while the block is undone (H1).
        if tip {
            steps.bls_ctx(cx.params, &at);
        }

        // Readiness guard G1 (CH3 §2.12 row 1, §2.17): before any evo state is read. A block G1
        // holds for was connected by a capable build; unreachable while G1's startup check holds.
        if !steps.complete()
            && let Some(rule) = g1_rule(cx.deployments, at.prev)
        {
            let text = refusal_text(cx.params, &at, &g1_capability(rule));
            self.abort.abort_node(&text, Some(&text));
            return Ok(DisconnectResult::Failed);
        }

        // v:2286-2292
        if cx.deployments.active_at(id, Deployment::Buried(BuriedDeployment::Dip0003))
            && !steps.verify_best_block(&at.hash)?
        {
            self.abort.abort_node(EVODB_INCONSISTENCY, None);
            return Ok(DisconnectResult::Failed);
        }

        // v:2298-2302 UndoReadFromDisk (node/blockstorage.cpp:539-575), the rest under its record.
        let Some(pos) = cx.reader.undo_pos(id).and_then(store_pos) else {
            tracing::error!("UndoReadFromDisk: no undo data available");
            tracing::error!("{UNDO_READ_FAILED}");
            return Ok(DisconnectResult::Failed);
        };
        let reader = files.read(|f| f.reader());
        let mut buf = std::mem::take(&mut self.scratch.undo_read);
        let read = reader.read_undo_record(pos, parent.as_bytes(), &mut buf, |undo| {
            undo_block(steps, overlay, block, &at, &parent, &undo, tip)
        });
        self.scratch.undo_read = buf;
        read.unwrap_or_else(|e| {
            if let Some(path) = e.open_failed() {
                // Core: flatfile.cpp:45 FlatFileSeq::Open
                tracing::info!("Unable to open file {}", path.display());
            }
            tracing::error!("UndoReadFromDisk: {e}");
            tracing::error!("{UNDO_READ_FAILED}");
            Ok(DisconnectResult::Failed)
        })
    }
}

// Core: validation.cpp:2304-2377 (DisconnectBlock after UndoReadFromDisk)
/// The block's coins undone with its undo record, then H6, H7 and, for the tip, H1's commit.
fn undo_block<E: KvEngine>(
    steps: &mut dyn AttemptSteps,
    overlay: &mut BlockOverlay<'_, E>,
    block: &Block,
    at: &BlockAt,
    parent: &BlockHash,
    undo: &BlockUndoRef<'_>,
    tip: bool,
) -> Result<DisconnectResult, Fatal> {
    let txs = block.txs();
    // v:2304-2307
    if undo.txs.len() + 1 != txs.len() {
        tracing::error!("DisconnectBlock(): block and undo data inconsistent");
        return Ok(DisconnectResult::Failed);
    }
    // v:2309-2313 (H9), before the coins.
    if !steps.undo_special_txs(block, at)? {
        tracing::error!("DisconnectBlock(): UndoSpecialTxsInBlock failed");
        return Ok(DisconnectResult::Failed);
    }

    // v:2322: fEnforceBIP30 is true, so no transaction is a BIP30 exception.
    let mut clean = true;
    // v:2325-2362: the transactions from last to first.
    for (i, tx) in txs.iter().enumerate().rev() {
        let txid = tx.txid();
        let coinbase = tx.is_coinbase();
        // Every spendable output is there, exactly as the block made it.
        for (o, out) in tx.outputs().iter().enumerate() {
            if is_unspendable(out.script_pubkey.as_bytes()) {
                continue;
            }
            let k = OutPointKey::new(&OutPoint {
                hash: txid,
                index: o as u32,
            });
            let spent = overlay.spend(&k).map_err(coins_failed)?;
            if spent.is_none_or(|c| c.as_ref() != CoinRef::from_output(out, at.height, coinbase)) {
                clean = false; // transaction output mismatch
            }
        }
        // The inputs, from last to first, but the coinbase's.
        if i > 0 {
            let tx_undo = &undo.txs[i - 1];
            if tx_undo.coins.len() != tx.inputs().len() {
                tracing::error!("DisconnectBlock(): transaction and undo data inconsistent");
                return Ok(DisconnectResult::Failed);
            }
            for (input, coin) in tx.inputs().iter().zip(&tx_undo.coins).rev() {
                match apply_tx_in_undo(overlay, *coin, &input.prevout)? {
                    DisconnectResult::Ok => {}
                    DisconnectResult::Unclean => clean = false,
                    DisconnectResult::Failed => return Ok(DisconnectResult::Failed),
                }
            }
        }
    }

    // v:2365-2366: the view's best block is the bracket's commit (D33, D36); H6.
    steps.set_best_block(parent)?;
    // v:2368-2371 (H7)
    steps.list_changed(true);
    // v:3320: DisconnectTip keeps the scheme once the block is gone, which only a clean
    // disconnect gets to (:3281-3284).
    if tip && clean {
        steps.commit_bls_scheme();
    }
    Ok(if clean {
        DisconnectResult::Ok
    } else {
        DisconnectResult::Unclean
    })
}

// Core: validation.cpp:2252-2278 ApplyTxInUndo
/// Restores the coin `undo` at `out`.
///
/// # Errors
///
/// A coins read error, or Core's `logic_error` from `AddCoin`.
fn apply_tx_in_undo<E: KvEngine>(
    overlay: &mut BlockOverlay<'_, E>,
    undo: CoinRef<'_>,
    out: &OutPoint,
) -> Result<DisconnectResult, Fatal> {
    // An unspent coin there is overwritten: unclean.
    let clean = !overlay.have_coin(out).map_err(read_failed)?;
    let mut coin = undo;
    if coin.height() == 0 {
        // Undo written before Dash v0.12.2.2 kept the height and coinbase flag only in the entry
        // of a transaction's last spent output; another output of the transaction has them.
        // Imports normalize such records (CH2 §3.5.2), so a node never meets one outside tests.
        #[cfg(not(test))]
        debug_assert!(false, "a legacy undo entry after import (CH2 §3.5.2)");
        let Some(alternate) = access_by_txid(&*overlay, &out.hash).map_err(read_failed)? else {
            return Ok(DisconnectResult::Failed);
        };
        coin = coin.with_metadata(alternate.height(), alternate.is_coinbase());
    }
    // `possible_overwrite` is exactly "an unspent coin is there", which `HaveCoin` just answered.
    overlay.add(&OutPointKey::new(out), CoinSrc::Bytes(coin), !clean).map_err(coins_failed)?;
    Ok(if clean {
        DisconnectResult::Ok
    } else {
        DisconnectResult::Unclean
    })
}
