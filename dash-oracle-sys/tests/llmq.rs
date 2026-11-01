//! Smoke tests of the oracle's LLMQ contexts (oracle.h "LLMQ contexts", ABI 11): one test per
//! entry point on synthetic regtest chains, the `llmq_S` digest's known answers (Core's and a
//! Rust twin), the context rules (exclusivity, poisoning, stateful calls run once, restart, the
//! per-call BLS scheme) and the fixture's byte-exact round trip. Contexts are process-wide, so
//! the tests take turns (each holds the pin's lock while its context lives).

mod common;

use std::collections::BTreeMap;
use std::os::raw::c_int;

use common::quorum::{MEMBERS, member_list, quorum_commitment, signed_commitment};
use common::{hex as unhex, to_hex as hex, transcribed};

use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, DEPLOYMENT_V24, ErrorKind, GetQuorumOutcome,
    LlmqContext, LlmqFixture, LlmqWindow, LockStatus, Member, MembersOutcome, MinedVerdict, MnList,
    Oracle, PassStatus, Precompute, QuorumMembers, ScanCache, ScanDemand, ThresholdState,
    TraceEvent, Verdict, ffi,
};
use dashcore::consensus::deserialize;
use dashcore::hashes::{Hash, sha256, sha256d};

/// Regtest LLMQ types (llmq/params.h).
const LLMQ_TEST: u8 = 100;
const LLMQ_TEST_DIP0024: u8 = 103;
/// Enabled by `IsQuorumTypeEnabled` on every chain but configured on mainnet only.
const LLMQ_400_60: u8 = 2;
/// Not a type Core knows.
const UNKNOWN_TYPE: u8 = 0xfe;

/// A synthetic header at `height` on `prev`, with regtest proof of work (a context's block read
/// checks it, as ReadBlockFromDisk does).
fn header(prev: [u8; 32], height: i32, branch: u8) -> [u8; 80] {
    let mut h = unmined_header(prev, height, branch);
    // 0x207fffff: the hash, as a big-endian number, at most 7f ff ff 00 .. 00.
    let target: Vec<u8> = [0x7f, 0xff, 0xff].into_iter().chain([0; 29]).collect();
    for nonce in 0u32.. {
        h[76..].copy_from_slice(&nonce.to_le_bytes());
        if hash_of(&h).iter().rev().copied().collect::<Vec<u8>>() <= target {
            return h;
        }
    }
    unreachable!()
}

fn unmined_header(prev: [u8; 32], height: i32, branch: u8) -> [u8; 80] {
    let mut h = Vec::with_capacity(80);
    h.extend(0x2000_0000i32.to_le_bytes());
    h.extend(prev);
    let mut merkle = [branch; 32];
    merkle[..4].copy_from_slice(&height.to_le_bytes());
    h.extend(merkle);
    h.extend((1_417_713_337u32 + 150 * height as u32).to_le_bytes());
    h.extend(0x207f_ffffu32.to_le_bytes());
    h.extend(0u32.to_le_bytes());
    h.try_into().unwrap()
}

fn hash_of(header: &[u8; 80]) -> [u8; 32] {
    deserialize::<dashcore::block::Header>(header).unwrap().block_hash().to_byte_array()
}

/// Headers `first ..= last` (the first on a placeholder parent), by height.
fn chain(first: i32, last: i32) -> Vec<[u8; 80]> {
    let mut prev = [0; 32];
    (first..=last)
        .map(|h| {
            let header = header(prev, h, 0);
            prev = hash_of(&header);
            header
        })
        .collect()
}

/// Regtest with DIP0003 from height 2: Core stores mined commitments only once it is active.
fn regtest() -> Chain {
    Chain::new("regtest", &["-dip3params=2:2"])
}

/// A regtest context holding `headers` from height `first`, each with the empty masternode list.
fn seeded(oracle: &Oracle, first: i32, headers: &[[u8; 80]]) -> LlmqContext {
    seeded_with(oracle, first, headers, |_, _| vec![])
}

/// As [`seeded`], each header with `list(hash, height)`.
fn seeded_with(
    oracle: &Oracle,
    first: i32,
    headers: &[[u8; 80]],
    list: impl Fn([u8; 32], i32) -> Vec<u8>,
) -> LlmqContext {
    let mut ctx = oracle.llmq_context(&regtest()).unwrap();
    for (i, h) in headers.iter().enumerate() {
        let height = first + i as i32;
        ctx.add_header(h, height).unwrap();
        ctx.set_mn_list(&hash_of(h), &list(hash_of(h), height)).unwrap();
    }
    ctx
}

/// The list at `height` of a chain whose quorums Core can store: [`MEMBERS`] from DIP0003's
/// activation (height 2), so LLMQ_TEST's quorums (size 3) have them as their members and
/// [`commitment`] is signed by them; empty before.
fn member_list_at(block: [u8; 32], height: i32) -> Vec<u8> {
    if height >= 2 {
        member_list(block, height, &MEMBERS)
    } else {
        vec![]
    }
}

/// [`seeded`] with [`member_list_at`]'s lists: commitments of its quorums can be mined.
fn quorum_seeded(oracle: &Oracle, first: i32, headers: &[[u8; 80]]) -> LlmqContext {
    seeded_with(oracle, first, headers, member_list_at)
}

fn members_outcomes(events: &[TraceEvent]) -> Vec<MembersOutcome> {
    events
        .iter()
        .filter_map(|e| match e {
            TraceEvent::Members {
                outcome,
                ..
            } => Some(*outcome),
            _ => None,
        })
        .collect()
}

fn rows(events: &[TraceEvent]) -> usize {
    events.iter().filter(|e| matches!(e, TraceEvent::Row { .. })).count()
}

// ---- the llmq_S digest ----

/// The row key of `(type, block hash)`: `("llmq_S", SerializeHash(type, hash))` serialized.
fn snapshot_key(llmq_type: u8, block: &[u8; 32]) -> Vec<u8> {
    let mut preimage = vec![llmq_type];
    preimage.extend(block);
    let mut key = vec![6];
    key.extend(b"llmq_S");
    key.extend(sha256d::Hash::hash(&preimage).to_byte_array());
    key
}

/// The Rust twin of `oracle_llmq_rows_digest` (oracle.h): rows `(type, block, snapshot)`.
fn rows_digest(rows: &[(u8, [u8; 32], Vec<u8>)]) -> [u8; 32] {
    keyed_rows_digest(&rows.iter().map(|(t, b, row)| (snapshot_key(*t, b), row.clone())).collect())
}

/// The digest of rows by their evodb key (a sorted map: the digest's order).
fn keyed_rows_digest(keyed: &BTreeMap<Vec<u8>, Vec<u8>>) -> [u8; 32] {
    let mut preimage = vec![keyed.len() as u8];
    for (key, row) in keyed {
        preimage.push(key.len() as u8);
        preimage.extend(key);
        preimage.push(row.len() as u8);
        preimage.extend(row);
    }
    sha256::Hash::hash(&preimage).to_byte_array()
}

/// LQ1 §6.2's four vectors (`/work/scratch/lq1/digest_kat.py`): A = (5, 11 x 32,
/// `00000000094d0100`), B = (103, 00 01 .. 1f, `01000000040b0202000000ffffffff`), B' = B's key
/// with row `03000000040000`.
fn kat_rows() -> [(u8, [u8; 32], Vec<u8>); 3] {
    let b_hash: [u8; 32] = std::array::from_fn(|i| i as u8);
    [
        (5, [0x11; 32], unhex("00000000094d0100")),
        (103, b_hash, unhex("01000000040b0202000000ffffffff")),
        (103, b_hash, unhex("03000000040000")),
    ]
}

const KAT_EMPTY: &str = "6e340b9cffb37a989ca544e6bb780a2c78901d3fb33738768511a30617afa01d";
const KAT_A: &str = "3fae486d7339fc149727a978309e4e6d5ba435dfb07d68380bde6b24385331d7";
const KAT_A_B: &str = "8d633cea2b2d3a36fa0961257ee7e9bcd1e1d161d9db6018e425c8ec836ad66b";
const KAT_A_B2: &str = "ef532660182377caea3e420b965b09967087c1091f7464ae4841a25dd30608a9";

#[test]
fn rows_digest_twin_matches_the_known_answers() {
    let [a, b, b2] = kat_rows();
    assert_eq!(hex(&rows_digest(&[])), KAT_EMPTY);
    assert_eq!(hex(&rows_digest(std::slice::from_ref(&a))), KAT_A);
    assert_eq!(hex(&rows_digest(&[b.clone(), a.clone()])), KAT_A_B, "order-free");
    assert_eq!(hex(&rows_digest(&[a, b2])), KAT_A_B2);
}

#[test]
fn rows_digest_matches_the_known_answers() {
    for oracle in Oracle::all() {
        let [a, b, b2] = kat_rows();
        let mut ctx = oracle.llmq_context(&regtest()).unwrap();
        assert_eq!(hex(&ctx.rows_digest().unwrap()), KAT_EMPTY);
        // Snapshots of blocks outside the index are Core rows too (keyed by hash only).
        ctx.set_snapshot(a.0, &a.1, &a.2).unwrap();
        assert_eq!(hex(&ctx.rows_digest().unwrap()), KAT_A);
        ctx.set_snapshot(b.0, &b.1, &b.2).unwrap();
        assert_eq!(hex(&ctx.rows_digest().unwrap()), KAT_A_B);
        // A later store of the same key replaces the row.
        ctx.set_snapshot(b2.0, &b2.1, &b2.2).unwrap();
        assert_eq!(hex(&ctx.rows_digest().unwrap()), KAT_A_B2);
        // Core's decoder decides what a snapshot is; nothing may follow it.
        let mut trailing = b2.2.clone();
        trailing.push(0);
        assert_eq!(ctx.set_snapshot(1, &[0; 32], &trailing).unwrap_err().kind, ErrorKind::Trailing);
        assert_eq!(ctx.set_snapshot(1, &[0; 32], &[1, 0]).unwrap_err().kind, ErrorKind::Decode);
    }
}

// ---- seeding ----

#[test]
fn add_header_builds_the_index_and_the_active_chain() {
    for oracle in Oracle::all() {
        let headers = chain(100, 102);
        let mut ctx = oracle.llmq_context(&regtest()).unwrap();
        ctx.add_header(&headers[0], 100).unwrap();
        // A duplicate, and a header whose parent is not at height - 1.
        assert_eq!(ctx.add_header(&headers[0], 100).unwrap_err().kind, ErrorKind::Argument);
        assert_eq!(ctx.add_header(&headers[1], 102).unwrap_err().kind, ErrorKind::Argument);
        assert_eq!(ctx.add_header(&headers[2], 101).unwrap_err().kind, ErrorKind::Argument);
        ctx.add_header(&headers[1], 101).unwrap();
        // A fork header: the active chain ends at the last header added.
        let fork = header(hash_of(&headers[0]), 101, 1);
        ctx.add_header(&fork, 101).unwrap();
        ctx.set_mn_list(&hash_of(&fork), &[]).unwrap();
        ctx.set_mn_list(&hash_of(&headers[1]), &[]).unwrap();
        assert_eq!(ctx.add_header(&[0; 80], -1).unwrap_err().kind, ErrorKind::Argument);
    }
}

#[test]
fn seeding_refuses_what_core_never_stores() {
    for oracle in Oracle::all() {
        let headers = chain(0, 2);
        let mut ctx = seeded(&oracle, 0, &headers[..2]);
        let unknown = hash_of(&headers[2]);
        assert_eq!(ctx.set_mn_list(&unknown, &[]).unwrap_err().kind, ErrorKind::Argument);
        assert_eq!(
            ctx.set_mn_list(&hash_of(&headers[0]), &[]).unwrap_err().kind,
            ErrorKind::Argument,
            "once"
        );
        assert_eq!(ctx.set_coinbase(&unknown, &[], true).unwrap_err().kind, ErrorKind::Argument);
        let block = hash_of(&headers[1]);
        assert_eq!(ctx.set_coinbase(&block, &[1, 0], true).unwrap_err().kind, ErrorKind::Decode);
        // A commitment whose llmqType is not the argument's, and the null commitment.
        let qc = commitment(LLMQ_TEST, &block, true);
        assert_eq!(ctx.set_mined(LLMQ_TEST_DIP0024, &qc, 1).unwrap_err().kind, ErrorKind::Argument);
        let null = commitment(LLMQ_TEST, &block, false);
        assert_eq!(ctx.set_mined(LLMQ_TEST, &null, 1).unwrap_err().kind, ErrorKind::Argument);
        // Mined at or below its base.
        assert_eq!(ctx.set_mined(LLMQ_TEST, &qc, 1).unwrap_err().kind, ErrorKind::Argument);
    }
}

#[test]
fn seeding_closes_after_a_computing_call_until_restart() {
    for oracle in Oracle::all() {
        let headers = chain(0, 72);
        let mut ctx = quorum_seeded(&oracle, 0, &headers);
        let tip = hash_of(&headers[72]);
        let qc = commitment(LLMQ_TEST, &hash_of(&headers[48]), true);
        // Calls that compute nothing leave seeding open.
        ctx.rows_digest().unwrap();
        ctx.type_enabled(LLMQ_TEST, Some(&tip), None, None).unwrap();
        ctx.set_coinbase(&tip, &coinbase(), true).unwrap();
        ctx.scan(LLMQ_TEST, &tip, 1, false).unwrap();
        let next = header(tip, 73, 0);
        let refused = [
            ctx.set_mined(LLMQ_TEST, &qc, 60).unwrap_err(),
            ctx.add_header(&next, 73).unwrap_err(),
            ctx.set_mn_list(&tip, &[]).unwrap_err(),
            ctx.set_coinbase(&tip, &coinbase(), false).unwrap_err(),
            ctx.set_snapshot(LLMQ_TEST_DIP0024, &tip, &kat_rows()[0].2).unwrap_err(),
            ctx.load(&fixture(&headers)).unwrap_err(),
        ];
        for err in refused {
            assert_eq!(err.kind, ErrorKind::Argument, "{err}");
            assert!(err.message.contains("seeding is closed"), "{err}");
        }
        // A restarted node: seeding opens again over the same databases.
        ctx.restart().unwrap();
        ctx.set_mined(LLMQ_TEST, &qc, 60).unwrap();
        assert_eq!(ctx.scan(LLMQ_TEST, &tip, 1, false).unwrap().0, [hash_of(&headers[48])]);
    }
}

/// oracle_llmq_set_mined's checks against the members Core computes from the seeded state
/// (ProcessBlock()'s queued signature checks, then Verify()'s member tail): a refusal, or members
/// the context cannot compute, leaves the context as it was and seeding open.
#[test]
fn set_mined_checks_the_commitment_against_its_members() {
    let signatures = "signatures do not verify against its quorum's members (bad-qc-invalid)";
    let tail = "a validMembers or signers bit is set at or beyond the quorum's member count";
    for oracle in Oracle::all() {
        let headers = chain(0, 72);
        let base = hash_of(&headers[48]);
        let qc = commitment(LLMQ_TEST, &base, true);
        let refused = |ctx: &mut LlmqContext, qc: &[u8], why: &str| {
            let digest = ctx.rows_digest().unwrap();
            let err = ctx.set_mined(LLMQ_TEST, qc, 60).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Argument, "{err}");
            assert!(err.message.contains(why), "{err}");
            assert_eq!(ctx.rows_digest().unwrap(), digest);
        };
        // The fixture of ABI 13: three bits set and no members (every list empty).
        let mut ctx = seeded(&oracle, 0, &headers);
        refused(&mut ctx, &qc, signatures);
        drop(ctx);

        let mut ctx = quorum_seeded(&oracle, 0, &headers);
        // quorumSig by another key (the members' aggregate in its place), and signers that are
        // not the members.
        let mut wrong_key = qc.clone();
        wrong_key.copy_within(215..311, 119);
        refused(&mut ctx, &wrong_key, signatures);
        refused(
            &mut ctx,
            &signed_commitment(LLMQ_TEST, base, 3, 3, &[0x41, 0x42, 0x43], 1),
            signatures,
        );
        refused(&mut ctx, &signed_commitment(LLMQ_TEST, base, 3, 3, &MEMBERS[..2], 1), signatures);
        // Seeding stays open after the refusals.
        ctx.set_mined(LLMQ_TEST, &qc, 60).unwrap();
        assert_eq!(ctx.scan(LLMQ_TEST, &hash_of(&headers[72]), 1, false).unwrap().0, [base]);
        drop(ctx);

        // Two members: three bits fail Verify()'s member tail, two bits are stored.
        let mut ctx = seeded_with(&oracle, 0, &headers, |block, height| {
            if height >= 2 {
                member_list(block, height, &MEMBERS[..2])
            } else {
                vec![]
            }
        });
        refused(&mut ctx, &signed_commitment(LLMQ_TEST, base, 3, 3, &MEMBERS[..2], 1), tail);
        ctx.set_mined(LLMQ_TEST, &signed_commitment(LLMQ_TEST, base, 3, 2, &MEMBERS[..2], 1), 60)
            .unwrap();
        drop(ctx);

        // The base's list is missing: no verdict, and the context is not poisoned.
        let mut ctx = oracle.llmq_context(&regtest()).unwrap();
        for (h, header) in headers.iter().enumerate() {
            ctx.add_header(header, h as i32).unwrap();
            if h != 48 {
                ctx.set_mn_list(&hash_of(header), &member_list_at(hash_of(header), h as i32))
                    .unwrap();
            }
        }
        let err = ctx.set_mined(LLMQ_TEST, &qc, 60).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{err}");
        assert!(err.message.contains("masternode list at height 48"), "{err}");
        ctx.set_mn_list(&base, &member_list_at(base, 48)).unwrap();
        ctx.set_mined(LLMQ_TEST, &qc, 60).unwrap();
    }
}

#[test]
fn the_active_chain_keeps_the_seeded_commitments() {
    for oracle in Oracle::all() {
        let headers = chain(0, 72);
        let mut ctx = oracle.llmq_context(&regtest()).unwrap();
        for (h, header) in headers[..=48].iter().enumerate() {
            ctx.add_header(header, h as i32).unwrap();
        }
        // A fork block at 49, then the main chain on to 72.
        let fork = header(hash_of(&headers[48]), 49, 1);
        ctx.add_header(&fork, 49).unwrap();
        for (h, header) in headers.iter().enumerate().skip(49) {
            ctx.add_header(header, h as i32).unwrap();
        }
        for (header, height) in headers.iter().zip(0..).chain([(&fork, 49)]) {
            ctx.set_mn_list(&hash_of(header), &member_list_at(hash_of(header), height)).unwrap();
        }
        // A base that is not the mining block's ancestor.
        let off_chain = commitment(LLMQ_TEST, &hash_of(&fork), true);
        let err = ctx.set_mined(LLMQ_TEST, &off_chain, 60).unwrap_err();
        assert!(err.message.contains("not on the active chain"), "{err}");
        ctx.set_mined(LLMQ_TEST, &commitment(LLMQ_TEST, &hash_of(&headers[48]), true), 60).unwrap();
        // The same commitment again is a second commitment of its quorum; another quorum's at
        // that height is not its DKG cycle's (a type that does not rotate mines one quorum per
        // cycle, so the one-per-height index refuses nothing the base rule lets through).
        let again = commitment(LLMQ_TEST, &hash_of(&headers[48]), true);
        let err = ctx.set_mined(LLMQ_TEST, &again, 60).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{err}");
        assert!(err.message.contains("mined at this height"), "{err}");
        let err = ctx
            .set_mined(LLMQ_TEST, &commitment(LLMQ_TEST, &hash_of(&headers[24]), true), 60)
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{err}");
        assert!(err.message.contains("base block of mined_height's DKG cycle"), "{err}");
        // Headers that end the active chain off mining block 60, or below it, are refused; one
        // extending a fork above it is not.
        for (prev, height) in [(59, 60), (49, 50)] {
            let off = header(hash_of(&headers[prev]), height, 2);
            let err = ctx.add_header(&off, height).unwrap_err();
            assert!(err.message.contains("mined a seeded commitment"), "{err}");
        }
        ctx.add_header(&header(hash_of(&headers[60]), 61, 2), 61).unwrap();
    }
}

/// A `CFinalCommitment` (version 3, basic BLS, not indexed) of LLMQ_TEST-sized bitsets for
/// `base`; `non_null`: [`MEMBERS`] valid and signing (quorum key ikm `[1; 32]`), otherwise the
/// null commitment.
fn commitment(llmq_type: u8, base: &[u8; 32], non_null: bool) -> Vec<u8> {
    if non_null {
        return quorum_commitment(llmq_type, *base, 3, &MEMBERS, 1);
    }
    let mut qc = Vec::new();
    qc.extend(3u16.to_le_bytes());
    qc.push(llmq_type);
    qc.extend(base);
    qc.extend([3, 0, 3, 0]); // DYNBITSET signers, validMembers (3 members)
    qc.extend([0; 48 + 32 + 96 + 96]); // quorumPublicKey, quorumVvecHash, quorumSig, membersSig
    qc
}

// ---- members ----

#[test]
fn members_gate_compute_cache_and_reset() {
    for oracle in Oracle::all() {
        let headers = chain(0, 60);
        let mut ctx = seeded(&oracle, 0, &headers);
        let genesis = hash_of(&headers[0]);
        let base = hash_of(&headers[48]);
        // Genesis has no previous block: the gate.
        let (members, events) = ctx.members(LLMQ_TEST, &genesis, false, false).unwrap();
        assert!(members.is_empty());
        assert_eq!(members_outcomes(&events), [MembersOutcome::Gate]);
        // An empty list computes no members, then the base's cache answers.
        let (members, events) = ctx.members(LLMQ_TEST, &base, false, false).unwrap();
        assert!(members.is_empty());
        assert_eq!(members_outcomes(&events), [MembersOutcome::Computed]);
        let (_, events) = ctx.members(LLMQ_TEST, &base, false, false).unwrap();
        assert_eq!(members_outcomes(&events), [MembersOutcome::BaseHit]);
        let (_, events) = ctx.members(LLMQ_TEST, &base, false, true).unwrap();
        assert_eq!(members_outcomes(&events), [MembersOutcome::Computed], "reset_cache");
        assert!(matches!(
            events[0],
            TraceEvent::Members {
                llmq_type: LLMQ_TEST,
                reset: true,
                n: 0,
                ..
            }
        ));
        // Enabled but not configured on regtest (Core asserts), and a type Core does not know.
        assert_eq!(
            ctx.members(LLMQ_400_60, &base, false, false).unwrap_err().kind,
            ErrorKind::Argument
        );
        assert_eq!(
            ctx.members(UNKNOWN_TYPE, &base, false, false).unwrap_err().kind,
            ErrorKind::Exception
        );
    }
}

/// At V20 a quorum's work block is 8 below its base: below genesis Core would dereference null.
#[test]
fn members_refuse_a_work_block_below_genesis() {
    for oracle in Oracle::all() {
        let chain_args = Chain::new("regtest", &["-dip3params=2:3", "-testactivationheight=v20@2"]);
        let headers = chain(0, 10);
        let mut ctx = oracle.llmq_context(&chain_args).unwrap();
        for (h, header) in headers.iter().enumerate() {
            ctx.add_header(header, h as i32).unwrap();
            ctx.set_mn_list(&hash_of(header), &[]).unwrap();
        }
        let err = ctx.members(LLMQ_TEST, &hash_of(&headers[1]), false, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{err}");
        assert!(err.message.contains("below genesis"), "{err}");
    }
}

/// Core's `diff_chain_base` test list (evo_snapshot_tests, three masternodes registered by height
/// 27) as evodb stores it, relabelled for `block` at `height`.
fn three_masternodes(block: &[u8; 32], height: i32) -> Vec<u8> {
    #[derive(serde::Deserialize)]
    struct Record {
        kind: String,
        case: String,
        disk: Option<String>,
    }
    let record = transcribed::<Record>("evo_snapshot_tests.json")
        .into_iter()
        .find(|r| r.kind == "list" && r.case == "diff_chain_base")
        .expect("the diff_chain_base list");
    let mut list = unhex(&record.disk.unwrap());
    list[..32].copy_from_slice(block);
    list[32..36].copy_from_slice(&height.to_le_bytes());
    list
}

#[test]
fn rotated_members_by_quorum_index() {
    for oracle in Oracle::all() {
        // DIP0003 from 100, so Core reads the lists of cycle 120's work block (112); V20 stays at
        // 432 (no coinbase reads).
        let chain_args = Chain::new("regtest", &["-dip3params=100:110"]);
        let headers = chain(0, 130);
        let mut ctx = oracle.llmq_context(&chain_args).unwrap();
        for (h, header) in headers.iter().enumerate() {
            ctx.add_header(header, h as i32).unwrap();
            let list = if h == 112 {
                three_masternodes(&hash_of(header), 112)
            } else {
                vec![]
            };
            ctx.set_mn_list(&hash_of(header), &list).unwrap();
        }
        let digest = ctx.rows_digest().unwrap();
        let (members, events) =
            ctx.members(LLMQ_TEST_DIP0024, &hash_of(&headers[120]), false, false).unwrap();
        assert_eq!(members_outcomes(&events), [MembersOutcome::Computed]);
        assert!(!members.is_empty() && members.iter().all(|m| m.pro_tx_hash[..8] == [0xa5; 8]));
        // The cycle's quarter computation stores the cycle's snapshot row, which the digest sees.
        assert_eq!(rows(&events), 1, "{events:?}");
        assert_ne!(ctx.rows_digest().unwrap(), digest);
        let (_, events) =
            ctx.members(LLMQ_TEST_DIP0024, &hash_of(&headers[121]), false, false).unwrap();
        assert_eq!(members_outcomes(&events), [MembersOutcome::IndexHit]);
        let (_, events) =
            ctx.members(LLMQ_TEST_DIP0024, &hash_of(&headers[122]), false, false).unwrap();
        assert_eq!(members_outcomes(&events), [MembersOutcome::IndexOutOfRange]);
    }
}

// ---- missing state and poisoning ----

/// Regtest's DIP0003 (and V20) activation without `-dip3params`: Core reads lists, and at V20
/// the work block's coinbase, from here.
const DIP0003_HEIGHT: i32 = 432;

/// Headers 0 ..= 460 with lists everywhere but `no_list`, and work block 448's coinbase unless
/// `no_coinbase`; the LLMQ_TEST quorum at 456 computes from work block 448.
fn v20_context(
    oracle: &Oracle,
    no_list: Option<i32>,
    coinbase_data: Option<bool>,
) -> (LlmqContext, Vec<[u8; 80]>) {
    let headers = chain(0, 460);
    let mut ctx = oracle.llmq_context(&Chain::regtest()).unwrap();
    for (h, header) in headers.iter().enumerate() {
        ctx.add_header(header, h as i32).unwrap();
        if no_list != Some(h as i32) {
            ctx.set_mn_list(&hash_of(header), &[]).unwrap();
        }
    }
    if let Some(have_data) = coinbase_data {
        ctx.set_coinbase(&hash_of(&headers[448]), &coinbase(), have_data).unwrap();
    }
    (ctx, headers)
}

#[test]
fn a_missing_list_poisons_the_context() {
    for oracle in Oracle::all() {
        // Below DIP0003 Core reads no list: none is missing.
        let headers = chain(0, 30);
        let mut ctx = oracle.llmq_context(&Chain::regtest()).unwrap();
        for (h, header) in headers.iter().enumerate() {
            ctx.add_header(header, h as i32).unwrap();
        }
        let (_, events) = ctx.members(LLMQ_TEST, &hash_of(&headers[24]), false, false).unwrap();
        assert_eq!(members_outcomes(&events), [MembersOutcome::Computed]);
        drop(ctx);
        const _: () = assert!(456 - 8 > DIP0003_HEIGHT, "work block 448 reads lists");
        let (mut ctx, headers) = v20_context(&oracle, Some(448), Some(true));
        let err = ctx.members(LLMQ_TEST, &hash_of(&headers[456]), false, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{err}");
        assert!(err.message.contains("masternode list at height 448"), "{err}");
        for err in [ctx.rows_digest().unwrap_err(), ctx.restart().unwrap_err()] {
            assert_eq!(err.kind, ErrorKind::ContextPoisoned, "{err}");
        }
    }
}

#[test]
fn a_missing_block_poisons_the_context_and_pruned_data_does_not() {
    for oracle in Oracle::all() {
        let (mut ctx, headers) = v20_context(&oracle, None, None);
        let err = ctx.members(LLMQ_TEST, &hash_of(&headers[456]), false, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{err}");
        assert!(err.message.contains("block") && err.message.contains("height 448"), "{err}");
        assert_eq!(
            ctx.members(LLMQ_TEST, &hash_of(&headers[456]), false, false).unwrap_err().kind,
            ErrorKind::ContextPoisoned
        );
        drop(ctx);
        // A block whose data is gone: Core's read fails and it takes the block-hash modifier, as
        // with a coinbase that holds no chainlock.
        let mut members = Vec::new();
        for have_data in [false, true] {
            let (mut ctx, headers) = v20_context(&oracle, None, Some(have_data));
            let (m, events) =
                ctx.members(LLMQ_TEST, &hash_of(&headers[456]), false, false).unwrap();
            assert_eq!(members_outcomes(&events), [MembersOutcome::Computed]);
            members.push(m);
        }
        assert_eq!(members[0], members[1]);
    }
}

#[test]
fn a_snapshot_read_below_the_first_header_poisons_the_context() {
    for oracle in Oracle::all() {
        // Cycle 24's previous cycle (height 0) is below the context's headers.
        let headers = chain(10, 30);
        let mut ctx = seeded(&oracle, 10, &headers);
        let err = ctx.members(LLMQ_TEST_DIP0024, &hash_of(&headers[14]), false, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{err}");
        assert!(err.message.contains("snapshot of height 0"), "{err}");
        assert_eq!(ctx.rows_digest().unwrap_err().kind, ErrorKind::ContextPoisoned);
        drop(ctx);
        // The same rotated read with height 0 in the context: Core finds no row there, which is
        // the context's database, so the members compute.
        let headers = chain(0, 30);
        let mut ctx = seeded(&oracle, 0, &headers);
        let (_, events) =
            ctx.members(LLMQ_TEST_DIP0024, &hash_of(&headers[24]), false, false).unwrap();
        assert_eq!(members_outcomes(&events), [MembersOutcome::Computed]);
        ctx.rows_digest().unwrap();
    }
}

#[test]
fn a_seeded_snapshot_shorter_than_the_list_poisons_the_context() {
    for oracle in Oracle::all() {
        // Cycle 120's members read cycle 96's snapshot and work block 88's list, which holds
        // three masternodes.
        let chain_args = Chain::new("regtest", &["-dip3params=80:90"]);
        let headers = chain(0, 130);
        let context = |bits: usize| {
            let mut ctx = oracle.llmq_context(&chain_args).unwrap();
            for (h, header) in headers.iter().enumerate() {
                ctx.add_header(header, h as i32).unwrap();
                let list = if h == 88 || h == 112 {
                    three_masternodes(&hash_of(header), h as i32)
                } else {
                    vec![]
                };
                ctx.set_mn_list(&hash_of(header), &list).unwrap();
            }
            // mnSkipListMode 0, `bits` members none of them active, no skip list.
            let mut snapshot = vec![0, 0, 0, 0, bits as u8];
            snapshot.extend(vec![0; bits.div_ceil(8)]);
            snapshot.push(0);
            ctx.set_snapshot(LLMQ_TEST_DIP0024, &hash_of(&headers[96]), &snapshot).unwrap();
            ctx
        };
        let base = hash_of(&headers[120]);
        let (members, _) = context(3).members(LLMQ_TEST_DIP0024, &base, false, false).unwrap();
        assert!(!members.is_empty());
        let mut ctx = context(1);
        let err = ctx.members(LLMQ_TEST_DIP0024, &base, false, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{err}");
        assert!(
            err.message.contains("marks 1 masternodes") && err.message.contains("has 3"),
            "{err}"
        );
        assert_eq!(ctx.rows_digest().unwrap_err().kind, ErrorKind::ContextPoisoned);
    }
}

// ---- stateful calls ----

#[test]
fn a_stateful_call_whose_output_does_not_fit_runs_once() {
    for oracle in Oracle::all() {
        let headers = chain(0, 60);
        let mut ctx = seeded(&oracle, 0, &headers);
        let base = hash_of(&headers[48]);
        // Empty buffers: the first call computes and keeps its outputs, the second collects them.
        let (members, trace) =
            ctx.members_raw(LLMQ_TEST_DIP0024, &base, false, false, (0, 0)).unwrap();
        let events = TraceEvent::decode_trace(&trace).unwrap();
        assert_eq!(members_outcomes(&events), [MembersOutcome::Computed], "ran once");
        assert_eq!(members, [0]);
        let after_one = ctx.rows_digest().unwrap();

        // Another call drops a pending output: the same call then runs again (and hits the cache).
        drop(ctx);
        let mut ctx = seeded(&oracle, 0, &headers);
        let raw = |ctx: &mut LlmqContext, cap: usize| -> (c_int, Vec<u8>) {
            let (mut m, mut t) = (vec![0u8; cap], vec![0u8; cap]);
            let (mut m_len, mut t_len) = (cap, cap);
            let status = unsafe {
                (ffi::v24_0::API.llmq_members)(
                    ctx.as_raw(),
                    LLMQ_TEST_DIP0024,
                    base.as_ptr(),
                    0,
                    0,
                    m.as_mut_ptr(),
                    &mut m_len,
                    t.as_mut_ptr(),
                    &mut t_len,
                )
            };
            t.truncate(t_len);
            (status, t)
        };
        assert_eq!(raw(&mut ctx, 0).0, ffi::ORACLE_E_BUFFER);
        assert_eq!(ctx.rows_digest().unwrap(), after_one, "the first call ran");
        let (status, trace) = raw(&mut ctx, 4096);
        assert_eq!(status, ffi::ORACLE_OK);
        let events = TraceEvent::decode_trace(&trace).unwrap();
        assert_eq!(members_outcomes(&events), [MembersOutcome::BaseHit], "ran again");
    }
}

#[test]
fn restart_clears_caches_and_keeps_the_database() {
    for oracle in Oracle::all() {
        let headers = chain(0, 60);
        let mut ctx = seeded(&oracle, 0, &headers);
        let base = hash_of(&headers[48]);
        ctx.members(LLMQ_TEST_DIP0024, &base, false, false).unwrap();
        let (_, events) = ctx.members(LLMQ_TEST_DIP0024, &base, false, false).unwrap();
        assert_eq!(members_outcomes(&events), [MembersOutcome::BaseHit]);
        let digest = ctx.rows_digest().unwrap();
        ctx.restart().unwrap();
        assert_eq!(ctx.rows_digest().unwrap(), digest, "rows survive");
        let (_, events) = ctx.members(LLMQ_TEST_DIP0024, &base, false, false).unwrap();
        assert_eq!(members_outcomes(&events), [MembersOutcome::Computed], "statics cleared");
    }
}

/// bls::bls_legacy_scheme is process-wide and every entry sets it for its call, so the other
/// tests of this binary would change it under the checks: they run alone in a child process.
#[test]
fn legacy_bls_is_restored_on_every_path() {
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "legacy_bls_is_restored_child", "--test-threads=1"])
        .env("LLMQ_BLS_CHILD", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&child.stdout);
    assert!(child.status.success(), "{stdout}{}", String::from_utf8_lossy(&child.stderr));
    assert!(stdout.contains("1 passed"), "the child ran the checks: {stdout}");
}

/// Run by `legacy_bls_is_restored_on_every_path` in a process of its own.
#[test]
fn legacy_bls_is_restored_child() {
    if std::env::var_os("LLMQ_BLS_CHILD").is_none() {
        return;
    }
    for oracle in Oracle::all() {
        let headers = chain(0, 30);
        let base = hash_of(&headers[24]);
        // The process's scheme (Core starts legacy) is the caller's: each call sets its own and
        // puts the caller's back, whichever it asked for.
        let caller = oracle.bls_legacy_scheme();
        for legacy in [true, false] {
            let mut ctx = seeded(&oracle, 0, &headers);
            ctx.members(LLMQ_TEST, &base, legacy, true).unwrap();
            assert_eq!(oracle.bls_legacy_scheme(), caller, "after a call with legacy_bls {legacy}");
            ctx.precompute(&base, legacy, Precompute::Connect).unwrap();
            assert_eq!(oracle.bls_legacy_scheme(), caller);
            // Core throws inside the call (an unknown type): the scheme is restored too, and the
            // context, whose caches may hold half a computation, is poisoned.
            assert_eq!(
                ctx.members(UNKNOWN_TYPE, &base, legacy, false).unwrap_err().kind,
                ErrorKind::Exception
            );
            assert_eq!(
                oracle.bls_legacy_scheme(),
                caller,
                "after a throw with legacy_bls {legacy}"
            );
            assert_eq!(ctx.rows_digest().unwrap_err().kind, ErrorKind::ContextPoisoned);
        }
    }
}

// ---- the other computing entries ----

#[test]
fn type_enabled_follows_core_and_its_overrides() {
    for oracle in Oracle::all() {
        let headers = chain(0, 2);
        let mut ctx = seeded(&oracle, 0, &headers);
        let prev = hash_of(&headers[1]);
        assert!(!ctx.type_enabled(LLMQ_TEST, None, None, None).unwrap(), "null prev");
        assert!(ctx.type_enabled(LLMQ_TEST, Some(&prev), None, None).unwrap());
        assert!(ctx.type_enabled(LLMQ_TEST_DIP0024, Some(&prev), None, None).unwrap());
        assert!(!ctx.type_enabled(LLMQ_TEST_DIP0024, Some(&prev), Some(false), None).unwrap());
        assert_eq!(
            ctx.type_enabled(UNKNOWN_TYPE, Some(&prev), None, None).unwrap_err().kind,
            ErrorKind::Exception
        );
        assert_eq!(
            ctx.type_enabled(LLMQ_TEST, Some(&[9; 32]), None, None).unwrap_err().kind,
            ErrorKind::Argument
        );
    }
}

#[test]
fn precompute_connects_and_undoes() {
    for oracle in Oracle::all() {
        let headers = chain(0, 60);
        let mut ctx = seeded(&oracle, 0, &headers);
        let genesis = hash_of(&headers[0]);
        assert_eq!(
            ctx.precompute(&genesis, false, Precompute::Connect).unwrap_err().kind,
            ErrorKind::Argument
        );
        // A rotated cycle's base: PreComputeQuorumMembers computes its members.
        let cycle = hash_of(&headers[48]);
        let events = ctx.precompute(&cycle, false, Precompute::Connect).unwrap();
        assert!(members_outcomes(&events).contains(&MembersOutcome::Computed), "{events:?}");
        let events = ctx.precompute(&cycle, false, Precompute::Connect).unwrap();
        assert!(
            !members_outcomes(&events).contains(&MembersOutcome::Computed),
            "cached: {events:?}"
        );
        let events = ctx.precompute(&cycle, false, Precompute::Undo).unwrap();
        assert!(members_outcomes(&events).contains(&MembersOutcome::Computed), "reset: {events:?}");
        assert!(events.iter().all(|e| matches!(
            e,
            TraceEvent::Members {
                reset: true,
                ..
            } | TraceEvent::Row { .. }
        )));
    }
}

#[test]
fn scan_finds_mined_quorums_and_reports_its_cache() {
    for oracle in Oracle::all() {
        let headers = chain(0, 72);
        let mut ctx = quorum_seeded(&oracle, 0, &headers);
        let tip = hash_of(&headers[72]);
        assert_eq!(
            ctx.scan(LLMQ_TEST, &tip, 0, false).unwrap(),
            (vec![], vec![]),
            "count 0: the gate"
        );
        let (bases, events) = ctx.scan(LLMQ_TEST, &tip, 2, false).unwrap();
        assert!(bases.is_empty());
        assert!(
            matches!(
                events.last(),
                Some(TraceEvent::Scan {
                    cache: ScanCache::Miss,
                    n: 0,
                    ..
                })
            ),
            "{events:?}"
        );
        // A commitment for base 48 mined at 60 (its mining window), seeded into a restarted node.
        let base = hash_of(&headers[48]);
        ctx.restart().unwrap();
        ctx.set_mined(LLMQ_TEST, &commitment(LLMQ_TEST, &base, true), 60).unwrap();
        let (bases, events) = ctx.scan(LLMQ_TEST, &tip, 2, false).unwrap();
        assert_eq!(bases, [base]);
        assert!(
            events.iter().any(|e| matches!(
                e,
                TraceEvent::GetQuorum {
                    outcome: GetQuorumOutcome::Built,
                    ..
                }
            )),
            "{events:?}"
        );
        let (again, events) = ctx.scan(LLMQ_TEST, &tip, 1, false).unwrap();
        assert_eq!(again, [base]);
        assert!(
            matches!(
                events.last(),
                Some(TraceEvent::Scan {
                    cache: ScanCache::HitSatisfied,
                    ..
                })
            ),
            "{events:?}"
        );
        let (overload, events) = ctx.scan(LLMQ_TEST, &tip, 2, true).unwrap();
        assert_eq!(overload, [base]);
        assert!(matches!(
            events.last(),
            Some(TraceEvent::Scan {
                cache: ScanCache::Bypass,
                cache_written: false,
                ..
            })
        ));
        assert_eq!(ctx.scan(LLMQ_400_60, &tip, 1, false).unwrap_err().kind, ErrorKind::Argument);
        assert_eq!(
            ctx.scan(LLMQ_TEST, &tip, 1 << 17, false).unwrap_err().kind,
            ErrorKind::Argument
        );
    }
}

#[test]
fn select_picks_a_quorum_for_signing() {
    for oracle in Oracle::all() {
        let headers = chain(0, 72);
        let mut ctx = quorum_seeded(&oracle, 0, &headers);
        assert_eq!(ctx.select(LLMQ_TEST, &[1; 32], None, 8).unwrap().0, None);
        let base = hash_of(&headers[48]);
        ctx.restart().unwrap();
        ctx.set_mined(LLMQ_TEST, &commitment(LLMQ_TEST, &base, true), 60).unwrap();
        assert_eq!(ctx.select(LLMQ_TEST, &[1; 32], None, 8).unwrap().0, Some(base));
        assert_eq!(ctx.select(LLMQ_TEST, &[1; 32], Some(72), 8).unwrap().0, Some(base));
        assert_eq!(
            ctx.select(LLMQ_400_60, &[1; 32], None, 8).unwrap_err().kind,
            ErrorKind::Argument
        );
        assert_eq!(
            ctx.select(LLMQ_TEST, &[1; 32], Some(0), i32::MIN).unwrap_err().kind,
            ErrorKind::Argument
        );
        assert_eq!(
            ctx.select(LLMQ_TEST, &[1; 32], Some(-2), 0).unwrap_err().kind,
            ErrorKind::Argument
        );
        // The tip's height minus a negative offset above int: Core's subtraction would overflow.
        let err = ctx.select(LLMQ_TEST, &[1; 32], None, -i32::MAX).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{err}");
        assert!(err.message.contains("overflows"), "{err}");
    }
}

// ---- exclusivity ----

#[test]
fn a_context_excludes_sessions_contexts_and_network_selection() {
    for oracle in Oracle::all() {
        let ctx = oracle.llmq_context(&regtest()).unwrap();
        // The safe API refuses on the context's thread.
        assert_eq!(
            oracle.llmq_context(&Chain::regtest()).err().unwrap().kind,
            ErrorKind::SessionLive
        );
        assert_eq!(
            oracle.chainparams_json(&Chain::regtest()).unwrap_err().kind,
            ErrorKind::SessionLive
        );
        // And the shim refuses the raw calls.
        let api = &ffi::v24_0::API;
        let regtest = c"regtest";
        let mut handle = std::ptr::null_mut();
        assert_eq!(
            unsafe { (api.llmq_ctx_new)(regtest.as_ptr(), std::ptr::null(), 0, &mut handle) },
            ffi::ORACLE_E_ARG
        );
        assert_eq!(
            unsafe { (api.set_network)(regtest.as_ptr(), std::ptr::null(), 0) },
            ffi::ORACLE_E_ARG
        );
        let mut session = std::ptr::null_mut();
        let empty_fixture = [0u8];
        let status = unsafe {
            (api.peer_session_new)(
                regtest.as_ptr(),
                std::ptr::null(),
                0,
                0,
                1,
                0,
                empty_fixture.as_ptr(),
                empty_fixture.len(),
                &mut session,
            )
        };
        assert_eq!(status, ffi::ORACLE_E_ARG);
        drop(ctx);
        // Free gives the process back: the network can be selected again.
        oracle.chainparams_json(&Chain::regtest()).unwrap();
        // A context's chain and options are Core's to accept.
        assert_eq!(
            oracle.llmq_context(&Chain::new("nonet", &[])).err().unwrap().kind,
            ErrorKind::Network
        );
    }
}

// ---- fixtures ----

fn fixture(headers: &[[u8; 80]]) -> LlmqFixture {
    let [a, b, _] = kat_rows();
    LlmqFixture {
        chain: "regtest".into(),
        args: vec!["-dip3params=2:2".into()],
        headers: headers.iter().enumerate().map(|(h, header)| (h as i32, *header)).collect(),
        lists: headers
            .iter()
            .zip(0..)
            .map(|(h, height)| (hash_of(h), member_list_at(hash_of(h), height)))
            .collect(),
        coinbases: vec![(hash_of(&headers[1]), coinbase(), false)],
        snapshots: vec![a, b],
        mined: vec![(LLMQ_TEST, commitment(LLMQ_TEST, &hash_of(&headers[48]), true), 60)],
    }
}

/// A version-1 coinbase: the null prevout, scriptSig OP_1 OP_0, one OP_TRUE output of 0.
fn coinbase() -> Vec<u8> {
    let mut tx = vec![1, 0, 0, 0, 1];
    tx.extend([0; 32]);
    tx.extend([0xff; 4]);
    tx.extend([2, 0x51, 0x00]);
    tx.extend([0xff; 4]);
    tx.push(1);
    tx.extend(0u64.to_le_bytes());
    tx.extend([1, 0x51]);
    tx.extend([0; 4]);
    tx
}

#[test]
fn fixtures_round_trip_byte_for_byte_and_load() {
    let headers = chain(0, 72);
    let fixture = fixture(&headers);
    let bytes = fixture.encode();
    assert_eq!(LlmqFixture::decode(&bytes).as_ref(), Some(&fixture));
    assert_eq!(LlmqFixture::decode(&bytes).unwrap().encode(), bytes);
    // Not Core's canonical encoding, or bytes after it.
    let mut long_size = bytes.clone();
    long_size.splice(1..2, [0xfd, 7, 0]);
    assert_eq!(LlmqFixture::decode(&long_size), None);
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(LlmqFixture::decode(&trailing), None);
    // A size above Core's MAX_SIZE, which ReadCompactSize refuses.
    let mut oversized = vec![1, 0xfe];
    oversized.extend(0x0200_0001u32.to_le_bytes());
    assert_eq!(LlmqFixture::decode(&oversized), None);

    for oracle in Oracle::all() {
        let mut loaded = oracle.llmq_context(&regtest()).unwrap();
        loaded.load(&fixture).unwrap();
        let digest = loaded.rows_digest().unwrap();
        let scanned = loaded.scan(LLMQ_TEST, &hash_of(&headers[72]), 2, false).unwrap().0;
        drop(loaded);
        let mut seeded = quorum_seeded(&oracle, 0, &headers);
        let [a, b, _] = kat_rows();
        for (t, h, row) in [a, b] {
            seeded.set_snapshot(t, &h, &row).unwrap();
        }
        seeded.set_mined(LLMQ_TEST, &fixture.mined[0].1, 60).unwrap();
        assert_eq!(seeded.rows_digest().unwrap(), digest);
        assert_eq!(seeded.scan(LLMQ_TEST, &hash_of(&headers[72]), 2, false).unwrap().0, scanned);
        drop(seeded);
        // The fixture's chain must be the context's.
        let mut other =
            oracle.llmq_context(&Chain::new("regtest", &["-llmqtestparams=3:2"])).unwrap();
        assert_eq!(other.load(&fixture).unwrap_err().kind, ErrorKind::Argument);
        assert_eq!(other.load_raw(&trailing).unwrap_err().kind, ErrorKind::Trailing);
        let refused = other.load_raw(&oversized).unwrap_err();
        assert!(refused.message.contains("size too large"), "{refused:?}");
    }
}

/// The scan windows of the chain's LLMQ types, from Core's chain parameters.
fn llmq_windows(oracle: &Oracle, chain: &Chain) -> Vec<LlmqWindow> {
    let params: serde_json::Value =
        serde_json::from_str(&oracle.chainparams_json(chain).unwrap()).unwrap();
    let int = |q: &serde_json::Value, key: &str| q[key].as_i64().unwrap() as i32;
    params["consensus"]["llmqs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| LlmqWindow {
            llmq_type: q["type"].as_u64().unwrap() as u8,
            use_rotation: q["useRotation"].as_bool().unwrap(),
            dkg_interval: int(q, "dkgInterval"),
            dkg_mining_window_start: int(q, "dkgMiningWindowStart"),
            dkg_mining_window_end: int(q, "dkgMiningWindowEnd"),
            signing_active_quorum_count: int(q, "signingActiveQuorumCount"),
            keep_old_connections: int(q, "keepOldConnections"),
        })
        .collect()
}

/// `headers` from `first` with an LLMQ_TEST commitment for each base in `bases` at or above
/// it, mined `delay` blocks after its base.
fn mined_cycles(headers: &[[u8; 80]], first: i32, bases: &[i32], delay: i32) -> LlmqFixture {
    let held = first as usize..headers.len();
    LlmqFixture {
        headers: held.clone().map(|h| (h as i32, headers[h])).collect(),
        lists: held
            .map(|h| (hash_of(&headers[h]), member_list_at(hash_of(&headers[h]), h as i32)))
            .collect(),
        coinbases: vec![],
        snapshots: vec![],
        mined: bases
            .iter()
            .filter(|base| **base >= first)
            .map(|base| {
                let qc = commitment(LLMQ_TEST, &hash_of(&headers[*base as usize]), true);
                (LLMQ_TEST, qc, base + delay)
            })
            .collect(),
        ..fixture(headers)
    }
}

/// The builder's scan simulation is Core's scan, and every fixture it accepts scans as the
/// whole chain does: with commitments mined at the window start, mid-window (a start inside
/// the window that has not mined its cycle yet reads the cycles before it) and with a failed
/// DKG (no commitment, so older quorums sit a cycle deeper).
#[test]
fn the_builder_accepts_only_fixtures_that_scan_as_the_whole_chain() {
    let headers = chain(0, 144);
    let every: Vec<i32> = (24..=120).step_by(24).collect();
    let failed_72: Vec<i32> = every.iter().copied().filter(|b| *b != 72).collect();
    let layouts = [
        ("window start", &every[..], 10),
        ("mid window", &every[..], 15),
        ("failed DKG", &failed_72[..], 10),
    ];
    for oracle in Oracle::all() {
        let windows = llmq_windows(&oracle, &regtest());
        let window = *windows.iter().find(|w| w.llmq_type == LLMQ_TEST).unwrap();
        let scanned = |fixture: &LlmqFixture, demand: &ScanDemand| {
            let mut ctx = oracle.llmq_context(&regtest()).unwrap();
            ctx.load(fixture).unwrap();
            let start = hash_of(&headers[demand.start_height as usize]);
            ctx.scan(LLMQ_TEST, &start, demand.count, false).ok().map(|(bases, _)| bases)
        };
        for (layout, bases, delay) in layouts {
            // 144 is before cycle 144's window; 132 is inside cycle 120's.
            for start_height in [144, 132] {
                let demand = ScanDemand {
                    llmq_type: LLMQ_TEST,
                    start_height,
                    count: window.keep_old_connections as u32,
                };
                let case = format!("{layout}, scan from {start_height}");
                let whole = mined_cycles(&headers, 0, bases, delay);
                let full = scanned(&whole, &demand).unwrap();
                assert_eq!(full.len(), demand.count as usize, "{case}");
                let simulated = whole.scan_base_heights(&window, &demand).unwrap();
                let simulated: Vec<[u8; 32]> =
                    simulated.iter().map(|h| hash_of(&headers[*h as usize])).collect();
                assert_eq!(simulated, full, "{case}");
                // Acceptance only narrows as the first header rises: the latest accepted
                // fixture holds the least of the chain.
                let accepted: Vec<i32> = (0..=start_height)
                    .filter(|first| {
                        mined_cycles(&headers, *first, bases, delay)
                            .check_scan_coverage(&windows, &[demand])
                            .is_ok()
                    })
                    .collect();
                let latest = *accepted.last().unwrap();
                assert_eq!(accepted, (0..=latest).collect::<Vec<_>>(), "{case}");
                let fixture = mined_cycles(&headers, latest, bases, delay);
                assert_eq!(scanned(&fixture, &demand).as_ref(), Some(&full), "{case}, {latest}");
                if (layout, start_height) == ("mid window", 132) {
                    // A depth-only floor (one quorum per cycle from the window's cycle, 120)
                    // admits a first header at 120 - 2 * 24 - 8; there Core finds two quorums.
                    let late = mined_cycles(&headers, 64, bases, delay);
                    let err = late.check_scan_coverage(&windows, &[demand]).unwrap_err();
                    assert!(err.contains("builds 2 of 3"), "{err}");
                    assert_eq!(scanned(&late, &demand).unwrap(), full[..2]);
                }
            }
        }

        // Below genesis Core scans nothing; an unknown type is refused.
        let early = ScanDemand {
            llmq_type: LLMQ_TEST,
            start_height: 5,
            count: 1,
        };
        assert_eq!(window.store_height(5), None);
        assert_eq!(window.store_height(30), Some(18));
        let late = mined_cycles(&headers, 20, &every, 10);
        late.check_scan_coverage(&windows, &[early]).unwrap();
        let unknown = ScanDemand {
            llmq_type: UNKNOWN_TYPE,
            ..early
        };
        assert!(late.check_scan_coverage(&windows, &[unknown]).is_err());
    }
}

/// A rotated quorum index that ran out inside the fixture: Core's walk takes, round by round,
/// each index's next older commitment, so one of index 0 below the first header would come
/// before index 1's later rounds. The fixture is refused though it builds enough quorums.
#[test]
fn the_builder_refuses_a_rotated_index_that_runs_out_above_its_first_header() {
    let headers = chain(0, 300);
    for oracle in Oracle::all() {
        let windows = llmq_windows(&oracle, &regtest());
        let window = *windows.iter().find(|w| w.llmq_type == LLMQ_TEST_DIP0024).unwrap();
        let (c, n) = (window.dkg_interval, window.signing_active_quorum_count);
        assert_eq!(n, 2);
        // Index 1 mines every cycle from 5 to 10; index 0 only cycles 4 and 10 (failed DKGs).
        let quorum = |cycle: i32, index: i32| {
            let base = cycle * c + index;
            let mut qc = 4u16.to_le_bytes().to_vec(); // indexed, basic BLS
            qc.push(LLMQ_TEST_DIP0024);
            qc.extend(hash_of(&headers[base as usize]));
            qc.extend((index as i16).to_le_bytes());
            (LLMQ_TEST_DIP0024, qc, base + window.dkg_mining_window_start)
        };
        let mined: Vec<_> = [(4, 0), (10, 0)]
            .into_iter()
            .chain((5..=10).map(|cycle| (cycle, 1)))
            .map(|(cycle, index)| quorum(cycle, index))
            .collect();
        let from = |first: i32| LlmqFixture {
            headers: (first..=300).map(|h| (h, headers[h as usize])).collect(),
            mined: mined
                .iter()
                .filter(|(_, qc, _)| {
                    (first..=300).any(|h| qc[3..35] == hash_of(&headers[h as usize]))
                })
                .cloned()
                .collect(),
            ..Default::default()
        };
        let demand = ScanDemand {
            llmq_type: LLMQ_TEST_DIP0024,
            start_height: 10 * c + window.dkg_mining_window_end + 1,
            count: window.keep_old_connections as u32,
        };
        let whole = from(0).scan_base_heights(&window, &demand).unwrap();
        assert_eq!(whole, [10 * c, 10 * c + 1, 4 * c, 9 * c + 1]);
        from(0).check_scan_coverage(&windows, &[demand]).unwrap();
        // From cycle 5's work block, index 0 holds cycle 10 only: the fixture builds four
        // quorums whose member reads it covers, but not Core's four.
        let late = from(5 * c - 8);
        let scanned = late.scan_base_heights(&window, &demand).unwrap();
        assert_eq!(scanned.len(), whole.len());
        assert_ne!(scanned, whole);
        let err = late.check_scan_coverage(&windows, &[demand]).unwrap_err();
        assert!(err.contains("runs out"), "{err}");
        // A scan of two takes round 0 only: index 0 running out later changes nothing, so the
        // same fixture is accepted for keepOldConnections 2 (its member reads are covered).
        let narrow = LlmqWindow {
            keep_old_connections: 2,
            ..window
        };
        let two = ScanDemand {
            count: 2,
            ..demand
        };
        assert_eq!(late.scan_base_heights(&narrow, &two).unwrap(), whole[..2]);
        late.check_scan_coverage(&[narrow], &[two]).unwrap();
        // A non-indexed version of a rotated type is under the key the scan does not read.
        let mut odd = from(0);
        odd.mined[0].1[..2].copy_from_slice(&3u16.to_le_bytes());
        odd.mined[0].1.truncate(35);
        assert!(!odd.scan_base_heights(&window, &demand).unwrap().contains(&(4 * c)));
    }
}

// ---- the regtest fixture (scripts/gen-llmq-regtest.py) ----

/// The fixture and Core's answers on the chain it holds (tests/data/llmq-regtest-v24.0.*).
fn regtest_fixture() -> (Vec<u8>, serde_json::Value) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/llmq-regtest-v24.0");
    let bytes = read_gz(&format!("{dir}.fixture.gz"));
    let expected: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{dir}.json")).unwrap()).unwrap();
    assert_eq!(
        hex(&sha256::Hash::hash(&bytes).to_byte_array()),
        expected["provenance"]["fixture_sha256"].as_str().unwrap()
    );
    (bytes, expected)
}

fn read_gz(path: &str) -> Vec<u8> {
    use std::io::Read;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

fn json_u8(v: &serde_json::Value) -> u8 {
    v.as_u64().unwrap() as u8
}

/// A JSON array of RPC-order hashes.
fn rpc_hashes(v: &serde_json::Value) -> Vec<[u8; 32]> {
    v.as_array().unwrap().iter().map(|h| common::rpc_hash(h.as_str().unwrap())).collect()
}

/// llmq_regtest_quorum_info: a context loaded with a chain release dashd built connects its
/// blocks as a node does (PreComputeQuorumMembers on each), then gives every mined quorum the
/// members `quorum info` lists, in order; builds the snapshot rows `quorum rotationinfo`
/// reports; and scans from the tip what `quorum list` lists.
#[test]
fn the_regtest_fixture_reproduces_quorum_info_rotationinfo_and_list() {
    let (bytes, expected) = regtest_fixture();
    let fixture = LlmqFixture::decode(&bytes).expect("a canonical fixture");
    assert_eq!(fixture.encode(), bytes);
    let tip = expected["tip"].as_i64().unwrap() as i32;
    let hash_at = |height: i64| hash_of(&fixture.headers[height as usize].1);
    let args: Vec<&str> = fixture.args.iter().map(String::as_str).collect();
    let chain = Chain::new(&fixture.chain, &args);
    // `quorum list <count>` at the tip: the demand and the bases it lists.
    let scans: Vec<(ScanDemand, Vec<[u8; 32]>)> = expected["scans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let demand = ScanDemand {
                llmq_type: json_u8(&s["type"]),
                start_height: tip,
                count: s["count"].as_u64().unwrap() as u32,
            };
            (demand, rpc_hashes(&s["bases"]))
        })
        .collect();
    let demands: Vec<ScanDemand> = scans.iter().map(|(d, _)| *d).collect();
    for oracle in Oracle::all() {
        let windows = llmq_windows(&oracle, &chain);
        fixture.check_scan_coverage(&windows, &demands).unwrap();

        // The fixture holds no snapshot rows, which a rotated quorum's members read from its
        // prior cycles: a node stored them on connecting those cycles' bases, before the blocks
        // mining the rotated commitments. Seed the rest, connect the blocks, then restart and
        // record the rotated commitments over the rows the connected blocks stored.
        let (rotated, unrotated): (Vec<_>, Vec<_>) = fixture
            .mined
            .iter()
            .cloned()
            .partition(|(_, qc, _)| u16::from_le_bytes([qc[0], qc[1]]).is_multiple_of(2));
        assert!(!rotated.is_empty() && !unrotated.is_empty());
        let mut ctx = oracle.llmq_context(&chain).unwrap();
        ctx.load(&LlmqFixture {
            mined: unrotated,
            ..fixture.clone()
        })
        .unwrap();
        let digest = ctx.rows_digest().unwrap();
        for (llmq_type, qc, height) in &rotated {
            let err = ctx.set_mined(*llmq_type, qc, *height).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Context, "{err}");
            assert!(err.to_string().contains("may store on connecting it"), "{err}");
        }
        // The rows the refused checks' member computations stored were rolled back.
        assert_eq!(ctx.rows_digest().unwrap(), digest);
        // V19 is active from height 1: the basic BLS scheme.
        let mut rows = BTreeMap::new();
        for height in 1..=i64::from(tip) {
            for event in ctx.precompute(&hash_at(height), false, Precompute::Connect).unwrap() {
                if let TraceEvent::Row {
                    key,
                    snapshot,
                } = event
                {
                    assert!(rows.insert(key.to_vec(), snapshot).is_none(), "a row stored twice");
                }
            }
        }
        ctx.restart().unwrap();
        for (llmq_type, qc, height) in &rotated {
            ctx.set_mined(*llmq_type, qc, *height).unwrap();
        }
        for row in expected["rotation_snapshots"].as_array().unwrap() {
            let key = snapshot_key(
                json_u8(&row["type"]),
                &hash_at(row["cycle_height"].as_i64().unwrap()),
            );
            assert_eq!(
                rows.get(&key).map(|s| hex(s)).as_deref(),
                row["snapshot"].as_str(),
                "{row}"
            );
        }
        let quorums = expected["quorums"].as_array().unwrap();
        assert!(quorums.iter().any(|q| q["base_height"].as_i64() < Some(300)), "a pre-V20 quorum");
        for q in quorums {
            let base = hash_at(q["base_height"].as_i64().unwrap());
            assert_eq!(base, common::rpc_hash(q["quorum_hash"].as_str().unwrap()));
            let (members, _) = ctx.members(json_u8(&q["type"]), &base, false, false).unwrap();
            let got: Vec<[u8; 32]> = members.iter().map(|m| m.pro_tx_hash).collect();
            assert_eq!(got, rpc_hashes(&q["members"]), "{q}");
        }
        for (demand, want) in &scans {
            let window = windows.iter().find(|w| w.llmq_type == demand.llmq_type).unwrap();
            let simulated: Vec<[u8; 32]> = fixture
                .scan_base_heights(window, demand)
                .unwrap()
                .iter()
                .take(demand.count as usize)
                .map(|h| hash_at((*h).into()))
                .collect();
            assert_eq!(&simulated, want, "simulated {demand:?}");
            let (bases, _) =
                ctx.scan(demand.llmq_type, &hash_at(tip.into()), demand.count, false).unwrap();
            assert_eq!(&bases, want, "{demand:?}");
        }
        // The rows the connected blocks stored are all of the context's rows.
        assert_eq!(ctx.rows_digest().unwrap(), keyed_rows_digest(&rows));
    }
}

// ---- mainnet and testnet samples (scripts/gen-llmq-samples.py) ----

/// The signers bitset of a `CFinalCommitment` (network serialization).
fn commitment_signers(qc: &[u8]) -> Vec<bool> {
    let version = u16::from_le_bytes([qc[0], qc[1]]);
    // nVersion, llmqType, quorumHash, then quorumIndex in the indexed versions (2 and 4).
    let mut at = 2
        + 1
        + 32
        + if version.is_multiple_of(2) {
            2
        } else {
            0
        };
    let n = match qc[at] {
        0xfd => {
            at += 3;
            usize::from(u16::from_le_bytes([qc[at - 2], qc[at - 1]]))
        }
        n => {
            at += 1;
            usize::from(n)
        }
    };
    (0..n).map(|i| qc[at + i / 8] & (1 << (i % 8)) != 0).collect()
}

fn llmq_samples() -> serde_json::Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/llmq-samples.json.gz");
    serde_json::from_slice(&read_gz(path)).unwrap()
}

/// A sample's chain, first height, headers and fixture (its list block's list and coinbase,
/// its commitments).
fn sample_fixture(sample: &serde_json::Value) -> (Chain, i32, Vec<[u8; 80]>, LlmqFixture) {
    let mainnet = sample["network"] == "mainnet";
    let first = sample["first_height"].as_i64().unwrap() as i32;
    let headers = common::headers(sample["headers"].as_str().unwrap());
    let list_block =
        hash_of(&headers[(sample["list_height"].as_i64().unwrap() - i64::from(first)) as usize]);
    let fixture = LlmqFixture {
        chain: if mainnet {
            "main"
        } else {
            "test"
        }
        .into(),
        args: vec![],
        headers: headers.iter().enumerate().map(|(i, h)| (first + i as i32, *h)).collect(),
        lists: vec![(list_block, unhex(sample["list"].as_str().unwrap()))],
        coinbases: vec![(list_block, unhex(sample["coinbase"].as_str().unwrap()), true)],
        snapshots: vec![],
        mined: sample["quorums"]
            .as_array()
            .unwrap()
            .iter()
            .map(|q| {
                let qc = unhex(q["commitment"].as_str().unwrap());
                (json_u8(&q["type"]), qc, q["mined_height"].as_i64().unwrap() as i32)
            })
            .collect(),
    };
    (
        if mainnet {
            Chain::main()
        } else {
            Chain::test()
        },
        first,
        headers,
        fixture,
    )
}

/// llmq_mainnet_testnet_membersig: in a context holding only the list and coinbase a node
/// reads, Core computes members with which each mined commitment verifies, signatures
/// included. membersSig is a secure aggregate of the signers' keys: it rejects another set of
/// signers (a signer swapped with a non-signer, or replaced by a masternode outside the
/// quorum), though not a reordering among signers.
#[test]
fn sampled_commitments_verify_with_the_members_a_context_computes() {
    let samples = llmq_samples();
    let samples = samples["samples"].as_array().unwrap();
    assert!(samples.iter().any(|s| s["network"] == "mainnet"));
    let testnet_span: Vec<i64> = samples
        .iter()
        .filter(|s| s["network"] == "testnet")
        .flat_map(|s| {
            s["quorums"].as_array().unwrap().iter().map(|q| q["base_height"].as_i64().unwrap())
        })
        .collect();
    assert!(testnet_span.iter().max().unwrap() - testnet_span.iter().min().unwrap() >= 200);
    for oracle in Oracle::all() {
        let v19 = |chain: &Chain| {
            let params: serde_json::Value =
                serde_json::from_str(&oracle.chainparams_json(chain).unwrap()).unwrap();
            params["consensus"]["V19Height"].as_i64().unwrap()
        };
        // Every quorum's members, as the context computes them.
        let computed: Vec<Vec<Vec<Member>>> = samples
            .iter()
            .map(|sample| {
                let (chain, first, headers, fixture) = sample_fixture(sample);
                let v19 = v19(&chain);
                let mut ctx = oracle.llmq_context(&chain).unwrap();
                ctx.load(&fixture).unwrap();
                let quorums = sample["quorums"].as_array().unwrap();
                quorums
                    .iter()
                    .map(|q| {
                        let base = q["base_height"].as_i64().unwrap();
                        let block = hash_of(&headers[(base - i64::from(first)) as usize]);
                        ctx.members(json_u8(&q["type"]), &block, base < v19, false).unwrap().0
                    })
                    .collect()
            })
            .collect();
        for (sample, members_of) in samples.iter().zip(&computed) {
            let (chain, first, headers, _) = sample_fixture(sample);
            let v19 = v19(&chain);
            let outsiders: Vec<&Member> = samples
                .iter()
                .zip(&computed)
                .filter(|(other, _)| other["network"] == sample["network"])
                .flat_map(|(_, m)| m.iter().flatten())
                .collect();
            for (q, members) in sample["quorums"].as_array().unwrap().iter().zip(members_of) {
                let what = format!("{} {q}", oracle.pin());
                let what = &what[..what.len().min(200)];
                let qc = unhex(q["commitment"].as_str().unwrap());
                let base = q["base_height"].as_i64().unwrap();
                let context = |members: Vec<Vec<u8>>| {
                    Context::new(
                        first,
                        headers.clone(),
                        q["mined_height"].as_i64().unwrap() < v19,
                        vec![(DEPLOYMENT_V24, ThresholdState::Defined)],
                        0,
                        Checkpoints::Enabled {
                            best_header_height: first + headers.len() as i32 - 1,
                        },
                        CoinsView::Unchecked(vec![]),
                        MnList::Absent,
                        vec![QuorumMembers {
                            llmq_type: json_u8(&q["type"]),
                            quorum_hash: hash_of(&headers[(base - i64::from(first)) as usize]),
                            members,
                        }],
                    )
                };
                let dmns: Vec<Vec<u8>> = members.iter().map(|m| m.dmn.clone()).collect();
                let verdict =
                    oracle.verify_commitment(&chain, &qc, &context(dmns.clone()), true).unwrap();
                assert!(verdict.is_accept(), "{what}: {verdict:?}");

                let signers = commitment_signers(&qc);
                let signer = signers.iter().position(|s| *s).expect("a signer");
                let mut wrong = Vec::new();
                if let Some(j) = signers.iter().position(|s| !*s).filter(|j| *j < dmns.len()) {
                    let mut swapped = dmns.clone();
                    swapped.swap(signer, j);
                    wrong.push(("a signer swapped with a non-signer", swapped));
                }
                let outsider = outsiders
                    .iter()
                    .find(|o| members.iter().all(|m| m.pro_tx_hash != o.pro_tx_hash))
                    .expect("a masternode outside the quorum");
                let mut replaced = dmns.clone();
                replaced[signer] = outsider.dmn.clone();
                wrong.push(("a signer replaced by a non-member", replaced));
                for (how, members) in wrong {
                    let verdict =
                        oracle.verify_commitment(&chain, &qc, &context(members), true).unwrap();
                    assert!(matches!(verdict, Verdict::Reject(_)), "{what}: accepted {how}");
                }
            }
        }
    }
}

// ---- the lock capture chain (O2a-LQ3, ABI 26) ----

const LQ3: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/lq3-regtest-v24.0");

/// The capture chain's fixture, checked against its recorded digest.
fn lq3_fixture() -> LlmqFixture {
    let bytes = read_gz(&format!("{LQ3}.fixture.gz"));
    let expected: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{LQ3}.json")).unwrap()).unwrap();
    assert_eq!(
        hex(&sha256::Hash::hash(&bytes).to_byte_array()),
        expected["provenance"]["fixture_sha256"].as_str().unwrap()
    );
    LlmqFixture::decode(&bytes).unwrap()
}

fn lq3_context(oracle: &Oracle, fixture: &LlmqFixture) -> LlmqContext {
    let args: Vec<&str> = fixture.args.iter().map(String::as_str).collect();
    let mut ctx = oracle.llmq_context(&Chain::new(&fixture.chain, &args)).unwrap();
    ctx.load(fixture).unwrap();
    ctx
}

/// The captured messages: (command, payload, inventory hash in Core's byte order).
fn lq3_messages() -> Vec<(String, Vec<u8>, [u8; 32])> {
    let captures: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{LQ3}.captures.json")).unwrap())
            .unwrap();
    captures["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["command"].as_str().unwrap().to_string(),
                unhex(m["payload"].as_str().unwrap()),
                common::rpc_hash(m["inv_hash"].as_str().unwrap()),
            )
        })
        .collect()
}

/// oracle_llmq_verify_*: on the chain that relayed them, Core accepts every captured message in
/// the basic scheme (one pass for each; an InstantSend lock by its cycle's quorum), names its
/// inventory hash, and accepts every mined commitment.
#[test]
fn every_captured_message_verifies_on_its_chain() {
    let oracle = Oracle::new(dash_oracle_sys::Pin::V24_0);
    let fixture = lq3_fixture();
    let mut ctx = lq3_context(&oracle, &fixture);
    let bases: Vec<[u8; 32]> =
        fixture.mined.iter().map(|(_, qc, _)| qc[3..35].try_into().unwrap()).collect();
    for (command, payload, inv) in lq3_messages() {
        let (verdict, _) = match command.as_str() {
            "clsig" => ctx.verify_chainlock(&payload, false),
            "isdlock" => ctx.verify_islock(&payload, false),
            "qsigrec" => ctx.verify_qsigrec(&payload, false),
            other => panic!("{other}"),
        }
        .unwrap();
        assert_eq!(verdict.status, LockStatus::Checked, "{command}");
        assert_eq!(verdict.passes.len(), 1, "{command}: {verdict:?}");
        assert_eq!(verdict.passes[0].status, PassStatus::Valid, "{command}");
        assert!(bases.contains(&verdict.passes[0].quorum.unwrap()), "{command}");
        assert_eq!(verdict.hash, inv, "{command}");
        if command == "isdlock" {
            // Non-rotated: the cycle hash names the quorum.
            assert_eq!(
                &verdict.passes[0].quorum.unwrap()[..],
                &payload[payload.len() - 128..payload.len() - 96]
            );
        }
    }
    for (_, qc, _) in &fixture.mined {
        assert_eq!(ctx.verify_mined(qc, false).unwrap().0, MinedVerdict::Accepted);
    }
}

/// The verdict entries' refusals and failures: Core's deserializer throwing or leaving bytes, a
/// ChainLock height whose start height Core's int subtraction overflows, a null or unknown-base
/// mined commitment; a cycle hash the context lacks on a context below genesis is missing
/// context (not poisoning), on one from genesis Core's unknown cycle; a second pass only when
/// the first did not verify; a stateful call (seeding closes).
#[test]
fn verdict_entries_refuse_and_fail_as_documented() {
    let oracle = Oracle::new(dash_oracle_sys::Pin::V24_0);
    let fixture = lq3_fixture();
    let messages = lq3_messages();
    let of = |c: &str| messages.iter().find(|(m, ..)| m == c).unwrap().1.clone();
    let (clsig, isdlock, qsigrec) = (of("clsig"), of("isdlock"), of("qsigrec"));
    // The lock's cycle hash changed: a block no context holds.
    let mut cycle = isdlock.clone();
    let at = cycle.len() - 128;
    cycle[at] ^= 1;
    {
        let mut ctx = lq3_context(&oracle, &fixture);
        assert_eq!(kind(ctx.verify_qsigrec(&qsigrec[..192], false)), ErrorKind::Decode);
        assert_eq!(
            kind(ctx.verify_qsigrec(&[&qsigrec[..], &[0]].concat(), false)),
            ErrorKind::Trailing
        );
        assert_eq!(
            kind(ctx.verify_islock(&isdlock[..isdlock.len() - 1], false)),
            ErrorKind::Decode
        );
        // A ChainLock signature canonical in neither scheme: CBLSWrapper's read throws.
        let mut unreadable = clsig.clone();
        unreadable[36..].fill(0xff);
        assert_eq!(kind(ctx.verify_chainlock(&unreadable, false)), ErrorKind::Decode);
        for (height, refused) in [(i32::MIN + 8, false), (i32::MIN + 7, true)] {
            let mut c = clsig.clone();
            c[..4].copy_from_slice(&height.to_le_bytes());
            let got = ctx.verify_chainlock(&c, false);
            assert_eq!(got.is_err(), refused, "{height}");
            if let Ok((v, _)) = got {
                assert_eq!(v.passes[0].status, PassStatus::NoQuorum);
            }
        }
        let (qc_type, qc, _) = &fixture.mined[0];
        let mined_kind =
            |ctx: &mut LlmqContext, b: &[u8]| ctx.verify_mined(b, false).unwrap_err().kind;
        assert_eq!(
            mined_kind(&mut ctx, &commitment(*qc_type, &qc[3..35].try_into().unwrap(), false)),
            ErrorKind::Argument
        );
        let mut unknown = qc.clone();
        unknown[3] ^= 1;
        assert_eq!(mined_kind(&mut ctx, &unknown), ErrorKind::Argument);
        // An unknown cycle on a context from genesis: Core's verdict.
        assert_eq!(ctx.verify_islock(&cycle, false).unwrap().0.status, LockStatus::UnknownCycle);
        // Seeding closed: the verdicts are stateful calls.
        let (h, header) = fixture.headers.last().unwrap();
        assert_eq!(ctx.add_header(header, h + 1).unwrap_err().kind, ErrorKind::Argument);
        // Another message's signature fails both passes (the second at the old active set).
        let mut wrong = isdlock.clone();
        let n = wrong.len();
        wrong[n - 96..].copy_from_slice(&qsigrec[97..]);
        let (v, _) = ctx.verify_islock(&wrong, false).unwrap();
        assert_eq!(v.status, LockStatus::Checked);
        assert_eq!(v.passes.iter().map(|p| p.status).collect::<Vec<_>>(), [PassStatus::Invalid; 2]);
    }
    // Below genesis: the headers from 100 only.
    let late = LlmqFixture {
        headers: fixture.headers.iter().filter(|(h, _)| *h >= 100).cloned().collect(),
        lists: vec![],
        coinbases: vec![],
        mined: vec![],
        ..fixture.clone()
    };
    let mut ctx = lq3_context(&oracle, &late);
    assert_eq!(kind(ctx.verify_islock(&cycle, false)), ErrorKind::Context);
    // Not poisoned: the same call is refused again, not as poisoned.
    assert_eq!(kind(ctx.verify_islock(&cycle, false)), ErrorKind::Context);
    // A null cycle hash never resolves: Core's verdict on any context.
    let mut null = isdlock.clone();
    null[at..at + 32].fill(0);
    assert_eq!(ctx.verify_islock(&null, false).unwrap().0.status, LockStatus::UnknownCycle);
}

fn kind(
    got: dash_oracle_sys::Result<(dash_oracle_sys::LockVerdict, Vec<TraceEvent>)>,
) -> ErrorKind {
    got.unwrap_err().kind
}
