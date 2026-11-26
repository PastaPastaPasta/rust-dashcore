//! `getblocktemplate`'s longpoll wait (`rpc/mining.cpp:781-804` at v24.0.2), run with the GBT
//! mutex released as Core releases `cs_main`.

use std::time::Instant;

use dash_primitives::BlockHash;
use dashd_chainstate::chain::WaitEnd;

use super::{GbtError, GbtParts, LongpollTimes};
use crate::{MiningMempool, TemplateError};

/// Waits while V's best block is `watched` and RPC runs, until a deadline finds the mempool's
/// update counter moved off `tx_updated`. The first deadline is `times.first` after the call on
/// the steady clock, each later one `times.step` after the previous. The counter is read only at
/// a deadline, with no lock held, as Core reads it "without holding the mempool lock". Refusals
/// are not checked again after the wake (Core's TODO at `:804`).
///
/// `parts.status.rpc_running()` is read under the tip watch's mutex: it must not block.
///
/// # Errors
///
/// [`GbtError::ShuttingDown`] when RPC stopped.
pub(super) fn wait(
    parts: &GbtParts,
    mempool: &dyn MiningMempool,
    watched: BlockHash,
    tx_updated: u32,
    times: LongpollTimes,
) -> Result<(), GbtError> {
    // Core: v24.0.2 rpc/mining.cpp:785-799
    let mut deadline = Instant::now() + times.first;
    loop {
        let end = parts
            .chain
            .wait_tip_while(deadline, &mut |best| {
                best == Some(watched) && parts.status.rpc_running()
            })
            .map_err(|e| TemplateError::Failed(e.to_string()))?;
        match end {
            WaitEnd::Woken => break,
            WaitEnd::Deadline => {
                if mempool.transactions_updated() != tx_updated {
                    break;
                }
                deadline += times.step;
            }
        }
    }
    // :802-803
    if !parts.status.rpc_running() {
        return Err(GbtError::ShuttingDown);
    }
    Ok(())
}
