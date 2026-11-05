//! LQ1's L1 targets (`node_l1::llmq`): the boundary rows, the proptest twins, and every quorum of
//! the recorded chains (`dash-oracle-sys/tests/data`, provenance in its `PROVENANCE.md`): the
//! regtest functional-test chain against `quorum info` and `quorum rotationinfo` (LQ1b's rotated
//! cycles), and the mainnet and testnet samples against Core's context. SB8 adds Core's `Verify`
//! of every recorded non-rotated commitment under both list views (the masternode list and the
//! SML), the §5.1 mutation gate against Core's `Verify`, and the regtest CbTx ChainLocks against
//! Core's scan and selection.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_dml::codec::core_disk::decode_list_s3;
use dash_dml::testkit::dmn_bytes;
use dash_dml::{Dmn, MnCold, MnList};
use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_evo::effect::EffectSink;
use dash_evo::llmq::reference::{
    ModelEvent, ModelQuorumManager, ModelRows, ModelViews, PerTypeCache, RuntimeRows,
};
use dash_evo::llmq::{
    CommitmentError, CommitmentReject, LlmqCommitment, LlmqCtx, LlmqError, LlmqSource, MemberError,
    MembersRoute, MinedCommitmentReader, MnListView, PrecomputeMode, QuorumMaterializer,
    QuorumMemberSource, QuorumMembers, QuorumSnapshot, SIGN_HEIGHT_OFFSET, ScanEntry, ScanEvent,
    ScanOverload, ScratchMnList, SigPath, UncachedMembers, VerifiedQuorum, compute_quorum_members,
    is_quorum_rotation_enabled, members_gate, members_route, members_uncached, scan_cache_size,
    scan_quorums, select_from_scan, select_quorum_for_signing, select_start_height,
    verify_commitment, verify_quorum,
};
use dash_evo::locks::{
    ClsigHashSchemes, ISLOCK_MAX_INPUTS, IslockSchemes, LockError, LockReject, LockSchemes,
    QuorumScan, QuorumScanView, QuorumStatus, RecSigBan, RecSigError, RecSigIgnore, RecSigVerdict,
    RecoveredSig, ScanLookup, ScannedQuorum, chainlock_request_id, clsig_hash, isdlock_hash,
    islock_request_id, verify_chainlock, verify_chainlock_sig, verify_islock, verify_qsigrec,
};
use dash_evo::sml::{SmlEntry, SmlList, SmlMember, SmlScratch};
use dash_evo::{BlsCtx, bls_ctx_after, bls_ctx_for_block};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, DEPLOYMENT_V24, ErrorKind, GetQuorumOutcome,
    LlmqContext, LlmqFixture, LockStatus, LockVerdict, MembersOutcome, MinedVerdict,
    MnList as OracleMnList, Oracle, PassStatus, Pin, Precompute,
    QuorumMembers as OracleQuorumMembers, ScanCache, ThresholdState, TraceEvent, Verdict,
};
use dash_p2p_core::{ClSig, IsdLock};
use dash_params::LlmqType;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_pkc::bls::core::{BlsPkValue, BlsSigValue};
use dash_pkc::bls::{BlsEagerPk, BlsEagerSig, BlsSchemeKind};
use dash_primitives::{BlockHash, DynBitset, QuorumHash, QuorumVvecHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use node_l1::llmq::{
    Answer, HeightDeployments, HeldChain, Loaded, MAX_CALLS, MapSource, MinedIndex, MinedSpec,
    NoRows, QuorumCall, QuorumOp, RotCall, RotOp, SCAN_OPS, SELECT_OPS, Scenario, TYPES, ViewError,
    call_bytes, coinbase_facts, compare_members, compare_quorum_calls, config_byte, core_call,
    core_event, core_members_event, header_hash, mined_bytes, nine, nine_phased, open,
    rot_call_bytes, rot_scenario_bytes, rows_digest, scenario_bytes, seed_mined, snapshot_bytes,
    twelve,
};
use node_l1::{
    Outcome, compare_quorum_members, compare_quorum_members_rot, compare_scan_quorums,
    compare_select_quorum, compare_type_enabled,
};
use proptest::prelude::*;

// ---- boundary rows ----

/// The call types by index (`node_l1::llmq`'s table).
const TEST: u8 = 0;
const TEST_V17: u8 = 1;
const TEST_DIP0024: u8 = 2;
const TEST_INSTANTSEND: u8 = 3;
const TEST_PLATFORM: u8 = 4;
const LLMQ_50_60: u8 = 5;
const DEVNET: u8 = 6;
const UNKNOWN: u8 = 7;

fn with_calls(mut input: Vec<u8>, calls: &[(u8, u32, bool)]) -> Vec<u8> {
    assert!(calls.len() <= MAX_CALLS);
    for (ty, base, reset) in calls {
        input.extend([*ty, *base as u8, u8::from(*reset)]);
    }
    input
}

/// Core's answer to each call of a members input: its outcome and member count, or its refusal.
fn core_answers(input: &[u8]) -> Vec<Result<(MembersOutcome, u64), ErrorKind>> {
    let mut rest = input;
    let scenario = Scenario::read(&mut rest).expect("a scenario");
    let (_, _, mut core) =
        scenario.load(&Oracle::new(Pin::V24_0)).unwrap_or_else(|e| panic!("Core loads it: {e:?}"));
    let mut answers = Vec::new();
    for call in rest.as_chunks::<3>().0.iter().take(MAX_CALLS) {
        let base = *node_l1::llmq::headers()[usize::from(call[1])].1.as_bytes();
        let answer = core.members(TYPES[usize::from(call[0])], &base, false, call[2] & 1 != 0);
        match answer {
            Ok((_, events)) => answers.push(Ok(core_members_event(&events).expect("one event"))),
            Err(e) => {
                answers.push(Err(e.kind));
                break;
            }
        }
    }
    answers
}

use MembersOutcome::{BaseHit, Computed, Gate, IndexOutOfRange};

/// One boundary row: the input agrees, and Core answers as `expected` (what the row exercises).
fn row(name: &str, input: &[u8], expected: &[Result<(MembersOutcome, u64), ErrorKind>]) {
    assert_eq!(compare_quorum_members(input, Pin::V24_0), Ok(Outcome::BothAccept), "{name}");
    assert_eq!(core_answers(input), expected, "{name}: Core's answers");
}

#[test]
fn members_boundary_rows() {
    let never = 3;
    row(
        "pre-V20: compute, hit, reset clears the type's cache",
        &with_calls(
            scenario_bytes(config_byte(never, 0, never, false), 96, true, &nine()),
            &[
                (TEST, 24, false),
                (TEST, 24, false),
                (TEST, 48, false),
                (TEST, 24, true),
                (TEST, 48, false),
            ],
        ),
        &[
            Ok((Computed, 3)),
            Ok((BaseHit, 3)),
            Ok((Computed, 3)),
            Ok((Computed, 3)),
            Ok((Computed, 3)),
        ],
    );
    row(
        "V20 from 2: the work block 8 below the base, lists changing every block",
        &with_calls(
            scenario_bytes(config_byte(0, 0, never, false), 96, false, &nine_phased()),
            &[(TEST, 24, false), (TEST, 48, false), (TEST, 72, false), (TEST, 96, false)],
        ),
        &[Ok((Computed, 3)), Ok((Computed, 3)), Ok((Computed, 3)), Ok((Computed, 3))],
    );
    row(
        "V20 at 40: bases on both sides of it",
        &with_calls(
            scenario_bytes(config_byte(1, 0, never, false), 96, true, &nine_phased()),
            &[(TEST, 24, false), (TEST, 48, false)],
        ),
        &[Ok((Computed, 3)), Ok((Computed, 3))],
    );
    // LLMQ_TEST keeps 3 connections: past 6 entries the cache keeps the 3 newest, exactly: the
    // third newest (28) is kept and the fourth (27) is not, so Core's trace decides the size.
    let mut calls: Vec<(u8, u32, bool)> = (24..=30).map(|h| (TEST, h, false)).collect();
    calls.extend([(TEST, 30, false), (TEST, 28, false), (TEST, 27, false), (TEST, 24, false)]);
    let mut expected = vec![Ok((Computed, 3))];
    expected.extend(std::iter::repeat_n(Ok((Computed, 0)), 6));
    expected.extend([Ok((BaseHit, 0)), Ok((BaseHit, 0)), Ok((Computed, 0)), Ok((Computed, 3))]);
    row(
        "eviction: non-base heights compute nothing and are cached; the 3 newest are kept",
        &with_calls(scenario_bytes(config_byte(never, 0, never, false), 48, true, &nine()), &calls),
        &expected,
    );
    let evo_pool = [(0, 3), (8, 3), (16, 3), (1, 3), (2, 3), (3, 3), (4, 3)];
    row(
        "Platform from V19: Evo only",
        &with_calls(
            scenario_bytes(config_byte(never, 0, never, false), 48, true, &evo_pool),
            &[(TEST_PLATFORM, 24, false), (TEST_PLATFORM, 48, false)],
        ),
        &[Ok((Computed, 3)), Ok((Computed, 3))],
    );
    // Evo 0, 8, 16 and regular 1, 2: a regular masternode is among Core's members (N1).
    row(
        "Platform before V19: every type",
        &with_calls(
            scenario_bytes(config_byte(never, never, never, false), 48, true, &evo_pool[..5]),
            &[(TEST_PLATFORM, 24, false)],
        ),
        &[Ok((Computed, 3))],
    );
    row(
        "banned and unconfirmed skipped; equal scores ordered by collateral; size 5",
        &with_calls(
            scenario_bytes(
                config_byte(never, 0, never, true),
                48,
                true,
                &[(1, 3), (2, 0x13), (3, 0x17), (4, 0x1b), (5, 3), (6, 0x13), (7, 3)],
            ),
            &[(TEST, 24, false), (TEST, 48, false)],
        ),
        &[Ok((Computed, 5)), Ok((Computed, 5))],
    );
    row(
        "gates and rotated routes",
        &with_calls(
            scenario_bytes(config_byte(never, 0, 0, false), 48, true, &nine()),
            &[
                (TEST, 0, false),
                (UNKNOWN, 0, false),
                (TEST_V17, 24, false),
                (TEST_INSTANTSEND, 24, false),
                (LLMQ_50_60, 24, false),
                (TEST_DIP0024, 26, false),
                (TEST_DIP0024, 5, false),
                (TEST_DIP0024, 24, false),
            ],
        ),
        &[
            Ok((Gate, 0)),
            Ok((Gate, 0)),
            Ok((Gate, 0)),
            Ok((Gate, 0)),
            Ok((Gate, 0)),
            Ok((IndexOutOfRange, 0)),
            Ok((Computed, 0)),
            Ok((Computed, 1)),
        ],
    );
    row(
        "InstantSend before DIP0024",
        &with_calls(
            scenario_bytes(config_byte(never, 0, never, false), 48, true, &nine()),
            &[(TEST_INSTANTSEND, 24, false), (TEST_DIP0024, 24, false)],
        ),
        &[Ok((Computed, 3)), Ok((Gate, 0))],
    );
    for (name, ty, kind) in [
        ("enabled, not configured", LLMQ_50_60, ErrorKind::Argument),
        ("devnet, not configured", DEVNET, ErrorKind::Argument),
        ("unknown", UNKNOWN, ErrorKind::Exception),
    ] {
        row(
            name,
            &with_calls(
                scenario_bytes(config_byte(never, 0, never, false), 48, true, &nine()),
                &[(TEST, 24, false), (ty, 24, false)],
            ),
            &[Ok((Computed, 3)), Err(kind)],
        );
    }
    row(
        "V20 from 2: bases at and below 8",
        &with_calls(
            scenario_bytes(config_byte(0, 0, never, false), 48, true, &nine()),
            &[(TEST, 8, false), (TEST, 5, false)],
        ),
        &[Ok((Computed, 0)), Err(ErrorKind::Argument)],
    );
}

/// Every config byte's scenario loads on both sides, so no input reads as out of domain for a
/// scenario one side alone refuses.
#[test]
fn every_scenario_config_loads_on_both_sides() {
    for v20 in 0..4 {
        for v19 in 0..4 {
            for dip24 in 0..4 {
                for size5 in [false, true] {
                    let config = config_byte(v20, v19, dip24, size5);
                    let input = scenario_bytes(config, 24, true, &nine());
                    assert!(open(&mut &input[..], Pin::V24_0).is_ok(), "config {config:#04x}");
                }
            }
        }
    }
}

#[test]
fn type_enabled_boundary_rows() {
    let base = scenario_bytes(config_byte(3, 0, 1, false), 48, true, &[]);
    let mut answered = Vec::new();
    for ty in 0..7u8 {
        for prev in [0u8, 28, 29, 48, 200] {
            answered.push([ty, prev, 0]);
        }
    }
    // A type Core does not know, with no previous block: false, before the type is read.
    answered.push([UNKNOWN, 200, 0]);
    answered.extend([[3, 40, 1], [3, 40, 2], [3, 40, 4], [3, 40, 8], [5, 40, 5], [5, 40, 10]]);
    for chunk in answered.chunks(MAX_CALLS) {
        let mut input = base.clone();
        input.extend(chunk.iter().flatten());
        assert_eq!(compare_type_enabled(&input, Pin::V24_0), Ok(Outcome::BothAccept), "{chunk:?}");
    }
    // Types Core does not know, with a previous block: it throws, and an input of nothing else
    // is a rejection; after an answered query, an acceptance.
    for query in [[UNKNOWN, 0, 0], [UNKNOWN, 29, 0], [0x07, 10, 0x80], [0xfe, 48, 0x80]] {
        let alone = [&base[..], &query].concat();
        assert_eq!(compare_type_enabled(&alone, Pin::V24_0), Ok(Outcome::BothReject), "{query:?}");
        let after = [&base[..], &[0, 28, 0], &query].concat();
        assert_eq!(compare_type_enabled(&after, Pin::V24_0), Ok(Outcome::BothAccept), "{query:?}");
    }
}

// ---- scans and selection ----

/// The scenario `input` with mined commitments ([`mined_bytes`]).
fn with_mined(input: Vec<u8>, mined: &[(u8, u32, u8, u8)]) -> Vec<u8> {
    [input, mined_bytes(mined)].concat()
}

/// What Core's trace says of one call: its scan event's cache use, and its `GetQuorum` outcomes.
#[derive(Debug, PartialEq, Eq)]
struct Traced {
    scan: Option<ScanCache>,
    get_quorum: Vec<GetQuorumOutcome>,
    result: usize,
}

/// Core alone over a scan or selection input, seeded as the comparison seeds it.
fn core_trace(input: &[u8], ops: &[QuorumOp]) -> Vec<Result<Traced, ErrorKind>> {
    let mut rest = input;
    let Loaded {
        params,
        chain,
        source,
        mut core,
    } = open(&mut rest, Pin::V24_0).unwrap_or_else(|_| panic!("Core loads it"));
    let deployments = HeightDeployments {
        params: &params,
    };
    let bls = bls_ctx_after(&deployments, Some(chain.at(chain.tip())));
    let ctx = LlmqCtx::new(&params, &chain, &deployments, bls).expect("in domain");
    let specs = MinedSpec::read(&mut rest, &params, chain.tip());
    seed_mined(&specs, &params, &ctx, &chain, &source, &mut core)
        .expect("seeded")
        .expect("in domain");
    let mut answers = Vec::new();
    for bytes in rest.as_chunks::<6>().0.iter().take(MAX_CALLS) {
        let call = QuorumCall::read(bytes, ops, chain.tip());
        if call.op == QuorumOp::Restart {
            core.restart().expect("restart");
            continue;
        }
        let at = chain.hash(call.block(chain.tip())).expect("held");
        match core_call(&mut core, &call, at.as_bytes(), bls.is_legacy()) {
            Ok((answer, trace)) => answers.push(Ok(Traced {
                scan: trace.iter().find_map(|e| match e {
                    TraceEvent::Scan {
                        cache,
                        ..
                    } => Some(*cache),
                    _ => None,
                }),
                get_quorum: trace
                    .iter()
                    .filter_map(|e| match e {
                        TraceEvent::GetQuorum {
                            outcome,
                            ..
                        } => Some(*outcome),
                        _ => None,
                    })
                    .collect(),
                result: match answer {
                    Answer::Bases(bases) => bases.len(),
                    Answer::Members(members) => members.len(),
                },
            })),
            Err(e) => {
                answers.push(Err(e.kind));
                break;
            }
        }
    }
    answers
}

fn traced(scan: Option<ScanCache>, get_quorum: &[GetQuorumOutcome], result: usize) -> Traced {
    Traced {
        scan,
        get_quorum: get_quorum.to_vec(),
        result,
    }
}

/// One scan or selection row: the input agrees, and Core traces it as `expected`.
fn quorum_row(name: &str, ops: &[QuorumOp], input: &[u8], expected: &[Result<Traced, ErrorKind>]) {
    let compared = compare_quorum_calls(input, Pin::V24_0, ops);
    assert_eq!(compared, Ok(Outcome::BothAccept), "{name}");
    assert_eq!(core_trace(input, ops), expected, "{name}: Core's trace");
}

/// LLMQ_TEST commitments for the bases 24, 48 and 72, each mined 12 blocks later.
fn three_mined(input: Vec<u8>) -> Vec<u8> {
    with_mined(input, &[(TEST, 24, 12, 0), (TEST, 48, 12, 0), (TEST, 72, 12, 0)])
}

#[test]
fn scan_boundary_rows() {
    use GetQuorumOutcome::{Built, CacheHit};
    use ScanCache::{Bypass, HitContinued, HitSatisfied, Miss};
    let never = 3;
    let (scan, members, restart) = (0, 1, 2);
    let none = [0; 3];
    let base = || scenario_bytes(config_byte(never, 0, never, false), 96, true, &nine());
    let mut input = three_mined(base());
    for c in [
        // Store 90 (before 96's window): keepOldConnections (3) built, the first 2 returned.
        call_bytes(scan, TEST, false, 96, 2, none),
        call_bytes(scan, TEST, false, 96, 1, none),
        // Three cached, five wanted: a continuation below base 24 finds nothing more.
        call_bytes(scan, TEST, false, 96, 5, none),
        call_bytes(scan, TEST, true, 96, 2, none),
        call_bytes(members, TEST, false, 72, 0, none),
        // After the window of 72 (mined at 84): store 90 again.
        call_bytes(scan, TEST, false, 91, 0x80 | 3, none),
        call_bytes(restart, TEST, false, 0, 0, none),
        // In the window: store 89, cold caches.
        call_bytes(scan, TEST, false, 89, 2, none),
        // Store 18: nothing mined at or below it.
        call_bytes(scan, TEST, false, 30, 2, none),
        call_bytes(scan, TEST, false, 96, 0, none),
        // A type Core does not know: it throws.
        call_bytes(scan, UNKNOWN, false, 96, 2, none),
    ] {
        input.extend(c);
    }
    quorum_row(
        "miss, hit, continuation, overload, members, restart, empty, refusal",
        &SCAN_OPS,
        &input,
        &[
            Ok(traced(Some(Miss), &[Built, Built, Built], 2)),
            Ok(traced(Some(HitSatisfied), &[], 1)),
            Ok(traced(Some(HitContinued), &[], 3)),
            Ok(traced(Some(Bypass), &[CacheHit, CacheHit, CacheHit], 2)),
            Ok(traced(None, &[], 3)),
            Ok(traced(Some(HitSatisfied), &[], 3)),
            Ok(traced(Some(Miss), &[Built, Built, Built], 2)),
            Ok(traced(Some(Miss), &[], 0)),
            Ok(traced(None, &[], 0)),
            Err(ErrorKind::Exception),
        ],
    );

    // More than keepOldConnections built: the fourth unpopulated, the cache keeps three. Store
    // 114 (before 120's window).
    let mut input = with_mined(
        scenario_bytes(config_byte(never, 0, never, false), 120, true, &nine()),
        &[(TEST, 24, 12, 0), (TEST, 48, 12, 0), (TEST, 72, 12, 0), (TEST, 96, 12, 0)],
    );
    input.extend(call_bytes(scan, TEST, false, 120, 4, none));
    input.extend(call_bytes(scan, TEST, false, 120, 4, none));
    quorum_row(
        "populate_cache past keepOldConnections, then a continuation from the cache",
        &SCAN_OPS,
        &input,
        &[
            Ok(traced(Some(Miss), &[Built, Built, Built, Built], 4)),
            Ok(traced(Some(HitContinued), &[CacheHit], 4)),
        ],
    );

    // A commitment based at genesis: Core never stores it (CFinalCommitment::Verify rejects a
    // parentless base), so no cached base is genesis and the scan finds 24's quorum alone.
    let mut input = with_mined(base(), &[(TEST, 0, 12, 0), (TEST, 24, 12, 0)]);
    input.extend(call_bytes(scan, TEST, false, 47, 3, none));
    quorum_row(
        "a commitment based at genesis",
        &SCAN_OPS,
        &input,
        &[Ok(traced(Some(Miss), &[Built], 1))],
    );

    // Rotated: indexed rounds, each quorum built from the cycle's rotated members. The cycle of
    // 96 is the first whose four quarters hold minSize (4) members, so the first a node mines.
    let mut input = with_mined(
        scenario_bytes(config_byte(never, 0, 0, false), 120, true, &nine()),
        &[(TEST_DIP0024, 96, 12, 0), (TEST_DIP0024, 96, 12, 1)],
    );
    input.extend(call_bytes(scan, TEST_DIP0024, false, 120, 2, none));
    quorum_row("rotated rounds", &SCAN_OPS, &input, &[Ok(traced(Some(Miss), &[Built, Built], 2))]);
}

#[test]
fn select_boundary_rows() {
    use GetQuorumOutcome::{Built, CacheHit};
    use ScanCache::{HitSatisfied, Miss};
    let never = 3;
    let (select, scan) = (0, 1);
    let tip = 0xff;
    let mut input =
        three_mined(scenario_bytes(config_byte(never, 0, never, false), 96, true, &nine()));
    for c in [
        // Start 88, in 72's window: the quorums of 72 (mined at 84) and 48.
        call_bytes(select, TEST, false, tip, 8, [1, 2, 0]),
        call_bytes(select, TEST, false, tip, 8, [7, 7, 0]),
        // Start 52, before 48's window: store 42, the quorum of 24 alone.
        call_bytes(select, TEST, false, 60, 8, [1, 2, 0]),
        call_bytes(scan, TEST, false, 88, 2, [0; 3]),
        // Above the tip (by a negative offset, then by the sign height), and below genesis: no
        // start block.
        call_bytes(select, TEST, false, tip, (-1i8) as u8, [1, 2, 0]),
        call_bytes(select, TEST, false, 107, 8, [1, 2, 0]),
        call_bytes(select, TEST, false, 3, 8, [1, 2, 0]),
        call_bytes(select, TEST, false, 8, 8, [1, 2, 0]),
        call_bytes(select, UNKNOWN, false, tip, 8, [1, 2, 0]),
    ] {
        input.extend(c);
    }
    quorum_row(
        "selections from the tip and a height, the window, both ends, refusal",
        &SELECT_OPS,
        &input,
        &[
            Ok(traced(Some(Miss), &[Built, Built, Built], 1)),
            Ok(traced(Some(HitSatisfied), &[], 1)),
            Ok(traced(Some(Miss), &[CacheHit], 1)),
            Ok(traced(Some(HitSatisfied), &[], 2)),
            Ok(traced(None, &[], 0)),
            Ok(traced(None, &[], 0)),
            Ok(traced(None, &[], 0)),
            Ok(traced(None, &[], 0)),
            Err(ErrorKind::Argument),
        ],
    );

    // Rotated: the signer bit (62) picks index 0 or 1 (the cycle of 96, as above).
    for (byte, name) in [(0x00, "signer 0"), (0x40, "signer 1")] {
        let mut input = with_mined(
            scenario_bytes(config_byte(never, 0, 0, false), 120, true, &nine()),
            &[(TEST_DIP0024, 96, 12, 0), (TEST_DIP0024, 96, 12, 1)],
        );
        input.extend(call_bytes(select, TEST_DIP0024, false, tip, 8, [1, 2, byte]));
        quorum_row(name, &SELECT_OPS, &input, &[Ok(traced(Some(Miss), &[Built, Built], 1))]);
    }
}

/// Every LQ1a fuzz target agrees with Core on each of its public seeds
/// (`node-fuzz/public-seeds`, written by the `llmq_seeds` example), each seed is current, and
/// each target compares more than it skips.
#[test]
fn fuzz_targets_agree_on_their_public_seeds() {
    use bitcoin_hashes::sha256;
    let seeds = concat!(env!("CARGO_MANIFEST_DIR"), "/../node-fuzz/public-seeds");
    let mut outcomes = std::collections::BTreeMap::<(&str, String), usize>::new();
    for (target, input) in node_l1::llmq::seed_inputs() {
        let digest = sha256::Hash::hash(&input).to_byte_array();
        let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        let path = format!("{seeds}/{target}/{name}");
        assert_eq!(std::fs::read(&path).ok(), Some(input.clone()), "{path}: rerun llmq_seeds");
        let entry = node_l1::TARGETS.iter().find(|t| t.name == target).expect("a target");
        let outcome = (entry.compare)(&input, Pin::V24_0)
            .unwrap_or_else(|d| panic!("{target}: {path} diverges: {d}"));
        *outcomes.entry((target, format!("{outcome:?}"))).or_default() += 1;
    }
    eprintln!("{outcomes:?}");
    for target in [
        "diff_quorum_members",
        "diff_type_enabled",
        "diff_scan_quorums",
        "diff_select_quorum",
        "diff_quorum_members_rot",
    ] {
        let count = |o: Outcome| outcomes.get(&(target, format!("{o:?}"))).copied().unwrap_or(0);
        assert!(count(Outcome::BothAccept) > count(Outcome::OutOfDomain), "{target}: {outcomes:?}");
    }
}

// ---- the rotated target (LQ1b) ----

/// Core's answer to one rotated call: the modes of the rows it stored, and its members events'
/// `(outcome, n)`.
type RotAnswer = (Vec<i32>, Vec<(MembersOutcome, u64)>);

/// Core's trace of each call of a rotated input without seeded rows (members, precomputes and
/// restarts only): per computing call, the modes of the rows it stored and its members events'
/// `(outcome, n)`.
fn core_rot_answers(input: &[u8]) -> Vec<RotAnswer> {
    let mut rest = input;
    let scenario = Scenario::read_rot(&mut rest).expect("a scenario");
    let (chain, _, mut core) =
        scenario.load(&Oracle::new(Pin::V24_0)).unwrap_or_else(|e| panic!("Core loads it: {e:?}"));
    let mut answers = Vec::new();
    for bytes in rest.as_chunks::<4>().0 {
        let call = RotCall::read(bytes, chain.tip());
        let hash = *chain.hash(call.height).expect("held").as_bytes();
        let trace = match call.op {
            RotOp::Restart => {
                core.restart().expect("a restart");
                continue;
            }
            RotOp::SeedRow => panic!("no seeded rows here"),
            RotOp::Members => {
                core.members(call.llmq_type, &hash, false, call.flag).expect("members").1
            }
            RotOp::Precompute => {
                let mode = if call.flag {
                    Precompute::Undo
                } else {
                    Precompute::Connect
                };
                core.precompute(&hash, false, mode).expect("a precompute")
            }
        };
        let rows = trace
            .iter()
            .filter_map(|e| match e {
                TraceEvent::Row {
                    snapshot,
                    ..
                } => Some(i32::from_le_bytes(snapshot[..4].try_into().expect("a mode"))),
                _ => None,
            })
            .collect();
        let members = trace
            .iter()
            .filter_map(|e| match e {
                TraceEvent::Members {
                    outcome,
                    n,
                    ..
                } => Some((*outcome, *n)),
                _ => None,
            })
            .collect();
        answers.push((rows, members));
    }
    answers
}

/// One rotated row: the input agrees, and Core answers as `expected`.
fn rot_row(name: &str, input: &[u8], expected: &[RotAnswer]) {
    assert_eq!(compare_quorum_members_rot(input, Pin::V24_0), Ok(Outcome::BothAccept), "{name}");
    assert_eq!(core_rot_answers(input), expected, "{name}: Core's answers");
}

#[test]
fn rotated_boundary_rows() {
    use MembersOutcome::IndexHit;
    let (members, precompute, restart) = (0, 1, 2);
    let dip24 = TEST_DIP0024;
    let base = rot_scenario_bytes([0xff, 0, 0], 168, false, true, &twelve());

    // Connecting every cycle base: each stores its row, and a cycle's quorums grow by a quarter
    // per previous cycle with a row (H - C, H - 2C, H - 3C), to four; a later index is a hit.
    let mut input = base.clone();
    for cycle in (24..=144u16).step_by(24) {
        input.extend(rot_call_bytes(precompute, dip24, false, cycle, 0));
    }
    input.extend(rot_call_bytes(members, dip24, false, 145, 0));
    let computed = |n| (vec![0], vec![(Computed, n)]);
    rot_row(
        "connected cycles",
        &input,
        &[
            computed(1),
            computed(2),
            computed(3),
            computed(4),
            computed(4),
            computed(4),
            (vec![], vec![(IndexHit, 4)]),
        ],
    );

    // Out of cycle order: the cycle of 144 first reads no row of 120 and stops there; 96 and 120
    // then store theirs, but 144 is cached until a reset; after a restart it reads them.
    let mut input = base.clone();
    for (op, height, flag) in [
        (members, 144, false),
        (members, 96, false),
        (members, 120, false),
        (members, 144, false),
        (members, 144, true),
        (restart, 0, false),
        (members, 168, false),
        (precompute, 168, true),
    ] {
        input.extend(rot_call_bytes(op, dip24, flag, height, 0));
    }
    rot_row(
        "rows absent, then present",
        &input,
        &[
            computed(1),
            computed(1),
            computed(2),
            (vec![], vec![(BaseHit, 1)]),
            computed(3),
            computed(4),
            computed(4),
        ],
    );

    // A precompute at genesis is skipped (Core never connects genesis through block processing
    // and refuses it), and seeding stays open.
    let mut input = base.clone();
    input.extend(rot_call_bytes(precompute, dip24, false, 0, 0));
    input.extend(rot_call_bytes(3, dip24, false, 48, 0x29));
    input.extend(rot_call_bytes(members, dip24, false, 72, 0));
    assert_eq!(compare_quorum_members_rot(&input, Pin::V24_0), Ok(Outcome::BothAccept));

    let connect = |pool: &[[u8; 4]], activations: [u8; 3]| {
        let mut input = rot_scenario_bytes(activations, 168, false, true, pool);
        for cycle in (24..=168u16).step_by(24) {
            input.extend(rot_call_bytes(precompute, dip24, false, cycle, 0));
        }
        input
    };
    let mode = |m: i32, n| (vec![m], vec![(Computed, n)]);

    // Four masternodes: the new quarters skip used ones, so the rows carry skip lists (mode 1),
    // which the later cycles read.
    let four: Vec<[u8; 4]> = (1..=4).map(|i| [i, 3, 0, 0]).collect();
    rot_row(
        "skip lists stored and read",
        &connect(&four, [0xff, 0, 0]),
        &[mode(0, 1), mode(0, 2), mode(1, 3), mode(1, 4), mode(1, 4), mode(1, 4), mode(1, 4)],
    );

    // 1 moves its service at 80 and 2, reusing 1's service, is listed from then on: at 96 the
    // used sets hold 1's old state, every candidate is used or collides, and Core returns no new
    // quarters and stores no row (quirk 28e-2), so 120 reads no row of 96.
    let moved = [[1, 3, 0, 1 << 6 | 10], [2, 0x23, 0, 0], [3, 3, 0, 0], [4, 3, 0, 0]];
    rot_row(
        "a previous member's service taken: no row",
        &connect(&moved, [0xff, 0, 0]),
        &[
            mode(0, 1),
            mode(1, 2),
            mode(1, 3),
            (vec![], vec![(Computed, 3)]),
            mode(0, 1),
            mode(0, 2),
            mode(1, 3),
        ],
    );

    // 3 is removed at 49 and 4 banned at 64: before V19 the used sets keep the removed member,
    // after it they drop it (`skipRemovedMNs`), so the cycle of 96 differs; the banned one is
    // dropped either way.
    let gone =
        [[1, 3, 0, 0], [2, 3, 0, 0], [3, 3, 0x21, 0], [4, 3, 0, 8], [5, 3, 0, 0], [6, 3, 0, 0]];
    let tail = [mode(1, 4), mode(1, 4), mode(1, 4)];
    rot_row(
        "removed and banned before V19",
        &connect(&gone, [0xff, 0xff, 0]),
        &[[mode(0, 1), mode(0, 2), mode(0, 3), mode(1, 4)].as_slice(), &tail].concat(),
    );
    rot_row(
        "removed and banned after V19",
        &connect(&gone, [0xff, 0, 0]),
        &[[mode(0, 1), mode(0, 2), mode(0, 3), mode(1, 4)].as_slice(), &tail].concat(),
    );
    assert_ne!(
        core_rows_at(&connect(&gone, [0xff, 0xff, 0]), 96),
        core_rows_at(&connect(&gone, [0xff, 0, 0]), 96),
        "V19's `skipRemovedMNs` changes the cycle of 96"
    );
}

/// The bytes of the row Core stores for the cycle at `height`, connecting `input`'s blocks to it.
fn core_rows_at(input: &[u8], height: u32) -> Option<Vec<u8>> {
    let mut rest = input;
    let scenario = Scenario::read_rot(&mut rest).expect("a scenario");
    let (chain, _, mut core) = scenario.load(&Oracle::new(Pin::V24_0)).expect("Core loads it");
    let mut trace = Vec::new();
    for h in 1..=height {
        let hash = chain.hash(h).expect("held");
        trace = core.precompute(hash.as_bytes(), false, Precompute::Connect).expect("a precompute");
    }
    trace.into_iter().find_map(|e| match e {
        TraceEvent::Row {
            snapshot,
            ..
        } => Some(snapshot),
        _ => None,
    })
}

// ---- the proptest twins ----

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, .. ProptestConfig::default() })]

    #[test]
    fn quorum_members_twin(input in prop::collection::vec(any::<u8>(), 3..80)) {
        let compared = compare_quorum_members(&input, Pin::V24_0);
        prop_assert!(compared.is_ok(), "{compared:?}");
    }

    #[test]
    fn quorum_members_rot_twin(input in prop::collection::vec(any::<u8>(), 5..160)) {
        let compared = compare_quorum_members_rot(&input, Pin::V24_0);
        prop_assert!(compared.is_ok(), "{compared:?}");
    }

    #[test]
    fn type_enabled_twin(input in prop::collection::vec(any::<u8>(), 3..60)) {
        let compared = compare_type_enabled(&input, Pin::V24_0);
        prop_assert!(compared.is_ok(), "{compared:?}");
    }

    #[test]
    fn scan_quorums_twin(input in prop::collection::vec(any::<u8>(), 3..120)) {
        let compared = compare_scan_quorums(&input, Pin::V24_0);
        prop_assert!(compared.is_ok(), "{compared:?}");
    }

    #[test]
    fn select_quorum_twin(input in prop::collection::vec(any::<u8>(), 3..120)) {
        let compared = compare_select_quorum(&input, Pin::V24_0);
        prop_assert!(compared.is_ok(), "{compared:?}");
    }
}

// ---- the recorded chains ----

fn data(name: &str) -> String {
    format!("{}/../dash-oracle-sys/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn read_gz(path: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).expect("the recorded data"))
        .read_to_end(&mut bytes)
        .expect("gzip");
    bytes
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// An RPC-order hash.
fn rpc_hash(s: &str) -> [u8; 32] {
    let mut h: [u8; 32] = unhex(s).try_into().expect("32 bytes");
    h.reverse();
    h
}

/// The node's members of a base through the public non-rotated steps (as SB8 composes them).
fn node_members(
    ctx: &LlmqCtx<'_, ViewError>,
    source: &MapSource,
    llmq_type: u8,
    base: u32,
) -> QuorumMembers<Arc<Dmn>> {
    let llmq_type = LlmqType::from_base(llmq_type);
    let base = BlockId(base);
    assert!(members_gate(ctx, llmq_type, base).expect("held"), "gated");
    assert_eq!(members_route(ctx, llmq_type, base), Ok(MembersRoute::NonRotated));
    compute_quorum_members(ctx, source, llmq_type, base).expect("held")
}

/// A sample's headers, from the first's parent (`hashPrevBlock`), which Core's context also
/// holds below them; the block index reaches the work block of a quorum based at `first`, whose
/// height alone Core reads before V20.
fn sample_chain(first: u32, headers: &[[u8; 80]]) -> HeldChain {
    let parent = BlockHash::from(<[u8; 32]>::try_from(&headers[0][4..36]).expect("32 bytes"));
    let hashes = headers.iter().map(|h| header_hash(h).expect("a header"));
    HeldChain::new(first - 1, std::iter::once(parent).chain(hashes)).indexed_from(first - 8)
}

/// One recorded quorum sample: its headers from `first`, and the work block's list and coinbase.
struct Sample {
    first: u32,
    headers: Vec<[u8; 80]>,
    chain: HeldChain,
    list_height: u32,
    list: Vec<u8>,
    coinbase: Vec<u8>,
}

impl Sample {
    fn read(sample: &serde_json::Value) -> Sample {
        let first = sample["first_height"].as_u64().expect("a height") as u32;
        let headers: Vec<[u8; 80]> = unhex(sample["headers"].as_str().expect("headers"))
            .chunks(80)
            .map(|h| h.try_into().expect("80 bytes"))
            .collect();
        Sample {
            first,
            chain: sample_chain(first, &headers),
            headers,
            list_height: sample["list_height"].as_u64().expect("a height") as u32,
            list: unhex(sample["list"].as_str().expect("a list")),
            coinbase: unhex(sample["coinbase"].as_str().expect("a coinbase")),
        }
    }
}

fn params_of(network: Network, args: &[String]) -> ChainSetup {
    let builder = args.iter().fold(ChainParamsBuilder::new(network, Release::V24_0), |b, arg| {
        let (name, value) = arg.split_once('=').expect("-name=value");
        b.arg(ChainOption::from_name(name).expect("a chain option"), value)
    });
    builder.build().expect("the recorded chain's parameters")
}

/// llmq_regtest_quorum_info, non-rotated: the node gives every LLMQ_TEST quorum of the
/// functional-test chain (`-llmqtestparams=4:4 -dip3params=2:2 -testactivationheight=v20@300`)
/// the members `quorum info` lists, and so does Core's context; the post-V20 quorums take the
/// coinbase-ChainLock modifier.
/// The recorded regtest chain (`llmq-regtest-v24.0`): its expectations, parameters, chain and
/// source, and Core's context loaded with it.
struct Regtest {
    expected: serde_json::Value,
    params: ChainSetup,
    chain: HeldChain,
    source: MapSource,
    core: LlmqContext,
}

fn regtest() -> Regtest {
    recorded_regtest("llmq-regtest-v24.0")
}

/// A recorded regtest chain (`<stem>.fixture.gz` and `<stem>.json`) and Core's context on it.
fn recorded_regtest(stem: &str) -> Regtest {
    let bytes = read_gz(&data(&format!("{stem}.fixture.gz")));
    let fixture = LlmqFixture::decode(&bytes).expect("a canonical fixture");
    let expected: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(data(&format!("{stem}.json"))).expect("the expectations"),
    )
    .expect("json");
    let params = params_of(Network::Regtest, &fixture.args);
    let hashes: Vec<_> =
        fixture.headers.iter().map(|(_, h)| header_hash(h).expect("a header")).collect();
    let height_of: HashMap<[u8; 32], u32> =
        hashes.iter().zip(0u32..).map(|(h, height)| (*h.as_bytes(), height)).collect();
    let chain = HeldChain::new(0, hashes.iter().copied());
    let mut source = MapSource::default();
    for (block, list) in &fixture.lists {
        let list = decode_list_s3(&mut &list[..]).expect("an evodb list");
        source.lists.insert(height_of[block], Arc::new(list));
    }
    for (block, tx, have_data) in &fixture.coinbases {
        let height = height_of[block];
        source.coinbases.insert(height, coinbase_facts(tx, *have_data).expect("a coinbase"));
    }
    let oracle = Oracle::new(Pin::V24_0);
    let args: Vec<&str> = fixture.args.iter().map(String::as_str).collect();
    let mut core = oracle.llmq_context(&Chain::new(&fixture.chain, &args)).expect("a context");
    // The rotated commitments need the snapshot rows a node stores on connecting their cycles,
    // which the fixture does not hold (O2a-SP's `set_mined` refuses them before): load the
    // non-rotated ones, which every test here reads (rotated members read no commitment).
    let unrotated = LlmqFixture {
        mined: fixture
            .mined
            .iter()
            .filter(|(t, ..)| {
                !params.consensus.llmq(LlmqType::from_base(*t)).is_some_and(|l| l.use_rotation)
            })
            .cloned()
            .collect(),
        ..fixture
    };
    core.load(&unrotated).expect("the fixture loads");
    Regtest {
        expected,
        params,
        chain,
        source,
        core,
    }
}

#[test]
fn regtest_quorums_match_quorum_info() {
    let Regtest {
        expected,
        params,
        chain,
        source,
        mut core,
    } = regtest();
    let deployments = HeightDeployments {
        params: &params,
    };
    let bls = bls_ctx_after(&deployments, Some(BlockId(chain.tip())));
    let ctx = LlmqCtx::new(&params, &chain, &deployments, bls).expect("in domain");
    let mut chainlock_forms = 0;
    let quorums: Vec<&serde_json::Value> = expected["quorums"]
        .as_array()
        .expect("quorums")
        .iter()
        .filter(|q| q["type"] == 100)
        .collect();
    for q in &quorums {
        let base = q["base_height"].as_u64().expect("a height") as u32;
        let ours = node_members(&ctx, &source, 100, base);
        let want: Vec<[u8; 32]> = q["members"]
            .as_array()
            .expect("members")
            .iter()
            .map(|m| rpc_hash(m.as_str().expect("a hash")))
            .collect();
        let got: Vec<[u8; 32]> = ours.iter().map(|d| *d.pro_tx_hash.as_bytes()).collect();
        assert_eq!(got, want, "quorum info at {base}");
        let base_hash = chain.hash(base).expect("held");
        let (theirs, _) = core.members(100, base_hash.as_bytes(), false, false).expect("members");
        assert_eq!(compare_members(&ours, &theirs), Ok(()), "Core at {base}");
        let work = &source.coinbases[&(base - 8)];
        if base >= 300 && nonnull_coinbase_chainlock(work, base - 8, true, bls).is_some() {
            chainlock_forms += 1;
        }
    }
    assert_eq!(quorums.len(), 5);
    assert_eq!(chainlock_forms, 3, "the post-V20 quorums take the ChainLock modifier");
}

/// llmq_regtest_quorum_info, rotated: connecting every block of the functional-test chain
/// through the model's `PreComputeQuorumMembers` traces as Core's context does, block by block
/// (rows included), stores the rows `quorum rotationinfo` reported on the recording node, and
/// then gives every quorum (the rotated cycles at 384, 408 and 432, both indexes) the members
/// `quorum info` lists, as Core's context does.
#[test]
fn regtest_rotation_matches_rotationinfo_and_quorum_info() {
    let Regtest {
        expected,
        params,
        chain,
        source,
        mut core,
    } = regtest();
    let deployments = HeightDeployments {
        params: &params,
    };
    let bls = bls_ctx_after(&deployments, Some(BlockId(chain.tip())));
    let ctx = LlmqCtx::new(&params, &chain, &deployments, bls).expect("in domain");
    let index = seed_mined(&[], &params, &ctx, &chain, &source, &mut core)
        .expect("no divergence")
        .expect("no commitments to seed");
    let views = ModelViews {
        ctx: &ctx,
        src: &source,
        reader: &index,
        tip: BlockId(chain.tip()),
    };
    let mut manager = ModelQuorumManager::<Arc<Dmn>>::new(&params);
    let mut rows = ModelRows::new();
    let mut stored = 0;
    for height in 1..=chain.tip() {
        let block = BlockId(height);
        manager
            .precompute(&views, &mut RuntimeRows::new(&mut rows), block, PrecomputeMode::Connect)
            .expect("held");
        let ours: Vec<TraceEvent> = manager.take_events().iter().map(core_event).collect();
        let hash = chain.hash(height).expect("held");
        let theirs = core.precompute(hash.as_bytes(), bls.is_legacy(), Precompute::Connect);
        assert_eq!(ours, theirs.expect("Core connects it"), "block {height}");
        stored += ours.iter().filter(|e| matches!(e, TraceEvent::Row { .. })).count();
    }
    let snapshots = expected["rotation_snapshots"].as_array().expect("rows");
    for row in snapshots {
        let ty = row["type"].as_u64().expect("a type") as u8;
        let cycle = chain.hash(row["cycle_height"].as_u64().expect("a height") as u32);
        let ours = rows.get(&(ty, cycle.expect("held"))).map(snapshot_bytes);
        assert_eq!(ours, Some(unhex(row["snapshot"].as_str().expect("hex"))), "{row}");
    }
    // H - C, H - 2C and H - 3C of the cycles at 384, 408 and 432: the cycles 312 .. 408.
    assert_eq!(snapshots.len(), 5);
    assert_eq!(stored, rows.len(), "one row per cycle");
    assert_eq!(rows_digest(&rows), core.rows_digest().expect("a digest"), "every row");

    let mut rotated = 0;
    for q in expected["quorums"].as_array().expect("quorums") {
        let llmq_type = q["type"].as_u64().expect("a type") as u8;
        let base = q["base_height"].as_u64().expect("a height") as u32;
        let ours = manager
            .members(
                &views,
                &mut RuntimeRows::new(&mut rows),
                LlmqType::from_base(llmq_type),
                BlockId(base),
                false,
            )
            .expect("held");
        let want: Vec<[u8; 32]> = q["members"]
            .as_array()
            .expect("members")
            .iter()
            .map(|m| rpc_hash(m.as_str().expect("a hash")))
            .collect();
        let got: Vec<[u8; 32]> = ours.iter().map(|d| *d.pro_tx_hash.as_bytes()).collect();
        assert_eq!(got, want, "quorum info: {llmq_type} at {base}");
        let base_hash = chain.hash(base).expect("held");
        let (theirs, _) =
            core.members(llmq_type, base_hash.as_bytes(), bls.is_legacy(), false).expect("members");
        assert_eq!(compare_members(&ours, &theirs), Ok(()), "Core: {llmq_type} at {base}");
        rotated += usize::from(llmq_type == 103);
    }
    assert_eq!(rotated, 6);
    assert_eq!(rows_digest(&rows), core.rows_digest().expect("a digest"), "no row added");
}

/// The rows digest's twin, and the row codec, against the known answers Core's
/// `oracle_llmq_rows_digest` gives (`dash-oracle-sys/tests/llmq.rs`, LQ1 §6.2): A, then A and B,
/// then A and B' (B's key, a later row).
#[test]
fn the_rows_digest_twin_matches_cores_known_answers() {
    let snapshot = |mode: i32, bits: &[u8], n: usize, skip_list: Vec<i32>| QuorumSnapshot {
        active_quorum_members: (0..n).map(|i| bits[i / 8] >> (i % 8) & 1 != 0).collect(),
        skip_list_mode: mode,
        skip_list,
    };
    let a = snapshot(0, &[0x4d, 0x01], 9, vec![]);
    let b = snapshot(1, &[0x0b], 4, vec![2, -1]);
    let b2 = snapshot(3, &[0x00], 4, vec![]);
    assert_eq!(snapshot_bytes(&a), unhex("00000000094d0100"));
    assert_eq!(snapshot_bytes(&b), unhex("01000000040b0202000000ffffffff"));
    assert_eq!(snapshot_bytes(&b2), unhex("03000000040000"));
    let a_key = (5, BlockHash::from([0x11; 32]));
    let b_key = (103, BlockHash::from(std::array::from_fn::<u8, 32, _>(|i| i as u8)));
    let mut rows = ModelRows::new();
    let digest = |rows: &ModelRows| rows_digest(rows).to_vec();
    assert_eq!(
        digest(&rows),
        unhex("6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d")
    );
    rows.insert(a_key, a);
    assert_eq!(
        digest(&rows),
        unhex("3fae486d7339fc149727a978309e4e6d5ba435dfb07d68380bde6b24385331d7")
    );
    rows.insert(b_key, b);
    assert_eq!(
        digest(&rows),
        unhex("8d633cea2b2d3a36fa0961257ee7e9bcd1e1d161d9db6018e425c8ec836ad66b")
    );
    rows.insert(b_key, b2);
    assert_eq!(
        digest(&rows),
        unhex("ef532660182377caea3e420b965b09967087c1091f7464ae4841a25dd30608a9")
    );
}

/// llmq_mainnet_testnet_membersig: the node gives every sampled mainnet and testnet quorum
/// Core's members (with which, `dash-oracle-sys`'s test shows, the mined commitment verifies).
#[test]
fn sampled_quorums_match_core() {
    let samples: serde_json::Value =
        serde_json::from_slice(&read_gz(&data("llmq-samples.json.gz"))).expect("json");
    let oracle = Oracle::new(Pin::V24_0);
    let mut compared = 0;
    for sample in samples["samples"].as_array().expect("samples") {
        let mainnet = sample["network"] == "mainnet";
        let (network, chain_name) = if mainnet {
            (Network::Main, "main")
        } else {
            (Network::Testnet3, "test")
        };
        let params = params_of(network, &[]);
        let Sample {
            first,
            headers,
            chain,
            list_height,
            list,
            coinbase,
        } = Sample::read(sample);
        let mut source = MapSource::default();
        source.lists.insert(list_height, Arc::new(decode_list_s3(&mut &list[..]).expect("a list")));
        source.coinbases.insert(list_height, coinbase_facts(&coinbase, true).expect("a coinbase"));
        let deployments = HeightDeployments {
            params: &params,
        };
        let fixture = LlmqFixture {
            chain: chain_name.into(),
            args: vec![],
            headers: headers.iter().zip(first..).map(|(h, height)| (height as i32, *h)).collect(),
            lists: vec![(*chain.hash(list_height).expect("held").as_bytes(), list.clone())],
            coinbases: vec![(*chain.hash(list_height).expect("held").as_bytes(), coinbase, true)],
            snapshots: vec![],
            mined: vec![],
        };
        let mut core = oracle.llmq_context(&Chain::new(chain_name, &[])).expect("a context");
        core.load(&fixture).expect("the sample loads");
        for q in sample["quorums"].as_array().expect("quorums") {
            let llmq_type = q["type"].as_u64().expect("a type") as u8;
            let base = q["base_height"].as_u64().expect("a height") as u32;
            let bls = bls_ctx_after(&deployments, Some(BlockId(base)));
            let ctx = LlmqCtx::new(&params, &chain, &deployments, bls).expect("in domain");
            let ours = node_members(&ctx, &source, llmq_type, base);
            let base_hash = chain.hash(base).expect("held");
            let (theirs, events) = core
                .members(llmq_type, base_hash.as_bytes(), bls.is_legacy(), false)
                .expect("members");
            assert_eq!(core_members_event(&events).map(|e| e.0), Some(MembersOutcome::Computed));
            assert!(!ours.is_empty());
            assert_eq!(
                compare_members(&ours, &theirs),
                Ok(()),
                "{} {llmq_type} at {base}",
                sample["network"]
            );
            compared += 1;
        }
    }
    assert!(compared >= 20, "{compared}");
}

/// The node's members never come from a list it does not hold: the sample's work block withheld
/// is `Missing(work)`, the not-held rule (EV addendum §2.1), and a withheld work-block coinbase
/// is `Missing(work)` too, never the hash-form modifier.
#[test]
fn a_withheld_list_or_coinbase_is_missing_never_a_value() {
    let samples: serde_json::Value =
        serde_json::from_slice(&read_gz(&data("llmq-samples.json.gz"))).expect("json");
    let sample = samples["samples"]
        .as_array()
        .expect("samples")
        .iter()
        .find(|s| s["quorums"][0]["base_height"] == 1_987_800)
        .expect("the post-V20 mainnet sample");
    let params = params_of(Network::Main, &[]);
    let Sample {
        headers,
        chain,
        list_height: work,
        list,
        coinbase,
        ..
    } = Sample::read(sample);
    let list: Arc<MnList> = Arc::new(decode_list_s3(&mut &list[..]).expect("a list"));
    let facts = coinbase_facts(&coinbase, true).expect("a coinbase");
    let deployments = HeightDeployments {
        params: &params,
    };
    let bls = bls_ctx_after(&deployments, Some(BlockId(work + 8)));
    let ctx = LlmqCtx::new(&params, &chain, &deployments, bls).expect("in domain");
    assert!(
        nonnull_coinbase_chainlock(&facts, work, true, bls).is_some(),
        "the work block carries a ChainLock"
    );
    let ty = LlmqType::from_base(4);
    let base = BlockId(work + 8);

    let mut source = MapSource::default();
    source.coinbases.insert(work, facts.clone());
    assert_eq!(
        compute_quorum_members(&ctx, &source, ty, base).map(|m| m.len()),
        Err(MemberError::Missing(BlockId(work)))
    );
    let mut source = MapSource::default();
    source.lists.insert(work, Arc::clone(&list));
    assert_eq!(
        compute_quorum_members(&ctx, &source, ty, base).map(|m| m.len()),
        Err(MemberError::Missing(BlockId(work)))
    );
    // Discrimination: the hash form a block without a ChainLock would select gives other members.
    source.coinbases.insert(work, CoinbaseFacts::NoTransactions);
    let pro_tx_hashes = |m: QuorumMembers<Arc<Dmn>>| -> Vec<[u8; 32]> {
        m.iter().map(|d| *d.pro_tx_hash.as_bytes()).collect()
    };
    let hash_form = pro_tx_hashes(compute_quorum_members(&ctx, &source, ty, base).expect("held"));
    source.coinbases.insert(work, facts);
    let chainlock_form =
        pro_tx_hashes(compute_quorum_members(&ctx, &source, ty, base).expect("held"));
    assert_ne!(hash_form, chainlock_form);
    // A chain that does not hold the base's parent errs; it never reads as "gated".
    let short =
        HeldChain::new(work + 8, headers[8..].iter().map(|h| header_hash(h).expect("a header")));
    let ctx = LlmqCtx::new(&params, &short, &deployments, bls).expect("in domain");
    assert_eq!(members_gate(&ctx, ty, base), Err(MemberError::View(ViewError::NotHeld(work + 7))));
}

// ---- SB8: Core's `Verify` and the lock checks over the recorded chains ----

/// The SML entry Core's `CDeterministicMN::to_sml_entry()` makes of a masternode
/// (`evo/deterministicmns.cpp` v24:43-50): the state's version, and its operator key written
/// under that version's scheme.
fn sml_entry(dmn: &Dmn) -> SmlEntry {
    let cold = &dmn.cold;
    SmlEntry {
        version: u16::try_from(cold.version).expect("a u16 version"),
        pro_reg_tx_hash: dmn.pro_tx_hash,
        confirmed_hash: BlockHash::from(*cold.confirmed_hash.as_bytes()),
        net_info: (*cold.net_info).clone(),
        operator_key: cold.pub_key_operator.clone().to_bytes_as(cold.key_scheme()),
        voting_key_id: *cold.key_id_voting.as_bytes(),
        is_valid: dmn.is_valid(),
        mn_type: dmn.mn_type,
        platform_http_port: cold.platform_http_port,
        platform_node_id: cold.platform_node_id,
    }
}

/// Core's `GetSML()` of a list: every masternode's entry.
fn sml_of(list: &MnList) -> SmlList {
    let mut entries = Vec::new();
    MnListView::for_each(list, false, &mut |d: &Arc<Dmn>| entries.push(sml_entry(d)));
    SmlList::from_entries(entries)
}

/// [`MapSource`] with each list replaced by its SML: SPV's source.
struct SmlSource {
    lists: HashMap<u32, Arc<SmlList>>,
    coinbases: HashMap<u32, CoinbaseFacts>,
}

impl SmlSource {
    fn of(source: &MapSource) -> SmlSource {
        SmlSource {
            lists: source.lists.iter().map(|(h, l)| (*h, Arc::new(sml_of(l)))).collect(),
            coinbases: source.coinbases.clone(),
        }
    }
}

impl LlmqSource<SmlList> for SmlSource {
    type Error = ViewError;

    fn mn_list(&self, at: BlockId) -> Result<Option<Arc<SmlList>>, ViewError> {
        Ok(self.lists.get(&at.0).cloned())
    }

    fn coinbase_facts(&self, at: BlockId) -> Result<Option<CoinbaseFacts>, ViewError> {
        Ok(self.coinbases.get(&at.0).cloned())
    }
}

/// A member list given outright (the verifier-only tests, EV addendum m3).
struct Given(Vec<Arc<Dmn>>);

impl QuorumMemberSource for Given {
    type Entry = Arc<Dmn>;
    type Error = ViewError;

    fn members(
        &self,
        _: LlmqType,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<ViewError>> {
        Ok(self.0.iter().cloned().collect())
    }
}

/// `b` resized to `n` bits, keeping the bits below both sizes.
fn resized(b: &DynBitset, n: u64) -> DynBitset {
    let mut data = vec![0u8; n.div_ceil(8) as usize];
    for i in (0..n.min(b.num_bits)).filter(|&i| b.get(i) == Some(true)) {
        data[(i / 8) as usize] |= 1 << (i % 8);
    }
    DynBitset {
        num_bits: n,
        data,
    }
}

/// `b` with only its first `k` set bits.
fn first_set(b: &DynBitset, k: usize) -> DynBitset {
    let mut out = b.clone();
    for i in b.iter_set_bits().skip(k) {
        out.data[(i / 8) as usize] &= !(1 << (i % 8));
    }
    out
}

/// One structural mutation of a commitment Core accepted, made on the commitment's fields and
/// judged on its re-encoding: how, the commitment, how many of the members `Verify` reads, and
/// the rejection it must give (each passes every earlier check).
struct Mutation {
    how: &'static str,
    qc: LlmqCommitment,
    members: usize,
    expect: CommitmentReject,
}

/// Opus r1 F3's list on `qc` with `members` members and the type's `min_size`: the mutations
/// applicable to this commitment (the count ones need `min_size > 0`, the cleared signer bit a
/// valid count above `min_size`, the borrowed signature `other_sig`, a quorum signature of an
/// earlier commitment of the same version), so the count varies per sample. A non-rotated
/// commitment's wire form carries no `quorumIndex` (versions 1 and 3), so that mutation is the
/// unit tests' (SB8 D24).
fn structural_mutations(
    qc: &LlmqCommitment,
    members: usize,
    min_size: usize,
    other_sig: Option<&BlsEagerSig>,
) -> Vec<Mutation> {
    let mut out = Vec::new();
    let mut push = |how, members, expect, edit: &dyn Fn(&mut LlmqCommitment)| {
        let mut m = qc.clone();
        edit(&mut m);
        out.push(Mutation {
            how,
            qc: m,
            members,
            expect,
        });
    };
    let version = |delta: i32| {
        let v = (i32::from(qc.version) + delta) as u16;
        let expect = CommitmentReject::Version {
            found: v,
            expected: qc.version,
        };
        (v, expect)
    };
    for (how, delta) in [("nVersion - 1", -1), ("nVersion + 1", 1)] {
        let (v, expect) = version(delta);
        push(how, members, expect, &|m| {
            m.version = v;
            // The layout of the new version: versions 2 and 4 carry an index.
            m.quorum_index = matches!(v, 2 | 4).then_some(0);
        });
    }
    let size = qc.signers.num_bits;
    push("signers one bit longer", members, CommitmentReject::SignersSize, &|m| {
        m.signers = resized(&m.signers, size + 1)
    });
    push("validMembers one bit shorter", members, CommitmentReject::ValidMembersSize, &|m| {
        m.valid_members = resized(&m.valid_members, size - 1)
    });
    if min_size > 0 {
        push("validMembers below minSize", members, CommitmentReject::ValidMembersCount, &|m| {
            m.valid_members = first_set(&m.valid_members, min_size - 1)
        });
        push("signers below minSize", members, CommitmentReject::SignersCount, &|m| {
            m.signers = first_set(&m.signers, min_size - 1)
        });
    }
    push("a zeroed quorum key", members, CommitmentReject::QuorumPublicKey, &|m| {
        m.quorum_public_key = BlsEagerPk::new(BlsPkValue::invalid(), BlsSchemeKind::Basic)
    });
    push("a null vvec hash", members, CommitmentReject::VvecHash, &|m| {
        m.quorum_vvec_hash = QuorumVvecHash::from([0; 32])
    });
    push("a zeroed membersSig", members, CommitmentReject::MembersSigObject, &|m| {
        m.members_sig = BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic)
    });
    push("a zeroed quorumSig", members, CommitmentReject::QuorumSigObject, &|m| {
        m.quorum_sig = BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic)
    });
    // Members shorter than the last valid bit.
    let last_valid = qc.valid_members.iter_set_bits().last().expect("valid members") as usize;
    push(
        "members end below a valid bit",
        last_valid,
        CommitmentReject::ValidMemberBit(last_valid),
        &|_| {},
    );
    // A signer whose valid bit is cleared, the members ending at it.
    let last_signer = qc.signers.iter_set_bits().last().expect("signers") as usize;
    if qc.valid_members.count_ones() as usize > min_size {
        push(
            "members end at a signer not valid",
            last_signer,
            CommitmentReject::SignerBit(last_signer),
            &|m| {
                m.valid_members.data[last_signer / 8] &= !(1 << (last_signer % 8));
            },
        );
    }
    if let Some(sig) = other_sig {
        push("another commitment's quorumSig", members, CommitmentReject::QuorumSignature, &|m| {
            m.quorum_sig = sig.clone()
        });
    }
    out
}

/// Core's log line for `r` on the quorum `hash` (SB8 D24): `[llmq] CFinalCommitment -- q[<hash>] `,
/// then `r`'s `Display`, then the value Core appends for the reasons that carry one (`=<n>`, or
/// `. <name>=<n>` for the counts) and nothing for every other reason.
fn core_line_names(debug_message: &str, hash: &QuorumHash, r: &CommitmentReject) -> bool {
    let mut rpc = *hash.as_bytes();
    rpc.reverse();
    let rpc: String = rpc.iter().map(|b| format!("{b:02x}")).collect();
    let head = format!("CFinalCommitment -- q[{rpc}] {r}");
    let value_after = match r {
        CommitmentReject::LlmqType
        | CommitmentReject::QuorumIndex
        | CommitmentReject::SignersSize
        | CommitmentReject::ValidMembersSize => Some("="),
        CommitmentReject::ValidMembersCount => Some(". validMembersCount="),
        CommitmentReject::SignersCount => Some(". signersCount="),
        _ => None,
    };
    let suffix_fits = |rest: &str| match value_after {
        None => rest.is_empty(),
        Some(name) => rest.strip_prefix(name).is_some_and(|n| {
            let digits = n.strip_prefix('-').unwrap_or(n);
            !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
        }),
    };
    // The shim prefixes each line with its log category (`[llmq] `).
    debug_message.lines().any(|line| {
        line.strip_prefix("[llmq] ")
            .and_then(|line| line.strip_prefix(&head))
            .is_some_and(suffix_fits)
    })
}

/// A mined commitment as SB8 verifies it under both list views, in the context of the block
/// that mined it: the same members (proTxHash order) and the same verified quorum.
fn verify_under_both_views(
    ctx: LlmqCtx<'_, ViewError>,
    source: &MapSource,
    sml: &SmlSource,
    qc: &LlmqCommitment,
    base: u32,
) -> VerifiedQuorum {
    let llmq_type = qc.llmq_type;
    let mn_members: Vec<[u8; 32]> = compute_quorum_members(&ctx, source, llmq_type, BlockId(base))
        .expect("held")
        .iter()
        .map(|d| *d.pro_tx_hash.as_bytes())
        .collect();
    let sml_members: Vec<[u8; 32]> = compute_quorum_members(&ctx, sml, llmq_type, BlockId(base))
        .expect("held")
        .iter()
        .map(|m| *m.entry().pro_reg_tx_hash.as_bytes())
        .collect();
    assert_eq!(mn_members, sml_members, "members at {base}");
    let (mut rows, mut sml_rows) = (NoRows, NoRows);
    let mn = UncachedMembers::new(ctx, source, &mut rows);
    let ours = verify_quorum(&ctx, &mn, qc, BlockId(base), SigPath::Mined, &mut Vec::new());
    let sm = UncachedMembers::new(ctx, sml, &mut sml_rows);
    let theirs = verify_quorum(&ctx, &sm, qc, BlockId(base), SigPath::Mined, &mut Vec::new());
    assert_eq!(ours, theirs, "the views agree at {base}");
    let quorum = ours.unwrap_or_else(|e| panic!("{llmq_type:?} at {base}: {e:?}"));
    assert_eq!(quorum.quorum_hash(), &qc.quorum_hash);
    quorum
}

/// llmq_mainnet_testnet_membersig, SB8's side: every sampled commitment verifies, signatures
/// included, under the masternode-list view and the SML view (the same members, the same
/// quorum), with the flag of the block that mined it. Then the §5.1 mutation gate against Core's
/// own `Verify` on the same member lists: two signers swapped and a non-signer replaced leave
/// both verdicts accept; a signer swapped with a non-signer, or replaced by a masternode outside
/// the quorum, makes both reject.
#[test]
fn sampled_commitments_verify_under_both_views_and_mutations_match_core() {
    let samples: serde_json::Value =
        serde_json::from_slice(&read_gz(&data("llmq-samples.json.gz"))).expect("json");
    let oracle = Oracle::new(Pin::V24_0);
    let mut verified = 0;
    let mut kinds: HashMap<&str, usize> = HashMap::new();
    let mut structural: HashMap<&str, usize> = HashMap::new();
    let mut other_quorum_sig: HashMap<u16, BlsEagerSig> = HashMap::new();
    for sample in samples["samples"].as_array().expect("samples") {
        let (network, chain_name) = if sample["network"] == "mainnet" {
            (Network::Main, "main")
        } else {
            (Network::Testnet3, "test")
        };
        let params = params_of(network, &[]);
        let Sample {
            first,
            headers,
            chain,
            list_height,
            list,
            coinbase,
        } = Sample::read(sample);
        let mut source = MapSource::default();
        let work_list = Arc::new(decode_list_s3(&mut &list[..]).expect("a list"));
        source.lists.insert(list_height, work_list.clone());
        source.coinbases.insert(list_height, coinbase_facts(&coinbase, true).expect("a coinbase"));
        let sml = SmlSource::of(&source);
        let deployments = HeightDeployments {
            params: &params,
        };
        let core_chain = Chain::new(chain_name, &[]);
        for q in sample["quorums"].as_array().expect("quorums") {
            let base = q["base_height"].as_u64().expect("a height") as u32;
            let mined = q["mined_height"].as_u64().expect("a height") as u32;
            let qc_bytes = unhex(q["commitment"].as_str().expect("a commitment"));
            let qc = LlmqCommitment::decode(&mut &qc_bytes[..]).expect("a commitment");
            let bls = bls_ctx_for_block(&params, mined);
            let ctx = LlmqCtx::new(&params, &chain, &deployments, bls).expect("in domain");
            verify_under_both_views(ctx, &source, &sml, &qc, base);
            verified += 1;

            let members: Vec<Arc<Dmn>> =
                compute_quorum_members(&ctx, &source, qc.llmq_type, BlockId(base))
                    .expect("held")
                    .to_vec();
            let core_verify_bytes = |qc_bytes: &[u8], members: &[Arc<Dmn>]| {
                let context = Context::new(
                    first as i32,
                    headers.clone(),
                    bls.is_legacy(),
                    vec![(DEPLOYMENT_V24, ThresholdState::Defined)],
                    0,
                    Checkpoints::Enabled {
                        best_header_height: (first as usize + headers.len() - 1) as i32,
                    },
                    CoinsView::Unchecked(vec![]),
                    OracleMnList::Absent,
                    vec![OracleQuorumMembers {
                        llmq_type: qc.llmq_type.to_base(),
                        quorum_hash: *qc.quorum_hash.as_bytes(),
                        // Core's disk form, as the oracle's verify context takes them.
                        members: members.iter().map(|d| dmn_bytes(d)).collect(),
                    }],
                );
                oracle.verify_commitment(&core_chain, qc_bytes, &context, true).expect("a verdict")
            };
            let core_verify = |members: &[Arc<Dmn>]| core_verify_bytes(&qc_bytes, members);
            assert!(core_verify(&members).is_accept(), "Core at {base}");

            // The structural checks against Core's `Verify` on the same bytes: the verdict and
            // Core's log line (Opus r1 F3). Each applicable mutation runs on this sample; all 13
            // forms occur across the corpus (asserted after the loop, SB8 D24).
            let mut reencoded = Vec::new();
            qc.encode(&mut reencoded);
            assert_eq!(reencoded, qc_bytes, "the commitment re-encodes as Core wrote it");
            let min_size = params.consensus.llmq(qc.llmq_type).expect("configured").min_size;
            let other_sig = other_quorum_sig.get(&qc.version);
            for Mutation {
                how,
                qc: mutated,
                members: len,
                expect,
            } in structural_mutations(&qc, members.len(), usize::from(min_size), other_sig)
            {
                let mut bytes = Vec::new();
                mutated.encode(&mut bytes);
                let read = LlmqCommitment::decode(&mut &bytes[..]).expect("a commitment");
                let ours = verify_commitment(
                    &ctx,
                    &Given(members[..len].to_vec()),
                    &read,
                    BlockId(base),
                    Some(SigPath::Sync),
                    &mut Vec::new(),
                );
                assert_eq!(ours, Err(CommitmentError::Rejected(expect)), "{how} at {base}");
                match core_verify_bytes(&bytes, &members[..len]) {
                    Verdict::Reject(r) => assert!(
                        core_line_names(&r.debug_message, &qc.quorum_hash, &expect),
                        "{how} at {base}: Core {:?}, ours {expect}",
                        r.debug_message
                    ),
                    Verdict::Accept => panic!("{how} at {base}: Core accepts"),
                }
                *structural.entry(how).or_default() += 1;
            }
            other_quorum_sig.insert(qc.version, qc.quorum_sig.clone());

            let signer = |i: usize| qc.signers.get(i as u64) == Some(true);
            let signers: Vec<usize> = (0..members.len()).filter(|&i| signer(i)).collect();
            let non_signer = (0..members.len()).find(|&i| !signer(i));
            let mut outsider = None;
            MnListView::for_each(&*work_list, false, &mut |d: &Arc<Dmn>| {
                if outsider.is_none() && members.iter().all(|m| m.pro_tx_hash != d.pro_tx_hash) {
                    outsider = Some(d.clone());
                }
            });
            let outsider = outsider.expect("a masternode outside the quorum");
            let mut variants: Vec<(&str, bool, Vec<Arc<Dmn>>)> = Vec::new();
            if let [a, b, ..] = signers[..] {
                let mut m = members.clone();
                m.swap(a, b);
                variants.push(("two signers swapped", true, m));
            }
            if let Some(j) = non_signer {
                let mut m = members.clone();
                m[j] = outsider.clone();
                variants.push(("a non-signer replaced", true, m));
                let mut m = members.clone();
                m.swap(signers[0], j);
                variants.push(("a signer swapped with a non-signer", false, m));
            }
            let mut m = members.clone();
            m[signers[0]] = outsider.clone();
            variants.push(("a signer replaced by an outsider", false, m));
            for (how, accept, m) in variants {
                let ours = verify_commitment(
                    &ctx,
                    &Given(m.clone()),
                    &qc,
                    BlockId(base),
                    // The oracle's `Verify(…, checkSigs = true)`: the synchronous path.
                    Some(SigPath::Sync),
                    &mut Vec::new(),
                );
                let theirs = core_verify(&m);
                assert_eq!(
                    ours.is_ok(),
                    theirs.is_accept(),
                    "{how} at {base}: {ours:?} {theirs:?}"
                );
                assert_eq!(ours.is_ok(), accept, "{how} at {base}");
                if !accept {
                    assert_eq!(
                        ours,
                        Err(CommitmentError::Rejected(CommitmentReject::MembersSignature)),
                        "{how}"
                    );
                }
                *kinds.entry(how).or_default() += 1;
            }
        }
    }
    assert!(verified >= 20, "{verified}");
    assert_eq!(kinds.len(), 4, "every mutation exercised: {kinds:?}");
    assert_eq!(structural.len(), 13, "all 13 forms occur across the corpus: {structural:?}");
}

/// The recorded regtest chain's mined commitments: `(type, commitment, mined height)`.
fn regtest_mined() -> Vec<(u8, Vec<u8>, i32)> {
    recorded_mined("llmq-regtest-v24.0")
}

fn recorded_mined(stem: &str) -> Vec<(u8, Vec<u8>, i32)> {
    let bytes = read_gz(&data(&format!("{stem}.fixture.gz")));
    LlmqFixture::decode(&bytes).expect("a canonical fixture").mined
}

/// The height of each block of a held chain, by hash.
fn heights(chain: &HeldChain) -> HashMap<[u8; 32], u32> {
    (0..=chain.tip()).map(|h| (*chain.hash(h).expect("held").as_bytes(), h)).collect()
}

/// llmq_regtest_quorum_info, SB8's side: every non-rotated commitment the functional-test chain
/// mined verifies under both list views, with the flag of the block that mined it.
#[test]
fn regtest_commitments_verify_under_both_views() {
    let Regtest {
        params,
        chain,
        source,
        ..
    } = regtest();
    let sml = SmlSource::of(&source);
    let deployments = HeightDeployments {
        params: &params,
    };
    let height_of = heights(&chain);
    let mut verified: HashMap<u8, usize> = HashMap::new();
    for (llmq_type, qc_bytes, mined) in regtest_mined() {
        let llmq = params.consensus.llmq(LlmqType::from_base(llmq_type)).expect("configured");
        if llmq.use_rotation {
            continue; // SB11 (EV addendum m3)
        }
        let qc = LlmqCommitment::decode(&mut &qc_bytes[..]).expect("a commitment");
        let base = height_of[qc.quorum_hash.as_bytes()];
        let bls = bls_ctx_for_block(&params, mined as u32);
        let ctx = LlmqCtx::new(&params, &chain, &deployments, bls).expect("in domain");
        verify_under_both_views(ctx, &source, &sml, &qc, base);
        *verified.entry(llmq_type).or_default() += 1;
    }
    assert_eq!(verified.get(&100), Some(&5), "every LLMQ_TEST quorum: {verified:?}");
}

/// A scan Core computed at one selection start block, its entries verified by SB8.
struct OneScan {
    tip: u32,
    start: u32,
    scan: QuorumScan,
}

impl QuorumScanView for OneScan {
    type Error = String;

    fn tip_height(&self) -> u32 {
        self.tip
    }

    fn scan(&self, _: LlmqType, start: u32) -> Result<ScanLookup, String> {
        assert_eq!(start, self.start, "the selection start block");
        Ok(ScanLookup::Held(self.scan.clone()))
    }

    fn scan_at_tip(&self, _: LlmqType, _: usize) -> Result<ScanLookup, String> {
        unreachable!("ChainLocks read no active-quorum set")
    }

    fn block_height(&self, _: &BlockHash) -> Result<Option<u32>, String> {
        unreachable!("ChainLocks look up no cycle block")
    }

    fn quorum(&self, _: LlmqType, _: &QuorumHash) -> Result<Option<QuorumStatus>, String> {
        unreachable!("ChainLocks select")
    }
}

/// L0's ChainLock vectors from Core's own chain: every best ChainLock a post-V20 CbTx of the
/// functional-test chain carries (`CheckCbTxBestChainlock`, `evo/cbtx.cpp` v24) verifies, with
/// the tip at the coinbase's parent, against Core's scan at `height - 8` (each entry verified by
/// SB8 under both views), and selects the quorum Core's `SelectQuorumForSigning` selects. The
/// selection gate on real data: the selected quorum unverified makes the lock unavailable,
/// another unverified quorum changes nothing, and another block hash is Core's invalid.
#[test]
fn regtest_coinbase_chainlocks_verify_against_cores_scan() {
    let Regtest {
        params,
        chain,
        source,
        mut core,
        ..
    } = regtest();
    let sml = SmlSource::of(&source);
    let deployments = HeightDeployments {
        params: &params,
    };
    let height_of = heights(&chain);
    let mined = regtest_mined();
    let llmq_type = params.consensus.llmq_type_chain_locks;
    let llmq = params.consensus.llmq(llmq_type).expect("configured");
    assert!(!llmq.use_rotation);
    let verified_quorum = |base_hash: &[u8; 32]| {
        let (_, qc_bytes, mined) = mined
            .iter()
            .find(|(t, qc, _)| *t == llmq_type.to_base() && qc[3..35] == base_hash[..])
            .expect("the commitment Core scanned");
        let qc = LlmqCommitment::decode(&mut &qc_bytes[..]).expect("a commitment");
        let bls = bls_ctx_for_block(&params, *mined as u32);
        let ctx = LlmqCtx::new(&params, &chain, &deployments, bls).expect("in domain");
        let base = height_of[base_hash];
        ScannedQuorum::verified(
            BlockId(base),
            verify_under_both_views(ctx, &source, &sml, &qc, base),
        )
    };
    let mut coinbases: Vec<(&u32, &CoinbaseFacts)> = source.coinbases.iter().collect();
    coinbases.sort_by_key(|(h, _)| **h);
    let mut checked = std::collections::BTreeSet::new();
    let mut gated = 0;
    for (&height, facts) in coinbases {
        // V20 at the fixture's `-testactivationheight=v20@300`, read from its parameters.
        let bls = bls_ctx_for_block(&params, height);
        let v20 = height >= params.consensus.v20_height.to_u32();
        let Some((sig, diff)) = nonnull_coinbase_chainlock(facts, height, v20, bls) else {
            continue;
        };
        let cl_height = height - diff - 1;
        if !checked.insert(cl_height) {
            continue;
        }
        let start = cl_height - SIGN_HEIGHT_OFFSET;
        let start_hash = chain.hash(start).expect("held");
        let count = u32::from(llmq.signing_active_quorum_count);
        let (bases, _) =
            core.scan(llmq_type.to_base(), start_hash.as_bytes(), count, false).expect("a scan");
        let mut view = OneScan {
            tip: height - 1,
            start,
            scan: QuorumScan {
                rotation_enabled: false,
                entries: bases.iter().map(&verified_quorum).collect(),
            },
        };
        let id = chainlock_request_id(cl_height as i32);
        let (selected, _) = core
            .select(llmq_type.to_base(), &id, Some(cl_height as i32), SIGN_HEIGHT_OFFSET as i32)
            .expect("a selection");
        let block = chain.hash(cl_height).expect("held");
        let verify = |view: &OneScan, block: &BlockHash| {
            verify_chainlock_sig(&params, view, bls, cl_height as i32, block, sig.value())
        };
        let lock = verify(&view, &block).unwrap_or_else(|e| panic!("CL {cl_height}: {e:?}"));
        assert_eq!(Some(*lock.quorum.quorum_hash().as_bytes()), selected, "CL {cl_height}");
        let other = BlockHash::from([0x5a; 32]);
        assert_eq!(
            verify(&view, &other),
            Err(LockError::Rejected(LockReject::InvalidSignature)),
            "CL {cl_height}"
        );
        // The selection gate: unverify each entry in turn.
        for (i, base) in bases.iter().enumerate() {
            let entry = view.scan.entries[i].clone();
            let selected_here = lock.quorum.quorum_hash().as_bytes() == base;
            view.scan.entries[i] =
                ScannedQuorum::unverified(entry.base(), entry.quorum_hash(), entry.quorum_index());
            let got = verify(&view, &block);
            if selected_here {
                assert_eq!(got, Err(LockError::Rejected(LockReject::QuorumUnavailable)));
            } else {
                assert_eq!(
                    got.as_ref().map(|l| l.quorum.quorum_hash()),
                    Ok(lock.quorum.quorum_hash())
                );
                gated += 1;
            }
            view.scan.entries[i] = entry;
        }
    }
    assert!(checked.len() >= 10, "{} ChainLocks", checked.len());
    assert!(gated > 0, "a scan of two or more quorums");
}

/// Core's non-chain `GetQuorum` as a lock check reads it: the commitment mined for the base in
/// the active chain (ending at `tip`), as its key unchecked ([`ScannedQuorum::mined`]; node-l1
/// gets dash-evo's `mined` through dashd-chainstate, its one owner).
struct MinedKeys<'a> {
    chain: &'a HeldChain,
    index: &'a MinedIndex,
    tip: BlockId,
}

impl QuorumMaterializer for MinedKeys<'_> {
    type Quorum = ScannedQuorum;
    type Error = ViewError;

    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        base: BlockId,
        _: bool,
    ) -> Result<Option<ScannedQuorum>, LlmqError<ViewError>> {
        let hash = self.chain.hash(base.0).ok_or(MemberError::View(ViewError::NotHeld(base.0)))?;
        let quorum_hash = QuorumHash::from(*hash.as_bytes());
        let mined = self.index.mined_commitment(llmq_type, &quorum_hash, self.tip);
        Ok(mined.map_err(MemberError::View)?.map(|(qc, _)| ScannedQuorum::mined(base, &qc)))
    }
}

type ScanCacheHost = PerTypeCache<BlockHash, Vec<ScannedQuorum>>;

/// The node's lock view as CH7b-3 builds it (LQ3b-3): `scan` is Core's cached
/// `ScanQuorums(type, start, signingActiveQuorumCount)` through LQ3b-2's [`scan_quorums`], over
/// the node's mined index and one scan cache that lives across checks, with the tip at the
/// coinbase's parent. Each scan's start height and cache event are recorded.
struct ProductionScan<'a> {
    ctx: &'a LlmqCtx<'a, ViewError>,
    chain: &'a HeldChain,
    index: &'a MinedIndex,
    tip: Cell<u32>,
    cache: RefCell<ScanCacheHost>,
    scans: RefCell<Vec<(u32, Option<ScanEvent>)>>,
}

impl QuorumScanView for ProductionScan<'_> {
    type Error = String;

    fn tip_height(&self) -> u32 {
        self.tip.get()
    }

    fn scan(&self, llmq_type: LlmqType, start: u32) -> Result<ScanLookup, String> {
        let at = self.chain.at(start);
        let llmq = self.ctx.llmq(llmq_type).map_err(|e| format!("{e:?}"))?;
        let rotation_enabled =
            is_quorum_rotation_enabled(self.ctx, llmq, at).map_err(|e| format!("{e:?}"))?;
        let mut mat = MinedKeys {
            chain: self.chain,
            index: self.index,
            tip: self.chain.at(self.tip.get()),
        };
        let count = usize::from(llmq.signing_active_quorum_count);
        let scanned = scan_quorums(
            self.ctx,
            self.index,
            &mut *self.cache.borrow_mut(),
            &mut mat,
            llmq_type,
            at,
            count,
            ScanOverload::Cached,
        )
        .map_err(|e| format!("{e:?}"))?;
        self.scans.borrow_mut().push((start, scanned.event));
        Ok(ScanLookup::Held(QuorumScan {
            rotation_enabled,
            entries: scanned.quorums,
        }))
    }

    fn scan_at_tip(&self, _: LlmqType, _: usize) -> Result<ScanLookup, String> {
        unreachable!("ChainLocks read no active-quorum set")
    }

    fn block_height(&self, _: &BlockHash) -> Result<Option<u32>, String> {
        unreachable!("ChainLocks look up no cycle block")
    }

    fn quorum(&self, _: LlmqType, _: &QuorumHash) -> Result<Option<QuorumStatus>, String> {
        unreachable!("ChainLocks select")
    }
}

/// A production scan event as Core's trace reports it.
fn scan_trace(chain: &HeldChain, llmq_type: LlmqType, event: ScanEvent) -> TraceEvent {
    core_event(&ModelEvent::Scan {
        llmq_type,
        store: chain.hash(event.store.0).expect("held"),
        cache: event.cache,
        cache_written: event.cache_written,
        n: event.n,
    })
}

/// LQ3b-3, the twin of [`regtest_coinbase_chainlocks_verify_against_cores_scan`]: every best
/// ChainLock a post-V20 CbTx of the functional-test chain carries verifies through the production
/// scan (LQ3b-2) over the node's mined index with the mined commitments' keys (LQ3b-1), with the
/// tip at the coinbase's parent and one scan cache across the chain, as Core's process-wide one.
/// Each check selects the quorum Core's `SelectQuorumForSigning` selects, and its scan meets
/// Core's scan cache as Core's does: the same store block, cache use, write and count, call by
/// call. `select_quorum_for_signing` over a second cache agrees with both.
#[test]
fn regtest_coinbase_chainlocks_verify_through_the_production_scan() {
    let Regtest {
        params,
        chain,
        source,
        mut core,
        ..
    } = regtest();
    let deployments = HeightDeployments {
        params: &params,
    };
    let index =
        MinedIndex::recorded(&chain, &params, &regtest_mined()).expect("a canonical recording");
    let ctx = LlmqCtx::new(&params, &chain, &deployments, bls_ctx_for_block(&params, chain.tip()))
        .expect("in domain");
    let llmq_type = params.consensus.llmq_type_chain_locks;
    let view = ProductionScan {
        ctx: &ctx,
        chain: &chain,
        index: &index,
        tip: Cell::new(0),
        cache: RefCell::new(ScanCacheHost::sized(&params, scan_cache_size)),
        scans: RefCell::new(Vec::new()),
    };
    let mut second = ScanCacheHost::sized(&params, scan_cache_size);
    let mut coinbases: Vec<(&u32, &CoinbaseFacts)> = source.coinbases.iter().collect();
    coinbases.sort_by_key(|(h, _)| **h);
    let mut checked = std::collections::BTreeSet::new();
    let mut uses = HashMap::new();
    for (&height, facts) in coinbases {
        let bls = bls_ctx_for_block(&params, height);
        let v20 = height >= params.consensus.v20_height.to_u32();
        let Some((sig, diff)) = nonnull_coinbase_chainlock(facts, height, v20, bls) else {
            continue;
        };
        let cl_height = height - diff - 1;
        if !checked.insert(cl_height) {
            continue;
        }
        let id = chainlock_request_id(cl_height as i32);
        let offset = SIGN_HEIGHT_OFFSET as i32;
        let (selected, trace) = core
            .select(llmq_type.to_base(), &id, Some(cl_height as i32), offset)
            .expect("a selection");
        let core_scan: Vec<&TraceEvent> =
            trace.iter().filter(|e| matches!(e, TraceEvent::Scan { .. })).collect();

        view.tip.set(height - 1);
        let block = chain.hash(cl_height).expect("held");
        let lock = verify_chainlock_sig(&params, &view, bls, cl_height as i32, &block, sig.value())
            .unwrap_or_else(|e| panic!("CL {cl_height}: {e:?}"));
        assert_eq!(Some(*lock.quorum.quorum_hash().as_bytes()), selected, "CL {cl_height}");
        let (start, event) = view.scans.borrow_mut().pop().expect("one scan");
        assert!(view.scans.borrow().is_empty(), "CL {cl_height}: one scan per check");
        assert_eq!(start, cl_height - SIGN_HEIGHT_OFFSET, "CL {cl_height}: the start block");
        let ours: Vec<TraceEvent> =
            event.map(|e| scan_trace(&chain, llmq_type, e)).into_iter().collect();
        assert_eq!(ours.iter().collect::<Vec<_>>(), core_scan, "CL {cl_height}: the scan cache");
        *uses.entry(format!("{:?}", event.map(|e| e.cache))).or_insert(0) += 1;

        let mut mat = MinedKeys {
            chain: &chain,
            index: &index,
            tip: chain.at(height - 1),
        };
        let selection = select_quorum_for_signing(
            &ctx,
            &index,
            &mut second,
            &mut mat,
            llmq_type,
            chain.at(height - 1),
            &id,
            cl_height as i32,
            offset,
        )
        .expect("held");
        let quorum = selection.quorum.as_ref().map(|q| *q.quorum_hash().as_bytes());
        assert_eq!(quorum, selected, "CL {cl_height}: select_quorum_for_signing");
        assert_eq!(selection.scan, event, "CL {cl_height}: the second cache's event");
    }
    assert!(checked.len() >= 10, "{} ChainLocks", checked.len());
    // Non-vacuous: both a cold and a warm cache occur.
    for cache in ["Some(Miss)", "Some(HitSatisfied)"] {
        assert!(uses.contains_key(cache), "{cache} in {uses:?}");
    }
}

/// The SML's missing properties (SB8, LQ1 §2.2.4): Core's `AddMN` (the list's scratch) rejects a
/// second masternode holding a collateral or an owner key another holds; the SML carries neither,
/// so its scratch accepts both (an under-reject, never an over-reject). A shared operator key, which
/// the SML carries, both reject.
#[test]
fn the_sml_scratch_under_rejects_what_the_sml_does_not_carry() {
    let samples: serde_json::Value =
        serde_json::from_slice(&read_gz(&data("llmq-samples.json.gz"))).expect("json");
    let Sample {
        list,
        ..
    } = Sample::read(&samples["samples"][0]);
    let list = decode_list_s3(&mut &list[..]).expect("a list");
    let mut dmns: Vec<Arc<Dmn>> = Vec::new();
    MnListView::for_each(&list, false, &mut |d: &Arc<Dmn>| {
        if dmns.len() < 2 && !d.cold.is_shared() {
            dmns.push(d.clone());
        }
    });
    let [a, b] = &dmns[..] else {
        panic!("two masternodes")
    };
    let with = |edit: &dyn Fn(&mut Dmn, &mut MnCold)| {
        let mut dmn = (**b).clone();
        let mut cold = (*b.cold).clone();
        edit(&mut dmn, &mut cold);
        dmn.cold = Arc::new(cold);
        Arc::new(dmn)
    };
    let cases = [
        ("collateral", with(&|d, _| d.collateral = a.collateral), false),
        ("owner key", with(&|_, c| c.key_id_owner = a.cold.key_id_owner), false),
        ("operator key", with(&|_, c| c.pub_key_operator = a.cold.pub_key_operator.clone()), true),
    ];
    for (what, b2, sml_rejects) in cases {
        let mut scratch = list.new_scratch();
        scratch.try_add(a).expect("the first");
        assert!(scratch.try_add(&b2).is_err(), "Core rejects a shared {what}");
        let mut sml = SmlScratch::default();
        sml.try_add(&SmlMember::new(&sml_entry(a))).expect("the first");
        assert_eq!(sml.try_add(&SmlMember::new(&sml_entry(&b2))).is_err(), sml_rejects, "{what}");
    }
}

// ---- O2a-LQ3: Core's verdicts on locks, recovered signatures and mined commitments ----

/// A recorded regtest chain (PROVENANCE.md): its parameters, chain and source, and every mined
/// non-rotated quorum as SB8 verifies it, by type and quorum hash, with the height it was mined
/// at. The LQ3a capture network's chain is `lq3-regtest-v24.0`.
struct LockChain {
    params: ChainSetup,
    chain: HeldChain,
    height_of: HashMap<[u8; 32], u32>,
    quorums: HashMap<(u8, [u8; 32]), VerifiedQuorum>,
    /// (type, quorum hash, mined height) of each mined non-rotated quorum, in mining order.
    mined: Vec<(u8, [u8; 32], u32)>,
}

/// A lock capture and the chain it was taken on (`<stem>.captures.json` beside the fixture).
#[derive(Clone, Copy)]
struct LockCapture {
    stem: &'static str,
    /// The scheme every captured message was signed in: the capture's accepts are under it alone.
    signed: BlsCtx,
}

/// O2a-LQ3's capture: V19 active, non-rotated InstantSend (`llmq_test_instantsend`).
const LQ3: LockCapture = LockCapture {
    stem: "lq3-regtest-v24.0",
    signed: BlsCtx::BASIC,
};

/// BL-218's capture: V19 deferred past the tip (every signature legacy) and rotated InstantSend
/// (`llmq_test_dip0024`), cycles at 264 and 288, tip 311 (D29-D31).
const LQ3R: LockCapture = LockCapture {
    stem: "lq3r-regtest-v24.0",
    signed: BlsCtx::LEGACY,
};

/// A capture's chain, with its rotated quorums, and Core's context on it.
fn lock_chain(cap: LockCapture) -> (LockChain, LlmqContext) {
    lock_chain_of(cap.stem, true)
}

/// Every block of a recorded chain connected through `PreComputeQuorumMembers` on both sides, as
/// a node connects it: that stores the snapshot rows rotated members read, and Core's rows must
/// equal the model's by digest. Then Core's context restarts and seeds the rotated commitments,
/// which need those rows (O2a-SP's order: load, precompute, restart, `set_mined`; D29).
fn connect_rotation(
    params: &ChainSetup,
    chain: &HeldChain,
    source: &MapSource,
    core: &mut LlmqContext,
    rotated: &[(u8, Vec<u8>, i32)],
) -> ModelRows {
    let deployments = HeightDeployments {
        params,
    };
    let bls = bls_ctx_after(&deployments, Some(BlockId(chain.tip())));
    let ctx = LlmqCtx::new(params, chain, &deployments, bls).expect("in domain");
    let index = seed_mined(&[], params, &ctx, chain, source, core)
        .expect("no divergence")
        .expect("the chain connects");
    let views = ModelViews {
        ctx: &ctx,
        src: source,
        reader: &index,
        tip: BlockId(chain.tip()),
    };
    let mut manager = ModelQuorumManager::<Arc<Dmn>>::new(params);
    let mut rows = ModelRows::new();
    for height in 1..=chain.tip() {
        manager
            .precompute(
                &views,
                &mut RuntimeRows::new(&mut rows),
                BlockId(height),
                PrecomputeMode::Connect,
            )
            .expect("held");
        manager.take_events();
        let hash = chain.hash(height).expect("held");
        core.precompute(hash.as_bytes(), bls.is_legacy(), Precompute::Connect)
            .expect("Core connects it");
    }
    assert_eq!(rows_digest(&rows), core.rows_digest().expect("a digest"), "every row");
    core.restart().expect("a restart");
    for (llmq_type, qc, mined) in rotated {
        core.set_mined(*llmq_type, qc, *mined).expect("Core seeds the rotated commitment");
    }
    rows
}

/// A recorded chain and Core's context on it. With `rotated`, the rotated commitments are
/// verified (SB8, members from the snapshot rows) and seeded in Core's context too
/// ([`connect_rotation`]); without, they are skipped.
fn lock_chain_of(stem: &str, rotated: bool) -> (LockChain, LlmqContext) {
    let Regtest {
        params,
        chain,
        source,
        mut core,
        ..
    } = recorded_regtest(stem);
    let sml = SmlSource::of(&source);
    let deployments = HeightDeployments {
        params: &params,
    };
    let height_of = heights(&chain);
    let is_rotated =
        |t: u8| params.consensus.llmq(LlmqType::from_base(t)).expect("configured").use_rotation;
    let all = recorded_mined(stem);
    let rotated_qcs: Vec<_> = all.iter().filter(|(t, ..)| is_rotated(*t)).cloned().collect();
    let rows = if rotated && !rotated_qcs.is_empty() {
        connect_rotation(&params, &chain, &source, &mut core, &rotated_qcs)
    } else {
        ModelRows::new()
    };
    let mut quorums = HashMap::new();
    let mut mined_at = Vec::new();
    for (llmq_type, qc_bytes, mined) in all {
        let rotation = is_rotated(llmq_type);
        if rotation && !rotated {
            continue;
        }
        let qc = LlmqCommitment::decode(&mut &qc_bytes[..]).expect("a commitment");
        if !rotation {
            mined_at.push((llmq_type, *qc.quorum_hash.as_bytes(), mined as u32));
        }
        let base = height_of[qc.quorum_hash.as_bytes()];
        let bls = bls_ctx_for_block(&params, mined as u32);
        let ctx = LlmqCtx::new(&params, &chain, &deployments, bls).expect("in domain");
        let quorum = if rotation {
            verify_rotated_under_both_views(ctx, &source, &sml, &rows, &qc, base)
        } else {
            verify_under_both_views(ctx, &source, &sml, &qc, base)
        };
        quorums.insert((llmq_type, *qc.quorum_hash.as_bytes()), quorum);
    }
    let lc = LockChain {
        params,
        chain,
        height_of,
        quorums,
        mined: mined_at,
    };
    (lc, core)
}

/// [`verify_under_both_views`] for a rotated commitment: the members read the snapshot rows
/// (each view on its own copy, as each stores the rows it computes).
fn verify_rotated_under_both_views(
    ctx: LlmqCtx<'_, ViewError>,
    source: &MapSource,
    sml: &SmlSource,
    rows: &ModelRows,
    qc: &LlmqCommitment,
    base: u32,
) -> VerifiedQuorum {
    let (mut mn_rows, mut sml_rows) = (rows.clone(), rows.clone());
    let mut mn_rows = RuntimeRows::new(&mut mn_rows);
    let mut sml_rows = RuntimeRows::new(&mut sml_rows);
    let mn = UncachedMembers::new(ctx, source, &mut mn_rows);
    let ours = verify_quorum(&ctx, &mn, qc, BlockId(base), SigPath::Mined, &mut Vec::new());
    let sm = UncachedMembers::new(ctx, sml, &mut sml_rows);
    let theirs = verify_quorum(&ctx, &sm, qc, BlockId(base), SigPath::Mined, &mut Vec::new());
    assert_eq!(ours, theirs, "the views agree at {base}");
    let quorum = ours.unwrap_or_else(|e| panic!("{:?} at {base}: {e:?}", qc.llmq_type));
    assert_eq!(quorum.quorum_hash(), &qc.quorum_hash);
    quorum
}

/// LQ3a's view of the capture chain at its tip: Core's scans (LQ1 checks them), each quorum as
/// SB8 verifies it, and the chain's block heights. Every lookup is held.
struct CaptureView<'a> {
    lc: &'a LockChain,
    core: std::cell::RefCell<&'a mut LlmqContext>,
}

impl<'a> CaptureView<'a> {
    fn new(lc: &'a LockChain, core: &'a mut LlmqContext) -> Self {
        CaptureView {
            lc,
            core: std::cell::RefCell::new(core),
        }
    }

    fn core_scan(&self, llmq_type: LlmqType, start: u32, count: usize) -> ScanLookup {
        let start_hash = self.lc.chain.hash(start).expect("held");
        let count = u32::try_from(count).expect("a small count");
        let (bases, _) = self
            .core
            .borrow_mut()
            .scan(llmq_type.to_base(), start_hash.as_bytes(), count, false)
            .expect("Core's scan");
        let entries = bases
            .iter()
            .map(|base| {
                let quorum = self.lc.quorums[&(llmq_type.to_base(), *base)].clone();
                ScannedQuorum::verified(BlockId(self.lc.height_of[base]), quorum)
            })
            .collect();
        let llmq = self.lc.params.consensus.llmq(llmq_type).expect("configured");
        let deployments = HeightDeployments {
            params: &self.lc.params,
        };
        let ctx = LlmqCtx::new(&self.lc.params, &self.lc.chain, &deployments, BlsCtx::BASIC)
            .expect("in domain");
        let rotation_enabled =
            is_quorum_rotation_enabled(&ctx, llmq, BlockId(start)).expect("held");
        ScanLookup::Held(QuorumScan {
            rotation_enabled,
            entries,
        })
    }
}

impl QuorumScanView for CaptureView<'_> {
    type Error = String;

    fn tip_height(&self) -> u32 {
        self.lc.chain.tip()
    }

    fn scan(&self, llmq_type: LlmqType, start: u32) -> Result<ScanLookup, String> {
        let llmq = self.lc.params.consensus.llmq(llmq_type).expect("configured");
        Ok(self.core_scan(llmq_type, start, usize::from(llmq.signing_active_quorum_count)))
    }

    fn scan_at_tip(&self, llmq_type: LlmqType, count: usize) -> Result<ScanLookup, String> {
        Ok(self.core_scan(llmq_type, self.lc.chain.tip(), count))
    }

    fn block_height(&self, hash: &BlockHash) -> Result<Option<u32>, String> {
        Ok(self.lc.height_of.get(hash.as_bytes()).copied())
    }

    fn quorum(
        &self,
        llmq_type: LlmqType,
        hash: &QuorumHash,
    ) -> Result<Option<QuorumStatus>, String> {
        let quorum = self.lc.quorums.get(&(llmq_type.to_base(), *hash.as_bytes()));
        Ok(quorum.map(|q| QuorumStatus::Verified(q.clone())))
    }
}

/// A verdict on a message, as both sides give it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LockOutcome {
    /// Accepted, by the quorum.
    Accept([u8; 32]),
    /// The payload does not read: Core's deserializer throws, or (a ChainLock's eager signature)
    /// ours reads none.
    Unreadable,
    /// Refused before any verification, as Core names the check.
    Refused(LockStatus),
    /// No pass verified: the last pass's quorum rejected the signature (`invalid`), or it found
    /// none. Core's response follows the last pass alone: an `isdlock`'s second pass is the one
    /// that scores the peer (`net_instantsend.cpp` v24:189-199, :340-353; D23).
    NotVerified {
        invalid: bool,
    },
}

/// Core's answer on a message: the outcome, the inventory hash where Core read the message, and
/// each pass that ran: its status and its quorum (None: the pass found none).
struct CoreLock {
    outcome: LockOutcome,
    hash: Option<[u8; 32]>,
    pass_quorums: Vec<Option<[u8; 32]>>,
    pass_statuses: Vec<PassStatus>,
}

/// Core's outcome; None where Core refuses the call (an argument Core's arithmetic overflows on).
/// The trailing-bytes refusal is the shim's, not Core's (Core's handlers process the message as
/// read), so no case gives it.
fn core_lock_outcome(
    got: dash_oracle_sys::Result<(LockVerdict, Vec<TraceEvent>)>,
) -> Option<CoreLock> {
    let verdict = match got {
        Ok((verdict, _)) => verdict,
        Err(e) if e.kind == ErrorKind::Decode => {
            return Some(CoreLock {
                outcome: LockOutcome::Unreadable,
                hash: None,
                pass_quorums: Vec::new(),
                pass_statuses: Vec::new(),
            });
        }
        Err(e) if e.kind == ErrorKind::Argument => return None,
        Err(e) => panic!("Core: {e}"),
    };
    let outcome = if verdict.status == LockStatus::Oversized {
        // Both are Core's misbehaving-100 refusal; ours checks the size inside TriviallyValid.
        LockOutcome::Refused(LockStatus::TriviallyInvalid)
    } else if verdict.status != LockStatus::Checked {
        LockOutcome::Refused(verdict.status)
    } else if let Some(pass) = verdict.passes.iter().find(|p| p.status == PassStatus::Valid) {
        LockOutcome::Accept(pass.quorum.expect("a verifying pass has a quorum"))
    } else {
        let last = verdict.passes.last().expect("a checked message ran a pass");
        LockOutcome::NotVerified {
            invalid: last.status == PassStatus::Invalid,
        }
    };
    Some(CoreLock {
        outcome,
        hash: Some(verdict.hash),
        pass_quorums: verdict.passes.iter().map(|p| p.quorum).collect(),
        pass_statuses: verdict.passes.iter().map(|p| p.status).collect(),
    })
}

/// Ours from a lock verdict: None for no verdict ([`LockError::BlsUnsupported`]).
fn our_lock_outcome<T>(
    got: Result<T, LockError<String>>,
    quorum: impl Fn(&T) -> [u8; 32],
) -> Option<LockOutcome> {
    Some(match got {
        Ok(lock) => LockOutcome::Accept(quorum(&lock)),
        Err(LockError::Rejected(r)) => match r {
            LockReject::InvalidSignature => LockOutcome::NotVerified {
                invalid: true,
            },
            LockReject::NoQuorum => LockOutcome::NotVerified {
                invalid: false,
            },
            LockReject::Malformed => LockOutcome::Refused(LockStatus::TriviallyInvalid),
            LockReject::UnknownCycle => LockOutcome::Refused(LockStatus::UnknownCycle),
            LockReject::InvalidCycle => LockOutcome::Refused(LockStatus::InvalidCycle),
            LockReject::MalformedSignature => LockOutcome::Refused(LockStatus::MalformedSignature),
            LockReject::QuorumUnavailable | LockReject::OutsideHorizon => {
                panic!("the capture view holds every quorum: {r:?}")
            }
        },
        Err(LockError::BlsUnsupported) => return None,
        Err(e) => panic!("ours: {e}"),
    })
}

/// The tallies of one message kind's comparisons.
#[derive(Debug, Default)]
struct Tally {
    compared: usize,
    accepted: usize,
    /// Accepts under the legacy scheme (a capture's accepts are all under the scheme it was
    /// signed in).
    accepted_legacy: usize,
    /// Ours gave no verdict (BLS outside the reproduced domain, legacy scheme only).
    no_verdict: usize,
    /// Core's outcomes, each accept as `Accept`.
    outcomes: std::collections::BTreeSet<String>,
}

impl Tally {
    /// Ours (None: no verdict) against Core's, under `bls`.
    fn check(&mut self, what: &str, bls: BlsCtx, ours: Option<LockOutcome>, core: LockOutcome) {
        self.compared += 1;
        self.outcomes.insert(match &core {
            LockOutcome::Accept(_) => "Accept".into(),
            other => format!("{other:?}"),
        });
        let Some(ours) = ours else {
            // SB8 D12 / LQ3a D15: only the legacy scheme's signature check reaches it, and Core's
            // check there does not verify.
            assert_eq!(bls, BlsCtx::LEGACY, "{what}: no verdict under the basic scheme");
            assert!(matches!(core, LockOutcome::NotVerified { .. }), "{what}: Core: {core:?}");
            self.no_verdict += 1;
            return;
        };
        assert_eq!(ours, core, "{what}");
        let accept = usize::from(matches!(core, LockOutcome::Accept(_)));
        self.accepted += accept;
        self.accepted_legacy += accept * usize::from(bls == BlsCtx::LEGACY);
    }

    /// Every accept was under the scheme the capture signed in (the other scheme reads the same
    /// key and signature bytes differently, so it never verifies them).
    fn assert_accepts_signed(&self, cap: LockCapture) {
        let legacy = if cap.signed == BlsCtx::LEGACY {
            self.accepted
        } else {
            0
        };
        assert_eq!(self.accepted_legacy, legacy, "{}: {self:?}", cap.stem);
    }

    /// Every outcome in `expected` occurred, and no other.
    fn assert_outcomes(&self, expected: &[&str]) {
        let expected: std::collections::BTreeSet<String> =
            expected.iter().map(|s| s.to_string()).collect();
        assert_eq!(self.outcomes, expected, "{self:?}");
    }
}

/// The quorum `SelectQuorumForSigning` picks over the view's scan for `id` at `sign_height` and
/// `sign_offset` (dash-evo's selection, LQ1 §3.9): the quorum Core's pass reports.
fn selected_quorum(
    lc: &LockChain,
    view: &CaptureView<'_>,
    llmq_type: LlmqType,
    id: &[u8; 32],
    sign_height: i32,
    sign_offset: i32,
) -> Option<[u8; 32]> {
    let llmq = lc.params.consensus.llmq(llmq_type).expect("configured");
    let tip = i32::try_from(lc.chain.tip()).expect("a small tip");
    let start = select_start_height(tip, sign_height, sign_offset)?;
    let ScanLookup::Held(scan) = view.scan(llmq_type, start).expect("held") else {
        panic!("the capture view holds every scan")
    };
    select_from_scan(llmq_type, llmq, scan.rotation_enabled, &scan.entries, id)
        .map(|q| *q.quorum_hash().as_bytes())
}

const SCHEMES: [BlsCtx; 2] = [BlsCtx::BASIC, BlsCtx::LEGACY];

fn legacy(bls: BlsCtx) -> bool {
    bls == BlsCtx::LEGACY
}

/// A capture's messages of one command, as payload bytes.
fn captured(cap: LockCapture, command: &str) -> Vec<Vec<u8>> {
    let captures: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(data(&format!("{}.captures.json", cap.stem)))
            .expect("the captures"),
    )
    .expect("json");
    let found: Vec<Vec<u8>> = captures["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|m| m["command"] == command)
        .map(|m| unhex(m["payload"].as_str().expect("payload")))
        .collect();
    assert!(!found.is_empty(), "no {command}");
    found
}

/// `payload` with `bytes` written at `at`.
fn written(payload: &[u8], at: usize, bytes: &[u8]) -> Vec<u8> {
    let mut out = payload.to_vec();
    out[at..at + bytes.len()].copy_from_slice(bytes);
    out
}

/// `payload` with the byte at `at` flipped.
fn flipped(payload: &[u8], at: usize) -> Vec<u8> {
    let mut out = payload.to_vec();
    out[at] ^= 1;
    out
}

/// The payload changes every message kind takes: its signature's last byte flipped, another
/// message's signature, all-zero and all-0xff signatures, and a byte short. A byte after is not
/// compared: Core's handlers process a message as read, so only the shim refuses it.
fn common_mutations(payload: &[u8], other_sig: &[u8]) -> Vec<(String, Vec<u8>)> {
    let sig = payload.len() - 96;
    vec![
        ("as captured".into(), payload.to_vec()),
        ("sig last byte flipped".into(), flipped(payload, payload.len() - 1)),
        ("sig first byte flipped".into(), flipped(payload, sig)),
        ("another message's sig".into(), written(payload, sig, other_sig)),
        ("zero sig".into(), written(payload, sig, &[0; 96])),
        ("0xff sig".into(), written(payload, sig, &[0xff; 96])),
        ("a byte short".into(), payload[..payload.len() - 1].to_vec()),
    ]
}

/// The decode of ours, the whole payload: None if it does not read (or leaves bytes, which no
/// case does).
fn decode_whole<T>(payload: &[u8], decode: impl Fn(&mut &[u8]) -> Option<T>) -> Option<T> {
    let mut r = payload;
    let value = decode(&mut r)?;
    r.is_empty().then_some(value)
}

/// O2a-LQ3, `clsig`: every captured ChainLock and its changes, under both schemes, get Core's
/// `VerifyChainLock` verdict (oracle_llmq_verify_chainlock), the quorum Core selects and Core's
/// inventory hash from LQ3a's `verify_chainlock` and `clsig_hash`.
#[test]
fn captured_chainlocks_match_cores_verdicts() {
    let (tally, clsigs, selections) = chainlock_differential(LQ3);
    assert_eq!(tally.accepted, clsigs, "each captured ChainLock, once: {tally:?}");
    tally.assert_accepts_signed(LQ3);
    tally.assert_outcomes(&[
        "Accept",
        "Unreadable",
        "NotVerified { invalid: true }",
        "NotVerified { invalid: false }",
    ]);
    assert!(selections > 0, "no failing pass found a quorum");
}

/// BL-218 (D30), `clsig` on the legacy chain: each captured ChainLock is accepted under the
/// legacy scheme alone, as Core accepts it (Opus-h r1 F6: a legacy accept compared with Core).
#[test]
fn legacy_chainlocks_match_cores_verdicts() {
    let (tally, clsigs, selections) = chainlock_differential(LQ3R);
    assert_eq!(tally.accepted, clsigs, "each captured ChainLock, once: {tally:?}");
    tally.assert_accepts_signed(LQ3R);
    tally.assert_outcomes(&[
        "Accept",
        "Unreadable",
        "NotVerified { invalid: true }",
        "NotVerified { invalid: false }",
    ]);
    assert!(selections > 0, "no failing pass found a quorum");
}

/// The `clsig` differential on one capture: the tally, the number of captured ChainLocks and the
/// number of failing passes whose quorum was compared.
fn chainlock_differential(cap: LockCapture) -> (Tally, usize, usize) {
    let (lc, mut core) = lock_chain(cap);
    let clsigs = captured(cap, "clsig");
    let cl_type = lc.params.consensus.llmq_type_chain_locks;
    // Each ChainLocks-type quorum's first signing height and the one before it: the scan from
    // `height - SIGN_HEIGHT_OFFSET` holds it from `mined + SIGN_HEIGHT_OFFSET` on.
    let offset = SIGN_HEIGHT_OFFSET as i32;
    let boundaries: Vec<i32> = lc
        .mined
        .iter()
        .filter(|(t, ..)| *t == cl_type.to_base())
        .flat_map(|&(_, _, mined)| [mined as i32 + offset - 1, mined as i32 + offset])
        .collect();
    assert!(boundaries.len() >= 4, "{boundaries:?}");
    let mut tally = Tally::default();
    let mut selections = 0;
    for (n, payload) in clsigs.iter().enumerate() {
        let other_sig = &clsigs[(n + 1) % clsigs.len()][36..];
        let height = i32::from_le_bytes(payload[..4].try_into().expect("4 bytes"));
        let mut cases = common_mutations(payload, other_sig);
        let heights =
            [height - 1, height + 1, -1, 0, 7, 8, 9, i32::MAX, i32::MIN + 8, i32::MIN + 7];
        for h in heights.into_iter().chain(boundaries.iter().copied()) {
            cases.push((format!("height {h}"), written(payload, 0, &h.to_le_bytes())));
        }
        cases.push(("block hash flipped".into(), flipped(payload, 4)));
        for (how, bytes) in cases {
            for bls in SCHEMES {
                let what = format!("{} clsig {n} {how} {bls:?}", cap.stem);
                let Some(CoreLock {
                    outcome: core_outcome,
                    hash: core_hash,
                    pass_quorums,
                    ..
                }) = core_lock_outcome(core.verify_chainlock(&bytes, legacy(bls)))
                else {
                    assert_eq!(how, format!("height {}", i32::MIN + 7), "{what}: Core refused");
                    continue;
                };
                let clsig = decode_whole(&bytes, |r| ClSig::decode(r).ok());
                // A pass that did not verify used the quorum dash-evo's selection picks.
                if let (
                    Some(clsig),
                    LockOutcome::NotVerified {
                        ..
                    },
                ) = (&clsig, &core_outcome)
                {
                    let view = CaptureView::new(&lc, &mut core);
                    let id = chainlock_request_id(clsig.height);
                    let ours = selected_quorum(&lc, &view, cl_type, &id, clsig.height, offset);
                    assert_eq!(pass_quorums, vec![ours], "{what}: the pass's quorum");
                    selections += usize::from(ours.is_some());
                }
                let ours = match &clsig {
                    None => Some(LockOutcome::Unreadable),
                    Some(clsig) => {
                        let view = CaptureView::new(&lc, &mut core);
                        match verify_chainlock(&lc.params, &view, LockSchemes::both(bls), clsig) {
                            // An eager signature canonical in neither scheme: Core's read throws.
                            Err(LockError::Rejected(LockReject::MalformedSignature)) => {
                                Some(LockOutcome::Unreadable)
                            }
                            got => our_lock_outcome(got, |l| *l.quorum.quorum_hash().as_bytes()),
                        }
                    }
                };
                if let (Some(clsig), Some(hash)) = (&clsig, core_hash) {
                    assert_eq!(
                        clsig_hash(clsig, ClsigHashSchemes::both(bls)),
                        Some(hash),
                        "{what}: the inventory hash"
                    );
                }
                tally.check(&what, bls, ours, core_outcome);
            }
        }
    }
    (tally, clsigs.len(), selections)
}

/// An `isdlock` payload's field offsets: (txid, cycle hash, inputs count).
fn isdlock_layout(payload: &[u8]) -> (usize, usize, usize) {
    let n = usize::from(payload[1]);
    assert!(n < 0xfd, "a one-byte input count");
    let txid = 2 + 36 * n;
    (txid, txid + 32, n)
}

/// An `isdlock` payload with `inputs` (each 36 bytes) in place of its own.
fn with_inputs(payload: &[u8], inputs: &[[u8; 36]]) -> Vec<u8> {
    let (txid, _, _) = isdlock_layout(payload);
    let mut out = vec![payload[0]];
    dash_consensus::primitives::write_compact_size(inputs.len() as u64, &mut out);
    inputs.iter().for_each(|i| out.extend(i));
    out.extend(&payload[txid..]);
    out
}

/// O2a-LQ3, `isdlock`: every captured InstantSend lock and its changes, under both schemes, get
/// Core's verdict (oracle_llmq_verify_islock: NetInstantSend's checks, each pass verifying the one
/// lock), its passes' quorums and Core's inventory hash from LQ3a's `verify_islock` and
/// `isdlock_hash`. Non-rotated InstantSend (`llmq_test_instantsend`).
#[test]
fn captured_islocks_match_cores_verdicts() {
    let d = islock_differential(LQ3);
    // Each captured lock and its version-2 rewrite: the version is not in the sign hash, and
    // Core reads any version.
    assert_eq!(d.tally.accepted, 2 * d.locks, "each captured lock: {:?}", d.tally);
    d.tally.assert_accepts_signed(LQ3);
    d.tally.assert_outcomes(&[
        "Accept",
        "Unreadable",
        "Refused(TriviallyInvalid)",
        "Refused(UnknownCycle)",
        "Refused(InvalidCycle)",
        "Refused(MalformedSignature)",
        "NotVerified { invalid: true }",
        "NotVerified { invalid: false }",
    ]);
    assert!(d.selections > 0, "no failing pass found a quorum");
}

/// BL-218 (D30), `isdlock` with rotated InstantSend (`llmq_test_dip0024`, mainnet's path) on the
/// legacy chain (cycles 264 and 288, tip 311): each pass's quorum is the one
/// `SelectQuorumForSigning` picks at the lock's sign height and the pass's offset. A lock carries
/// the cycle of the quorum that signed it, so each captured lock verifies in the first pass. The
/// two signed by cycle 264's quorums, rewritten to cycle 288, sign at the tip (288 + 24 > 311):
/// cycle 288's quorums reject them and the previous set's (offset 24, from 287) accept them. Any
/// lock on cycle 264 that does not verify there (a changed copy of either of the two, or the
/// cycle-288 lock rewritten to 264) signs at 287: cycle 264's quorum rejects it, and the previous
/// set (from 263) has no quorum, so Core's last pass is no quorum and ours is the same (D23).
/// Every accept is legacy (F6).
#[test]
fn rotated_legacy_islocks_match_cores_verdicts() {
    let d = islock_differential(LQ3R);
    assert_eq!(d.second_pass_accepts, 2, "{d:?}");
    assert_eq!(
        d.tally.accepted,
        2 * d.locks + d.second_pass_accepts,
        "each captured lock, its version-2 rewrite and the two second-pass accepts: {:?}",
        d.tally
    );
    d.tally.assert_accepts_signed(LQ3R);
    d.tally.assert_outcomes(&[
        "Accept",
        "Unreadable",
        "Refused(TriviallyInvalid)",
        "Refused(UnknownCycle)",
        "Refused(InvalidCycle)",
        "Refused(MalformedSignature)",
        "NotVerified { invalid: true }",
        "NotVerified { invalid: false }",
    ]);
    assert!(d.selections > 0, "no failing pass found a quorum");
    assert!(d.invalid_then_no_quorum > 0, "no D23 case: {d:?}");
}

/// What the `isdlock` differential on one capture saw.
#[derive(Debug)]
struct IslockRun {
    tally: Tally,
    /// Captured locks.
    locks: usize,
    /// The passes of Core's rejections that found a quorum (each compared with ours).
    selections: usize,
    /// Core's accepts by the second pass (the previous active set).
    second_pass_accepts: usize,
    /// Core's verdicts whose first pass rejected the signature and whose second found no quorum,
    /// where ours is the same no-score rejection (D23).
    invalid_then_no_quorum: usize,
}

fn islock_differential(cap: LockCapture) -> IslockRun {
    let (lc, mut core) = lock_chain(cap);
    let islocks = captured(cap, "isdlock");
    let is_type = lc.params.consensus.llmq_type_dip0024_instant_send;
    let llmq = lc.params.consensus.llmq(is_type).expect("configured");
    let interval = i32::from(llmq.dkg_interval);
    let tip = i64::from(lc.chain.tip());
    let block = |h: u32| *lc.chain.hash(h).expect("held").as_bytes();
    let mut bases: Vec<u32> = lc.quorums.keys().map(|(_, b)| lc.height_of[b]).collect();
    bases.sort_unstable();
    bases.dedup();
    // The InstantSend type's cycles: its quorums of index 0.
    let mut cycles: Vec<u32> = lc
        .quorums
        .iter()
        .filter(|((t, _), q)| *t == is_type.to_base() && q.quorum_index() == 0)
        .map(|((_, b), _)| lc.height_of[b])
        .collect();
    cycles.sort_unstable();
    assert!(cycles.len() >= 2, "{cycles:?}");
    let mut run = IslockRun {
        tally: Tally::default(),
        locks: islocks.len(),
        selections: 0,
        second_pass_accepts: 0,
        invalid_then_no_quorum: 0,
    };
    for (n, payload) in islocks.iter().enumerate() {
        let other_sig = {
            let other = &islocks[(n + 1) % islocks.len()];
            other[other.len() - 96..].to_vec()
        };
        let (txid, cycle, inputs) = isdlock_layout(payload);
        let own: Vec<[u8; 36]> =
            payload[2..txid].chunks(36).map(|c| c.try_into().expect("36")).collect();
        let cycle_height =
            lc.height_of[&<[u8; 32]>::try_from(&payload[cycle..cycle + 32]).expect("32")];
        let other_base = *bases.iter().find(|&&b| b != cycle_height).expect("two quorums");
        let mut cases = common_mutations(payload, &other_sig);
        // Two refusals at once, each early one with a later one: Core's order decides.
        let no_inputs = with_inputs(payload, &[]);
        let (_, no_inputs_cycle, _) = isdlock_layout(&no_inputs);
        let sig = payload.len() - 96;
        cases.extend([
            ("no inputs, unknown cycle".to_string(), flipped(&no_inputs, no_inputs_cycle)),
            (
                "no inputs, cycle off a boundary".into(),
                written(&no_inputs, no_inputs_cycle, &block(cycle_height + 1)),
            ),
            ("unknown cycle, zero sig".into(), written(&flipped(payload, cycle), sig, &[0; 96])),
            (
                "cycle off a boundary, zero sig".into(),
                written(&written(payload, cycle, &block(cycle_height + 1)), sig, &[0; 96]),
            ),
        ]);
        cases.extend([
            ("txid flipped".to_string(), flipped(payload, txid)),
            ("zero txid".into(), written(payload, txid, &[0; 32])),
            ("cycle hash flipped".into(), flipped(payload, cycle)),
            ("null cycle hash".into(), written(payload, cycle, &[0; 32])),
            ("cycle off a boundary".into(), written(payload, cycle, &block(cycle_height + 1))),
            ("the other quorum's cycle".into(), written(payload, cycle, &block(other_base))),
            ("a cycle with no quorum".into(), written(payload, cycle, &block(24))),
            ("first input's index + 1".into(), flipped(payload, 2 + 32)),
            ("version 2".into(), written(payload, 0, &[2])),
            ("no inputs".into(), with_inputs(payload, &[])),
            ("an input twice".into(), with_inputs(payload, &[&own[..], &own[..1]].concat())),
        ]);
        for &c in cycles.iter().filter(|&&c| c != cycle_height) {
            cases.push((format!("cycle {c}"), written(payload, cycle, &block(c))));
        }
        if n == 0 {
            // One past InstantSendLock::MAX_INPUTS, every input distinct.
            let many: Vec<[u8; 36]> = (0..=ISLOCK_MAX_INPUTS as u32)
                .map(|i| {
                    let mut input = [0u8; 36];
                    input[..4].copy_from_slice(&i.to_le_bytes());
                    input
                })
                .collect();
            cases.push(("one input too many".into(), with_inputs(payload, &many)));
        }
        assert!(inputs > 0);
        for (how, bytes) in cases {
            for bls in SCHEMES {
                let what = format!("{} isdlock {n} {how} {bls:?}", cap.stem);
                let CoreLock {
                    outcome: core_outcome,
                    hash: core_hash,
                    pass_quorums,
                    pass_statuses,
                } = core_lock_outcome(core.verify_islock(&bytes, legacy(bls))).expect(&what);
                let islock = decode_whole(&bytes, |r| IsdLock::decode(r).ok());
                let checked = matches!(
                    core_outcome,
                    LockOutcome::Accept(_) | LockOutcome::NotVerified { .. }
                );
                let invalid_then_no_quorum =
                    matches!(pass_statuses[..], [PassStatus::Invalid, PassStatus::NoQuorum]);
                if let (Some(islock), true) = (&islock, checked) {
                    let expected: Vec<Option<[u8; 32]>> = if llmq.use_rotation {
                        // `SelectQuorumForSigning` at the sign height (the tip, or the cycle's
                        // last block once the tip is past it) and each pass's offset.
                        let cycle = i64::from(lc.height_of[islock.cycle_hash.as_bytes()]);
                        let sign_height = if cycle + i64::from(interval) < tip {
                            (cycle + i64::from(interval) - 1) as i32
                        } else {
                            -1
                        };
                        let id = islock_request_id(&islock.inputs);
                        let view = CaptureView::new(&lc, &mut core);
                        [0, interval]
                            .into_iter()
                            .take(pass_quorums.len())
                            .map(|offset| {
                                selected_quorum(&lc, &view, is_type, &id, sign_height, offset)
                            })
                            .collect()
                    } else {
                        // Non-rotated: each pass's quorum is the one the cycle hash names
                        // (`GetQuorum(type, cycleHash)`), so both passes end alike (R2-N1).
                        if let [first, second] = pass_statuses[..] {
                            assert_eq!(first, second, "{what}: the passes' statuses");
                        }
                        let named = lc
                            .quorums
                            .contains_key(&(is_type.to_base(), *islock.cycle_hash.as_bytes()));
                        vec![named.then_some(*islock.cycle_hash.as_bytes()); pass_quorums.len()]
                    };
                    assert_eq!(pass_quorums, expected, "{what}: the passes' quorums");
                    if !matches!(core_outcome, LockOutcome::Accept(_)) {
                        run.selections += pass_quorums.iter().filter(|q| q.is_some()).count();
                    }
                    if let [PassStatus::Invalid | PassStatus::NoQuorum, PassStatus::Valid] =
                        pass_statuses[..]
                    {
                        run.second_pass_accepts += 1
                    }
                }
                let ours = match &islock {
                    None => Some(LockOutcome::Unreadable),
                    Some(islock) => {
                        let view = CaptureView::new(&lc, &mut core);
                        let got =
                            verify_islock(&lc.params, &view, IslockSchemes::both(bls), islock);
                        our_lock_outcome(got, |l| *l.quorum.quorum_hash().as_bytes())
                    }
                };
                if let (Some(islock), Some(hash)) = (&islock, core_hash) {
                    assert_eq!(isdlock_hash(islock), hash, "{what}: the inventory hash");
                }
                // D23 compared, not only reached: ours gives a verdict, and it is Core's.
                run.invalid_then_no_quorum += usize::from(
                    invalid_then_no_quorum
                        && ours
                            == Some(LockOutcome::NotVerified {
                                invalid: false,
                            }),
                );
                run.tally.check(&what, bls, ours, core_outcome);
            }
        }
    }
    run
}

/// Ours from a recovered-signature verdict: None for no verdict.
fn our_recsig_outcome(got: Result<RecSigVerdict, RecSigError<String>>) -> Option<LockOutcome> {
    Some(match got {
        Ok(RecSigVerdict::Accept(q)) => LockOutcome::Accept(*q.quorum_hash().as_bytes()),
        Ok(RecSigVerdict::Ignore(RecSigIgnore::InactiveQuorum)) => {
            LockOutcome::Refused(LockStatus::InactiveQuorum)
        }
        Ok(RecSigVerdict::Ban(RecSigBan::UnknownLlmqType)) => {
            LockOutcome::Refused(LockStatus::UnknownType)
        }
        Ok(RecSigVerdict::Ban(RecSigBan::MalformedSignature)) => {
            LockOutcome::Refused(LockStatus::MalformedSignature)
        }
        Ok(RecSigVerdict::Ban(RecSigBan::InvalidSignature)) => LockOutcome::NotVerified {
            invalid: true,
        },
        // The capture view holds every quorum Core's scan lists (LQ3a D6).
        Err(RecSigError::QuorumUnavailable) => panic!("ours: the view lacks a quorum"),
        Err(RecSigError::BlsUnsupported) => return None,
        Err(RecSigError::View(e)) => panic!("ours: {e}"),
    })
}

/// O2a-LQ3, `qsigrec`: every captured recovered signature and its changes, under both schemes,
/// get Core's intake verdict (oracle_llmq_verify_qsigrec: the one signature verified on its own),
/// and Core's inventory hash, from LQ3a's `verify_qsigrec` and `RecoveredSig::hash`.
#[test]
fn captured_recovered_sigs_match_cores_verdicts() {
    let (tally, recsigs) = recsig_differential(LQ3);
    assert_eq!(tally.accepted, recsigs, "each captured sig, once: {tally:?}");
    tally.assert_accepts_signed(LQ3);
    // Core's no-quorum pass is unreachable with consistent state: an active quorum is one
    // `GetQuorum` builds (LQ3a D6).
    tally.assert_outcomes(&[
        "Accept",
        "Unreadable",
        "Refused(UnknownType)",
        "Refused(InactiveQuorum)",
        "Refused(MalformedSignature)",
        "NotVerified { invalid: true }",
    ]);
}

/// BL-218 (D30), `qsigrec` on the legacy chain, `llmq_test` and the rotated `llmq_test_dip0024`
/// recovered sigs the InstantSend locks produced: each is accepted under the legacy scheme alone.
#[test]
fn legacy_recovered_sigs_match_cores_verdicts() {
    let (tally, recsigs) = recsig_differential(LQ3R);
    assert_eq!(tally.accepted, recsigs, "each captured sig, once: {tally:?}");
    tally.assert_accepts_signed(LQ3R);
    tally.assert_outcomes(&[
        "Accept",
        "Unreadable",
        "Refused(UnknownType)",
        "Refused(InactiveQuorum)",
        "Refused(MalformedSignature)",
        "NotVerified { invalid: true }",
    ]);
}

/// The `qsigrec` differential on one capture: the tally and the number of captured sigs.
fn recsig_differential(cap: LockCapture) -> (Tally, usize) {
    let (lc, mut core) = lock_chain(cap);
    let recsigs = captured(cap, "qsigrec");
    let mut bases: Vec<[u8; 32]> = lc.quorums.keys().map(|(_, b)| *b).collect();
    bases.sort_unstable();
    bases.dedup();
    let mut types: Vec<u8> = lc.quorums.keys().map(|(t, _)| *t).collect();
    types.sort_unstable();
    types.dedup();
    let mut tally = Tally::default();
    for (n, payload) in recsigs.iter().enumerate() {
        let other_sig = &recsigs[(n + 1) % recsigs.len()][97..];
        let other_base = *bases.iter().find(|b| b[..] != payload[1..33]).expect("two quorums");
        let other_type = *types.iter().find(|&&t| t != payload[0]).expect("two types");
        let mut cases = common_mutations(payload, other_sig);
        for t in [0u8, 6, 255, other_type] {
            cases.push((format!("type {t}"), written(payload, 0, &[t])));
        }
        cases.extend([
            ("quorum hash flipped".to_string(), flipped(payload, 1)),
            ("the other quorum".into(), written(payload, 1, &other_base)),
            ("id flipped".into(), flipped(payload, 33)),
            ("msg hash flipped".into(), flipped(payload, 65)),
        ]);
        for (how, bytes) in cases {
            for bls in SCHEMES {
                let what = format!("{} qsigrec {n} {how} {bls:?}", cap.stem);
                let CoreLock {
                    outcome: core_outcome,
                    hash: core_hash,
                    pass_quorums,
                    ..
                } = core_lock_outcome(core.verify_qsigrec(&bytes, legacy(bls))).expect(&what);
                let rec = decode_whole(&bytes, RecoveredSig::decode);
                if let (
                    Some(rec),
                    LockOutcome::NotVerified {
                        ..
                    },
                ) = (&rec, &core_outcome)
                {
                    // The pass checks the quorum the message names.
                    assert_eq!(
                        pass_quorums,
                        vec![Some(*rec.quorum_hash.as_bytes())],
                        "{what}: the pass's quorum"
                    );
                }
                let ours = match &rec {
                    None => Some(LockOutcome::Unreadable),
                    Some(rec) => {
                        let view = CaptureView::new(&lc, &mut core);
                        our_recsig_outcome(verify_qsigrec(
                            &lc.params,
                            &view,
                            LockSchemes::both(bls),
                            rec,
                        ))
                    }
                };
                if let (Some(rec), Some(hash)) = (&rec, core_hash) {
                    assert_eq!(rec.hash(), hash, "{what}: the inventory hash");
                }
                tally.check(&what, bls, ours, core_outcome);
            }
        }
    }
    (tally, recsigs.len())
}

/// O2a-LQ3 D21, the active window's upper edge (`IsQuorumActive`: the newest `keepOldConnections`
/// quorums at the tip, `llmq/signing.cpp` v24:701-712). On `llmq-regtest-v24.0`, which mines
/// more quorums of a type than its `keepOldConnections`, a `qsigrec` named for each mined
/// non-rotated quorum gets Core's verdict from LQ3a's `verify_qsigrec`: the older ones are
/// inactive in both. Core answers before it reads the signature, so any signature will do; a
/// captured one reads valid under the basic scheme.
#[test]
fn recovered_sigs_of_every_mined_quorum_match_cores_active_window() {
    let (lc, mut core) = lock_chain_of("llmq-regtest-v24.0", false);
    let sig = captured(LQ3, "qsigrec")[0][97..].to_vec();
    let mut tally = Tally::default();
    let mut inactive = HashMap::<u8, usize>::new();
    for &(llmq_type, base, _) in &lc.mined {
        let mut bytes = vec![llmq_type];
        bytes.extend(base);
        bytes.extend([0x1d; 32]);
        bytes.extend([0x2e; 32]);
        bytes.extend(&sig);
        for bls in SCHEMES {
            let what = format!("qsigrec type {llmq_type} quorum {} {bls:?}", lc.height_of[&base]);
            let CoreLock {
                outcome: core_outcome,
                hash: core_hash,
                ..
            } = core_lock_outcome(core.verify_qsigrec(&bytes, legacy(bls))).expect(&what);
            let rec = decode_whole(&bytes, RecoveredSig::decode).expect("a qsigrec");
            assert_eq!(core_hash, Some(rec.hash()), "{what}: the inventory hash");
            let view = CaptureView::new(&lc, &mut core);
            let ours =
                our_recsig_outcome(verify_qsigrec(&lc.params, &view, LockSchemes::both(bls), &rec));
            if core_outcome == LockOutcome::Refused(LockStatus::InactiveQuorum)
                && bls == BlsCtx::BASIC
            {
                *inactive.entry(llmq_type).or_default() += 1;
            }
            tally.check(&what, bls, ours, core_outcome);
        }
    }
    // Every type with more mined quorums than its window has exactly the excess inactive.
    let mut windows = 0;
    for (t, n) in lc.mined.iter().fold(HashMap::<u8, usize>::new(), |mut m, (t, ..)| {
        *m.entry(*t).or_default() += 1;
        m
    }) {
        let keep = lc
            .params
            .consensus
            .llmq(LlmqType::from_base(t))
            .expect("configured")
            .keep_old_connections;
        let keep = usize::from(keep);
        assert_eq!(
            inactive.get(&t).copied().unwrap_or(0),
            n.saturating_sub(keep),
            "type {t}: {tally:?}"
        );
        windows += usize::from(n > keep);
    }
    assert!(windows > 0, "no type mines past its window: {:?}", lc.mined);
    assert!(tally.outcomes.contains("Refused(InactiveQuorum)"), "{tally:?}");
    assert!(tally.outcomes.contains("NotVerified { invalid: true }"), "{tally:?}");
}

/// Core's mined-path verdict as ours names it: accepted, or rejected by `Verify`'s structure
/// (false) or by the queued signature checks (true).
fn core_mined(verdict: MinedVerdict) -> Result<(), bool> {
    match verdict {
        MinedVerdict::Accepted => Ok(()),
        MinedVerdict::VerifyFailed => Err(false),
        MinedVerdict::SignaturesFailed => Err(true),
    }
}

/// O2a-LQ3, the mined path (SB8 D18's follow-up): every commitment the three recorded chains
/// mined, and its structural and signature changes, under the mining block's scheme and the
/// other one, get Core's verdict (oracle_llmq_verify_mined: `CheckLLMQCommitment`'s
/// `Verify(checkSigs=false)`, then `ProcessBlock`'s queued checks) from SB8's
/// `verify_commitment(…, Some(SigPath::Mined))`; Core computes its own members. Rotated
/// commitments (BL-218, D29) read their members through the snapshot rows both sides store on
/// connecting every block ([`connect_rotation`]).
#[test]
fn mined_commitments_match_cores_mined_path() {
    let mut compared = 0;
    let mut rotated_compared: HashMap<&str, usize> = HashMap::new();
    let mut kinds: HashMap<String, usize> = HashMap::new();
    for stem in ["llmq-regtest-v24.0", "lq3-regtest-v24.0", "lq3r-regtest-v24.0"] {
        let Regtest {
            params,
            chain,
            source,
            mut core,
            ..
        } = recorded_regtest(stem);
        let deployments = HeightDeployments {
            params: &params,
        };
        let height_of = heights(&chain);
        let mined = recorded_mined(stem);
        let is_rotated =
            |t: u8| params.consensus.llmq(LlmqType::from_base(t)).expect("configured").use_rotation;
        let rotated_qcs: Vec<_> = mined.iter().filter(|(t, ..)| is_rotated(*t)).cloned().collect();
        let rows = if rotated_qcs.is_empty() {
            ModelRows::new()
        } else {
            connect_rotation(&params, &chain, &source, &mut core, &rotated_qcs)
        };
        let decoded: Vec<LlmqCommitment> = mined
            .iter()
            .map(|(_, b, _)| LlmqCommitment::decode(&mut &b[..]).expect("a commitment"))
            .collect();
        for (i, (qc, (_, _, mined_at))) in decoded.iter().zip(&mined).enumerate() {
            let other = &decoded[(i + 1) % decoded.len()];
            let base = height_of[qc.quorum_hash.as_bytes()];
            let llmq = params.consensus.llmq(qc.llmq_type).expect("configured");
            let mined_bls = bls_ctx_for_block(&params, *mined_at as u32);
            let rotation = is_rotated(qc.llmq_type.to_base());
            let members = {
                let ctx =
                    LlmqCtx::new(&params, &chain, &deployments, mined_bls).expect("in domain");
                let mut rows = rows.clone();
                members_uncached(
                    &ctx,
                    &source,
                    &mut RuntimeRows::new(&mut rows),
                    qc.llmq_type,
                    BlockId(base),
                )
                .expect("held")
            };
            assert!(!members.is_empty(), "{stem} {:?} at {base}: members", qc.llmq_type);
            let mut cases: Vec<(String, LlmqCommitment)> = vec![("as mined".into(), qc.clone())];
            let other_sig = (other.version == qc.version).then_some(&other.quorum_sig);
            for m in structural_mutations(qc, members.len(), usize::from(llmq.min_size), other_sig)
            {
                // Core computes the members itself: only the changes of the commitment.
                if m.members == members.len() {
                    cases.push((m.how.into(), m.qc));
                }
            }
            let mut with = |how: &str, edit: &dyn Fn(&mut LlmqCommitment)| {
                let mut m = qc.clone();
                edit(&mut m);
                cases.push((how.into(), m));
            };
            if other.version == qc.version {
                with("another commitment's membersSig", &|m| {
                    m.members_sig = other.members_sig.clone()
                });
            }
            with("membersSig and quorumSig swapped", &|m| {
                std::mem::swap(&mut m.members_sig, &mut m.quorum_sig)
            });
            let signer = qc.signers.iter_set_bits().next().expect("a signer") as usize;
            with("the first signer's bit cleared", &|m| {
                m.signers.data[signer / 8] &= !(1 << (signer % 8))
            });
            for (how, m) in cases {
                let mut bytes = Vec::new();
                m.encode(&mut bytes);
                for bls in [
                    mined_bls,
                    if legacy(mined_bls) {
                        BlsCtx::BASIC
                    } else {
                        BlsCtx::LEGACY
                    },
                ] {
                    let what = format!("{stem} {:?} at {base} {how} {bls:?}", qc.llmq_type);
                    let (verdict, _) = core.verify_mined(&bytes, legacy(bls)).expect(&what);
                    let ctx = LlmqCtx::new(&params, &chain, &deployments, bls).expect("in domain");
                    let (mut rows, mut effects) = (rows.clone(), Vec::new());
                    let mut rows = RuntimeRows::new(&mut rows);
                    let members = UncachedMembers::new(ctx, &source, &mut rows);
                    let ours = match verify_commitment(
                        &ctx,
                        &members,
                        &m,
                        BlockId(base),
                        Some(SigPath::Mined),
                        &mut effects,
                    ) {
                        Ok(()) => Ok(()),
                        Err(CommitmentError::Rejected(r)) => Err(matches!(
                            r,
                            CommitmentReject::MemberSignature
                                | CommitmentReject::MembersSignature
                                | CommitmentReject::QuorumSignature
                        )),
                        Err(CommitmentError::BlsUnsupported) => {
                            assert_eq!(bls, BlsCtx::LEGACY, "{what}: no verdict");
                            assert_eq!(verdict, MinedVerdict::SignaturesFailed, "{what}");
                            *kinds.entry("no verdict".into()).or_default() += 1;
                            continue;
                        }
                        Err(e) => panic!("{what}: {e:?}"),
                    };
                    assert_eq!(ours, core_mined(verdict), "{what}");
                    *kinds.entry(format!("{verdict:?}")).or_default() += 1;
                    compared += 1;
                    if rotation {
                        *rotated_compared.entry(stem).or_default() += 1;
                    }
                }
            }
        }
    }
    for kind in ["Accepted", "VerifyFailed", "SignaturesFailed"] {
        assert!(kinds.get(kind).is_some_and(|&n| n > 0), "{kind}: {kinds:?}");
    }
    assert_eq!(compared, 830, "verdicts: {kinds:?}");
    // The six rotated commitments of llmq-regtest (cycles 384, 408, 432) and the four of lq3r
    // (264, 288), each with its changes, per chain: a chain that drops out fails (D29).
    assert_eq!(
        rotated_compared,
        HashMap::from([("llmq-regtest-v24.0", 168), ("lq3r-regtest-v24.0", 112)]),
        "rotated verdicts per chain: {kinds:?}"
    );
}
