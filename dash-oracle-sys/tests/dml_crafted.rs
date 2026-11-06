//! `oracle_dml_block` on crafted regtest blocks, one per masternode-list operator of EV2 §6.5,
//! against the verdicts a release dashd gave them as getblocktemplate proposals
//! (`tests/data/dml-crafted-v24.0.json`, scripts/gen-dml-crafted.py, tests/data/PROVENANCE.md).

mod common;

use std::collections::BTreeMap;

use common::{Coverage, coins, hex};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, DmlBlock, DmlStage, ErrorKind, MnList, Oracle,
    ThresholdState, Verdict,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Fixture {
    dashd: String,
    /// Each chain's headers from genesis (hex).
    chains: BTreeMap<String, String>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    operator: String,
    v24: bool,
    context: CaseContext,
    block: String,
    /// The release's proposal answer (it accepted none of these blocks).
    core_reason: String,
    /// Where in ProcessSpecialTxsInBlock Core gives `core_reason`: "check" (the
    /// CheckSpecialTxInner loop), "build" (BuildNewListFromBlock), or null when Core rejects
    /// the block in a step oracle_dml_block leaves out (the quorum block processor, the CbTx
    /// merkle roots).
    stage: Option<String>,
}

#[derive(Deserialize)]
struct CaseContext {
    tip: usize,
    list: String,
    coins: Value,
    deployments: Vec<(u8, u8)>,
    adjusted_time: i64,
    bls_legacy: bool,
}

impl Case {
    fn chain(&self) -> Chain {
        if self.v24 {
            Chain::new("regtest", &["-vbparams=v24:-1:999999999999"])
        } else {
            Chain::regtest()
        }
    }

    /// The case's context, its headers from `first_height` to the tip.
    fn context(&self, chains: &BTreeMap<String, String>, first_height: usize) -> Context {
        let c = &self.context;
        let headers = common::headers(
            &chains[if self.v24 {
                "v24"
            } else {
                "default"
            }],
        );
        Context::new(
            first_height as i32,
            headers[first_height..=c.tip].to_vec(),
            c.bls_legacy,
            c.deployments.iter().map(|&(d, s)| (d, ThresholdState::try_from(s).unwrap())).collect(),
            c.adjusted_time,
            Checkpoints::Disabled,
            CoinsView::AtTip(coins(&c.coins)),
            MnList::Supplied(hex(&c.list)),
            vec![],
        )
    }

    fn dml_block(&self, oracle: &Oracle, ctx: &Context, just_check: bool) -> DmlBlock {
        oracle
            .dml_block(&self.chain(), &hex(&self.block), ctx, true, just_check)
            .unwrap_or_else(|e| panic!("{}: {e}", self.name))
    }
}

/// Core's reason, at the stage Core gives it. When Core rejects in a step oracle_dml_block
/// leaves out, the oracle accepts: the quorum block processor (`bad-qc-block`) or the CbTx's
/// merkleRootMNList, which was made for the block without the crafted transactions.
#[test]
fn dml_block_matches_release_proposals() {
    let fixture: Fixture = common::load_data("dml-crafted-v24.0.json");
    assert_eq!(fixture.dashd, "/Dash Core:24.0.0/");
    let mut coverage = Coverage::default();
    for oracle in Oracle::all() {
        for case in &fixture.cases {
            let what = format!("{} ({})", case.name, case.operator);
            let ctx = case.context(&fixture.chains, 0);
            // A getblocktemplate proposal is TestBlockValidity(): fJustCheck, no ProcessBlock().
            let got = case.dml_block(&oracle, &ctx, true);
            let processed = case.dml_block(&oracle, &ctx, false);
            assert_eq!(
                (&processed.verdict, processed.stage, &processed.list),
                (&got.verdict, got.stage, &got.list),
                "{what}: ProcessBlock() changed the outcome"
            );
            match case.stage.as_deref() {
                None => {
                    assert!(
                        got.verdict.is_accept(),
                        "{what}: {:?} at {:?}",
                        got.verdict,
                        got.stage
                    );
                    assert!(
                        ["bad-qc-block", "bad-cbtx-mnmerkleroot"]
                            .contains(&case.core_reason.as_str()),
                        "{what}: {}",
                        case.core_reason
                    );
                    assert!(got.list.is_some(), "{what}");
                }
                Some(stage) => {
                    let Verdict::Reject(r) = &got.verdict else {
                        panic!("{what}: accepted");
                    };
                    let stage = match stage {
                        "check" => DmlStage::Check,
                        "build" => DmlStage::Build,
                        other => panic!("{what}: stage {other}"),
                    };
                    assert_eq!(
                        (r.reason.as_str(), got.stage),
                        (case.core_reason.as_str(), Some(stage)),
                        "{what}"
                    );
                }
            }
            coverage.hit(case.name.clone());
        }
    }
    assert_eq!(fixture.cases.len(), 15);
    coverage
        .require_per_pin(&fixture.cases.iter().map(|c| (c.name.as_str(), 1)).collect::<Vec<_>>());
}

/// A block that does not extend the context's tip is out of ConnectBlock()'s domain.
#[test]
fn dml_block_refuses_a_block_off_the_tip() {
    let fixture: Fixture = common::load_data("dml-crafted-v24.0.json");
    let case = &fixture.cases[0];
    let mut block = hex(&case.block);
    block[4] ^= 1; // hashPrevBlock
    for oracle in Oracle::all() {
        let err = oracle
            .dml_block(&case.chain(), &block, &case.context(&fixture.chains, 0), true, false)
            .expect_err("hashPrevBlock is not the tip");
        assert_eq!(err.kind, ErrorKind::Argument, "{err}");
    }
}

/// A commitment whose quorum base block the headers lack is a context refusal, not a verdict.
#[test]
fn dml_block_refuses_a_missing_quorum_base() {
    let fixture: Fixture = common::load_data("dml-crafted-v24.0.json");
    let case = fixture.cases.iter().find(|c| c.name == "qc_old_quorum_hash").unwrap();
    for oracle in Oracle::all() {
        let tip_only = case.context(&fixture.chains, case.context.tip);
        let err = oracle
            .dml_block(&case.chain(), &hex(&case.block), &tip_only, true, true)
            .expect_err("the quorum base is below the headers");
        assert_eq!(err.kind, ErrorKind::Context, "{err}");
    }
}
