//! Smoke tests of the contextual entry points (ABI 6) on every linked pin.
//!
//! Expected verdicts come from Dash Core: the mainnet samples were accepted by Core as
//! part of the chain (`tests/data/contextual-mainnet.json`, with `protx listdiff` and
//! fees from the reference dashd's RPC), and the rejections are Core's own
//! `trivially_invalid.json` vectors (`tests/data/PROVENANCE.md`).

mod common;

use common::{Coverage, coins, empty_list, extra_payload, headers, hex, to_hex, trivial_vectors};
use dash_oracle_sys::{
    BlsKind, BlsMode, Chain, Checkpoints, CoinsView, Context, DEPLOYMENT_V24, DmlEffects, DmlStage,
    ErrorKind, ListEncoding, MnList, Oracle, QuorumMembers, ResultCode, SessionConfig,
    ThresholdState, Verdict,
};
use serde_json::Value;

fn fixture() -> Value {
    common::load_data("contextual-mainnet.json")
}

/// The height of the last of `headers` from `first_height`.
fn tip_of(first_height: i32, headers: &[[u8; 80]]) -> i32 {
    first_height + headers.len() as i32 - 1
}

/// A node that knows no header past the tip: no checkpoint above it.
fn checkpoints_to_tip(first_height: i32, headers: &[[u8; 80]]) -> Checkpoints {
    Checkpoints::Enabled {
        best_header_height: tip_of(first_height, headers),
    }
}

/// The context of a sample at its parent: `first_height`, `headers`, `adjusted_time`
/// and `prev_list`, before V19 (legacy BLS scheme) with the given coins.
fn sample_context(sample: &Value, coins: CoinsView) -> Context {
    let first_height = sample["first_height"].as_i64().unwrap() as i32;
    let headers = headers(sample["headers"].as_str().unwrap());
    Context::new(
        first_height,
        headers.clone(),
        true,
        MAINNET_DEPLOYMENTS.to_vec(),
        sample["adjusted_time"].as_i64().unwrap() + 60,
        checkpoints_to_tip(first_height, &headers),
        coins,
        MnList::Supplied(hex(sample["prev_list"].as_str().unwrap())),
        vec![],
    )
}

/// The context of the sampled post-DIP0003 block: its parent's chain and list.
fn dip3_context(sample: &Value) -> Context {
    sample_context(sample, CoinsView::AtTip(coins(&sample["coins"])))
}

fn supplied(list: &MnList) -> &[u8] {
    match list {
        MnList::Supplied(bytes) => bytes,
        MnList::Absent => panic!("no list"),
    }
}

/// Mainnet's version-bits deployments in these samples: V24 not started (DEFINED);
/// Unlisted deployments are DEFINED.
const MAINNET_DEPLOYMENTS: [(u8, ThresholdState); 1] = [(DEPLOYMENT_V24, ThresholdState::Defined)];

/// The headers of a commitment sample from `first_height`, before V19, with no coins,
/// no list and the given quorum members.
fn commitment_context(
    first_height: i32,
    headers: Vec<[u8; 80]>,
    quorums: Vec<QuorumMembers>,
) -> Context {
    let checkpoints = checkpoints_to_tip(first_height, &headers);
    Context::new(
        first_height,
        headers,
        true,
        MAINNET_DEPLOYMENTS.to_vec(),
        0,
        checkpoints,
        CoinsView::Unchecked(vec![]),
        MnList::Absent,
        quorums,
    )
}

fn assert_accept(verdict: Verdict, what: &str) {
    assert!(verdict.is_accept(), "{what}: {verdict:?}");
}

/// `protx listdiff` without what the oracle cannot render (see examples/dml_listdiff.rs).
fn normalize_listdiff(mut diff: Value) -> Value {
    let obj = diff.as_object_mut().unwrap();
    obj.remove("baseHeight");
    obj.remove("blockHeight");
    for mn in obj["addedMNs"].as_array_mut().unwrap() {
        let mn = mn.as_object_mut().unwrap();
        mn.remove("collateralAddress");
        let state = mn["state"].as_object_mut().unwrap();
        if state.get("service") == state.get("addresses").and_then(|a| a["core_p2p"].get(0)) {
            state.remove("service");
        }
    }
    obj["addedMNs"]
        .as_array_mut()
        .unwrap()
        .sort_by_key(|mn| mn["proTxHash"].as_str().unwrap().to_string());
    diff
}

#[test]
fn dml_apply_reproduces_listdiff() {
    let sample = &fixture()["dip3_block"];
    let block = hex(sample["block"].as_str().unwrap());
    let ctx = dip3_context(sample);
    for oracle in Oracle::all() {
        let result = oracle.dml_apply(&Chain::main(), &block, &ctx).unwrap();
        assert_accept(result.verdict, "dml_apply");
        assert_eq!(result.payee, None, "{}: the parent's list is empty", oracle.pin());
        let new_list = result.new_list.unwrap();
        let json =
            oracle.dml_listdiff_json(&Chain::main(), supplied(&ctx.mn_list), &new_list).unwrap();
        let ours = normalize_listdiff(serde_json::from_str(&json).unwrap());
        let expected = normalize_listdiff(sample["expected_listdiff"].clone());
        assert_eq!(ours, expected, "{}", oracle.pin());
        assert_eq!(ours["addedMNs"].as_array().unwrap().len(), 14);
    }
}

/// The block with `extra` appended to its transactions (fewer than 253 before and after).
fn with_extra_tx(block: &[u8], extra: &[u8]) -> Vec<u8> {
    let mut out = block.to_vec();
    assert!(out[80] < 0xfc);
    out[80] += 1;
    out.extend_from_slice(extra);
    out
}

/// Where a special transaction's extra payload starts: after its inputs, outputs, nLockTime
/// and the payload's CompactSize length.
fn payload_start(raw: &[u8]) -> usize {
    let mut at = 4;
    let compact = |at: &mut usize| {
        let (n, len) = match raw[*at] {
            0xfd => (u16::from_le_bytes([raw[*at + 1], raw[*at + 2]]) as usize, 3),
            n @ ..0xfd => (n as usize, 1),
            _ => panic!("long CompactSize"),
        };
        *at += len;
        n
    };
    for _ in 0..compact(&mut at) {
        at += 36;
        at += compact(&mut at) + 4;
    }
    for _ in 0..compact(&mut at) {
        at += 8;
        at += compact(&mut at);
    }
    at += 4;
    let len = compact(&mut at);
    assert_eq!(at + len, raw.len());
    at
}

fn assert_dml_reject(got: &dash_oracle_sys::DmlBlock, stage: DmlStage, reason: &str, what: &str) {
    match &got.verdict {
        Verdict::Reject(r) => {
            assert_eq!((got.stage, r.reason.as_str()), (Some(stage), reason), "{what}")
        }
        Verdict::Accept => panic!("{what}: accepted"),
    }
    assert_eq!(got.list, None, "{what}");
}

/// `oracle_dml_block` on mainnet's first DIP3 block: the list it builds is the one
/// `protx listdiff` reports, with or without `ProcessBlock()`, and equals `dml_apply`'s apart
/// from the block hash. `BuildNewListFromBlock()` lets a repeated ProRegTx replace its own
/// masternode (CheckBlock's duplicate-txid rule keeps it out of real blocks) and rejects a
/// second ProRegTx for the first one's service; the check loop rejects a ProRegTx without its
/// collateral.
#[test]
fn dml_block_reproduces_listdiff() {
    let sample = &fixture()["dip3_block"];
    let block = hex(sample["block"].as_str().unwrap());
    let ctx = dip3_context(sample);
    let regs: Vec<Vec<u8>> = sample["special_txs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["type"] == 1)
        .map(|t| hex(t["hex"].as_str().unwrap()))
        .collect();
    // ProRegTx version 1: version, type, mode, collateral, then the 18-byte service.
    let service = |raw: &[u8]| payload_start(raw) + 2 + 2 + 2 + 36;
    let mut same_service = regs[1].clone();
    let (from, to) = (service(&regs[0]), service(&regs[1]));
    same_service[to..to + 18].copy_from_slice(&regs[0][from..from + 18]);
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let applied = oracle.dml_apply(&Chain::main(), &block, &ctx).unwrap().canonical.unwrap();
        let mut lists = vec![];
        for (check_sigs, just_check) in [(true, false), (true, true), (false, false)] {
            let got =
                oracle.dml_block(&Chain::main(), &block, &ctx, check_sigs, just_check).unwrap();
            assert_accept(got.verdict, "dml_block");
            assert_eq!((got.stage, got.payee), (None, None), "{pin}: the parent's list is empty");
            lists.push(got.list.unwrap());
        }
        assert!(lists.windows(2).all(|w| w[0] == w[1]), "{pin}");
        let (canonical, hash) = &lists[0];
        assert_ne!(canonical[..32], [0; 32], "{pin}: ProcessBlock's block hash");
        assert_eq!(canonical[32..], applied[32..], "{pin}");
        let read =
            oracle.mnlist_canonical(&Chain::main(), ListEncoding::Canonical, canonical).unwrap();
        assert_eq!(&read.hash, hash, "{pin}");
        let json =
            oracle.dml_listdiff_json(&Chain::main(), supplied(&ctx.mn_list), &read.disk).unwrap();
        let ours = normalize_listdiff(serde_json::from_str(&json).unwrap());
        assert_eq!(ours, normalize_listdiff(sample["expected_listdiff"].clone()), "{pin}");

        let repeated = with_extra_tx(&block, &regs[0]);
        let got = oracle.dml_block(&Chain::main(), &repeated, &ctx, true, false).unwrap();
        assert_accept(got.verdict, "repeated ProRegTx");
        let (replaced, _) = got.list.unwrap();
        assert_eq!(replaced[32 + 4..32 + 8], 15u32.to_le_bytes(), "{pin}: registered again");
        assert_ne!(replaced[32..], canonical[32..], "{pin}");

        let got = oracle
            .dml_block(&Chain::main(), &with_extra_tx(&block, &same_service), &ctx, false, false)
            .unwrap();
        let what = format!("{pin} same service");
        assert_dml_reject(&got, DmlStage::Build, "bad-protx-dup-netinfo-entry", &what);
        let no_coins = Context {
            coins: CoinsView::AtTip(vec![]),
            ..ctx.clone()
        };
        let got = oracle.dml_block(&Chain::main(), &block, &no_coins, true, false).unwrap();
        assert_dml_reject(
            &got,
            DmlStage::Check,
            "bad-protx-collateral",
            &format!("{pin} no coins"),
        );
    }
}

/// `oracle_dml_block_effects` (ABI 26) on mainnet's first DIP3 block with registrations
/// (1,028,162): the same result as `oracle_dml_block`, the diff row (the empty list's diff to the
/// block's), no snapshot row (not a 576th height) and no platform-ban resets (no listed
/// masternode). A just-check run and a check-loop rejection leave no effects.
#[test]
fn dml_block_effects_on_the_first_dip3_block() {
    let sample = &fixture()["dip3_block"];
    let block = hex(sample["block"].as_str().unwrap());
    let ctx = dip3_context(sample);
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let plain = oracle.dml_block(&Chain::main(), &block, &ctx, true, false).unwrap();
        let (got, effects) =
            oracle.dml_block_effects(&Chain::main(), &block, &ctx, true, false).unwrap();
        assert_eq!(got, plain, "{pin}");
        let row = effects.diff_row.expect("an accepted block's diff row");
        // CDeterministicMNListDiff: the block's 14 registrations added, none updated or removed.
        assert_eq!(row[0], 14, "{pin}: 14 additions");
        assert!(row.ends_with(&[0, 0]), "{pin}: no updates, no removals");
        assert_eq!(effects.snapshot_row, None, "{pin}: 1,028,162 is not a 576th height");
        assert!(effects.resets.is_empty(), "{pin}");

        let (_, effects) =
            oracle.dml_block_effects(&Chain::main(), &block, &ctx, true, true).unwrap();
        assert_eq!(effects, DmlEffects::default(), "{pin}: just_check");
        let no_coins = Context {
            coins: CoinsView::AtTip(vec![]),
            ..ctx.clone()
        };
        let (got, effects) =
            oracle.dml_block_effects(&Chain::main(), &block, &no_coins, true, false).unwrap();
        assert_eq!(got.stage, Some(DmlStage::Check), "{pin}");
        assert_eq!(effects, DmlEffects::default(), "{pin}: a check-loop rejection");
    }
}

#[test]
fn mined_special_txs_pass() {
    let sample = &fixture()["dip3_block"];
    let ctx = dip3_context(sample);
    let mut coverage = Coverage::default();
    for oracle in Oracle::all() {
        for tx in sample["special_txs"].as_array().unwrap() {
            let raw = hex(tx["hex"].as_str().unwrap());
            for check_sigs in [true, false] {
                let verdict =
                    oracle.check_special_tx(&Chain::main(), &raw, &ctx, check_sigs).unwrap();
                assert_accept(
                    verdict,
                    &format!("{} type {} {}", oracle.pin(), tx["type"], tx["txid"]),
                );
            }
            coverage.hit(format!("type {}", tx["type"]));
        }
        // Without the collaterals Core rejects the ProRegTxs that reference one.
        let no_coins = Context {
            coins: CoinsView::AtTip(vec![]),
            ..ctx.clone()
        };
        let reg =
            sample["special_txs"].as_array().unwrap().iter().find(|t| t["type"] == 1).unwrap();
        match oracle
            .check_special_tx(&Chain::main(), &hex(reg["hex"].as_str().unwrap()), &no_coins, true)
            .unwrap()
        {
            Verdict::Reject(r) => {
                assert_eq!(r.reason, "bad-protx-collateral", "{}", oracle.pin());
                assert_eq!(r.result, ResultCode::Tx(7), "{}: TX_BAD_SPECIAL", oracle.pin());
                assert!(!r.reason_truncated && !r.debug_truncated);
            }
            Verdict::Accept => {
                panic!("{}: accepted a ProRegTx without its collateral", oracle.pin())
            }
        }
    }
    coverage.require_per_pin(&[("type 1", 14), ("type 5", 1)]);
}

#[test]
fn mined_block_passes_contextual_checks() {
    let sample = &fixture()["dip3_block"];
    let ctx = dip3_context(sample);
    let block = hex(sample["block"].as_str().unwrap());
    let header: [u8; 80] = hex(sample["header"].as_str().unwrap()).try_into().unwrap();
    for oracle in Oracle::all() {
        assert_accept(
            oracle.contextual_check_header(&Chain::main(), &header, &ctx).unwrap(),
            "header",
        );
        assert_accept(
            oracle.contextual_check_block(&Chain::main(), &block, &ctx).unwrap(),
            "block",
        );
        for tx in sample["special_txs"].as_array().unwrap() {
            let raw = hex(tx["hex"].as_str().unwrap());
            assert_accept(oracle.contextual_check_tx(&Chain::main(), &raw, &ctx).unwrap(), "tx");
        }
        // A header from more than two hours after the adjusted time.
        let mut future = header;
        let time = common::header_time(&header) + 3 * 3600;
        future[68..72].copy_from_slice(&time.to_le_bytes());
        match oracle.contextual_check_header(&Chain::main(), &future, &ctx).unwrap() {
            Verdict::Reject(r) => {
                assert_eq!(r.reason, "time-too-new", "{}", oracle.pin());
                assert_eq!(r.result, ResultCode::Block(8), "{}: BLOCK_TIME_FUTURE", oracle.pin());
            }
            Verdict::Accept => panic!("{}: accepted a header from the future", oracle.pin()),
        }
        // DGW reads 24 headers, so a shorter context is not enough.
        let short = Context {
            first_height: ctx.first_height + ctx.headers.len() as i32 - 5,
            headers: ctx.headers[ctx.headers.len() - 5..].to_vec(),
            ..ctx.clone()
        };
        let err = oracle.contextual_check_header(&Chain::main(), &header, &short).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{}: {err}", oracle.pin());
    }
}

#[test]
fn mined_tx_inputs_and_sequence_locks() {
    let sample = &fixture()["dip3_block"];
    let ctx = dip3_context(sample);
    let tx = &sample["normal_tx"];
    let raw = hex(tx["hex"].as_str().unwrap());
    let spent = coins(&tx["coins"]);
    let height = ctx.first_height + ctx.headers.len() as i32;
    let prev_heights: Vec<i32> = spent.iter().map(|c| c.height as i32).collect();
    for oracle in Oracle::all() {
        let inputs = oracle.check_tx_inputs(&raw, &spent, height).unwrap();
        assert_accept(inputs.verdict, "inputs");
        assert_eq!(inputs.fee, Some(tx["fee"].as_i64().unwrap()), "{}", oracle.pin());
        // Missing a coin
        match oracle.check_tx_inputs(&raw, &spent[1..], height).unwrap().verdict {
            Verdict::Reject(r) => assert_eq!(r.reason, "bad-txns-inputs-missingorspent"),
            Verdict::Accept => panic!("accepted a transaction with a missing input"),
        }
        let flags = 1; // LOCKTIME_VERIFY_SEQUENCE
        // The coins are older than the context's headers: Core would read median times below them.
        if prev_heights.iter().any(|&h| h - 1 - 10 < ctx.first_height) {
            let err = oracle
                .sequence_locks(&Chain::main(), &raw, &ctx, &prev_heights, flags)
                .unwrap_err();
            assert_eq!(err.kind, ErrorKind::Context, "{}: {err}", oracle.pin());
        }
        // With only heights that need no median time (sequence locks off before version 2),
        // the block the transaction was mined in satisfies them.
        let tx_version = u16::from_le_bytes(raw[0..2].try_into().unwrap());
        if tx_version < 2 {
            let heights_at_tip = vec![height; prev_heights.len()];
            let locks =
                oracle.sequence_locks(&Chain::main(), &raw, &ctx, &heights_at_tip, flags).unwrap();
            assert!(locks.satisfied, "{}: {locks:?}", oracle.pin());
            assert_eq!((locks.min_height, locks.min_time), (-1, -1));
        }
        // No coin is newer than the block spending it.
        let too_new = vec![height + 1; prev_heights.len()];
        let err = oracle.sequence_locks(&Chain::main(), &raw, &ctx, &too_new, flags).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{}: {err}", oracle.pin());
    }
}

/// The mined commitment sample and its context.
fn mined_commitment() -> (Vec<u8>, Context) {
    let sample = &fixture()["commitment"];
    let qc = hex(sample["commitment"].as_str().unwrap());
    let members = QuorumMembers {
        llmq_type: sample["llmq_type"].as_u64().unwrap() as u8,
        quorum_hash: hex(sample["quorum_hash"].as_str().unwrap()).try_into().unwrap(),
        members: sample["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| hex(m.as_str().unwrap()))
            .collect(),
    };
    let ctx = commitment_context(
        sample["first_height"].as_i64().unwrap() as i32,
        headers(sample["headers"].as_str().unwrap()),
        vec![members],
    );
    (qc, ctx)
}

#[test]
fn mined_commitment_verifies() {
    let (qc, ctx) = mined_commitment();
    for oracle in Oracle::all() {
        for check_sigs in [true, false] {
            let verdict = oracle.verify_commitment(&Chain::main(), &qc, &ctx, check_sigs).unwrap();
            assert_accept(verdict, &format!("{} check_sigs {check_sigs}", oracle.pin()));
        }
        // With one member, the validMembers bits of the other 49 must not be set.
        let mut truncated = ctx.clone();
        truncated.quorums[0].members.truncate(1);
        match oracle.verify_commitment(&Chain::main(), &qc, &truncated, true).unwrap() {
            Verdict::Reject(r) => {
                assert_eq!(r.result, ResultCode::Bool, "{}", oracle.pin());
                assert!(
                    r.debug_message.contains("invalid validMembers bitset"),
                    "{}: {}",
                    oracle.pin(),
                    r.debug_message
                )
            }
            Verdict::Accept => panic!("{}: accepted wrong members", oracle.pin()),
        }
        let no_members = Context {
            quorums: vec![],
            ..ctx.clone()
        };
        let err = oracle.verify_commitment(&Chain::main(), &qc, &no_members, true).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{}: {err}", oracle.pin());
        // Known checkpoints above the tip leave the verdict alone.
        let later_checkpoints = Context {
            checkpoints: Checkpoints::Enabled {
                best_header_height: CHECKPOINT_QUORUM_BASE.0,
            },
            ..ctx.clone()
        };
        let verdict = oracle.verify_commitment(&Chain::main(), &qc, &later_checkpoints, true);
        assert_accept(verdict.unwrap(), &format!("{} with later checkpoints", oracle.pin()));
        // A base block the context knows only as a checkpoint above the tip (no ancestry):
        // missing context, not Verify()'s "parentless quorum base block" rejection.
        let mut at_checkpoint = qc.clone();
        at_checkpoint[3..35].copy_from_slice(&common::rpc_hash(CHECKPOINT_QUORUM_BASE.1));
        let err = oracle
            .verify_commitment(&Chain::main(), &at_checkpoint, &later_checkpoints, true)
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{}: {err}", oracle.pin());
    }
}

/// A mainnet checkpoint (chainparams at the pin) at an LLMQ_50_60 quorum base height.
const CHECKPOINT_QUORUM_BASE: (i32, &str) =
    (1_850_400, "00000000000000261bdbe99c01fcba992e577efa6cc41aae564b8ca9f112b2a3");

/// Sol r2 F1, from a session to the calls after it: free restores the logger's categories,
/// levels and line format from before the session, after a session and after starts that fail
/// part-way, so a reason Core logs (at debug level, in BCLog::LLMQ) reads as it did before.
/// In a child process: no other test's oracle call may run while a session exists.
#[test]
fn sessions_leave_the_logger_as_they_found_it() {
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sessions_leave_the_logger_child", "--nocapture", "--test-threads=1"])
        .env("O1C_LOGGER_RESTORE_CHILD", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&child.stdout);
    assert!(child.status.success(), "{stdout}{}", String::from_utf8_lossy(&child.stderr));
    assert!(stdout.contains("1 passed"), "{stdout}");
}

#[test]
fn sessions_leave_the_logger_child() {
    if std::env::var_os("O1C_LOGGER_RESTORE_CHILD").is_none() {
        return;
    }
    let (qc, mut truncated) = mined_commitment();
    truncated.quorums[0].members.truncate(1);
    let reason = |oracle: &Oracle| match oracle
        .verify_commitment(&Chain::main(), &qc, &truncated, true)
        .unwrap()
    {
        Verdict::Reject(r) => r.debug_message,
        Verdict::Accept => panic!("{}: accepted wrong members", oracle.pin()),
    };
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let before = reason(&oracle);
        assert!(before.contains("invalid validMembers bitset"), "{pin}: {before}");
        // The session logs with source locations, thread names and microseconds, and hides
        // LLMQ's debug lines.
        let config = SessionConfig {
            chain: Chain::new("regtest", &["-loglevel=llmq:info"]),
            our_services: 1 | 4,
            mocktime: 1_417_713_337 + 86_400,
            rng_seed: 1,
            blocks: Vec::new(),
            min_chain_work: None,
            max_tip_age: None,
            check_block_index: true,
        };
        drop(oracle.peer_session_new(&config).unwrap().expect("a session"));
        assert_eq!(reason(&oracle), before, "{pin}: after a session");
        // A fixture block that does not become the tip (a header and no transactions): the
        // start fails once the node exists.
        let mut not_a_tip = vec![0; 80];
        not_a_tip.push(0);
        let refused = SessionConfig {
            blocks: vec![not_a_tip],
            ..config.clone()
        };
        assert!(oracle.peer_session_new(&refused).unwrap().is_none(), "{pin}");
        assert_eq!(reason(&oracle), before, "{pin}: after a refused start");
        // An option ChainTestingSetup refuses: the start throws inside TestingSetup.
        let thrown = SessionConfig {
            chain: Chain::new("regtest", &["-loglevel=llmq:info", "-minimumchainwork=xyz"]),
            ..config
        };
        let thrown = oracle.peer_session_new(&thrown).map(|s| s.is_some());
        assert_eq!(thrown.unwrap_err().kind, ErrorKind::Exception, "{pin}");
        assert_eq!(reason(&oracle), before, "{pin}: after a start that threw");
    }
}

#[test]
fn bls_decode_and_verify() {
    let sample = &fixture()["proupserv"];
    let pubkey: [u8; 48] = hex(sample["pubkey_operator"].as_str().unwrap()).try_into().unwrap();
    let sig: [u8; 96] = hex(sample["sig"].as_str().unwrap()).try_into().unwrap();
    let msg: [u8; 32] = hex(sample["msg"].as_str().unwrap()).try_into().unwrap();
    for oracle in Oracle::all() {
        // A mined legacy operator key and signature (Core verified it in CheckProUpServTx).
        assert!(oracle.bls_verify(true, &pubkey, &sig, &msg).unwrap(), "{}", oracle.pin());
        let mut other = msg;
        other[0] ^= 1;
        assert!(!oracle.bls_verify(true, &pubkey, &sig, &other).unwrap(), "{}", oracle.pin());
        for (kind, bytes) in [(BlsKind::PublicKey, &pubkey[..]), (BlsKind::Signature, &sig[..])] {
            for mode in [BlsMode::Eager, BlsMode::Lazy] {
                let decoded = oracle.bls_decode(kind, mode, true, bytes).unwrap();
                assert!(decoded.valid, "{} {kind:?} {mode:?}", oracle.pin());
                assert_eq!(decoded.reserialized, bytes, "{} {kind:?} {mode:?}", oracle.pin());
            }
        }
        // All-zero bytes decode to an invalid object in either scheme (A1 §1.4.1).
        for legacy in [true, false] {
            let decoded =
                oracle.bls_decode(BlsKind::PublicKey, BlsMode::Eager, legacy, &[0; 48]).unwrap();
            assert_eq!((decoded.valid, decoded.reserialized), (false, vec![0; 48]));
        }
        let err = oracle.bls_decode(BlsKind::SecretKey, BlsMode::Lazy, true, &[0; 32]).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument);
    }
}

#[test]
fn bls_exec_script() {
    use dash_oracle_sys::ffi::{
        ORACLE_BLS_FLAG_LEGACY as LEGACY, ORACLE_BLS_OP_DECODE as DECODE, ORACLE_BLS_OP_SER as SER,
        ORACLE_BLS_OP_VERIFY as VERIFY, ORACLE_OK, ORACLE_REJECT,
    };
    let sample = &fixture()["proupserv"];
    let pubkey = hex(sample["pubkey_operator"].as_str().unwrap());
    let sig = hex(sample["sig"].as_str().unwrap());
    let msg = hex(sample["msg"].as_str().unwrap());
    let mut other = msg.clone();
    other[0] ^= 1;
    // r0 = pubkey, r1 = sig (legacy, eager); verify both messages; serialize r0.
    let mut script = Vec::new();
    for (kind, bytes, dst) in [(BlsKind::PublicKey, &pubkey, 0u8), (BlsKind::Signature, &sig, 1)] {
        script.extend([DECODE, LEGACY, kind as u8, BlsMode::Eager as u8, bytes.len() as u8]);
        script.extend(bytes);
        script.push(dst);
    }
    for h in [&msg, &other] {
        script.extend([VERIFY, LEGACY, 1, 0]);
        script.extend(h);
    }
    script.extend([SER, LEGACY, 0]);
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let records = oracle.bls_exec(&script).unwrap();
        let got: Vec<_> = records.iter().map(|r| (r.status, r.relic_code)).collect();
        let want = [ORACLE_OK, ORACLE_OK, ORACLE_OK, ORACLE_REJECT, ORACLE_OK].map(|s| (s, 0));
        assert_eq!(got, want, "{pin}");
        assert_eq!((&records[0].output, &records[1].output), (&pubkey, &sig), "{pin}");
        assert!(records[2].output.is_empty() && records[3].output.is_empty(), "{pin}");
        assert_eq!(records[4].output, pubkey, "{pin}");

        // A reserved flag bit, or an operand naming an empty register, is a malformed
        // script; a truncated one fails to decode.
        let err = oracle.bls_exec(&[SER, 0x10, 0]).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{pin}");
        let err = oracle.bls_exec(&[SER, LEGACY, 7]).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{pin}");
        let err = oracle.bls_exec(&script[..script.len() - 1]).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Decode, "{pin}");
        assert!(oracle.bls_exec(&[]).unwrap().is_empty(), "{pin}");
        bls_exec_core_order(&oracle);
    }
}

/// Where Core's order of checks decides the answer (oracle.h's notes).
fn bls_exec_core_order(oracle: &Oracle) {
    use dash_oracle_sys::ffi::{
        ORACLE_BLS_OP_AGG_SK as AGG_SK, ORACLE_BLS_OP_DECODE as DECODE,
        ORACLE_BLS_OP_IES_ENCRYPT as IES_ENCRYPT, ORACLE_BLS_OP_SER as SER,
        ORACLE_BLS_OP_SUB_SIG as SUB_SIG, ORACLE_BLS_OP_VERIFY_AGG as VERIFY_AGG, ORACLE_E_ARG,
        ORACLE_OK, ORACLE_REJECT,
    };
    let pin = oracle.pin();
    let decode = |kind: BlsKind, mode: BlsMode, bytes: &[u8], dst: u8| {
        let mut op = vec![DECODE, 0, kind as u8, mode as u8, bytes.len() as u8];
        op.extend(bytes);
        op.push(dst);
        op
    };
    let statuses = |script: &[u8]| -> Vec<i32> {
        oracle.bls_exec(script).unwrap().iter().map(|r| r.status).collect()
    };

    // A lazy read of bytes off the curve serializes zeros once Get() fails.
    let records = oracle.bls_exec(&decode(BlsKind::PublicKey, BlsMode::Lazy, &[1; 48], 0)).unwrap();
    assert_eq!((records[0].status, &records[0].output), (ORACLE_REJECT, &vec![0; 48]), "{pin}");

    // An invalid signature fails VerifyInsecureAggregated before its assert
    // on the lists.
    let zero_sig = decode(BlsKind::Signature, BlsMode::Eager, &[0; 96], 0);
    let script = [zero_sig.as_slice(), &[VERIFY_AGG, 0, 0, 0, 0]].concat();
    assert_eq!(statuses(&script), [ORACLE_REJECT, ORACLE_REJECT], "{pin}");

    // SubInsecure's assert case empties its destination.
    let script = [zero_sig.as_slice(), &[SUB_SIG, 0, 0, 0, 0]].concat();
    assert_eq!(statuses(&script), [ORACLE_REJECT, ORACLE_E_ARG], "{pin}");
    let err = oracle.bls_exec(&[script.as_slice(), &[SER, 0, 0]].concat()).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Argument, "{pin}");

    // A valid zero key (1 + (r - 1)) is no ephemeral key InitEncrypt makes.
    let mut one = [0u8; 32];
    one[31] = 1;
    let r_minus_1 = hex("73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000000");
    let script = [
        decode(BlsKind::SecretKey, BlsMode::Eager, &one, 0),
        decode(BlsKind::SecretKey, BlsMode::Eager, &r_minus_1, 1),
        vec![AGG_SK, 0, 2, 0, 1, 2],
        vec![IES_ENCRYPT, 0, 2],
        vec![0; 32],
        vec![0, 0],
    ]
    .concat();
    assert_eq!(statuses(&script), [ORACLE_OK, ORACLE_OK, ORACLE_OK, ORACLE_E_ARG], "{pin}");
}

/// A regtest chain of `n` headers from height 0 (only linkage matters to the context).
fn regtest_headers(oracle: &Oracle, n: usize) -> Vec<[u8; 80]> {
    let mut out = Vec::new();
    let mut prev = [0u8; 32];
    for i in 0..n {
        let mut h = common::header(1_417_713_337 + 600 * i as u32, 0x207f_ffff);
        h[0..4].copy_from_slice(&4i32.to_le_bytes());
        h[4..36].copy_from_slice(&prev);
        prev = oracle.header_decode(&h).unwrap().hash;
        out.push(h);
    }
    out
}

#[test]
fn trivially_invalid_vectors_through_check_special_tx() {
    // Core's test: regtest with V19 at 100, the tip at 98 (legacy) or 100 (basic); DIP0003
    // must be active here because CheckSpecialTx() gates on it before the trivial checks.
    let chain = Chain::new("regtest", &["-testactivationheight=v19@100", "-dip3params=2:2"]);
    let mut coverage = Coverage::default();
    for oracle in Oracle::all() {
        let all = regtest_headers(&oracle, 101);
        for row in trivial_vectors("trivially_invalid.json") {
            let tip = if row[2] == "basic" {
                100
            } else {
                98
            };
            // v23.1.8's copy of the file labels one row "gettxpayload-fail", its test's marker
            // for GetTxPayload() failing; CheckProUpRegTx() then rejects with bad-protx-payload,
            // the reason v24's copy records.
            let expected = row[4].as_str();
            for bls_legacy in [true, false] {
                let ctx = Context::new(
                    0,
                    all[..=tip].to_vec(),
                    bls_legacy,
                    vec![],
                    1_500_000_000,
                    Checkpoints::Disabled,
                    CoinsView::Unchecked(vec![]),
                    empty_list(),
                    vec![],
                );
                match oracle.check_special_tx(&chain, &hex(&row[3]), &ctx, true).unwrap() {
                    Verdict::Reject(r) => {
                        assert_eq!(r.reason, expected, "{} {} {}", oracle.pin(), row[1], row[0])
                    }
                    Verdict::Accept => panic!("{} accepted {}", oracle.pin(), row[0]),
                }
                coverage.hit(expected.to_string());
            }
        }
    }
    coverage.require_per_pin(&[
        ("bad-protx-version", 6),
        ("bad-protx-key-null", 4),
        ("bad-protx-reason", 2),
    ]);
}

fn fixture_asset_unlocks() -> Vec<Vec<u8>> {
    let data: Value = common::load_data("v24-asset-unlocks.json");
    data["asset_unlocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| hex(u["hex"].as_str().unwrap()))
        .collect()
}

#[test]
fn trivially_valid_payloads_decode() {
    for oracle in Oracle::all() {
        for row in trivial_vectors("trivially_valid.json") {
            let tx = hex(&row[3]);
            let tx_type = u16::from_le_bytes(tx[2..4].try_into().unwrap());
            let payload = extra_payload(&tx);
            let decoded = oracle.payload_decode(tx_type, row[2] == "legacy", &payload).unwrap();
            assert!(decoded.consumed_all, "{} {}", oracle.pin(), row[0]);
            assert_eq!(to_hex(&decoded.reencoded), to_hex(&payload), "{} {}", oracle.pin(), row[0]);
        }
        // Asset unlocks (v24 regtest, tests/data/v24-asset-unlocks.json) carry a quorumSig the
        // scheme flag decodes and re-encodes, in the basic scheme after V19.
        let unlocks = fixture_asset_unlocks();
        for raw in &unlocks {
            let payload = extra_payload(raw);
            let decoded = oracle.payload_decode(9, false, &payload).unwrap();
            assert!(decoded.consumed_all, "{}", oracle.pin());
            assert_eq!(
                to_hex(&decoded.reencoded),
                to_hex(&payload),
                "{}: asset unlock re-encoding",
                oracle.pin()
            );
        }
        assert!(!unlocks.is_empty());
        // A truncated payload: Core's deserializer throws.
        let tx = hex(&trivial_vectors("trivially_valid.json")[0][3]);
        let payload = extra_payload(&tx);
        let err = oracle.payload_decode(1, true, &payload[..payload.len() - 1]).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Decode, "{}", oracle.pin());
        let unsupported = oracle.payload_decode(10, false, &[]).unwrap_err().kind;
        assert_eq!(unsupported, ErrorKind::Decode);
    }
}

#[test]
fn contexts_are_refused_when_malformed() {
    let sample = &fixture()["dip3_block"];
    let ctx = dip3_context(sample);
    let block = hex(sample["block"].as_str().unwrap());
    for oracle in Oracle::all() {
        let mut broken = ctx.clone();
        broken.headers.swap(0, 1);
        let err = oracle.dml_apply(&Chain::main(), &block, &broken).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{}: {err}", oracle.pin());
        // A list of another block than the tip.
        let mut other = ctx.clone();
        other.mn_list = MnList::Supplied({
            let mut list = supplied(&ctx.mn_list).to_vec();
            list[0] ^= 1;
            list
        });
        let err = oracle.dml_apply(&Chain::main(), &block, &other).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{}: {err}", oracle.pin());
        // Bytes after the list.
        let mut trailing = ctx.clone();
        trailing.mn_list = MnList::Supplied([supplied(&ctx.mn_list), &[0]].concat());
        let err = oracle.dml_apply(&Chain::main(), &block, &trailing).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{}: {err}", oracle.pin());
        // A best header below the tip.
        let below_tip = Context {
            checkpoints: Checkpoints::Enabled {
                best_header_height: tip_of(ctx.first_height, &ctx.headers) - 1,
            },
            ..ctx.clone()
        };
        let err = oracle.dml_apply(&Chain::main(), &block, &below_tip).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{}: {err}", oracle.pin());
    }
}

/// A version-3 special transaction without inputs or outputs carrying `payload`.
fn special_tx(tx_type: u16, payload: &[u8]) -> Vec<u8> {
    let mut tx = vec![3, 0];
    tx.extend(tx_type.to_le_bytes());
    tx.extend([0, 0, 0, 0, 0, 0]); // no inputs, no outputs, lock time 0
    tx.push(payload.len() as u8);
    tx.extend(payload);
    tx
}

/// A block of the sampled header with the given transactions.
fn block_of(header: &[u8], txs: &[Vec<u8>]) -> Vec<u8> {
    let mut block = header.to_vec();
    block.push(txs.len() as u8);
    for tx in txs {
        block.extend(tx);
    }
    block
}

#[test]
fn guards_refuse_what_core_would_crash_on() {
    let sample = &fixture()["dip3_block"];
    let ctx = dip3_context(sample);
    let header = hex(sample["header"].as_str().unwrap());
    let coinbase = hex(sample["special_txs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["type"] == 5)
        .unwrap()["hex"]
        .as_str()
        .unwrap());
    let commitment = &fixture()["commitment"];
    for oracle in Oracle::all() {
        // ProTx payloads of version 0: their serializers stop after the version.
        for tx_type in [1u16, 2, 3, 4] {
            let block = block_of(&header, &[coinbase.clone(), special_tx(tx_type, &[0, 0])]);
            let err = oracle.dml_apply(&Chain::main(), &block, &ctx).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Argument, "{} type {tx_type}: {err}", oracle.pin());
        }
        // A commitment whose quorum base block is below the headers.
        let height = commitment["height"].as_u64().unwrap() as u32;
        let base = commitment["first_height"].as_i64().unwrap() as i32;
        let mut payload = vec![1, 0];
        payload.extend(height.to_le_bytes());
        payload.extend(hex(commitment["commitment"].as_str().unwrap()));
        let mut qc_tx = vec![3, 0, 6, 0, 0, 0, 0, 0, 0, 0];
        qc_tx.extend([0xfd]);
        qc_tx.extend((payload.len() as u16).to_le_bytes());
        qc_tx.extend(&payload);
        let all = headers(commitment["headers"].as_str().unwrap());
        let above_base = Context {
            mn_list: empty_list(),
            ..commitment_context(base + 1, all[1..].to_vec(), vec![])
        };
        let err = oracle.check_special_tx(&Chain::main(), &qc_tx, &above_base, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{}: {err}", oracle.pin());
        let qc_header = {
            let mut h = all.last().unwrap().to_vec();
            h[4..36].copy_from_slice(&oracle.header_decode(all.last().unwrap()).unwrap().hash);
            h
        };
        let err = oracle.dml_apply(
            &Chain::main(),
            &block_of(&qc_header, &[coinbase.clone(), qc_tx.clone()]),
            &above_base,
        );
        assert_eq!(err.unwrap_err().kind, ErrorKind::Context, "{}", oracle.pin());
        // More members than the LLMQ type's size.
        let mut members: Vec<Vec<u8>> = commitment["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| hex(m.as_str().unwrap()))
            .collect();
        members.push(members[0].clone());
        let too_many = commitment_context(
            base,
            all.clone(),
            vec![QuorumMembers {
                llmq_type: commitment["llmq_type"].as_u64().unwrap() as u8,
                quorum_hash: hex(commitment["quorum_hash"].as_str().unwrap()).try_into().unwrap(),
                members,
            }],
        );
        let err =
            oracle.verify_commitment(&Chain::main(), &payload[6..], &too_many, true).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{}: {err}", oracle.pin());
        // An adjusted time Core cannot add MAX_FUTURE_BLOCK_TIME to in nanoseconds; the cap
        // itself is accepted.
        let late = Context {
            adjusted_time: i64::MAX / 1_000_000_000 - 7200 + 1,
            ..ctx.clone()
        };
        let err = oracle
            .contextual_check_header(&Chain::main(), &header.clone().try_into().unwrap(), &late)
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{}: {err}", oracle.pin());
        let at_cap = Context {
            adjusted_time: i64::MAX / 1_000_000_000 - 7200,
            ..ctx.clone()
        };
        let verdict = oracle.contextual_check_header(
            &Chain::main(),
            &header.clone().try_into().unwrap(),
            &at_cap,
        );
        assert_accept(verdict.unwrap(), "the adjusted-time cap");
        // Asset unlocks read the quorum manager.
        let unlock = &fixture_asset_unlocks()[0];
        let err = oracle.check_special_tx(&Chain::main(), unlock, &ctx, false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{}: {err}", oracle.pin());
        // Coins no coins view holds, and a negative spend height.
        let tx = &sample["normal_tx"];
        let raw = hex(tx["hex"].as_str().unwrap());
        let mut spent = coins(&tx["coins"]);
        assert_eq!(oracle.check_tx_inputs(&raw, &spent, -1).unwrap_err().kind, ErrorKind::Argument);
        spent[0].amount = -1;
        assert_eq!(oracle.check_tx_inputs(&raw, &spent, 1).unwrap_err().kind, ErrorKind::Argument);
    }
}

/// The ProUpServTx sample (8e5bdc5f…, mined at 1028257) and its context: the headers and
/// the list after its parent.
fn proupserv() -> (Vec<u8>, Context) {
    let sample = &fixture()["proupserv"];
    (hex(sample["tx"].as_str().unwrap()), sample_context(sample, CoinsView::AtTip(vec![])))
}

/// A context without a list is not an empty list (GPT O2a r1, major 1). Core accepted
/// the mined ProUpServTx against the list after its parent and rejects it against an
/// explicitly empty one; with no list the oracle refuses (ORACLE_E_CONTEXT) instead of
/// letting Core read an empty list it was never given.
#[test]
fn absent_list_is_missing_context() {
    let (tx, ctx) = proupserv();
    let dip3 = &fixture()["dip3_block"];
    let block = hex(dip3["block"].as_str().unwrap());
    for oracle in Oracle::all() {
        for check_sigs in [true, false] {
            let verdict = oracle.check_special_tx(&Chain::main(), &tx, &ctx, check_sigs).unwrap();
            assert_accept(verdict, &format!("{} check_sigs {check_sigs}", oracle.pin()));
        }
        let empty = Context {
            mn_list: empty_list(),
            ..ctx.clone()
        };
        match oracle.check_special_tx(&Chain::main(), &tx, &empty, true).unwrap() {
            Verdict::Reject(r) => assert_eq!(r.reason, "bad-protx-hash", "{}", oracle.pin()),
            Verdict::Accept => panic!("{}: accepted an update of no masternode", oracle.pin()),
        }
        let absent = Context {
            mn_list: MnList::Absent,
            ..ctx.clone()
        };
        let err = oracle.check_special_tx(&Chain::main(), &tx, &absent, true).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{}: {err}", oracle.pin());
        // Checks that read no list run without one.
        assert_accept(oracle.contextual_check_tx(&Chain::main(), &tx, &absent).unwrap(), "tx");
        // Rebuilding a list needs the one before.
        let no_prev_list = Context {
            mn_list: MnList::Absent,
            ..dip3_context(dip3)
        };
        let err = oracle.dml_apply(&Chain::main(), &block, &no_prev_list).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{}: {err}", oracle.pin());
    }
}

/// The sampled DIP0003 block applied on its parent's list, and the context after it.
/// The new list gets the block's hash, as CDeterministicMNManager::ProcessBlock() sets
/// it before storing the list (RebuildListFromBlock() leaves it null).
fn after_dip3_block(oracle: &Oracle) -> (Context, Vec<u8>) {
    let sample = &fixture()["dip3_block"];
    let ctx = dip3_context(sample);
    let block = hex(sample["block"].as_str().unwrap());
    let header: [u8; 80] = hex(sample["header"].as_str().unwrap()).try_into().unwrap();
    let mut after = oracle.dml_apply(&Chain::main(), &block, &ctx).unwrap().new_list.unwrap();
    assert_eq!(after[..32], [0; 32], "{}", oracle.pin());
    after[..32].copy_from_slice(&oracle.header_decode(&header).unwrap().hash);
    let mut headers = ctx.headers.clone();
    headers.push(header);
    let next = Context {
        checkpoints: checkpoints_to_tip(ctx.first_height, &headers),
        headers,
        adjusted_time: i64::from(common::header_time(&header)) + 60,
        mn_list: MnList::Supplied(after.clone()),
        ..ctx
    };
    (next, after)
}

/// Re-registering a masternode of the sampled block on the list that block produced:
/// Core rejects the duplicate key, and without the list the oracle refuses rather than
/// accept it against no masternodes (the false accept GPT O2a r1 major 1 led to).
#[test]
fn duplicate_registration_needs_the_list() {
    let sample = &fixture()["dip3_block"];
    let reg = hex(sample["special_txs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["type"] == 1)
        .unwrap()["hex"]
        .as_str()
        .unwrap());
    for oracle in Oracle::all() {
        let (next, _) = after_dip3_block(&oracle);
        match oracle.check_special_tx(&Chain::main(), &reg, &next, true).unwrap() {
            Verdict::Reject(r) => assert_eq!(r.reason, "bad-protx-dup-key", "{}", oracle.pin()),
            Verdict::Accept => panic!("{}: accepted a duplicate registration", oracle.pin()),
        }
        let absent = Context {
            mn_list: MnList::Absent,
            ..next
        };
        let err = oracle.check_special_tx(&Chain::main(), &reg, &absent, true).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{}: {err}", oracle.pin());
    }
}

/// ContextualCheckBlockHeader() rejects a header below the last checkpoint the node
/// knows (GPT O2a r1, major 2). Mainnet's first checkpoint above the sampled block is
/// 1067570 and its last 2487500 (chainparams at the pin).
#[test]
fn header_check_follows_known_checkpoints() {
    let sample = &fixture()["dip3_block"];
    let ctx = dip3_context(sample);
    let header: [u8; 80] = hex(sample["header"].as_str().unwrap()).try_into().unwrap();
    let tip = tip_of(ctx.first_height, &ctx.headers);
    for oracle in Oracle::all() {
        for (checkpoints, rejected) in [
            (Checkpoints::Disabled, false),
            (
                Checkpoints::Enabled {
                    best_header_height: tip,
                },
                false,
            ),
            (
                Checkpoints::Enabled {
                    best_header_height: 1_067_569,
                },
                false,
            ),
            (
                Checkpoints::Enabled {
                    best_header_height: 1_067_570,
                },
                true,
            ),
            (
                Checkpoints::Enabled {
                    best_header_height: 2_487_500,
                },
                true,
            ),
        ] {
            let with = Context {
                checkpoints,
                ..ctx.clone()
            };
            let verdict = oracle.contextual_check_header(&Chain::main(), &header, &with).unwrap();
            match (verdict, rejected) {
                (Verdict::Accept, false) => {}
                (Verdict::Reject(r), true) => {
                    assert_eq!(r.reason, "bad-fork-prior-to-checkpoint", "{}", oracle.pin());
                    assert_eq!(
                        r.result,
                        ResultCode::Block(9),
                        "{}: BLOCK_CHECKPOINT",
                        oracle.pin()
                    );
                }
                (verdict, _) => panic!("{} {checkpoints:?}: {verdict:?}", oracle.pin()),
            }
        }
    }
}

/// Masternode state no chain produces is refused before Core computes with it (GPT O2a
/// r1, major 3): Core subtracts the registered height from the tip's and adds PoSe
/// penalties in `int`. In-range extremes go through. Run under the ubsan flavor as well
/// (`DASH_ORACLE_FLAVOR=ubsan`), where any signed overflow aborts the test.
#[test]
fn list_state_out_of_range_is_refused() {
    let sample = &fixture()["dip3_block"];
    let header: [u8; 80] = hex(sample["header"].as_str().unwrap()).try_into().unwrap();
    let coinbase = hex(sample["special_txs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["type"] == 5)
        .unwrap()["hex"]
        .as_str()
        .unwrap());
    for oracle in Oracle::all() {
        let (next, after) = after_dip3_block(&oracle);
        let tip = tip_of(next.first_height, &next.headers);
        // A coinbase-only block on the new tip: RebuildListFromBlock() reads every
        // entry's registered height, and DecreaseScores() every penalty.
        let mut on_top = header;
        on_top[4..36].copy_from_slice(&oracle.header_decode(&header).unwrap().hash);
        let block = block_of(&on_top, std::slice::from_ref(&coinbase));
        // The first state: nVersion 1 then nRegisteredHeight 1028162 (both int, LE); after
        // the list's own hash and height only the 14 states hold that pattern.
        let pattern = [1i32.to_le_bytes(), tip.to_le_bytes()].concat();
        let at: Vec<usize> = (36..after.len() - pattern.len())
            .filter(|&i| after[i..i + pattern.len()] == pattern[..])
            .collect();
        assert_eq!(at.len(), 14, "{}", oracle.pin());
        let registered = at[0] + 4;
        let with = |offset: usize, value: i32| Context {
            mn_list: MnList::Supplied({
                let mut list = after.clone();
                list[registered + offset..registered + offset + 4]
                    .copy_from_slice(&value.to_le_bytes());
                list
            }),
            ..next.clone()
        };
        // Offsets from nRegisteredHeight: nLastPaidHeight +4, nConsecutivePayments +8,
        // nPoSePenalty +12 (bounded by INT_MAX / 2, leaving room for CalcPenalty(66)),
        // nPoSeRevivedHeight +16, nPoSeBanHeight +20.
        for (field, offset, accepted, refused) in [
            ("registered height", 0, &[0, tip][..], &[i32::MIN, -1, tip + 1][..]),
            ("last paid height", 4, &[tip], &[-1, tip + 1]),
            ("consecutive payments", 8, &[tip], &[-1, tip + 1, i32::MAX]),
            ("PoSe revived height", 16, &[-1, tip], &[-2, tip + 1, i32::MIN]),
            ("PoSe ban height", 20, &[-1, tip], &[-2, tip + 1, i32::MIN]),
            ("PoSe penalty", 12, &[i32::MAX / 2], &[-1, i32::MAX / 2 + 1, i32::MAX]),
        ] {
            for &value in accepted {
                let result = oracle.dml_apply(&Chain::main(), &block, &with(offset, value));
                assert_accept(
                    result.unwrap().verdict,
                    &format!("{} {field} {value}", oracle.pin()),
                );
            }
            for &value in refused {
                let err =
                    oracle.dml_apply(&Chain::main(), &block, &with(offset, value)).unwrap_err();
                assert_eq!(
                    err.kind,
                    ErrorKind::Argument,
                    "{} {field} {value}: {err}",
                    oracle.pin()
                );
            }
        }
    }
}

/// The transactions of a block with fewer than 253 of them.
fn block_txs(oracle: &Oracle, block: &[u8]) -> Vec<Vec<u8>> {
    assert!(block[80] < 0xfd);
    let mut at = 81;
    (0..block[80])
        .map(|_| {
            let n = oracle.tx_decode(&block[at..]).unwrap().consumed;
            at += n;
            block[at - n..at].to_vec()
        })
        .collect()
}

/// The first rejection of a block proposal in TestBlockValidity()'s order:
/// ContextualCheckBlockHeader(), CheckBlock() (no proof of work, as proposals are
/// checked), ContextualCheckBlock(), then CheckSpecialTx() on each transaction, which
/// ConnectBlock() runs first (ProcessSpecialTxsInBlock()).
fn first_rejection(oracle: &Oracle, chain: &Chain, block: &[u8], ctx: &Context) -> Option<String> {
    let header: [u8; 80] = block[..80].try_into().unwrap();
    let stages = [
        oracle.contextual_check_header(chain, &header, ctx).unwrap(),
        oracle.check_block(chain, block, false, true).unwrap(),
        oracle.contextual_check_block(chain, block, ctx).unwrap(),
    ];
    let special = block_txs(oracle, block)
        .into_iter()
        .map(|tx| oracle.check_special_tx(chain, &tx, ctx, true).unwrap());
    stages.into_iter().chain(special).find_map(|verdict| match verdict {
        Verdict::Reject(r) => Some(r.reason),
        Verdict::Accept => None,
    })
}

/// Core's own verdicts on crafted regtest blocks and sequence locks, from the pin's
/// release dashd (`tests/data/contextual-regtest-<pin>.json`, made by
/// `scripts/gen-contextual-regtest.py`): getblocktemplate proposals for header, block
/// and special-transaction rules, and testmempoolaccept for relative locks on both sides
/// of the boundary, before DIP0003 (tip 300) and after (tip 440).
#[test]
fn regtest_verdicts_match_release_dashd() {
    let mut coverage = Coverage::default();
    for oracle in Oracle::all() {
        let data: Value =
            common::load_data(&format!("contextual-regtest-{}.json", oracle.pin().name()));
        let chain = Chain::regtest();
        for point in data["tips"].as_array().unwrap() {
            let headers = headers(point["headers"].as_str().unwrap());
            let deployments = point["deployments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| {
                    let state = ThresholdState::try_from(d[1].as_u64().unwrap() as u8).unwrap();
                    (d[0].as_u64().unwrap() as u8, state)
                })
                .collect();
            let ctx = Context::new(
                0,
                headers.clone(),
                point["bls_legacy"].as_bool().unwrap(),
                deployments,
                point["adjusted_time"].as_i64().unwrap(),
                checkpoints_to_tip(0, &headers),
                CoinsView::Unchecked(vec![]),
                empty_list(), // no masternode was registered on this chain
                vec![],
            );
            let tip = point["tip"].as_i64().unwrap();
            for case in point["blocks"].as_array().unwrap() {
                let block = hex(case["block"].as_str().unwrap());
                let core = case["core_reason"].as_str().map(str::to_string);
                assert!(
                    core.is_some(),
                    "{} {}: every case is a rejection",
                    oracle.pin(),
                    case["name"]
                );
                let ours = first_rejection(&oracle, &chain, &block, &ctx);
                assert_eq!(ours, core, "{} tip {tip} {}", oracle.pin(), case["name"]);
                coverage.hit(core.unwrap());
            }
            // The unmutated block passes every stage (Core mined it on this tip).
            let mined = hex(point["mined_block"].as_str().unwrap());
            assert_eq!(
                first_rejection(&oracle, &chain, &mined, &ctx),
                None,
                "{} tip {tip}",
                oracle.pin()
            );
            for lock in point["sequence_locks"].as_array().unwrap() {
                let tx = hex(lock["tx"].as_str().unwrap());
                let prev_heights: Vec<i32> = lock["prev_heights"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|h| h.as_i64().unwrap() as i32)
                    .collect();
                let locks = oracle.sequence_locks(&chain, &tx, &ctx, &prev_heights, 1).unwrap();
                assert_eq!(
                    locks.satisfied,
                    lock["satisfied"].as_bool().unwrap(),
                    "{} tip {tip} {}: {locks:?}",
                    oracle.pin(),
                    lock["name"]
                );
                coverage.hit(lock["name"].as_str().unwrap().to_string());
            }
        }
    }
    coverage.require_per_pin(&[
        ("bad-diffbits", 2),
        ("time-too-old", 2),
        ("time-too-new", 2),
        ("bad-version(0x00000003)", 2),
        ("bad-txns-nonfinal", 2),
        ("bad-cb-height", 1),
        ("bad-txns-cb-type", 1),
        // Was 3 over two pins: one at v24 and two the v23.1 pin rejected as an unknown type.
        ("bad-txns-type", 1),
        ("bad-protx-payload", 1),
        ("height-satisfied", 2),
        ("height-unsatisfied", 2),
        ("time-satisfied", 2),
        ("time-unsatisfied", 2),
    ]);
}

/// `oracle_versionbits_state` (ABI 12): the oracle's own refusals. The answers themselves are
/// compared with the node's in node-l1 (`diff_versionbits`, `l1_versionbits`).
#[test]
fn versionbits_state_refusals() {
    use dash_oracle_sys::{SignalChange, VbAnswer, VbQuery};
    let ctx = |headers: Vec<[u8; 80]>, first: i32| {
        Context::new(
            first,
            headers,
            false,
            vec![],
            0,
            Checkpoints::Disabled,
            CoinsView::Unchecked(vec![]),
            MnList::Absent,
            vec![],
        )
    };
    let regtest = Chain::new(
        "regtest",
        &["-dip3params=2:2", "-testactivationheight=v20@2", "-testactivationheight=mn_rr@2"],
    );
    for oracle in Oracle::all() {
        let headers = regtest_headers(&oracle, 30);
        let state = VbQuery::State {
            pos: 0,
            height: Some(29),
        };
        let answers =
            oracle.versionbits_state(&regtest, &ctx(headers.clone(), 0), &[], &[state]).unwrap();
        assert_eq!(
            answers,
            [VbAnswer::State(ThresholdState::Defined)],
            "the first 144-block period"
        );

        // a signal change below the headers, a signal above its change, a query above the tip
        let bad: [(&[SignalChange], VbQuery); 3] = [
            (
                &[SignalChange {
                    height: -1,
                    signals: vec![],
                }],
                state,
            ),
            (
                &[SignalChange {
                    height: 10,
                    signals: vec![(13, 11)],
                }],
                state,
            ),
            (
                &[],
                VbQuery::State {
                    pos: 0,
                    height: Some(30),
                },
            ),
        ];
        for (signals, query) in bad {
            let err = oracle
                .versionbits_state(&regtest, &ctx(headers.clone(), 0), signals, &[query])
                .unwrap_err();
            assert_eq!(err.kind, ErrorKind::Argument);
        }

        // a window of 2^32 is a period of 0 as int: refused before Core divides by it
        let zero = Chain::new("regtest", &["-vbparams=testdummy:0:1:0:4294967296:1"]);
        let err =
            oracle.versionbits_state(&zero, &ctx(headers.clone(), 0), &[], &[state]).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument);

        // the stage signals of a placeholder at which V20 is active
        let stage = VbQuery::SignalsStage {
            height: Some(5),
        };
        let err = oracle
            .versionbits_state(&regtest, &ctx(headers[10..].to_vec(), 10), &[], &[stage])
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context);
    }
}
