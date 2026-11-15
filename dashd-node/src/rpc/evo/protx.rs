use super::{ChainSlot, block_hash, block_index};
use dashd_rpc::evo::{
    BlockArg, BlockFailure, BlockRef, EvoError, MnListAt, MnListDiffFacts, MnListRequest,
};

pub(super) fn list(slot: &ChainSlot, request: MnListRequest) -> Result<MnListAt, EvoError> {
    let writer = slot.slot().clone().ok_or(EvoError::Unavailable)?;
    writer
        .chain
        .query(move |q| {
            let tip = q.chain.private_tip().ok_or(EvoError::Unavailable)?.0.height as i32;
            match request {
                MnListRequest::Height(height) => {
                    if !(1..=tip).contains(&height.unwrap_or(tip)) {
                        return Err(EvoError::InvalidHeight);
                    }
                }
                MnListRequest::Info {
                    block: Some(hash),
                    ..
                } => {
                    let arg = BlockArg {
                        name: "blockHash",
                        value_str: hash.to_string(),
                        syntax: Ok(BlockRef::Hash(hash)),
                    };
                    // Core: rpc/evo.cpp:1818
                    let reader = q.chain.reader();
                    let entry = reader
                        .lookup(&hash)
                        .and_then(|id| reader.entry(id))
                        .ok_or_else(|| arg.failure(BlockFailure::NotFound(hash)))?;
                    if !q.chain.active_contains_private(entry.id) {
                        return Err(EvoError::NotInMainChain);
                    }
                }
                MnListRequest::Info {
                    block: None,
                    ..
                } => {}
            }
            // TODO(RP2a-C1): use the production historical-list read after it lands.
            Err(EvoError::Unavailable)
        })
        .recv()
        .map_err(|_| EvoError::Unavailable)?
}

pub(super) fn list_diff(
    slot: &ChainSlot,
    base: BlockArg,
    block: BlockArg,
) -> Result<MnListDiffFacts, EvoError> {
    let writer = slot.slot().clone().ok_or(EvoError::Unavailable)?;
    writer
        .chain
        .query(move |q| {
            // Core: rpc/evo.cpp:1861
            let resolve = |arg: &BlockArg| {
                block_hash(q, arg)
                    .and_then(|hash| block_index(q, arg, hash, false))
                    .map_err(|_| arg.failure(BlockFailure::BadArg))
            };
            let base = resolve(&base)?;
            let block = resolve(&block)?;
            if !q.chain.active_contains_private(base.id)
                || !q.chain.active_contains_private(block.id)
            {
                return Err(EvoError::NotInMainChain);
            }
            // TODO(RP2a-C1): use the production historical-list read after it lands.
            Err(EvoError::Unavailable)
        })
        .recv()
        .map_err(|_| EvoError::Unavailable)?
}
