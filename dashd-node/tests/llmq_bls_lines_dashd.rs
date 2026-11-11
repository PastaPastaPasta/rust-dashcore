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

mod v24;

use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use dash_consensus::Block;
use dash_consensus::header::HeaderParams;
use dash_consensus::primitives::{TxRef, core_serialize, decode_tx_exact};
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

/// The lines of the mined-commitment path among `lines` (LQ2 D25, D26), in order. The
/// member-computation lines have other texts and fall outside it.
fn path_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| {
            l.contains("CFinalCommitment")
                || l.contains("[ProcessBlock]")
                || l.contains("ProcessCommitment -- ")
        })
        .cloned()
        .collect()
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
    }
    eprintln!("{} blocks, {lines} path lines, {built} Built", chain.len());
    assert!(built > 0, "a rotated commitment written");
}
