//! Every branch of the ported handlers and renderers, with Core's literal replies.

use dash_consensus::deployments::ThresholdState;
use dash_consensus::deployments::info::{Bip9Info, Bip9StatsInfo, BuriedInfo, DeploymentInfo};
use dash_consensus::{BlockValidationError, BlockValidationResult, Header};
use dash_params::{BuriedDeployment, DeploymentPos};
use serde_json::json;

use super::*;
use crate::chain::build::{self, BlockPlan, GENESIS_TIME, REGTEST_BITS, VERSION_BITS_TOP};

/// A node with one answer per call, recording what it was asked.
#[derive(Default)]
struct Stub {
    known: Option<Known>,
    processed: Option<Processed>,
    header: Option<BlockValidationError>,
    proposed: Option<Result<BlockState, NotTested>>,
    calls: Vec<&'static str>,
}

impl Node for Stub {
    fn lookup(&mut self, _: BlockHash) -> Option<Known> {
        self.calls.push("lookup");
        self.known
    }

    fn process_block(&mut self, _: Block) -> Processed {
        self.calls.push("process_block");
        self.processed.clone().expect("a result")
    }

    fn process_header(&mut self, _: Header) -> Option<BlockValidationError> {
        self.calls.push("process_header");
        self.header.clone()
    }

    fn propose(&mut self, _: Block) -> Result<BlockState, NotTested> {
        self.calls.push("propose");
        self.proposed.clone().expect("a verdict")
    }
}

fn genesis() -> BlockHash {
    BlockHash::from(build::genesis_hash())
}

/// Block 1 on regtest's genesis, its OP_TRUE coinbase first.
fn block() -> build::Built {
    BlockPlan {
        prev: build::genesis_hash(),
        version: VERSION_BITS_TOP,
        time: GENESIS_TIME + 150,
        bits: REGTEST_BITS,
        txs: vec![build::coinbase(1, build::subsidy(1), 1)],
        merkle_override: None,
    }
    .build()
}

fn rejected(reason: &'static str) -> BlockValidationError {
    BlockValidationError::new(BlockValidationResult::Consensus, reason)
}

fn known(scripts: bool, failed: bool) -> Option<Known> {
    Some(Known {
        scripts,
        failed,
    })
}

fn processed(new_block: bool, accepted: bool, checked: Option<BlockState>) -> Option<Processed> {
    Some(Processed {
        new_block,
        accepted,
        checked,
    })
}

#[test]
fn bip22_spells_each_state_as_core() {
    assert_eq!(bip22(&BlockState::Valid), Ok(Value::Null));
    assert_eq!(bip22(&BlockState::Invalid(rejected("bad-cb-amount"))), text("bad-cb-amount"));
    assert_eq!(bip22(&BlockState::Invalid(rejected(""))), text("rejected"));
    let state = BlockState::Error("System error while flushing".into());
    assert_eq!(bip22(&state), error(RPC_VERIFY_ERROR, "System error while flushing"));
    let with_debug = rejected("bad-txns-inputs-missingorspent").with_debug("CheckTxInputs: x");
    assert_eq!(bip22(&BlockState::Invalid(with_debug)), text("bad-txns-inputs-missingorspent"));
}

#[test]
fn submitblock_refuses_bytes_before_the_node_sees_them() {
    let mut node = Stub::default();
    assert_eq!(submit_block(&[], &mut node), error(-22, "Block decode failed"));
    assert_eq!(submit_block(&[1, 2, 3], &mut node), error(-22, "Block decode failed"));
    // A header with no transactions decodes; it has no coinbase.
    let mut empty = block().header.to_vec();
    empty.push(0);
    assert_eq!(submit_block(&empty, &mut node), error(-22, "Block does not start with a coinbase"));
    assert!(node.calls.is_empty());
}

#[test]
fn submitblock_answers_from_the_index_first() {
    let bytes = block().bytes;
    let mut node = Stub {
        known: known(true, false),
        ..Stub::default()
    };
    assert_eq!(submit_block(&bytes, &mut node), text("duplicate"));
    node.known = known(false, true);
    assert_eq!(submit_block(&bytes, &mut node), text("duplicate-invalid"));
    assert_eq!(node.calls, ["lookup", "lookup"]);
    // Known, neither valid to scripts nor failed: processed.
    node.known = known(false, false);
    node.processed = processed(true, true, Some(BlockState::Valid));
    assert_eq!(submit_block(&bytes, &mut node), Ok(Value::Null));
    assert_eq!(node.calls[2..], ["lookup", "process_block"]);
}

#[test]
fn submitblock_takes_bytes_after_the_block() {
    // Core: core_read.cpp:144-159 never checks the stream is spent.
    let mut bytes = block().bytes;
    bytes.extend([0xde, 0xad]);
    let mut node = Stub {
        processed: processed(true, true, Some(BlockState::Valid)),
        ..Stub::default()
    };
    assert_eq!(submit_block(&bytes, &mut node), Ok(Value::Null));
    assert_eq!(node.calls, ["lookup", "process_block"]);
}

#[test]
fn submitblock_reads_the_state_catcher() {
    let bytes = block().bytes;
    // Already stored, and accepted: a duplicate whatever was checked.
    let mut node = Stub {
        processed: processed(false, true, Some(BlockState::Valid)),
        ..Stub::default()
    };
    assert_eq!(submit_block(&bytes, &mut node), text("duplicate"));
    // No BlockChecked for the block: inconclusive.
    node.processed = processed(true, true, None);
    assert_eq!(submit_block(&bytes, &mut node), text("inconclusive"));
    node.processed = processed(false, false, None);
    assert_eq!(submit_block(&bytes, &mut node), text("inconclusive"));
    // Otherwise BIP22 over the caught state.
    let invalid = BlockState::Invalid(rejected("bad-cb-amount"));
    node.processed = processed(false, false, Some(invalid));
    assert_eq!(submit_block(&bytes, &mut node), text("bad-cb-amount"));
    node.processed = processed(true, true, Some(BlockState::Valid));
    assert_eq!(submit_block(&bytes, &mut node), Ok(Value::Null));
    // An error state throws.
    node.processed = processed(true, false, Some(BlockState::Error("x".into())));
    assert_eq!(submit_block(&bytes, &mut node), error(-25, "x"));
}

#[test]
fn submitheader_needs_the_previous_header() {
    let header = block().header;
    let mut node = Stub::default();
    let message = format!("Must submit previous header ({}) first", block_hash_hex(genesis()));
    assert_eq!(submit_header(&header, &mut node), error(-25, &message));
    assert_eq!(node.calls, ["lookup"]);
    node.known = known(true, false);
    assert_eq!(submit_header(&header, &mut node), Ok(Value::Null));
    node.header = Some(rejected("bad-diffbits").with_debug("incorrect proof of work"));
    assert_eq!(submit_header(&header, &mut node), error(-25, "bad-diffbits"));
}

#[test]
fn the_genesis_hash_is_core_hex() {
    // Core's regtest genesis, as `getblockhash 0` prints it.
    assert_eq!(
        block_hash_hex(genesis()),
        "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e"
    );
}

/// The proposal's checks before `TestBlockValidity` are V's, in one command (their order is
/// `dashd-chainstate`'s test): here, Core's strings for each answer.
#[test]
fn a_proposal_answers_in_cores_strings() {
    let built = block();
    let mut node = Stub::default();
    assert_eq!(proposal(&[], &mut node), error(-22, "Block decode failed"));
    assert!(node.calls.is_empty());
    for (not_tested, reply) in [
        (NotTested::Duplicate, "duplicate"),
        (NotTested::DuplicateInvalid, "duplicate-invalid"),
        (NotTested::DuplicateInconclusive, "duplicate-inconclusive"),
        (NotTested::NotBestPrev, "inconclusive-not-best-prevblk"),
    ] {
        node.proposed = Some(Err(not_tested));
        assert_eq!(proposal(&built.bytes, &mut node), text(reply));
    }
    node.proposed = Some(Ok(BlockState::Invalid(rejected("bad-cb-amount"))));
    assert_eq!(proposal(&built.bytes, &mut node), text("bad-cb-amount"));
    node.proposed = Some(Ok(BlockState::Valid));
    assert_eq!(proposal(&built.bytes, &mut node), Ok(Value::Null));
    assert_eq!(node.calls, ["propose"; 6]);
    // Unlike submitblock, a proposal has no coinbase check: the block reaches the validity test.
    let mut empty = built.header.to_vec();
    empty.push(0);
    node.proposed = Some(Ok(BlockState::Invalid(rejected("bad-blk-length"))));
    assert_eq!(proposal(&empty, &mut node), text("bad-blk-length"));
}

#[test]
fn the_block_rpcs_reply_null_or_not_found() {
    assert_eq!(block_rpc(Ok(())), Ok(Value::Null));
    assert_eq!(block_rpc(Err(BlockNotFound)), error(-5, "Block not found"));
}

#[test]
fn checked_spells_o8_classes() {
    let hash = genesis();
    // The genesis as Core's report spells it (see `the_genesis_hash_is_core_hex`).
    let wire: ChainHash = serde_json::from_value(json!(
        "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e"
    ))
    .expect("a hash");
    assert_eq!(checked(hash, &BlockState::Valid), Checked(wire, CheckResult::Valid, None, None));
    let e = BlockValidationError::new(BlockValidationResult::CachedInvalid, "duplicate");
    assert_eq!(
        checked(hash, &BlockState::Invalid(e)),
        Checked(
            wire,
            CheckResult::Invalid,
            Some("cached_invalid".into()),
            Some("duplicate".into())
        )
    );
    let e = BlockValidationError::new(BlockValidationResult::ChainLock, "bad-chainlock");
    assert_eq!(checked(hash, &BlockState::Invalid(e)).2.as_deref(), Some("chainlock"));
    assert_eq!(
        checked(hash, &BlockState::Error("x".into())),
        Checked(wire, CheckResult::Error, None, Some("x".into()))
    );
}

#[test]
fn difficulty_is_core_double() {
    assert_eq!(difficulty(REGTEST_BITS), 4.6565423739069247e-10);
    assert_eq!(difficulty(0x1d00_ffff), 1.0);
    assert_eq!(difficulty(0x1e0f_fff0), 0.000_244_140_625);
}

/// The reply's number is the 16-digit text Core writes, parsed: equal to the double for
/// regtest's bits only by chance.
#[test]
fn reply_doubles_are_univalue_text() {
    let core = |text: &str| serde_json::from_str::<Value>(text).expect("a number");
    for (bits, text) in [
        (REGTEST_BITS, "4.656542373906925e-10"),
        (0x1b04_04cb, "16307.42093852398"),
        (0x1e0f_ffff, "0.0002441371325370145"),
        (0x1d00_ffff, "1"),
        (0x1e0f_fff0, "0.000244140625"),
    ] {
        assert_eq!(univalue_double(difficulty(bits)), core(text), "{bits:#x}");
    }
    assert_eq!(univalue_double(1e16), core("1e+16"));
    assert_eq!(univalue_double(123_456_789_012_345.6), core("123456789012345.6"));
    assert_eq!(univalue_double(0.0001), core("0.0001"));
    assert_eq!(univalue_double(0.000_012_5), core("1.25e-05"));
}

#[test]
fn chain_tips_are_rows_by_height_then_hash() {
    let row = |height, hash: u8, status| TipRow {
        height,
        hash: BlockHash::from([hash; 32]),
        bits: REGTEST_BITS,
        chain_work: [0; 32],
        branchlen: 0,
        fork_point: BlockHash::from([hash; 32]),
        status,
    };
    let rows = chain_tips(&[row(3, 2, "valid-fork"), row(5, 9, "active"), row(3, 1, "invalid")]);
    let order: Vec<(u64, &str)> = rows
        .iter()
        .map(|r| (r["height"].as_u64().expect("height"), r["status"].as_str().expect("status")))
        .collect();
    assert_eq!(order, [(5, "active"), (3, "invalid"), (3, "valid-fork")]);
    let mut work = [0; 32];
    work[31] = 0x92;
    work[30] = 0x01;
    let mut active = row(200, 0x2b, "active");
    active.chain_work = work;
    assert_eq!(
        chain_tips(&[active])[0],
        json!({
            "branchlen": 0,
            "chainwork": "0000000000000000000000000000000000000000000000000000000000000192",
            "difficulty": serde_json::from_str::<Value>("4.656542373906925e-10").expect("a number"),
            "forkpoint": "2b".repeat(32),
            "hash": "2b".repeat(32),
            "height": 200,
            "status": "active",
        })
    );
}

#[test]
fn deployment_info_is_getdeploymentinfo() {
    let buried = DeploymentInfo::Buried(BuriedInfo {
        deployment: BuriedDeployment::Dip0003,
        active: false,
        height: 2000,
    });
    let started = DeploymentInfo::Bip9(Bip9Info {
        pos: DeploymentPos::TestDummy,
        bit: Some(28),
        start_time: 0,
        timeout: 999_999_999_999,
        ehf: false,
        ehf_height: None,
        activation_height: None,
        min_activation_height: 0,
        status: ThresholdState::Started,
        since: 12,
        status_next: ThresholdState::Started,
        statistics: Some(Bip9StatsInfo {
            period: 12,
            elapsed: 9,
            count: 0,
            threshold: Some(9),
            possible: Some(false),
        }),
        signalling: Some("---------".into()),
        height: None,
        active: false,
    });
    let defined = DeploymentInfo::Bip9(Bip9Info {
        pos: DeploymentPos::V24,
        bit: None,
        start_time: 0,
        timeout: i64::MAX,
        ehf: true,
        ehf_height: None,
        activation_height: None,
        min_activation_height: 0,
        status: ThresholdState::Defined,
        since: 0,
        status_next: ThresholdState::Defined,
        statistics: None,
        signalling: None,
        height: None,
        active: false,
    });
    let hash = BlockHash::from([0x2b; 32]);
    // Core's entries at the S0 image tip (O2b-3b probe of the POST section).
    assert_eq!(
        deployment_info(hash, 200, &[buried, started, defined]),
        json!({
            "hash": "2b".repeat(32),
            "height": 200,
            "deployments": {
                "dip0003": {"active": false, "height": 2000, "type": "buried"},
                "testdummy": {"active": false, "bip9": {"bit": 28, "ehf": false,
                    "min_activation_height": 0, "signalling": "---------", "since": 12,
                    "start_time": 0, "statistics": {"count": 0, "elapsed": 9, "period": 12,
                    "possible": false, "threshold": 9}, "status": "started",
                    "status_next": "started", "timeout": 999_999_999_999_i64}, "type": "bip9"},
                "v24": {"active": false, "bip9": {"ehf": true, "min_activation_height": 0,
                    "since": 0, "start_time": 0, "status": "defined", "status_next": "defined",
                    "timeout": i64::MAX}, "type": "bip9"},
            },
        })
    );
}

#[test]
fn locked_in_and_active_entries_carry_their_heights() {
    let info = DeploymentInfo::Bip9(Bip9Info {
        pos: DeploymentPos::TestDummy,
        bit: Some(28),
        start_time: 0,
        timeout: 999_999_999_999,
        ehf: false,
        ehf_height: Some(7),
        activation_height: Some(36),
        min_activation_height: 0,
        status: ThresholdState::LockedIn,
        since: 24,
        status_next: ThresholdState::Active,
        statistics: Some(Bip9StatsInfo {
            period: 12,
            elapsed: 11,
            count: 11,
            threshold: None,
            possible: None,
        }),
        signalling: Some("###########".into()),
        height: Some(36),
        active: true,
    });
    let v = deployment_info(BlockHash::from([1; 32]), 35, &[info]);
    let entry = &v["deployments"]["testdummy"];
    assert_eq!(entry["height"], 36);
    assert_eq!(entry["active"], true);
    assert_eq!(entry["bip9"]["activation_height"], 36);
    assert_eq!(entry["bip9"]["ehf_height"], 7);
    assert_eq!(entry["bip9"]["statistics"], json!({"period": 12, "elapsed": 11, "count": 11}));
}
