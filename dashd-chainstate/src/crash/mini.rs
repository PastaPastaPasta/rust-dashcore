//! The mini connect and disconnect both SUTs run on a [`BlockOverlay`]: `HaveInputs`, the spends
//! and adds, no scripts. A failed coins read has tripped the latch and reached the sink, so these
//! only return it.

use dash_chain::BlockRef;
use dash_consensus::coins::BlockUndoRef;
use dashd_store::faulty::{Abort, FaultyEngine};

use super::synth::{GenBlock, out_key};
use super::world::read_failed;
use crate::coins::{BlockOverlay, CoinSrc, OutPointKey};

type Overlay<'c> = BlockOverlay<'c, FaultyEngine>;

/// `g`'s transactions into `ov`: `Ok(false)` at the first one with an input no coin holds.
/// Genesis's outputs never enter the set.
pub(super) fn connect(ov: &mut Overlay<'_>, g: &GenBlock) -> Result<bool, Abort> {
    for tx in g.block.txs() {
        if !ov.have_inputs(tx).map_err(read_failed)? {
            return Ok(false);
        }
        if !tx.is_coinbase() {
            for input in tx.inputs() {
                ov.spend(&OutPointKey::new(&input.prevout)).map_err(read_failed)?;
            }
        }
        if g.height == 0 {
            continue;
        }
        for (o, out) in tx.outputs().iter().enumerate() {
            let src = CoinSrc::Output {
                out,
                height: g.height,
                coinbase: tx.is_coinbase(),
            };
            ov.add(&out_key(tx, o), src, tx.is_coinbase()).expect("a generated output adds");
        }
    }
    Ok(true)
}

/// `g`'s transactions out of `ov`, newest first, and its undo coins back in; then `ov` merges
/// into the coins at `parent`.
pub(super) fn disconnect(
    mut ov: Overlay<'_>,
    g: &GenBlock,
    undo: &BlockUndoRef<'_>,
    parent: BlockRef,
) -> Result<(), Abort> {
    for (i, tx) in g.block.txs().iter().enumerate().rev() {
        for o in 0..tx.outputs().len() {
            // The coinbase's second output is `OP_RETURN`.
            if i == 0 && o == 1 {
                continue;
            }
            ov.spend(&out_key(tx, o)).map_err(read_failed)?;
        }
        if i == 0 {
            continue;
        }
        for (input, coin) in tx.inputs().iter().zip(&undo.txs[i - 1].coins).rev() {
            ov.add(&OutPointKey::new(&input.prevout), CoinSrc::Bytes(*coin), true)
                .expect("an undo coin restores");
        }
    }
    ov.commit(parent).expect("a block's view merges");
    Ok(())
}
