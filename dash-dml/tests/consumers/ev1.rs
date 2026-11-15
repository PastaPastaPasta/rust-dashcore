//! EV1 (special-transaction contextual checks) against EV0: the pprev list's unique-property
//! probes, the BLS context at use, the per-block records it reads.
//!
//! Shape only: the order and the full set of checks are EV1's port of `CheckProRegTx`
//! (`evo/specialtxman.cpp` v24:1231-1285).

use std::sync::Arc;

use dash_consensus::{BlsCtx, TxValidationError, TxValidationResult};
use dash_dml::creditpool::PoolState;
use dash_dml::mnhf::EhfSignals;
use dash_dml::{EvoStore, StoreError};
use dash_dml::{KeyId, MnList, RawMnType, UniqueKey};
use dash_evo::EvoCtx;
use dash_primitives::{BlockHash, PlatformNodeId};

fn bad_special(reason: &'static str) -> TxValidationError {
    TxValidationError::new(TxValidationResult::BadSpecial, reason)
}

/// `CheckProRegTx`'s key checks against the list at `pindexPrev` (v24:1251-1281). The operator
/// key probe and `HasOperatorKeyUnderAnyScheme` use EV2a's `UniqueKey::operator_key` and
/// `MnList::has_operator_key_under_any_scheme`.
pub fn check_pro_reg_keys(
    prev_list: &MnList,
    owner: &KeyId,
    mn_type: RawMnType,
    platform_node_id: &PlatformNodeId,
) -> Result<(), TxValidationError> {
    if prev_list.has_unique(&UniqueKey::key_id(owner)) {
        return Err(bad_special("bad-protx-dup-key"));
    }
    if mn_type.is_evo() && prev_list.has_unique(&UniqueKey::platform_node_id(platform_node_id)) {
        return Err(bad_special("bad-protx-dup-platformnodeid"));
    }
    Ok(())
}

/// ProUpServTx/ProUpRevTx signatures are verified under the **global** scheme, whatever their
/// version (quirk 21): inside block processing that is the block's context (DEC-71: resolved at
/// use, never at decode).
pub fn pro_up_serv_sig_scheme(ctx: &EvoCtx<'_>) -> BlsCtx {
    ctx.bls()
}

/// The credit pool and MNHF signals after a block.
pub type PrevRecords = (Option<Arc<PoolState>>, Option<EhfSignals>);

/// The records EV1's asset-unlock and MNHF checks read for the block after `prev`: the pool and
/// signals after `prev`. EV3's resolvers add the activation gates and fall-backs over these
/// hash-keyed reads.
pub fn records_at_prev(store: &dyn EvoStore, prev: &BlockHash) -> Result<PrevRecords, StoreError> {
    Ok((store.credit_pool(prev)?, store.mnhf_signals(prev)?))
}

#[test]
fn key_checks_pass_on_an_empty_list() {
    let list = MnList::default_pre_dip3();
    let owner = KeyId::from([1u8; 20]);
    let node = PlatformNodeId::from([2u8; 20]);
    assert!(check_pro_reg_keys(&list, &owner, RawMnType::EVO, &node).is_ok());
}
