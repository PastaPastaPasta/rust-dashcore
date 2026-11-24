// Core: evo/deterministicmns.h:796-799 at v24.0.0
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_dml::{MnList, store::StoreError};
use dashd_chainstate::chain::EvoView;

#[test]
fn core_get_list_for_block_production_api() {
    let lookup = |view: &EvoView<'_>, block: BlockId| -> Result<Arc<MnList>, StoreError> {
        view.list_for_block(block)
    };
    let _ = std::hint::black_box(lookup);
}
