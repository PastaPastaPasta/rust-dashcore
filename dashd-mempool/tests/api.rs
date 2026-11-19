use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use dash_consensus::{BlockId, Tx, TxValidationResult};
use dash_primitives::{Transaction, TxType};
use dashd_mempool::api::MempoolRead;
use dashd_mempool::is::{IsAcceptHooks, IsMempool, NoIsHooks};
use dashd_mempool::options::{MemPoolLimits, MemPoolOptions};
use dashd_mempool::types::{
    LockPoints, MemPoolRemovalReason, MempoolAcceptResult, MempoolAcceptResultType,
};

#[test]
fn public_types_and_object_safe_ports_compile() {
    let _: Option<Arc<dyn IsMempool>> = None;
    let _: Option<&dyn MempoolRead> = None;
    let _: Arc<dyn IsAcceptHooks> = Arc::new(NoIsHooks);
    let _ = MemPoolOptions {
        limits: MemPoolLimits::default(),
        ..MemPoolOptions::default()
    };
    let _ = LockPoints {
        height: 1,
        time: 2,
        max_input_block: Some(BlockId(3)),
    };
    let _ = [
        MemPoolRemovalReason::Expiry,
        MemPoolRemovalReason::SizeLimit,
        MemPoolRemovalReason::Reorg,
        MemPoolRemovalReason::Block,
        MemPoolRemovalReason::Conflict,
        MemPoolRemovalReason::Manual,
    ];
    let _ = MempoolAcceptResult {
        result_type: MempoolAcceptResultType::Invalid,
        validation_result: TxValidationResult::ConflictLock,
        reason: "tx-txlock-conflict".into(),
        debug_message: String::new(),
        vsize: None,
        base_fee: None,
        effective_feerate: None,
        txids_fee_calculations: None,
    };
}

#[test]
fn disabled_instant_send_has_no_locks_or_waiters() {
    let tx = Tx::new(Transaction::new(2, TxType::new(0), vec![], vec![], 0, vec![]));
    let hooks: &dyn IsAcceptHooks = &NoIsHooks;
    assert!(!hooks.is_enabled());
    assert_eq!(hooks.conflicting_lock(&tx), None);
    assert!(!hooks.is_waiting_for_tx(&tx.txid()));
    assert!(!hooks.is_locked(&tx.txid()));
}

#[test]
fn crate_has_no_release_axis() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut pending = vec![root.join("src"), root.join("tests")];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(fs::read_dir(path).unwrap().map(|entry| entry.unwrap().path()));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            assert!(
                !fs::read_to_string(&path).unwrap().contains(concat!("Release", "::")),
                "{} names the release axis",
                path.display()
            );
        }
    }
}
