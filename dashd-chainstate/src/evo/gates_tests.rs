//! Each key's predicate driven on its own (plan §3.6, m-8), and the verdicts unchanged: every
//! call's result is `DmlChecks`'s.

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::{ConsensusCtx, DeploymentView, bls_ctx_for_block};
use dash_consensus::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::primitives::{Header, OutPoint, Transaction, TxIn, TxOut};
use dash_consensus::{Block, BlockId, Tx};
use dash_dml::specialtx::covenant::SHARED_COLLATERAL_SCRIPT;
use dash_dml::testkit::MemChain;
use dash_dml::{MnList, StoreError};
use dash_params::{BuriedDeployment, ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, BlockHeader, TxHash, TxType};
use dash_types::Numeric as _;

use super::super::ev1::{DmlChecks, Ev1Checks, PrevList, SpecialTxCtx};
use super::super::node::node_net_info;
use super::super::testkit::cbtx_bytes;
use super::*;
use crate::connect::{NoCheckpoints, SpecialTxRules};
use crate::store::CoinsReadError;

fn tx(
    version: i16,
    tx_type: u16,
    coinbase: bool,
    n: u8,
    script: &[u8],
    payload: Vec<u8>,
) -> Arc<Tx> {
    let hash = if coinbase {
        [0; 32]
    } else {
        [n; 32]
    };
    Arc::new(Tx::new(Transaction {
        version,
        tx_type: TxType::new(tx_type),
        inputs: vec![TxIn {
            prevout: OutPoint {
                hash: TxHash::from(hash),
                index: if coinbase {
                    u32::MAX
                } else {
                    0
                },
            },
            script_sig: vec![1, n].into(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value: 1,
            script_pubkey: script.to_vec().into(),
        }],
        lock_time: 0,
        extra_payload: payload,
    }))
}

fn block(txs: Vec<Arc<Tx>>) -> Block {
    Block::new(Header::new(BlockHeader::default()), txs)
}

struct NoCoins;

impl CoinsView for NoCoins {
    type Error = CoinsReadError;

    fn coin(
        &self,
        _: &OutPoint,
    ) -> Result<Option<dash_consensus::coins::CoinRef<'_>>, CoinsReadError> {
        Ok(None)
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// Regtest, its chain to one block past DIP0003, and DIP0003's height.
fn regtest() -> (ChainParams, MemChain, u32) {
    let params =
        ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().expect("regtest");
    let dip3 = params.consensus.deployment_height(BuriedDeployment::Dip0003).to_u32();
    let mut chain = MemChain::new();
    chain.extend(chain.genesis(), dip3 + 1);
    (params, chain, dip3)
}

fn cons<'a>(
    params: &'a ChainParams,
    chain: &'a MemChain,
    deployments: &'a dyn DeploymentView,
    prev: u32,
) -> ConsensusCtx<'a> {
    ConsensusCtx {
        params,
        height: prev + 1,
        prev: Some(BlockId(prev)),
        chain,
        deployments,
        checkpoints: &NoCheckpoints,
        bls: bls_ctx_for_block(params, prev + 1),
        median_time_past: 0,
        header_check_time: 0,
    }
}

fn cx<'a>(cons: &'a ConsensusCtx<'a>, list: &'a PrevList<'a>, v24: bool) -> SpecialTxCtx<'a> {
    SpecialTxCtx {
        cons,
        rules: SpecialTxRules {
            v24,
            evo_shares: false,
        },
        coins: &NoCoins,
        indexes: None,
        check_sigs: true,
        list,
        net_info: node_net_info(cons.params, false).expect("net info rules"),
    }
}

/// Step 1 when `vtx[0]` has raw type 5, then step 4 for every transaction, on both checks:
/// the same verdicts, and the counts.
fn counts(block: &Block, cx: SpecialTxCtx<'_>) -> BTreeMap<&'static str, u64> {
    let counting = Counting::new();
    let raw_cbtx = block.txs()[0].tx_type().to_base() == TRANSACTION_COINBASE;
    let (ours, theirs) = (counting.payloads(block), DmlChecks.payloads(block));
    if raw_cbtx {
        assert_eq!(counting.check_cbtx(block, &ours, cx), DmlChecks.check_cbtx(block, &theirs, cx));
    }
    for i in 0..block.txs().len() {
        assert_eq!(
            counting.check_tx(block, &ours, i, cx),
            DmlChecks.check_tx(block, &theirs, i, cx)
        );
    }
    counting.counts().snapshot()
}

fn want(pairs: &[(&'static str, u64)]) -> BTreeMap<&'static str, u64> {
    let mut all: BTreeMap<&'static str, u64> = KEYS.iter().map(|k| (*k, 0)).collect();
    all.extend(pairs.iter().copied());
    all
}

/// Below DIP3 every step-4 call enters CheckSpecialTxInner's wrapper (`w.stx`) but a raw-type-5
/// `vtx[0]`, and no gated body runs; step 1 enters `w.cbtx`, and `g.cbtx` only for a CbTx that
/// decodes.
#[test]
fn below_dip3_only_the_wrappers_count() {
    let (params, chain, _) = regtest();
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let resolve =
        || -> Result<Arc<MnList>, StoreError> { Ok(Arc::new(MnList::default_pre_dip3())) };
    let list = PrevList::new(&resolve);
    let cons = cons(&params, &chain, &deployments, 1);
    let off = cx(&cons, &list, false);
    let classical =
        block(vec![tx(1, 0, true, 0, &[0x51], vec![]), tx(2, 0, false, 1, &[0x51], vec![])]);
    assert_eq!(counts(&classical, off), want(&[("d.stx_hook", 2), ("w.stx", 2)]));
    // A special transaction below DIP3: wrapped, its gated body not entered.
    let special =
        block(vec![tx(1, 0, true, 0, &[0x51], vec![]), tx(3, 1, false, 1, &[0x51], vec![0])]);
    assert_eq!(counts(&special, off), want(&[("d.stx_hook", 2), ("w.stx", 2)]));
    // A raw-type-5 coinbase: step 4 skips it; its CbTx decodes only with a payload.
    let bare = block(vec![tx(2, TRANSACTION_COINBASE, true, 0, &[0x51], vec![])]);
    assert_eq!(counts(&bare, off), want(&[("d.stx_hook", 1), ("w.cbtx", 1)]));
    // The same coinbase with one ordinary transaction: only that one enters the wrapper.
    let one = block(vec![
        tx(2, TRANSACTION_COINBASE, true, 0, &[0x51], vec![]),
        tx(2, 0, false, 1, &[0x51], vec![]),
    ]);
    assert_eq!(counts(&one, off), want(&[("d.stx_hook", 2), ("w.stx", 1), ("w.cbtx", 1)]));
    let cbtx = block(vec![tx(3, TRANSACTION_COINBASE, true, 0, &[0x51], cbtx_bytes(2, 2, 0, 0))]);
    assert_eq!(counts(&cbtx, off), want(&[("d.stx_hook", 1), ("w.cbtx", 1), ("g.cbtx", 1)]));
}

/// From DIP3 a special transaction enters the gated body (`g.stx`); a classical one does not.
#[test]
fn from_dip3_a_special_transaction_is_gated() {
    let (params, chain, dip3) = regtest();
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let resolve =
        || -> Result<Arc<MnList>, StoreError> { Ok(Arc::new(MnList::default_pre_dip3())) };
    let list = PrevList::new(&resolve);
    let cons = cons(&params, &chain, &deployments, dip3);
    let on = cx(&cons, &list, false);
    let classical =
        block(vec![tx(1, 0, true, 0, &[0x51], vec![]), tx(2, 0, false, 1, &[0x51], vec![])]);
    assert_eq!(counts(&classical, on), want(&[("d.stx_hook", 2), ("w.stx", 2)]));
    let special =
        block(vec![tx(1, 0, true, 0, &[0x51], vec![]), tx(3, 1, false, 1, &[0x51], vec![0])]);
    assert_eq!(counts(&special, on), want(&[("d.stx_hook", 2), ("w.stx", 2), ("g.stx", 1)]));
}

/// Under `rules.v24` the template check runs first and counts `g.coll`; a transaction it
/// refuses never enters the wrapper (m-8), at any height. H8 counts `g.coll` too.
#[test]
fn under_v24_the_template_check_counts_first() {
    let (params, chain, _) = regtest();
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let resolve =
        || -> Result<Arc<MnList>, StoreError> { Ok(Arc::new(MnList::default_pre_dip3())) };
    let list = PrevList::new(&resolve);
    let cons = cons(&params, &chain, &deployments, 1);
    let v24 = cx(&cons, &list, true);
    let fine = block(vec![tx(1, 0, true, 0, &[0x51], vec![]), tx(2, 0, false, 1, &[0x51], vec![])]);
    assert_eq!(counts(&fine, v24), want(&[("d.stx_hook", 2), ("g.coll", 2), ("w.stx", 2)]));
    let template = block(vec![
        tx(1, 0, true, 0, &[0x51], vec![]),
        tx(2, 0, false, 1, &SHARED_COLLATERAL_SCRIPT, vec![]),
    ]);
    let got = counts(&template, v24);
    assert_eq!(got, want(&[("d.stx_hook", 2), ("g.coll", 2), ("w.stx", 1)]), "refused: no wrapper");

    let counting = Counting::new();
    let spend = tx(2, 0, false, 1, &[0x51], vec![]);
    assert_eq!(
        counting.shared_collateral_spends(&spend, &NoCoins),
        DmlChecks.shared_collateral_spends(&spend, &NoCoins)
    );
    assert_eq!(counting.counts().snapshot(), want(&[("g.coll", 1)]));
}

/// The registry finds a chainstate's live counts under its data directory, the newest first,
/// and forgets them with the checks.
#[test]
fn counts_are_found_under_the_data_directory() {
    let dir = std::path::Path::new("/l3-gates-test/leg");
    let first = node_ev1_checks(&dir.join("load-0/regtest/chainstate"));
    let found = counts_under(dir).expect("registered");
    assert!(Arc::ptr_eq(&found, first.counts()));
    let second = node_ev1_checks(&dir.join("load-1/regtest/chainstate"));
    assert!(Arc::ptr_eq(&counts_under(dir).expect("registered"), second.counts()), "the newest");
    drop((first, second, found));
    assert!(counts_under(dir).is_none(), "dead once the checks are gone");
    assert!(counts_under(std::path::Path::new("/l3-gates-test/other")).is_none());
}
