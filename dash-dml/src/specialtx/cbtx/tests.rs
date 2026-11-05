//! L0 for [`check_cbtx`] and [`cbtx_payload`] (EV1 §3.10, DEC-749): one refusal test per check,
//! each naming the mutant it kills, at heights read from Core's parameters; the pre-DIP0003
//! cases DEC-730 rests on; and testnet coinbases Core accepted.

use dash_consensus::ctx::{Deployment, DeploymentView, bls_ctx_for_block};
use dash_consensus::primitives::Tx;
use dash_consensus::validation::TxValidationResult;
use dash_consensus::{BlockId, ConsensusCtx};
use dash_params::{
    BuriedDeployment, ChainOption, ChainParamsBuilder, ChainSetup, Network, ParamsError, Release,
};
use dash_primitives::{CoinbaseCommitment, MerkleRoot, Transaction, TxType};
use dash_types::codec::BaseCodec;

use super::{cbtx_payload, check_cbtx};
use crate::specialtx::testctx::{NoCheckpoints, encoded, special, tx_of, txin, txout};
use crate::testkit::MemChain;
use crate::testkit::replay::{mainnet, testnet};

/// Buried deployments at `params`' heights for a check whose `pindexPrev` is at `prev_height`,
/// as `DeploymentActiveAt(*pindexPrev)` and `DeploymentActiveAfter(pindexPrev)` read them
/// (`deploymentstatus.h:13-27`). No versionbits deployment: `CheckCbTx` gets V24 as an argument.
struct AtPrev<'p> {
    params: &'p ChainSetup,
    prev_height: u32,
}

impl AtPrev<'_> {
    fn first(&self, d: Deployment) -> u32 {
        match d {
            Deployment::Buried(dep) => self.params.consensus.deployment_height(dep).to_u32(),
            _ => u32::MAX,
        }
    }
}

impl DeploymentView for AtPrev<'_> {
    fn active_after(&self, prev: Option<BlockId>, d: Deployment) -> bool {
        prev.map_or(0, |_| self.prev_height + 1) >= self.first(d)
    }

    fn active_at(&self, _: BlockId, d: Deployment) -> bool {
        self.prev_height >= self.first(d)
    }
}

/// `params`' activation height of `dep`.
fn height_of(params: &ChainSetup, dep: BuriedDeployment) -> u32 {
    params.consensus.deployment_height(dep).to_u32()
}

/// `CheckCbTx` of `cbtx` for the block at `height` (`pindexPrev` at `height - 1`), or with a
/// null `pindexPrev` when `height` is `None`: the reason, after asserting it is TX_CONSENSUS.
fn verdict(
    params: &ChainSetup,
    height: Option<u32>,
    cbtx: &CoinbaseCommitment,
    v24: bool,
) -> Result<(), String> {
    let chain = MemChain::new();
    let check_height = height.unwrap_or(0);
    let deployments = AtPrev {
        params,
        prev_height: check_height.saturating_sub(1),
    };
    let cons = ConsensusCtx {
        params,
        height: check_height,
        prev: height.map(|_| chain.genesis()),
        chain: &chain,
        deployments: &deployments,
        checkpoints: &NoCheckpoints,
        bls: bls_ctx_for_block(params, check_height),
        median_time_past: 0,
        header_check_time: 0,
    };
    check_cbtx(cbtx, &cons, v24).map_err(|e| {
        assert_eq!(e.result(), TxValidationResult::Consensus, "{}", e.reject_reason());
        e.reject_reason().to_owned()
    })
}

/// A CbTx of `version` at `height`. `CheckCbTx` reads only those two fields.
fn cbtx(version: u16, height: u32) -> CoinbaseCommitment {
    CoinbaseCommitment {
        version,
        height: i32::try_from(height).expect("a Core height"),
        merkle_root_mn_list: MerkleRoot::default(),
        merkle_root_quorums: (version >= 2).then(MerkleRoot::default),
        best_cl_height_diff: None,
        best_cl_signature: None,
        credit_pool_balance: None,
        merkle_root_asset_unlocks: None,
    }
}

/// The versions `check_cbtx` accepts at `height`, of 0 to 5.
fn accepted(params: &ChainSetup, height: u32, v24: bool) -> Vec<u16> {
    (0..=5).filter(|&v| verdict(params, Some(height), &cbtx(v, height), v24).is_ok()).collect()
}

/// Regtest with `-testactivationheight=<name>@<height>` for each of `heights` (Core's names).
fn regtest_with(heights: &[(&str, u32)]) -> Result<ChainSetup, ParamsError> {
    heights
        .iter()
        .fold(ChainParamsBuilder::new(Network::Regtest, Release::V24_0), |b, (name, h)| {
            b.arg(ChainOption::TestActivationHeight, format!("{name}@{h}"))
        })
        .build()
}

fn regtest(heights: &[(&str, u32)]) -> ChainSetup {
    regtest_with(heights).expect("Core's regtest parameters")
}

const VERSION: &str = "bad-cbtx-version";

/// Step 1 (`cbtx.cpp:26-28`): versions 0 and 5 up are refused, before the `pindexPrev` guard.
/// Kills M-1a (`== INVALID` dropped) and M-1b (`>= UNKNOWN` as `> UNKNOWN`).
#[test]
fn version_outside_one_to_four_is_refused() {
    let params = mainnet();
    for height in [None, Some(height_of(params, BuriedDeployment::V20) + 1)] {
        for version in [0, 5, 6, u16::MAX] {
            let h = height.unwrap_or(0);
            assert_eq!(verdict(params, height, &cbtx(version, h), false), Err(VERSION.into()));
        }
    }
}

/// Step 2's guard (`cbtx.cpp:30`): with a null `pindexPrev` only step 1 runs, so a CbTx of any
/// height and of versions 1 to 4 passes. Kills M-2g (the guard as a height reject).
#[test]
fn null_prev_runs_only_the_version_range() {
    let params = mainnet();
    for version in 1..=4 {
        assert_eq!(verdict(params, None, &cbtx(version, 7), false), Ok(()), "v{version}");
        assert_eq!(verdict(params, None, &cbtx(version, 7), true), Ok(()), "v{version}");
    }
}

/// Step 2 (`cbtx.cpp:31-33`): the height is `pindexPrev`'s plus one. Kills M-2 (the check
/// dropped) and M-2o (compared with `pindexPrev`'s height).
#[test]
fn height_must_follow_prev() {
    let params = mainnet();
    let h = height_of(params, BuriedDeployment::V20) + 10;
    assert_eq!(verdict(params, Some(h), &cbtx(3, h), false), Ok(()));
    for wrong in [h - 1, h + 1, 0] {
        assert_eq!(verdict(params, Some(h), &cbtx(3, wrong), false), Err("bad-cbtx-height".into()));
    }
}

/// Step 2 runs before steps 3 to 5: a wrong height and a wrong version give the height.
/// Kills M-2r (step 2 moved after step 4).
#[test]
fn height_is_checked_before_the_version_rules() {
    let params = mainnet();
    let h = height_of(params, BuriedDeployment::V20) + 10;
    assert_eq!(verdict(params, Some(h), &cbtx(1, h + 1), false), Err("bad-cbtx-height".into()));
}

/// Step 3 (`cbtx.cpp:35-38`): once DIP0008 is active *at* `pindexPrev`, version 1 is refused.
/// At DIP0008's own height `pindexPrev` is below it, so version 1 still passes. Kills M-3 (the
/// check dropped) and M-3a (`DeploymentActiveAfter` for `DeploymentActiveAt`).
#[test]
fn dip0008_at_prev_refuses_version_one() {
    for (net, params) in [("mainnet", mainnet()), ("testnet", testnet())] {
        let dip8 = height_of(params, BuriedDeployment::Dip0008);
        assert_eq!(accepted(params, dip8, false), [1, 2], "{net} at {dip8}");
        assert_eq!(accepted(params, dip8 + 1, false), [2], "{net} at {}", dip8 + 1);
    }
}

/// Step 4 (`cbtx.cpp:40-43`): V20 *after* `pindexPrev` needs version 3 up; before, version 3 up
/// is refused. Kills M-4a (`DeploymentActiveAt` for `DeploymentActiveAfter`), M-4l (the
/// `isV20 && < 3` half dropped) and M-4h (the `!isV20 && >= 3` half dropped).
#[test]
fn v20_after_prev_needs_version_three() {
    for (net, params) in [("mainnet", mainnet()), ("testnet", testnet())] {
        let v20 = height_of(params, BuriedDeployment::V20);
        assert_eq!(accepted(params, v20 - 1, false), [2], "{net} at {}", v20 - 1);
        assert_eq!(accepted(params, v20, false), [3], "{net} at {v20}");
    }
}

/// Step 5 (`cbtx.cpp:47-51`): with V20 active, V24 needs version 4 and its absence refuses it.
/// Kills M-5l (the `requires && < 4` half dropped) and M-5h (the `!requires && >= 4` half
/// dropped).
#[test]
fn v24_with_v20_needs_version_four() {
    let params = mainnet();
    let h = height_of(params, BuriedDeployment::V20) + 10;
    assert_eq!(accepted(params, h, true), [4]);
    assert_eq!(accepted(params, h, false), [3]);
}

/// Step 5's `requires_unlock_root = is_v24_active && isV20` (`cbtx.cpp:47`): V24 without V20
/// asks nothing of step 5, so version 2 passes and version 4 is refused (by step 4 first).
/// Kills M-5v (`requires_unlock_root = is_v24_active`).
#[test]
fn v24_without_v20_does_not_require_version_four() {
    let params = mainnet();
    let h = height_of(params, BuriedDeployment::V20) - 1;
    assert_eq!(accepted(params, h, true), [2]);
}

/// DEC-730: none of the steps reads DIP0003, so below it the CbTx is judged by the same rules.
/// Mainnet and testnet activate DIP0008 and V20 after DIP0003, so versions 1 and 2 pass below it.
#[test]
fn below_dip0003_on_mainnet_and_testnet() {
    for (net, params) in [("mainnet", mainnet()), ("testnet", testnet())] {
        let dip3 = height_of(params, BuriedDeployment::Dip0003);
        assert!(dip3 < height_of(params, BuriedDeployment::Dip0008), "{net}");
        for h in [2, dip3 - 1] {
            assert_eq!(accepted(params, h, false), [1, 2], "{net} at {h}");
        }
    }
}

/// DEC-730 on regtest at the pin's defaults: DIP0008 at 1 ("Always active unless overridden",
/// `chainparams.cpp:822`) and V20 at DIP0003's height (`:828`). Below DIP0003, version 1 passes
/// only in block 1, where `pindexPrev` is genesis, below DIP0008; from block 2 only version 2
/// passes. Versions 3 and 4 are refused (step 4) whatever V24 is.
#[test]
fn below_dip0003_on_regtest_defaults() {
    let params = regtest(&[]);
    let dip3 = height_of(&params, BuriedDeployment::Dip0003);
    assert_eq!(height_of(&params, BuriedDeployment::Dip0008), 1);
    assert_eq!(height_of(&params, BuriedDeployment::V20), dip3);
    assert_eq!(accepted(&params, 1, false), [1, 2]);
    for h in [2, dip3 - 1] {
        assert_eq!(accepted(&params, h, false), [2], "at {h}");
        assert_eq!(accepted(&params, h, true), [2], "at {h} with V24");
    }
}

/// DEC-730: V20 cannot activate below DIP0003 (`chainparams.cpp:963-964` asserts
/// `V20Height >= DIP0003Height` after `-testactivationheight` and `-dip3params` are applied at
/// `:879-880`; the parameters refuse both as Core does), so `isV20` is false for every
/// pre-DIP0003 block and versions 3 and 4 are refused there, whatever V24 is. At the lowest V20
/// allowed, DIP0003's own height, the block before still takes version 2 and that block takes
/// version 3.
#[test]
fn below_dip0003_v20_is_never_active() {
    let defaults = regtest(&[]);
    let dip3 = height_of(&defaults, BuriedDeployment::Dip0003);
    assert_eq!(
        regtest_with(&[("v20", dip3 - 1)]).unwrap_err(),
        ParamsError::V20BelowDip3,
        "Core asserts V20Height >= DIP0003Height"
    );
    let v20 = height_of(&defaults, BuriedDeployment::V20);
    let dip3_above_v20 = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, format!("{}:{}", v20 + 1, v20 + 2))
        .build();
    assert_eq!(dip3_above_v20.unwrap_err(), ParamsError::V20BelowDip3);
    let params = regtest(&[("v20", dip3)]);
    assert_eq!(accepted(&params, dip3 - 1, true), [2]);
    assert_eq!(accepted(&params, dip3, false), [3]);
    assert_eq!(accepted(&params, dip3, true), [4]);
}

/// DEC-730 with `-testactivationheight=dip0008@N` above DIP0003: below DIP0003 version 1 passes
/// again, as on mainnet.
#[test]
fn below_dip0003_with_dip0008_moved_above_it() {
    let dip3 = height_of(&regtest(&[]), BuriedDeployment::Dip0003);
    let params = regtest(&[("dip0008", dip3 + 1)]);
    assert_eq!(accepted(&params, dip3 - 1, false), [1, 2]);
}

/// The testnet coinbases Core accepted, on both sides of V20 (905,101 is V20's second block):
/// height, the CbTx version Core shows, the transaction.
fn testnet_coinbases() -> Vec<(u32, u16, Transaction)> {
    let text = include_str!("../../../tests/data/cbtx_testnet_coinbases.txt");
    let rows: Vec<_> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|line| {
            let mut cols = line.split(' ');
            let height: u32 = cols.next().unwrap().parse().unwrap();
            let version: u16 = cols.next().unwrap().parse().unwrap();
            let raw = unhex(cols.next().unwrap());
            (height, version, Transaction::decode(&mut &raw[..]).expect("Core's coinbase"))
        })
        .collect();
    assert_eq!(rows.len(), 3);
    rows
}

/// Both callers' path: [`cbtx_payload`], then [`check_cbtx`] for the block at `height`. V24 is not
/// active on testnet at the fixture's heights (a v3 CbTx would be refused by step 5).
fn caller_verdict(height: u32, tx: Transaction) -> Result<(), String> {
    let tx = Tx::new(tx);
    let payload = cbtx_payload(&tx, tx.payload()).map_err(|e| e.reject_reason().to_owned())?;
    verdict(testnet(), Some(height), payload, false)
}

/// Each coinbase Core accepted passes both functions at its height.
#[test]
fn testnet_coinbases_core_accepted() {
    for (height, version, raw) in testnet_coinbases() {
        let tx = Tx::new(raw.clone());
        let payload = cbtx_payload(&tx, tx.payload()).expect("a CbTx");
        assert_eq!((payload.version, i64::from(payload.height)), (version, i64::from(height)));
        assert_eq!(caller_verdict(height, raw), Ok(()), "at {height}");
    }
}

/// Direct [`check_cbtx`] boundaries on the decoded payloads, not a caller's verdict on edited
/// bytes: one block later the height is wrong, and the other side of V20's version rule is
/// refused (`cbtx.cpp:31-33`, `:40-43`).
#[test]
fn check_cbtx_boundaries_on_testnet_payloads() {
    let params = testnet();
    for (height, version, raw) in testnet_coinbases() {
        let tx = Tx::new(raw);
        let payload = cbtx_payload(&tx, tx.payload()).expect("a CbTx");
        let later = verdict(params, Some(height + 1), payload, false);
        assert_eq!(later, Err("bad-cbtx-height".into()), "at {height}");
        let mut other = payload.clone();
        other.version = if version == 2 {
            3
        } else {
            2
        };
        let edited = verdict(params, Some(height), &other, false);
        assert_eq!(edited, Err(VERSION.into()), "at {height}");
    }
}

/// The callers' verdict on a fixture with its wire bytes edited. The height (bytes 2..6) keeps the
/// payload's shape, so the path reaches `bad-cbtx-height` (`cbtx.cpp:31-33`). The version (bytes
/// 0..2) changes the shape `CCbTx` reads (`cbtx.h:54-67`): a v2 payload read as v3 runs out of
/// bytes, and a v3 one read as v2 leaves bytes behind, so `GetTxPayload` gives `nullopt` either
/// way (`specialtx.h:20-27`) and the path stops at `bad-cbtx-payload`, before `CheckCbTx`
/// (`specialtxman.cpp:251-255`, `:881-892`). Kills M-0e (a failed decode as `bad-cbtx-version`),
/// as `payload_must_decode_to_its_last_byte` does.
#[test]
fn testnet_coinbases_edited_on_the_wire() {
    for (height, version, raw) in testnet_coinbases() {
        let mut later = raw.clone();
        later.extra_payload[2..6].copy_from_slice(&(height as i32 + 1).to_le_bytes());
        assert_eq!(caller_verdict(height, later), Err("bad-cbtx-height".into()), "at {height}");
        let mut other = raw;
        let edited: u16 = if version == 2 {
            3
        } else {
            2
        };
        other.extra_payload[..2].copy_from_slice(&edited.to_le_bytes());
        let got = caller_verdict(height, other);
        assert_eq!(got, Err("bad-cbtx-payload".into()), "v{version} as v{edited} at {height}");
    }
}

/// A type-5 transaction with `payload` bytes, a coinbase or spending an output.
fn type5(coinbase: bool, payload: Vec<u8>) -> Tx {
    let input = if coinbase {
        txin([0; 32], u32::MAX)
    } else {
        txin([7; 32], 0)
    };
    tx_of(TxType::CoinbaseCommitment, vec![input], vec![txout(1, vec![0x51])], payload)
}

/// The caller's first reject (`specialtxman.cpp:248-250`, `:878-880`): a type-5 transaction that
/// is not a coinbase, even with a valid payload. Kills M-0c (the `IsCoinBase` check dropped).
#[test]
fn payload_of_a_non_coinbase_is_refused() {
    let tx = type5(false, encoded(&cbtx(2, 10)));
    assert!(!tx.is_coinbase());
    let err = cbtx_payload(&tx, tx.payload()).unwrap_err();
    assert_eq!(
        (err.result(), err.reject_reason()),
        (TxValidationResult::Consensus, "bad-cbtx-invalid")
    );
}

/// The caller's second reject (`specialtxman.cpp:251-255`, `:881-892`): `GetTxPayload` needs the
/// whole payload read (`specialtx.h:24`), so a trailing byte or a short payload is
/// `bad-cbtx-payload`, and the coinbase check comes first. Kills M-0p (`is_complete` dropped).
#[test]
fn payload_must_decode_to_its_last_byte() {
    let bytes = encoded(&cbtx(2, 10));
    let tx = type5(true, bytes.clone());
    assert_eq!(cbtx_payload(&tx, tx.payload()).map(|c| c.height), Ok(10));
    let mut trailing = bytes.clone();
    trailing.push(0);
    let short = bytes[..bytes.len() - 1].to_vec();
    for edited in [trailing, short, Vec::new()] {
        let tx = type5(true, edited.clone());
        let err = cbtx_payload(&tx, tx.payload()).unwrap_err();
        assert_eq!(
            (err.result(), err.reject_reason()),
            (TxValidationResult::Consensus, "bad-cbtx-payload")
        );
        let not_coinbase = type5(false, edited);
        let err = cbtx_payload(&not_coinbase, not_coinbase.payload()).unwrap_err();
        assert_eq!(err.reject_reason(), "bad-cbtx-invalid");
    }
}

/// `GetTxPayload<CCbTx>` of another type's slot is `nullopt` (`specialtx.h:33`): the slot of a
/// quorum commitment, or none at all, is `bad-cbtx-payload`.
#[test]
fn another_types_slot_is_not_a_cbtx() {
    let coinbase = type5(true, encoded(&cbtx(2, 10)));
    let other =
        special(TxType::QuorumCommitment, vec![txin([0; 32], u32::MAX)], vec![], &cbtx(2, 10));
    assert_eq!(
        cbtx_payload(&coinbase, other.payload()).unwrap_err().reject_reason(),
        "bad-cbtx-payload"
    );
    assert_eq!(cbtx_payload(&coinbase, None).unwrap_err().reject_reason(), "bad-cbtx-payload");
}

/// Core's block path keys on `vtx[0]->nType` (`specialtxman.cpp:876`), and below DIP0003 a
/// version-1 or version-2 coinbase may carry type bits 5 (a non-special version with another
/// `nType` is `bad-txns-type` only under DIP0003, `validation.cpp:355-378`). It has no payload
/// field, so `GetTxPayload` reads an empty payload and fails (`specialtx.h:20-27`):
/// `bad-cbtx-payload` (`specialtxman.cpp:891`). Kills M-0n (a `None` slot as `bad-cbtx-invalid`).
#[test]
fn a_pre_special_version_coinbase_of_type_five_has_no_payload() {
    for version in [1, 2] {
        let tx = Tx::new(Transaction {
            version,
            tx_type: TxType::CoinbaseCommitment,
            inputs: vec![txin([0; 32], u32::MAX)],
            outputs: vec![txout(1, vec![0x51])],
            lock_time: 0,
            extra_payload: Vec::new(),
        });
        assert!(tx.is_coinbase());
        assert_eq!(tx.tx_type(), TxType::CoinbaseCommitment, "v{version}");
        assert!(!tx.has_extra_payload_field() && tx.payload().is_none(), "v{version}");
        let err = cbtx_payload(&tx, tx.payload()).unwrap_err();
        let got = (err.result(), err.reject_reason());
        assert_eq!(got, (TxValidationResult::Consensus, "bad-cbtx-payload"), "v{version}");
    }
}

/// `IsCoinBase` needs exactly one input, a null one (`primitives/transaction.h:308-311`): a type-5
/// transaction whose first of two inputs is null is not a coinbase. Kills M-0i (the coinbase test
/// as "the first input is null").
#[test]
fn two_inputs_with_the_first_null_is_not_a_coinbase() {
    let bytes = encoded(&cbtx(2, 10));
    let inputs = vec![txin([0; 32], u32::MAX), txin([7; 32], 0)];
    let tx = tx_of(TxType::CoinbaseCommitment, inputs, vec![txout(1, vec![0x51])], bytes);
    assert!(!tx.is_coinbase());
    let err = cbtx_payload(&tx, tx.payload()).unwrap_err();
    assert_eq!(err.reject_reason(), "bad-cbtx-invalid");
}

fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}
