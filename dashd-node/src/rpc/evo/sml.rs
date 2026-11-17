use super::{ChainSlot, block_hash, block_index};
use dashd_rpc::evo::{BlockArg, EvoError, SmlDiffFacts};
use dashd_rpc::protocol::RpcError;

pub(super) fn diff(
    slot: &ChainSlot,
    base: BlockArg,
    block: BlockArg,
    extended: Result<bool, RpcError>,
) -> Result<SmlDiffFacts, EvoError> {
    let writer = slot.slot().clone().ok_or(EvoError::Unavailable)?;
    writer
        .chain
        .query(move |q| {
            // Core: rpc/evo.cpp:1899
            let base_hash = block_hash(q, &base)?;
            let block_hash = block_hash(q, &block)?;
            let _extended = extended.map_err(EvoError::Param)?;
            let base = block_index(q, &base, base_hash, true)?;
            let block = block_index(q, &block, block_hash, false)?;
            if !q.chain.active_contains_private(base.id)
                || !q.chain.active_contains_private(block.id)
            {
                return Err(EvoError::Failed(format!(
                    "block {base_hash} and {block_hash} are not in the same chain"
                )));
            }
            if base.height > block.height {
                return Err(EvoError::Failed(format!(
                    "base block {base_hash} is higher then block {block_hash}"
                )));
            }
            // TODO(RP2a-C1): use the production historical-list read after it lands.
            Err(EvoError::Unavailable)
        })
        .recv()
        .map_err(|_| EvoError::Unavailable)?
}
