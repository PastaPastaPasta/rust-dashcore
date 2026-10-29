use std::sync::Arc;

use dash_consensus::deployments::{Deployments, NoEhfSignals, VersionBitsCache};
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::primitives::{Header, OutPoint, Transaction, TxIn, TxOut};
use dash_dml::testkit::MemChain;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHeader, TxHash, TxType};

use super::*;
use crate::connect::pre_dip3_special_txs;

fn tx(version: i16, tx_type: u16, coinbase: bool, n: u8) -> Arc<Tx> {
    let prevout = if coinbase {
        OutPoint {
            hash: TxHash::from([0; 32]),
            index: u32::MAX,
        }
    } else {
        OutPoint {
            hash: TxHash::from([n; 32]),
            index: 0,
        }
    };
    Arc::new(Tx::new(Transaction {
        version,
        tx_type: TxType::new(tx_type),
        inputs: vec![TxIn {
            prevout,
            script_sig: vec![1, n].into(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value: 1,
            script_pubkey: vec![0x51].into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

/// The blocks whose verdicts differ below DIP0003: classical, a raw-type-5 coinbase at versions
/// 1 and 2, a raw-type-5 `vtx[0]` that is no coinbase, and a special transaction after the
/// coinbase.
fn blocks() -> Vec<Block> {
    let header = || Header::new(BlockHeader::default());
    let cb = tx(1, 0, true, 0);
    [
        vec![cb.clone(), tx(2, 0, false, 1)],
        vec![tx(1, TRANSACTION_COINBASE, true, 0)],
        vec![tx(2, TRANSACTION_COINBASE, true, 0)],
        vec![tx(2, TRANSACTION_COINBASE, false, 2)],
        vec![cb.clone(), tx(3, 1, false, 3)],
        vec![cb, tx(2, 1, false, 4), tx(3, 8, false, 5)],
    ]
    .into_iter()
    .map(|txs| Block::new(header(), txs))
    .collect()
}

/// A view with no coin: the pre-DIP3 checks read none.
struct NoCoins;

impl CoinsView for NoCoins {
    type Error = CoinsReadError;

    fn coin(
        &self,
        _: &dash_consensus::primitives::OutPoint,
    ) -> Result<Option<dash_consensus::coins::CoinRef<'_>>, CoinsReadError> {
        Ok(None)
    }

    fn best_block(&self) -> Option<dash_primitives::BlockHash> {
        None
    }
}

fn cx<'a>(deployments: &'a dyn DeploymentView, prev: u32) -> SpecialTxCtx<'a> {
    SpecialTxCtx {
        deployments,
        prev: Some(BlockId(prev)),
        rules: SpecialTxRules::default(),
        coins: &NoCoins,
        indexes: None,
        check_sigs: true,
    }
}

/// H4's order over the seam: step 1 with `fCheckCbTxMerkleRoots` and a raw-type-5 `vtx[0]`,
/// then step 4's body for every transaction.
fn run<C: Ev1Checks>(
    checks: &C,
    block: &Block,
    check_cbtx_merkle_roots: bool,
    cx: SpecialTxCtx<'_>,
) -> Result<Result<(), SpecialTxFailure>, Fatal> {
    let payloads = checks.payloads(block);
    let raw_cbtx =
        block.txs().first().is_some_and(|tx| tx.tx_type().to_base() == TRANSACTION_COINBASE);
    if check_cbtx_merkle_roots
        && raw_cbtx
        && let Err(err) = checks.check_cbtx(block, &payloads, cx)?
    {
        return Ok(Err(err));
    }
    for i in 0..block.txs().len() {
        if let Err(err) = checks.check_tx(block, &payloads, i, cx)? {
            return Ok(Err(err));
        }
    }
    Ok(Ok(()))
}

/// D44 (DEC-581): on a chain below DIP0003 the stand-in never refuses and gives `NoEvo`'s
/// verdict for every block, with and without the CbTx checks; at DIP0003 it refuses.
#[test]
fn the_stand_in_is_no_evo_below_dip3_and_refuses_from_it() {
    let params =
        ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().expect("regtest");
    let dip3 = params.consensus.deployment_height(BuriedDeployment::Dip0003).to_u32();
    assert!(dip3 > 1, "regtest's DIP0003 is above genesis");
    let mut chain = MemChain::new();
    chain.extend(chain.genesis(), dip3 + 1);
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let checks = Ev1NotBuilt;
    let blocks = blocks();

    for prev in [0, 1, dip3 / 2, dip3 - 2] {
        let cx = cx(&deployments, prev);
        for (n, block) in blocks.iter().enumerate() {
            for check in [false, true] {
                let want = pre_dip3_special_txs(block, check)
                    .map(|r| r.map_err(SpecialTxFailure::Invalid));
                let got = run(&checks, block, check, cx);
                assert!(
                    !matches!(got, Ok(Err(SpecialTxFailure::NotReady(_)))),
                    "prev {prev} block {n}"
                );
                assert_eq!(got, want, "prev {prev} block {n} check {check}");
            }
        }
    }

    let at_dip3 = cx(&deployments, dip3 - 1);
    let refused = SpecialTxFailure::NotReady(SPECIAL_TX_CHECKS);
    for block in &blocks {
        assert_eq!(checks.check_cbtx(block, &(), at_dip3), Ok(Err(refused.clone())));
        assert_eq!(checks.check_tx(block, &(), 0, at_dip3), Ok(Err(refused.clone())));
    }
    // V24 needs EV1's template check, at any height.
    let v24 = SpecialTxCtx {
        rules: SpecialTxRules {
            v24: true,
            evo_shares: false,
        },
        ..cx(&deployments, 1)
    };
    assert_eq!(checks.check_tx(&blocks[0], &(), 0, v24), Ok(Err(refused)));
}
