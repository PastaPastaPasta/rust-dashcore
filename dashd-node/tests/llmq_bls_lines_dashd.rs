//! BL-843: the `-debug=llmq` lines of a block's failing mined-commitment signature checks
//! (`llmq/utils.cpp` v24:561-579, ids from `llmq/commitment.cpp` v24:83-84 and :96), on the
//! SUT's log and against dashd v24.0.0's `debug.log`.
//!
//! The chain is setup-dashd's masternode network (`mn0-qc`, as `mn0_mn_dashd`): blocks 1 to 227
//! go to both nodes, then variants of the rotated mining block 228, whose two commitments queue
//! four checks (each one's members aggregate, then its quorum signature). A variant moves valid
//! signature objects to the wrong field, so `CheckLLMQCommitment` passes and only the queued
//! checks fail: `bad-qc-invalid`.
//!
//! The SUT logs through one global [`CoreSubscriber`] (the BLS threads log too), so the tests
//! here take turns.
//!
//! T6 needs dashd v24.0.0 (see `v24`); T5 runs the SUT alone.
//!
//! BL-849 (LQ2 D26): the other lines of the mined-commitment path (`CheckLLMQCommitment`,
//! `ProcessBlock`, `ProcessCommitment`, with `Verify`'s and `VerifySignatureAsync`'s dumps), the
//! category of each, and the whole path against dashd's, block by block.
//!
//! BL-851 (LQ2 D27): the lines of a mined commitment that fails a structural check, among them
//! the unconditional `[ProcessBlock] failed`, on crafted blocks against dashd's (the edits of
//! BL-851's `reject-lines.py`).
//!
//! BL-850 (LQ2 D28): the rotated member computation's lines join the path: on every block, after
//! a restart of both nodes, and over an undo and its reconnect.

mod v24;

use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use dash_consensus::Block;
use dash_consensus::header::HeaderParams;
use dash_consensus::primitives::{TxRef, core_serialize, decode_tx_exact, write_compact_size};
use dash_primitives::{QuorumHash, SpecialPayload, TxType};
use dashd_node::logging::{CoreSubscriber, Logger};
use dashd_node::sut::NodeSut;
use dashd_node::ui::Ui;
use serde_json::{Value, json};

use v24::{Dashd, blocks, dashd_binary, hex};

/// setup-dashd's masternode network, recorded by dashd v23.1.0 (blocks 1 to 406).
const QC_CHAIN: &str = include_str!("../../dashd-chainstate/tests/data/mn0-qc/blocks.txt");
const QC_ARGS: [&str; 3] =
    ["-dip3params=2:2", "-testactivationheight=v20@100", "-testactivationheight=mn_rr@100"];
/// The rotated mining block whose variants fail.
const ROTATED: usize = 228;

/// The global logger and the lines it has written.
struct Log {
    logger: Arc<Logger>,
    lines: Arc<Mutex<Vec<String>>>,
}

/// The logger behind the global subscriber, installed once.
fn log() -> &'static Log {
    static LOG: OnceLock<Log> = OnceLock::new();
    LOG.get_or_init(|| {
        let logger = Arc::new(Logger::new());
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&lines);
        logger.push_back_callback(move |s| sink.lock().unwrap().push(s.to_owned()));
        assert!(logger.start_logging());
        tracing::subscriber::set_global_default(CoreSubscriber::new(Arc::clone(&logger)))
            .expect("the only global subscriber");
        Log {
            logger,
            lines,
        }
    })
}

/// One test at a time on the global logger, with no category enabled until a SUT's `-debug`.
fn turn() -> MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    let turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(log().logger.disable_category_str("all"));
    turn
}

/// How many lines the SUT has logged.
fn sut_mark() -> usize {
    log().lines.lock().unwrap().len()
}

/// The SUT's lines since `mark`, the timestamp cut.
fn sut_lines_since(mark: usize) -> Vec<String> {
    log().lines.lock().unwrap()[mark..].iter().map(|l| untimed(l)).collect()
}

/// A log line without its timestamp (`2023-11-14T22:13:20Z `, then `(mocktime: <t>) ` under
/// `-mocktime`) and line end.
fn untimed(line: &str) -> String {
    let line = match line.split_once(' ') {
        Some((time, rest)) if time.starts_with("20") && time.ends_with('Z') => rest,
        _ => line,
    };
    let line = match line.strip_prefix("(mocktime: ").and_then(|l| l.split_once(") ")) {
        Some((_, rest)) => rest,
        None => line,
    };
    line.trim_end().to_owned()
}

/// The check lines of `lines` before `ConnectBlock`'s `bad-qc-invalid` line for `block`, which
/// must be there: the lines naming `CFinalCommitment -- q[…] invalid …`, each with its prefix
/// (`[llmq] `), in order.
fn check_lines(lines: &[String], block: &Block) -> Vec<String> {
    let failed = format!(
        "ConnectBlock(DASH): ProcessSpecialTxsInBlock for block {} failed with bad-qc-invalid",
        block.hash()
    );
    let end = lines
        .iter()
        .position(|l| l.contains(&failed))
        .unwrap_or_else(|| panic!("no {failed:?}: {lines:#?}"));
    lines[..end]
        .iter()
        .filter(|l| l.contains("CFinalCommitment -- q[") && l.contains("] invalid "))
        .cloned()
        .collect()
}

/// The texts of the rotated member computation's lines (LQ2 D28), `llmq/utils.cpp` v24:485,
/// :226, :412, :322, :525 and :538.
const MEMBER_LINES: [&str; 6] = [
    "ComputeQuorumMembersByQuarterRotation llmqType[",
    "GetQuorumQuarterMembersBySnapshot h[",
    "BuildNewQuorumQuarterMembers h[",
    "BuildQuorumSnapshot h[",
    "QuarterComposition h[",
    "QuorumComposition h[",
];

/// Whether `line` is one of the rotated member computation's (LQ2 D28).
fn is_member_line(line: &str) -> bool {
    MEMBER_LINES.iter().any(|m| line.contains(m))
}

/// The lines of the mined-commitment path among `lines` (LQ2 D25, D26, D27), with the rotated
/// member computation's (D28), in order.
fn path_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| {
            l.contains("CFinalCommitment")
                || l.contains("[ProcessBlock]")
                || l.contains("ProcessCommitment -- ")
                || l.contains("[GetQuorumBlockHash]")
                || is_member_line(l)
        })
        .cloned()
        .collect()
}

/// The member-computation lines among `lines` (LQ2 D28), in order.
fn member_lines(lines: &[String]) -> Vec<String> {
    lines.iter().filter(|l| is_member_line(l)).cloned().collect()
}

/// dashd's `debug.log` lines from byte `from`, the timestamp cut.
fn dashd_lines_since(debug_log: &std::path::Path, from: usize) -> Vec<String> {
    std::fs::read_to_string(debug_log).unwrap()[from..].lines().map(untimed).collect()
}

/// The length of dashd's `debug.log`.
fn dashd_mark(debug_log: &std::path::Path) -> usize {
    std::fs::read_to_string(debug_log).unwrap().len()
}

/// Both nodes' `-mocktime`: a minute after `chain`'s last block.
fn clock_after(chain: &[Arc<Block>]) -> i64 {
    chain.iter().map(|b| i64::from(b.header().time())).max().expect("blocks") + 60
}

/// The SUT on `args`, its clock after `chain`'s last block, logging through [`log`].
fn sut(dir: &std::path::Path, chain: &[Arc<Block>], args: &[&str]) -> NodeSut {
    let now = clock_after(chain);
    let ui = Arc::new(Ui::with_stderr(Arc::clone(&log().logger), Box::new(|_| {})));
    let argv: Vec<&str> = ["-regtest", "-printtoconsole=0"]
        .into_iter()
        .chain(QC_ARGS)
        .chain(args.iter().copied())
        .collect();
    NodeSut::new(dir, &argv, now, ui).expect("the SUT starts")
}

/// `block` to the SUT, which accepts it: whether V's tip is then `block`. A block that fails to
/// connect is refused through `BlockChecked` and the `ConnectBlock(DASH)` line
/// ([`check_lines`]), not through the accept's state.
fn sut_submit(sut: &NodeSut, block: &Arc<Block>) -> bool {
    let chain = sut.chain().expect("V runs");
    let r = chain.process_block(Arc::clone(block), true, None, true).recv().expect("V");
    assert!(r.accepted && r.state.is_valid(), "{r:?}");
    chain.view().snap.tip.tip_hash == block.hash()
}

/// `tx`, a quorum commitment, with its `quorumSig` and `membersSig` (the last 96 bytes each of
/// the serialized transaction: `commitment.h` v24:95-115 ends the payload, which ends the
/// transaction) taken from `quorum_sig` and `members_sig`: 0 the quorum signature, 1 the members
/// signature.
fn resigned(tx: &TxRef, quorum_sig: usize, members_sig: usize) -> TxRef {
    let mut bytes = core_serialize(&**tx);
    let n = bytes.len();
    let sigs = [bytes[n - 192..n - 96].to_vec(), bytes[n - 96..].to_vec()];
    bytes[n - 192..n - 96].copy_from_slice(&sigs[quorum_sig]);
    bytes[n - 96..].copy_from_slice(&sigs[members_sig]);
    let resigned: TxRef = Arc::new(decode_tx_exact(&bytes).expect("a transaction").into_value());
    let (was, is) = (commitment(tx), commitment(&resigned));
    let was = [was.quorum_sig(), was.members_sig()];
    assert!(is.quorum_sig() == was[quorum_sig] && is.members_sig() == was[members_sig]);
    resigned
}

/// `tx`'s commitment.
fn commitment(tx: &TxRef) -> dash_primitives::Commitment {
    let Some(Ok(d)) = tx.payload() else {
        panic!("a quorum commitment payload");
    };
    let SpecialPayload::QuorumCommitment(p) = d.payload() else {
        panic!("a quorum commitment payload");
    };
    p.commitment().clone()
}

/// The transaction indexes of `block`'s two commitments, quorum index 0 then 1.
fn commitment_txs(block: &Block) -> [usize; 2] {
    let at: Vec<usize> = (0..block.txs().len())
        .filter(|&i| block.txs()[i].tx_type() == TxType::QuorumCommitment)
        .filter(|&i| !commitment(&block.txs()[i]).is_null())
        .collect();
    at.try_into().expect("two commitments")
}

/// Core's line for a check of quorum `q` failing: `members` for the members aggregate.
fn core_line(q: &QuorumHash, members: bool) -> String {
    let mut bytes = *q.as_bytes();
    bytes.reverse();
    let kind = if members {
        "invalid aggregated members signature"
    } else {
        "invalid quorum signature"
    };
    format!("[llmq] CFinalCommitment -- q[{}] {kind}", hex(&bytes))
}

/// A variant of [`ROTATED`]: which checks fail, in push order (first commitment's members,
/// its quorum, second's members, its quorum), and the block.
struct Variant {
    name: &'static str,
    failing: Vec<String>,
    block: Arc<Block>,
}

/// [`ROTATED`] with each commitment's signatures taken as `sigs` gives (quorum's from, members'
/// from), mined again on its parent.
fn variant(
    name: &'static str,
    original: &Block,
    pow: &HeaderParams,
    sigs: [(usize, usize); 2],
) -> Variant {
    let mut txs = original.txs().to_vec();
    let mut failing = Vec::new();
    for (at, (quorum_sig, members_sig)) in commitment_txs(original).into_iter().zip(sigs) {
        let q = commitment(&txs[at]).quorum_hash();
        txs[at] = resigned(&txs[at], quorum_sig, members_sig);
        if members_sig != 1 {
            failing.push(core_line(&q, true));
        }
        if quorum_sig != 0 {
            failing.push(core_line(&q, false));
        }
    }
    let mut block = Block::new(*original.header(), txs);
    let mut tries = u64::MAX;
    assert_eq!(
        dashd_mining::grind(&mut block, pow.pow(), &mut tries, &|| false),
        dashd_mining::Grind::Found
    );
    Variant {
        name,
        failing,
        block: Arc::new(block),
    }
}

/// The variants of [`ROTATED`]: one check failing, the other kind, both of one commitment, and
/// all four.
fn variants(chain: &[Arc<Block>], sut: &NodeSut) -> Vec<Variant> {
    let original = &chain[ROTATED - 1];
    let pow = HeaderParams::from_chain_params(sut.params()).expect("regtest's");
    let keep = (0, 1);
    vec![
        variant("first's quorum signature", original, &pow, [(1, 1), keep]),
        variant("second's members aggregate", original, &pow, [keep, (0, 0)]),
        variant("both of the first", original, &pow, [(1, 0), keep]),
        variant("all four", original, &pow, [(1, 0), (1, 0)]),
    ]
}

/// BL-843 §2.2's rule for `lines` from `failing` with `threads` BLS threads: lines of failing
/// checks only, at most one per thread, at least one, and the one failing check's when one fails.
fn assert_rule(side: &str, lines: &[String], failing: &[String], threads: usize) {
    assert!(!lines.is_empty() && lines.len() <= failing.len().min(threads), "{side}: {lines:#?}");
    let mut left = failing.to_vec();
    for line in lines {
        let at = left.iter().position(|l| l == line);
        left.remove(at.unwrap_or_else(|| panic!("{side}: {line} did not fail")));
    }
    if failing.len() == 1 {
        assert_eq!(lines, failing, "{side}");
    }
}

/// BL-843 T5: the check lines are `LogPrint(BCLog::LLMQ, …)`: none without `-debug=llmq`, and
/// with it Core's line with the `[llmq]` prefix.
#[test]
fn the_check_lines_are_the_llmq_category() {
    let _turn = turn();
    let chain = blocks(QC_CHAIN);
    for debug in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut args = vec!["-parbls=1"];
        if debug {
            args.push("-debug=llmq");
        }
        let sut = sut(dir.path(), &chain, &args);
        for (i, block) in chain[..ROTATED - 1].iter().enumerate() {
            assert!(sut_submit(&sut, block), "V at {}", i + 1);
        }
        let v = variants(&chain, &sut).swap_remove(0);
        let mark = sut_mark();
        assert!(!sut_submit(&sut, &v.block));
        let logged = sut_lines_since(mark);
        let lines = check_lines(&logged, &v.block);
        if debug {
            assert_eq!(lines, v.failing, "{logged:#?}");
        } else {
            assert!(logged.iter().all(|l| !l.contains("CFinalCommitment -- ")), "{logged:#?}");
        }
        assert!(log().logger.disable_category_str("all"));
    }
}

/// BL-843 T6: dashd v24.0.0 and the SUT, both `-debug=llmq`, take blocks 1 to 227 and then each
/// variant of 228: both refuse it `bad-qc-invalid`. At `-parbls=1` they log the same check lines
/// before the `ConnectBlock(DASH)` line; at `-parbls=4` each side's lines follow BL-843 §2.2's
/// rule (which lines depends on the schedule, so the sides are not compared).
#[test]
fn the_check_lines_are_dashds() {
    let Some(binary) = dashd_binary() else {
        return;
    };
    let _turn = turn();
    let chain = blocks(QC_CHAIN);
    let now = clock_after(&chain);
    for parbls in [1, 4] {
        let dir = tempfile::tempdir().unwrap();
        let parbls_arg = format!("-parbls={parbls}");
        let args: Vec<&str> =
            QC_ARGS.into_iter().chain(["-debug=llmq", parbls_arg.as_str()]).collect();
        let dashd = Dashd::start_at(&binary, &dir.path().join("dashd"), now, &args);
        let sut_dir = dir.path().join("rdashd");
        std::fs::create_dir_all(&sut_dir).unwrap();
        let sut = sut(&sut_dir, &chain, &["-debug=llmq", parbls_arg.as_str()]);
        for (i, block) in chain[..ROTATED - 1].iter().enumerate() {
            let answer = dashd.call("submitblock", json!([hex(&core_serialize(&**block))]));
            assert_eq!(answer, Value::Null, "dashd at {}", i + 1);
            assert!(sut_submit(&sut, block), "V at {}", i + 1);
        }
        let debug_log = dashd.net_dir().join("debug.log");
        for v in variants(&chain, &sut) {
            let case = format!("-parbls={parbls}, {}", v.name);
            let logged = dashd_mark(&debug_log);
            let answer = dashd.call("submitblock", json!([hex(&core_serialize(&*v.block))]));
            assert_eq!(answer, json!("bad-qc-invalid"), "dashd, {case}");
            let dashd_lines = dashd_lines_since(&debug_log, logged);
            let dashds = check_lines(&dashd_lines, &v.block);

            let mark = sut_mark();
            assert!(!sut_submit(&sut, &v.block), "V, {case}");
            let sut_lines = sut_lines_since(mark);
            let ours = check_lines(&sut_lines, &v.block);
            let (dashd_path, our_path) = (path_lines(&dashd_lines), path_lines(&sut_lines));

            if parbls == 1 {
                assert_eq!(dashds.len(), 1, "dashd, {case}: {dashds:#?}");
                assert_eq!(ours, dashds, "{case}");
                // LQ2 D26: the whole path, the check line in its place.
                assert_eq!(our_path, dashd_path, "{case}");
            } else {
                assert_rule(&format!("dashd, {case}"), &dashds, &v.failing, parbls);
                assert_rule(&format!("V, {case}"), &ours, &v.failing, parbls);
                let others = |path: Vec<String>| -> Vec<String> {
                    path.into_iter().filter(|l| !v.failing.contains(l)).collect()
                };
                assert_eq!(others(our_path), others(dashd_path), "{case}");
            }
            eprintln!("{case}: dashd {dashds:?}, V {ours:?}");
        }
    }
}

/// BL-849 T5 (LQ2 D26): the count line is Core's `LogPrintf`, logged without `-debug` and
/// without a prefix once per block from the first one rotation is enabled at; every other line
/// of the path is `LogPrint(BCLog::LLMQ, …)`, absent without `-debug=llmq` and `[llmq]` with it.
#[test]
fn the_path_lines_are_their_categories() {
    let _turn = turn();
    let chain = blocks(QC_CHAIN);
    for debug in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut args = vec!["-parbls=1"];
        if debug {
            args.push("-debug=llmq");
        }
        let sut = sut(dir.path(), &chain, &args);
        let mark = sut_mark();
        for (i, block) in chain[..ROTATED].iter().enumerate() {
            assert!(sut_submit(&sut, block), "V at {}", i + 1);
        }
        let path = path_lines(&sut_lines_since(mark));
        let (counts, others): (Vec<&String>, Vec<&String>) =
            path.iter().partition(|l| l.starts_with("[ProcessBlock] h["));
        let heights: Vec<usize> = counts
            .iter()
            .map(|l| {
                let h = l.strip_prefix("[ProcessBlock] h[").and_then(|h| h.split_once(']'));
                h.expect("a height").0.parse().expect("a height")
            })
            .collect();
        let first = *heights.first().expect("count lines");
        assert_eq!(heights, (first..=ROTATED).collect::<Vec<_>>(), "{counts:#?}");
        if debug {
            assert!(!others.is_empty());
            assert!(others.iter().all(|l| l.starts_with("[llmq] ")), "{others:#?}");
            // BL-850 T5 (LQ2 D28): each member-computation line, `-debug=llmq`'s.
            for text in MEMBER_LINES {
                let prefixed = format!("[llmq] {text}");
                assert!(others.iter().any(|l| l.starts_with(&prefixed)), "no {prefixed}");
            }
        } else {
            assert!(others.is_empty(), "{others:#?}");
        }
        assert!(log().logger.disable_category_str("all"));
    }
}

/// BL-849 T6 (LQ2 D26): dashd v24.0.0 and the SUT, both `-debug=llmq -parbls=1`, take every
/// block of the recording; after each, the path lines each logged for it are the same, text and
/// order: rotated and non-rotated types, null commitments, the count line on every block from
/// the first rotated cycle, and `Built`.
#[test]
fn the_path_lines_are_dashds() {
    let Some(binary) = dashd_binary() else {
        return;
    };
    let _turn = turn();
    let chain = blocks(QC_CHAIN);
    let now = clock_after(&chain);
    let dir = tempfile::tempdir().unwrap();
    let args: Vec<&str> = QC_ARGS.into_iter().chain(["-debug=llmq", "-parbls=1"]).collect();
    let dashd = Dashd::start_at(&binary, &dir.path().join("dashd"), now, &args);
    let sut_dir = dir.path().join("rdashd");
    std::fs::create_dir_all(&sut_dir).unwrap();
    let sut = sut(&sut_dir, &chain, &["-debug=llmq", "-parbls=1"]);
    let debug_log = dashd.net_dir().join("debug.log");
    let (mut lines, mut built) = (0, 0);
    let mut members = [0usize; MEMBER_LINES.len()];
    for (i, block) in chain.iter().enumerate() {
        let logged = dashd_mark(&debug_log);
        let answer = dashd.call("submitblock", json!([hex(&core_serialize(&**block))]));
        assert_eq!(answer, Value::Null, "dashd at {}", i + 1);
        let dashds = path_lines(&dashd_lines_since(&debug_log, logged));
        let mark = sut_mark();
        assert!(sut_submit(&sut, block), "V at {}", i + 1);
        let ours = path_lines(&sut_lines_since(mark));
        assert_eq!(ours, dashds, "block {}", i + 1);
        lines += ours.len();
        built += ours.iter().filter(|l| l.ends_with(" Built")).count();
        for (count, text) in members.iter_mut().zip(MEMBER_LINES) {
            *count += ours.iter().filter(|l| l.contains(text)).count();
        }
    }
    eprintln!(
        "{} blocks, {lines} path lines, {built} Built, {members:?} member lines",
        chain.len()
    );
    assert!(built > 0, "a rotated commitment written");
    // BL-850 T4 (LQ2 D28): dashd v24.0.0's member lines over the recording, M1 to M6.
    assert_eq!(members, [16, 27, 11, 11, 32, 32], "each member line compared");
}

/// BL-850 T6 and T7 (LQ2 D28): dashd v24.0.0 and the SUT, both `-debug=llmq -parbls=1`, take
/// blocks 1 to 299 and restart on their datadirs, with empty member caches. Neither logs a member
/// line while it starts; block 300, which mines cycle 288's commitments, takes the miss in its
/// first commitment's `Verify`: the path lines, member lines among them, are dashd's, the member
/// lines between the first `CheckLLMQCommitment` dump and the first `Verify` dump. Then the rest
/// of the chain, `invalidateblock` of cycle base 384 (the undo's precompute resets the caches and
/// logs the cycle again) and `reconsiderblock` (the reconnect hits the refilled caches and logs
/// none): each node's member lines are dashd's.
#[test]
fn the_member_lines_after_a_restart_and_over_an_undo_are_dashds() {
    let Some(binary) = dashd_binary() else {
        return;
    };
    let _turn = turn();
    let chain = blocks(QC_CHAIN);
    let now = clock_after(&chain);
    let dir = tempfile::tempdir().unwrap();
    let args: Vec<&str> = QC_ARGS.into_iter().chain(["-debug=llmq", "-parbls=1"]).collect();
    let dashd_dir = dir.path().join("dashd");
    let mut dashd = Dashd::start_at(&binary, &dashd_dir, now, &args);
    let sut_dir = dir.path().join("rdashd");
    std::fs::create_dir_all(&sut_dir).unwrap();
    let mut sut = sut(&sut_dir, &chain, &["-debug=llmq", "-parbls=1"]);
    let debug_log = dashd.net_dir().join("debug.log");
    feed(&dashd, &sut, &chain, 299);

    let (logged, mark) = (dashd_mark(&debug_log), sut_mark());
    drop(dashd);
    dashd = Dashd::start_at(&binary, &dashd_dir, now, &args);
    sut.restart(dashd_node::sut::Teardown::Clean);
    assert_eq!(member_lines(&dashd_lines_since(&debug_log, logged)), Vec::<String>::new());
    assert_eq!(member_lines(&sut_lines_since(mark)), Vec::<String>::new(), "the SUT's start");

    let (logged, mark) = (dashd_mark(&debug_log), sut_mark());
    feed(&dashd, &sut, &chain, 300);
    let dashds = path_lines(&dashd_lines_since(&debug_log, logged));
    let ours = path_lines(&sut_lines_since(mark));
    assert_eq!(ours, dashds, "block 300 after the restart");
    let members: Vec<usize> =
        ours.iter().enumerate().filter(|(_, l)| is_member_line(l)).map(|(i, _)| i).collect();
    assert_eq!(members.len(), 10, "cycle 288: {ours:#?}");
    let check = ours.iter().position(|l| l.contains("CheckLLMQCommitment llmqType["));
    let verify = ours.iter().position(|l| l.contains("CFinalCommitment::Verify "));
    assert_eq!(check, Some(members[0] - 1), "{ours:#?}");
    assert_eq!(verify, Some(members[9] + 1), "{ours:#?}");

    feed(&dashd, &sut, &chain, chain.len());
    let (undone, before) = (chain[383].hash(), chain[382].hash());
    let v = sut.chain().expect("V runs");
    let (logged, mark) = (dashd_mark(&debug_log), sut_mark());
    dashd.call("invalidateblock", json!([undone.to_string()]));
    v.invalidate_block(undone).recv().expect("V").expect("in the index");
    assert_eq!(v.view().snap.tip.tip_hash, before, "384 undone");
    let dashds = member_lines(&dashd_lines_since(&debug_log, logged));
    assert_eq!(dashds.len(), 10, "dashd's reset: {dashds:#?}");
    assert!(dashds[0].contains("nHeight[384]"), "{dashds:#?}");
    assert_eq!(member_lines(&sut_lines_since(mark)), dashds, "invalidateblock 384");

    let (logged, mark) = (dashd_mark(&debug_log), sut_mark());
    dashd.call("reconsiderblock", json!([undone.to_string()]));
    v.reconsider_block(undone, false).recv().expect("V").expect("in the index");
    assert_eq!(v.view().snap.tip.tip_hash, chain[chain.len() - 1].hash(), "reconnected");
    assert_eq!(member_lines(&dashd_lines_since(&debug_log, logged)), Vec::<String>::new());
    assert_eq!(member_lines(&sut_lines_since(mark)), Vec::<String>::new(), "reconsiderblock");
}

// ---- BL-851 (LQ2 D27): the lines of a mined commitment that fails ----

/// A Bitcoin compact size at `bytes[*at..]`, advancing `at`.
fn compact_size(bytes: &[u8], at: &mut usize) -> u64 {
    let first = bytes[*at];
    let width = match first {
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
        _ => {
            *at += 1;
            return u64::from(first);
        }
    };
    let mut n = [0u8; 8];
    n[..width].copy_from_slice(&bytes[*at + 1..*at + 1 + width]);
    *at += 1 + width;
    u64::from_le_bytes(n)
}

/// A `CFinalCommitmentTxPayload`, field by field (`llmq/commitment.h` v24:95-115, :178-190), as
/// `reject-lines.py` edits it: the index is written for versions 2 and 4 only.
#[derive(Clone)]
struct Payload {
    version: u16,
    height: u32,
    qc_version: u16,
    llmq_type: u8,
    quorum_hash: [u8; 32],
    index: i16,
    signers: (u64, Vec<u8>),
    valid: (u64, Vec<u8>),
    key: Vec<u8>,
    vvec: Vec<u8>,
    quorum_sig: Vec<u8>,
    members_sig: Vec<u8>,
}

impl Payload {
    fn indexed(version: u16) -> bool {
        version == 2 || version == 4
    }

    fn parse(p: &[u8]) -> Payload {
        let u16_at = |at: usize| u16::from_le_bytes([p[at], p[at + 1]]);
        let mut at = 9;
        let quorum_hash = p[at..at + 32].try_into().expect("32 bytes");
        at += 32;
        let qc_version = u16_at(6);
        let mut index = 0;
        if Self::indexed(qc_version) {
            index = u16_at(at) as i16;
            at += 2;
        }
        let bitset = |at: &mut usize| {
            let n = compact_size(p, at);
            let len = n.div_ceil(8) as usize;
            let bits = p[*at..*at + len].to_vec();
            *at += len;
            (n, bits)
        };
        let signers = bitset(&mut at);
        let valid = bitset(&mut at);
        let mut take = |n: usize| {
            at += n;
            p[at - n..at].to_vec()
        };
        let (key, vvec, quorum_sig, members_sig) = (take(48), take(32), take(96), take(96));
        assert_eq!(at, p.len());
        Payload {
            version: u16_at(0),
            height: u32::from_le_bytes(p[2..6].try_into().expect("4 bytes")),
            qc_version,
            llmq_type: p[8],
            quorum_hash,
            index,
            signers,
            valid,
            key,
            vvec,
            quorum_sig,
            members_sig,
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend(self.version.to_le_bytes());
        p.extend(self.height.to_le_bytes());
        p.extend(self.qc_version.to_le_bytes());
        p.push(self.llmq_type);
        p.extend(self.quorum_hash);
        if Self::indexed(self.qc_version) {
            p.extend(self.index.to_le_bytes());
        }
        for (n, bits) in [&self.signers, &self.valid] {
            write_compact_size(*n, &mut p);
            p.extend(bits);
        }
        for field in [&self.key, &self.vvec, &self.quorum_sig, &self.members_sig] {
            p.extend(field);
        }
        p
    }

    fn is_null(&self) -> bool {
        self.signers.1.iter().chain(&self.valid.1).all(|&b| b == 0) && self.key == [0; 48]
    }

    /// This payload with `edit` applied.
    fn with(&self, edit: impl FnOnce(&mut Payload)) -> Payload {
        let mut p = self.clone();
        edit(&mut p);
        p
    }
}

/// `tx` with its extra payload replaced by `payload`, which need not decode.
fn with_payload(tx: &TxRef, payload: &[u8]) -> TxRef {
    let bytes = core_serialize(&**tx);
    let old = tx.extra_payload();
    let mut size = Vec::new();
    write_compact_size(old.len() as u64, &mut size);
    let mut out = bytes[..bytes.len() - old.len() - size.len()].to_vec();
    write_compact_size(payload.len() as u64, &mut out);
    out.extend(payload);
    let tx: TxRef = Arc::new(decode_tx_exact(&out).expect("a transaction").into_value());
    assert_eq!(tx.extra_payload(), payload);
    tx
}

/// The quorum-commitment transactions of `block`: their indexes and payloads.
fn payloads(block: &Block) -> Vec<(usize, Payload)> {
    block
        .txs()
        .iter()
        .enumerate()
        .filter(|(_, tx)| tx.tx_type() == TxType::QuorumCommitment)
        .map(|(i, tx)| (i, Payload::parse(tx.extra_payload())))
        .collect()
}

/// `block`'s transactions with the payload of the one at `at` replaced.
fn replaced(block: &Block, at: usize, payload: &[u8]) -> Vec<TxRef> {
    let mut txs = block.txs().to_vec();
    txs[at] = with_payload(&txs[at], payload);
    txs
}

/// A crafted block: dashd's answer to it, and the block, mined on `original`'s parent.
struct Crafted {
    name: String,
    reason: &'static str,
    block: Arc<Block>,
}

fn crafted(
    name: &str,
    reason: &'static str,
    original: &Block,
    txs: Vec<TxRef>,
    pow: &HeaderParams,
) -> Crafted {
    let mut block = Block::new(*original.header(), txs);
    let mut tries = u64::MAX;
    assert_eq!(
        dashd_mining::grind(&mut block, pow.pow(), &mut tries, &|| false),
        dashd_mining::Grind::Found
    );
    Crafted {
        name: name.to_owned(),
        reason,
        block: Arc::new(block),
    }
}

/// The parents of BL-851's crafted blocks, and the blocks on each: `reject-lines.py`'s, in its
/// order (`tools/reject-lines.out` has dashd's answers and lines).
fn crafted_blocks(chain: &[Arc<Block>], pow: &HeaderParams) -> Vec<(usize, Vec<Crafted>)> {
    const INVALID: &str = "bad-qc-invalid";
    let at = |h: usize| &*chain[h - 1];
    let real = |h: usize, llmq_type: u8| {
        payloads(at(h)).into_iter().find(|(_, p)| p.llmq_type == llmq_type && !p.is_null())
    };
    // `b` with the payload of its transaction `i` replaced.
    let swap = |name: &str, reason, b: &Block, i, payload: &[u8]| {
        crafted(name, reason, b, replaced(b, i, payload), pow)
    };
    let (i227, real100) = real(227, 100).expect("227's type-100 commitment");
    let mut stages = Vec::new();

    // A non-null commitment on a base with fewer members than its size: the bit checks.
    for h in [34, 130, 154] {
        let b = at(h);
        let (i, null) = payloads(b).into_iter().find(|(_, p)| p.llmq_type == 100).expect("100");
        let full = real100.with(|p| {
            (p.height, p.qc_version, p.quorum_hash) =
                (null.height, null.qc_version, null.quorum_hash);
            p.signers.1 = vec![0x07];
            p.valid.1 = vec![0x07];
        });
        let split = full.with(|p| {
            p.signers.1 = vec![0x03];
            p.valid.1 = vec![0x06];
        });
        stages.push((
            h - 1,
            vec![
                swap(&format!("{h}: every bit set"), INVALID, b, i, &full.bytes()),
                swap(&format!("{h}: signers 0, 1, valid 1, 2"), INVALID, b, i, &split.bytes()),
            ],
        ));
    }

    // `CheckLLMQCommitment` and `Verify` on 227's commitments.
    let b = at(227);
    let i = i227;
    let (ni, nc) = payloads(b).into_iter().find(|(_, p)| p.is_null()).expect("a null commitment");
    let genesis = *chain[0].header().prev_hash().as_bytes();
    let hash215 = *chain[214].hash().as_bytes();
    let edit = |e: &dyn Fn(&mut Payload)| real100.with(e).bytes();
    let resized = |n: isize| {
        let mut p = real100.bytes();
        p.resize(p.len().checked_add_signed(n).expect("a length"), 0);
        p
    };
    let cases: Vec<(&str, &'static str, usize, Vec<u8>)> = vec![
        ("227 payload cut by a byte", "bad-qc-payload", i, resized(-1)),
        ("227 payload with a byte more", "bad-qc-payload", i, resized(1)),
        ("227 llmqType 99", "bad-qc-commitment-type", i, edit(&|p| p.llmq_type = 99)),
        ("227 qcTx.nVersion 2", "bad-qc-version", i, edit(&|p| p.version = 2)),
        ("227 qcTx.nVersion 0", "bad-qc-version", i, edit(&|p| p.version = 0)),
        ("227 qcTx.nHeight 229", "bad-qc-height", i, edit(&|p| p.height = 229)),
        ("227 unknown quorumHash", "bad-qc-quorum-hash", i, edit(&|p| p.quorum_hash = [0x42; 32])),
        ("227 nVersion 1", INVALID, i, edit(&|p| p.qc_version = 1)),
        ("227 nVersion 0", INVALID, i, edit(&|p| p.qc_version = 0)),
        ("227 quorumHash genesis", INVALID, i, edit(&|p| p.quorum_hash = genesis)),
        ("227 quorumHash of 215", INVALID, i, edit(&|p| p.quorum_hash = hash215)),
        ("227 signers.size 4", INVALID, i, edit(&|p| p.signers.0 = 4)),
        ("227 validMembers.size 4", INVALID, i, edit(&|p| p.valid.0 = 4)),
        ("227 validMembers 1 bit", INVALID, i, edit(&|p| p.valid.1 = vec![0x01])),
        ("227 signers 1 bit", INVALID, i, edit(&|p| p.signers.1 = vec![0x01])),
        ("227 quorumPublicKey ff", "bad-qc-payload", i, edit(&|p| p.key = vec![0xff; 48])),
        ("227 quorumPublicKey zeros", INVALID, i, edit(&|p| p.key = vec![0; 48])),
        ("227 membersSig zeros", INVALID, i, edit(&|p| p.members_sig = vec![0; 96])),
        ("227 quorumSig zeros", INVALID, i, edit(&|p| p.quorum_sig = vec![0; 96])),
        ("227 both sizes 4", INVALID, i, edit(&|p| (p.signers.0, p.valid.0) = (4, 4))),
        ("227 quorumVvecHash null", INVALID, i, edit(&|p| p.vvec = vec![0; 32])),
        ("227 membersSig ff", "bad-qc-payload", i, edit(&|p| p.members_sig = vec![0xff; 96])),
        ("227 quorumSig ff", "bad-qc-payload", i, edit(&|p| p.quorum_sig = vec![0xff; 96])),
        (
            "227 null commitment signers.size 4",
            "bad-qc-invalid-null",
            ni,
            nc.with(|p| p.signers.0 = 4).bytes(),
        ),
        (
            "227 null commitment validMembers.size 2",
            "bad-qc-invalid-null",
            ni,
            nc.with(|p| p.valid.0 = 2).bytes(),
        ),
    ];
    stages.push((226, cases.into_iter().map(|(n, r, i, p)| swap(n, r, b, i, &p)).collect()));

    // The rotated mining block 228.
    let b = at(228);
    let rotated: Vec<(usize, Payload)> =
        payloads(b).into_iter().filter(|(_, p)| !p.is_null()).collect();
    let [(i0, c0), (i1, c1)] = <[_; 2]>::try_from(rotated).ok().expect("two commitments");
    stages.push((
        227,
        vec![
            swap("228 first's quorumIndex 1", INVALID, b, i0, &c0.with(|p| p.index = 1).bytes()),
            swap("228 second's quorumIndex -1", INVALID, b, i1, &c1.with(|p| p.index = -1).bytes()),
            swap("228 second's nVersion 2", INVALID, b, i1, &c1.with(|p| p.qc_version = 2).bytes()),
            swap("228 second's nVersion 3", INVALID, b, i1, &c1.with(|p| p.qc_version = 3).bytes()),
            swap("228 second is the first again", "bad-qc-dup", b, i1, &c0.bytes()),
        ],
    ));

    // `ProcessCommitment`'s `bad-qc-block`: an older cycle's commitment, its signatures valid.
    let b = at(251);
    let (i, _) = real(251, 100).expect("251's type-100 commitment");
    let old = real100.with(|p| p.height = 251).bytes();
    stages
        .push((250, vec![swap("251 with 227's type-100 commitment", "bad-qc-block", b, i, &old)]));
    let b = at(252);
    let now: Vec<usize> =
        payloads(b).into_iter().filter(|(_, p)| !p.is_null()).map(|(i, _)| i).collect();
    let at252 = |p: &Payload| p.with(|p| p.height = 252).bytes();
    stages.push((
        251,
        vec![
            swap("252 with 228's index-0 commitment first", "bad-qc-block", b, now[0], &at252(&c0)),
            swap(
                "252 with 228's index-1 commitment second",
                "bad-qc-block",
                b,
                now[1],
                &at252(&c1),
            ),
        ],
    ));
    // One of the cycle's commitments mined again.
    let b = at(253);
    let mut txs = b.txs().to_vec();
    let first = &at(252).txs()[now[0]];
    txs.push(with_payload(
        first,
        &Payload::parse(first.extra_payload()).with(|p| p.height = 253).bytes(),
    ));
    stages.push((
        252,
        vec![crafted("253 with 252's first commitment again", "bad-qc-not-allowed", b, txs, pow)],
    ));
    stages
}

/// The reason `lines` give for `block`'s connect failing, from `ConnectBlock(DASH)`'s line.
fn connect_reason(lines: &[String], block: &Block) -> Option<String> {
    let failed = format!(
        "ConnectBlock(DASH): ProcessSpecialTxsInBlock for block {} failed with ",
        block.hash()
    );
    lines.iter().find_map(|l| {
        let rest = &l[l.find(&failed)? + failed.len()..];
        Some(rest.split(',').next().unwrap_or(rest).to_owned())
    })
}

/// `block` to the SUT: its refusal's reason, from its lines since `mark`, or what went otherwise.
fn sut_reason(sut: &NodeSut, block: &Arc<Block>, mark: usize) -> String {
    let chain = sut.chain().expect("V runs");
    let r = chain.process_block(Arc::clone(block), true, None, true).recv().expect("V");
    if !(r.accepted && r.state.is_valid()) {
        return format!("not accepted: {r:?}");
    }
    if chain.view().snap.tip.tip_hash == block.hash() {
        return "connected".to_owned();
    }
    connect_reason(&sut_lines_since(mark), block)
        .unwrap_or_else(|| "no ConnectBlock(DASH) line".to_owned())
}

/// Blocks of `chain` from dashd's height to `upto` to both nodes, which accept each.
fn feed(dashd: &Dashd, sut: &NodeSut, chain: &[Arc<Block>], upto: usize) {
    let have = dashd.call("getblockcount", json!([])).as_u64().expect("a height") as usize;
    for (i, block) in chain.iter().enumerate().take(upto).skip(have) {
        let answer = dashd.call("submitblock", json!([hex(&core_serialize(&**block))]));
        assert_eq!(answer, Value::Null, "dashd at {}", i + 1);
        assert!(sut_submit(sut, block), "V at {}", i + 1);
    }
}

/// BL-851 T5 (LQ2 D27): `[ProcessBlock] failed` is Core's `LogPrintf`, with no prefix and without
/// `-debug`; `ProcessCommitment`'s reject line before it is `-debug=llmq`'s, with `[llmq]`.
#[test]
fn the_failed_line_is_unconditional() {
    let _turn = turn();
    let chain = blocks(QC_CHAIN);
    for debug in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut args = vec!["-parbls=1"];
        if debug {
            args.push("-debug=llmq");
        }
        let sut = sut(dir.path(), &chain, &args);
        for (i, block) in chain[..250].iter().enumerate() {
            assert!(sut_submit(&sut, block), "V at {}", i + 1);
        }
        let pow = HeaderParams::from_chain_params(sut.params()).expect("regtest's");
        let stages = crafted_blocks(&chain, &pow);
        let (_, blocks) = stages.iter().find(|(parent, _)| *parent == 250).expect("251's");
        let block = &blocks[0].block;
        let mark = sut_mark();
        assert!(!sut_submit(&sut, block));
        let path = path_lines(&sut_lines_since(mark));
        let failed: Vec<&String> =
            path.iter().filter(|l| l.contains("[ProcessBlock] failed")).collect();
        assert_eq!(failed.len(), 1, "{path:#?}");
        assert!(failed[0].starts_with("[ProcessBlock] failed h[251] llmqType[100] "), "{path:#?}");
        let rejected = path.iter().filter(|l| l.ends_with(" non equal quorumHash."));
        if debug {
            let rejected: Vec<&String> = rejected.collect();
            assert_eq!(rejected.len(), 1, "{path:#?}");
            assert!(rejected[0].starts_with("[llmq] ProcessCommitment -- height=251, "));
        } else {
            assert_eq!(rejected.count(), 0, "{path:#?}");
            // The count line and `failed`: Core's two `LogPrintf`s of the path.
            assert_eq!(path.len(), 2, "{path:#?}");
            assert!(path[0].starts_with("[ProcessBlock] h[251] numCommitmentsRequired["));
        }
        assert!(log().logger.disable_category_str("all"));
    }
}

/// BL-851 T6 (LQ2 D27): dashd v24.0.0 and the SUT, both `-debug=llmq`, take the recording up to
/// each crafted block's parent, then the crafted blocks: dashd answers each with the expected
/// reason, the SUT refuses it with the same one, and the path lines each logged for it are the
/// same, text and order. At `-parbls=1` for every block, at `-parbls=4` too for those that fail
/// in `ProcessCommitment` (no check fails there).
#[test]
fn the_reject_lines_are_dashds() {
    let Some(binary) = dashd_binary() else {
        return;
    };
    let _turn = turn();
    let chain = blocks(QC_CHAIN);
    let now = clock_after(&chain);
    for parbls in [1, 4] {
        let started = std::time::Instant::now();
        let dir = tempfile::tempdir().unwrap();
        let parbls_arg = format!("-parbls={parbls}");
        let args: Vec<&str> =
            QC_ARGS.into_iter().chain(["-debug=llmq", parbls_arg.as_str()]).collect();
        let dashd = Dashd::start_at(&binary, &dir.path().join("dashd"), now, &args);
        let sut_dir = dir.path().join("rdashd");
        std::fs::create_dir_all(&sut_dir).unwrap();
        let sut = sut(&sut_dir, &chain, &["-debug=llmq", parbls_arg.as_str()]);
        let pow = HeaderParams::from_chain_params(sut.params()).expect("regtest's");
        let debug_log = dashd.net_dir().join("debug.log");
        let (mut blocks, mut lines) = (0, 0);
        for (parent, crafted) in crafted_blocks(&chain, &pow) {
            if parbls != 1 && !(250..=251).contains(&parent) {
                continue;
            }
            feed(&dashd, &sut, &chain, parent);
            for c in crafted {
                let case = format!("-parbls={parbls}, {}", c.name);
                let logged = dashd_mark(&debug_log);
                let answer = dashd.call("submitblock", json!([hex(&core_serialize(&*c.block))]));
                assert_eq!(answer, json!(c.reason), "dashd, {case}");
                let dashd_lines = dashd_lines_since(&debug_log, logged);
                assert_eq!(
                    connect_reason(&dashd_lines, &c.block).as_deref(),
                    Some(c.reason),
                    "dashd, {case}"
                );
                let mark = sut_mark();
                let reason = sut_reason(&sut, &c.block, mark);
                assert_eq!(reason, c.reason, "V, {case}");
                let ours = path_lines(&sut_lines_since(mark));
                assert_eq!(ours, path_lines(&dashd_lines), "{case}");
                blocks += 1;
                lines += ours.len();
            }
        }
        eprintln!(
            "-parbls={parbls}: {blocks} crafted blocks, {lines} path lines, {:?}",
            started.elapsed()
        );
    }
}
