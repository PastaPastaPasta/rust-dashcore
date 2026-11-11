//! BL-647: H4's step 6 runs a block's mined-commitment signature checks on the runner its
//! `NodeEvoParts` give (Core's `m_bls_queue`, LQ2 D11), and `BlsCheckQueue` at any thread count
//! gives what `SerialBlsChecks` gives on dashd's masternode network: the same verdicts, reasons
//! and rows, with signatures failing beside a structural failure, an unknown base or a
//! second bad signature (`llmq/blockprocessor.cpp` v24:267-290).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dash_chain::Status;
use dash_consensus::coins::CoinsView;
use dash_consensus::primitives::{Tx, TxRef};
use dash_consensus::{Block, TxValidationError};
use dash_dml::specialtx::QuorumSeams;
use dash_evo::llmq::{
    BlsCheck, BlsCheckRunner, BlsVerdict, CommitmentReject, LlmqCommitment, SerialBlsChecks,
};
use dash_primitives::{BlockHash, CoinbaseCommitment, QuorumHash, SpecialPayload, TxType};

use super::evo_chain::{OpenSeams, QC_CHAIN, decode, linked, node_config_with, qc_chain_params};
use super::node_evo::qc_tx;
use super::support::{CheckedAt, Mined, Rig, RigConfig, mine_version};
use crate::chain::{Fatal, RecordingAbort};
use crate::checkqueue::bls::BlsCheckQueue;
use crate::connect::{NodeHelper, SpecialTxFailure};
use crate::evo::cl::AttemptClVerifier;
use crate::evo::ev1::{DmlChecks, Ev1Checks, SpecialTxCtx};
use crate::store::CoinsReadError;

/// The recording's mining blocks to its fourth: `llmq_test` at 227 and 251, `llmq_test_dip0024`
/// at 228 and 252 (two commitments there, quorum indexes 0 and 1).
const MINING: [u32; 4] = [227, 228, 251, 252];
/// A rotated mining block, and the end of the runs.
const ROTATED: u32 = 228;
const END: u32 = 252;

/// A runner that counts its calls, those with checks, and the checks, then runs them on `inner`.
struct Counting {
    inner: Arc<dyn BlsCheckRunner + Send + Sync>,
    calls: AtomicUsize,
    with_checks: AtomicUsize,
    checks: AtomicUsize,
}

impl BlsCheckRunner for Counting {
    fn run(&self, checks: Vec<BlsCheck>) -> BlsVerdict {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.with_checks.fetch_add(usize::from(!checks.is_empty()), Ordering::Relaxed);
        self.checks.fetch_add(checks.len(), Ordering::Relaxed);
        self.inner.run(checks)
    }
}

/// Serial checks, then the queue at 0, 1, 3 and 8 threads.
fn runners() -> Vec<(String, Arc<dyn BlsCheckRunner + Send + Sync>)> {
    let mut all: Vec<(String, Arc<dyn BlsCheckRunner + Send + Sync>)> =
        vec![("serial".to_owned(), Arc::new(SerialBlsChecks))];
    for threads in [0, 1, 3, 8] {
        let queue = BlsCheckQueue::new(threads).expect("the threads");
        all.push((format!("queue({threads})"), Arc::new(queue)));
    }
    all
}

/// The node's EV1 checks, but step 4 passes the type-6 transactions of the block `skip`, so
/// step 6 judges commitments step 4 would refuse first.
struct SkipQcAt(Option<BlockHash>);

impl Ev1Checks for SkipQcAt {
    type Payloads<'b> = <DmlChecks as Ev1Checks>::Payloads<'b>;

    fn payloads<'b>(&self, block: &'b Block) -> Self::Payloads<'b> {
        DmlChecks.payloads(block)
    }

    fn check_cbtx(
        &self,
        block: &Block,
        payloads: &Self::Payloads<'_>,
        cx: SpecialTxCtx<'_>,
    ) -> Result<Result<CoinbaseCommitment, SpecialTxFailure>, Fatal> {
        DmlChecks.check_cbtx(block, payloads, cx)
    }

    fn check_tx<'a>(
        &self,
        block: &Block,
        payloads: &Self::Payloads<'_>,
        i: usize,
        cx: SpecialTxCtx<'a>,
        quorum: Option<QuorumSeams<'a>>,
    ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
        let tx = &block.txs()[i];
        if Some(block.hash()) == self.0 && tx.tx_type() == TxType::QuorumCommitment {
            return Ok(Ok(()));
        }
        DmlChecks.check_tx(block, payloads, i, cx, quorum)
    }

    fn shared_collateral_spends(
        &self,
        tx: &Tx,
        view: &dyn CoinsView<Error = CoinsReadError>,
    ) -> Result<Result<(), TxValidationError>, Fatal> {
        DmlChecks.shared_collateral_spends(tx, view)
    }
}

/// One block's submit: whether it is valid in V's index and the `BlockChecked` states it got.
type Outcome = (u32, bool, String);
/// An `AbortNode` report: its log message and user message.
type Report = (String, Option<String>);
/// A runner's calls, calls with checks, and checks.
type Calls = (usize, usize, usize);
/// A runner's name and its lines for [`ROTATED`] ([`Ran::lines`]).
type RunnerLines = (String, Vec<String>);

/// What a run gives: each submitted block's outcome, the engine's rows and the layer's live mined
/// keys at the end, the abort reports, the runner's calls and checks, and the lines naming
/// [`ROTATED`]'s quorums or its failure ([`rotated_lines`]).
struct Ran {
    outcomes: Vec<Outcome>,
    stored: String,
    mined: Vec<Vec<u8>>,
    reports: Vec<Report>,
    calls: Calls,
    lines: Vec<String>,
}

/// Held by the tests whose [`ROTATED`] fails a signature, so that the lines naming its quorums
/// are one run's (BL-843).
static ROTATED_LINES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `hash` as Core's `uint256::ToString` prints it.
fn core_hex(hash: &QuorumHash) -> String {
    hash.as_bytes().iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// The lines logged so far that name `block`'s quorums (`CFinalCommitment -- q[…]`), or that are
/// `ConnectBlock`'s `ProcessSpecialTxsInBlock` failure line for it, in order.
fn rotated_lines(block: &Mined) -> Vec<String> {
    let quorums: Vec<String> = commitments(&block.block)
        .iter()
        .map(|(_, qc)| format!("CFinalCommitment -- q[{}]", core_hex(&qc.quorum_hash())))
        .collect();
    let failed =
        format!("ConnectBlock(DASH): ProcessSpecialTxsInBlock for block {} ", block.hash());
    super::support::lines_where(|l| {
        l.starts_with(&failed) || quorums.iter().any(|q| l.starts_with(q.as_str()))
    })
}

/// The recording to [`END`], `edit` applied (each edited block mined again on its parent),
/// submitted to a node V on `runner` with the node's EV1 checks (step 4 passing the type-6
/// transactions of the edited [`ROTATED`] when `skip`), until the first block V does not connect,
/// or the block at `stops`, whose connect stops V.
fn run(
    runner: Arc<dyn BlsCheckRunner + Send + Sync>,
    skip: bool,
    stops: Option<u32>,
    edit: &dyn Fn(&Mined) -> Option<Vec<TxRef>>,
) -> Ran {
    let p = qc_chain_params();
    let genesis = Mined::genesis(&p);
    let mut blocks = linked(&genesis, decode(QC_CHAIN));
    blocks.truncate(END as usize);
    let mut prev = genesis.hash();
    for m in &mut blocks {
        if m.block.header().prev_hash() != prev {
            break;
        }
        if let Some(txs) = edit(m) {
            let raw = m.block.header().raw();
            m.block = mine_version(&p, prev, raw.time(), txs, raw.version());
        }
        prev = m.hash();
    }
    let skip = skip.then(|| blocks[ROTATED as usize - 1].hash());
    super::support::install_accept_log();
    let logged_before = rotated_lines(&blocks[ROTATED as usize - 1]).len();
    let last = blocks.iter().map(|m| i64::from(m.time())).max().expect("blocks");
    let counting = Arc::new(Counting {
        inner: runner,
        calls: AtomicUsize::new(0),
        with_checks: AtomicUsize::new(0),
        checks: AtomicUsize::new(0),
    });
    let seams = OpenSeams {
        ev1: SkipQcAt(skip),
        cl: Box::new(AttemptClVerifier::default()),
        helper: Box::new(NodeHelper),
        blocks: Box::new(|b| b),
        bls_checks: counting.clone(),
        // Core's nesting: the BLS control inside a threaded script control.
        script_threads: 3,
    };
    let after_genesis = last - i64::from(genesis.time()) + 86_400;
    let (cfg, kept) = node_config_with(p, after_genesis, RigConfig::default(), seams);
    let rig = Rig::with(cfg);
    let checked = CheckedAt::on(&rig.signals, Arc::new(RecordingAbort::default()));
    let mut outcomes = Vec::new();
    for m in &blocks {
        if stops == Some(m.height) {
            rig.submit_stopping(m, "the reasonless refusal stops V");
            outcomes.push((m.height, false, String::new()));
            break;
        }
        let r = rig.submit(m);
        let valid = r.accepted
            && r.state.is_valid()
            && !rig.status(m).has(Status::FAILED_MASK)
            && rig.status(m).is_valid(Status::VALID_SCRIPTS);
        let states: Vec<String> =
            checked.calls_for(m.hash()).iter().map(|c| format!("{:?}", c.state)).collect();
        outcomes.push((m.height, valid, states.join("; ")));
        if !valid {
            break;
        }
    }
    if stops.is_none() {
        kept.assert_no_abort(&rig);
    }
    let lines = rotated_lines(&blocks[ROTATED as usize - 1]).split_off(logged_before);
    Ran {
        outcomes,
        stored: format!("{:?}", kept.stored().0),
        mined: kept.resident_mined(),
        reports: kept.reports(),
        calls: (
            counting.calls.load(Ordering::Relaxed),
            counting.with_checks.load(Ordering::Relaxed),
            counting.checks.load(Ordering::Relaxed),
        ),
        lines,
    }
}

/// The non-null commitments of `block`'s type-6 transactions, by transaction index.
fn commitments(block: &Block) -> Vec<(usize, LlmqCommitment)> {
    let mut out = Vec::new();
    for (i, tx) in block.txs().iter().enumerate() {
        if let Some(Ok(d)) = tx.payload()
            && let SpecialPayload::QuorumCommitment(p) = d.payload()
            && !p.commitment().is_null()
        {
            out.push((i, p.commitment().clone()));
        }
    }
    out
}

/// What a test changes of a commitment: its quorum hash and index, and either signature, the
/// rest kept.
#[derive(Default)]
struct Change {
    quorum_hash: Option<QuorumHash>,
    quorum_index: Option<i16>,
    quorum_sig_from_members: bool,
    members_sig_from_quorum: bool,
}

fn changed(qc: &LlmqCommitment, c: &Change) -> LlmqCommitment {
    let quorum_sig = if c.quorum_sig_from_members {
        qc.members_sig()
    } else {
        qc.quorum_sig()
    }
    .clone();
    let members_sig = if c.members_sig_from_quorum {
        qc.quorum_sig()
    } else {
        qc.members_sig()
    }
    .clone();
    let changed = dash_primitives::Commitment::new(
        qc.version(),
        qc.llmq_type(),
        c.quorum_hash.unwrap_or(qc.quorum_hash()),
        c.quorum_index.or(qc.quorum_index()),
        qc.signers().clone(),
        qc.valid_members().clone(),
        qc.quorum_public_key().clone(),
        qc.quorum_vvec_hash(),
        quorum_sig,
        members_sig,
    );
    assert!(!changed.is_null());
    changed
}

/// [`ROTATED`]'s transactions with its two commitments (quorum indexes 0 and 1, in that order)
/// changed by `first` and `second`.
fn rotated(first: Change, second: Change) -> impl Fn(&Mined) -> Option<Vec<TxRef>> {
    move |m: &Mined| {
        if m.height != ROTATED {
            return None;
        }
        let qcs = commitments(&m.block);
        assert_eq!(qcs.len(), 2, "two commitments at {ROTATED}");
        let mut txs = m.block.txs().to_vec();
        for ((i, qc), change) in qcs.iter().zip([&first, &second]) {
            txs[*i] = qc_tx(m.height, &changed(qc, change));
        }
        Some(txs)
    }
}

/// Every runner's outcomes, rows, mined keys and abort reports are serial's; returns serial's
/// outcomes and reports, each runner's calls and checks, and each runner's lines for
/// [`ROTATED`] ([`Ran::lines`]).
fn same_as_serial(
    skip: bool,
    stops: Option<u32>,
    edit: &dyn Fn(&Mined) -> Option<Vec<TxRef>>,
) -> (Vec<Outcome>, Vec<Report>, Vec<Calls>, Vec<RunnerLines>) {
    let mut serial: Option<Ran> = None;
    let mut counts = Vec::new();
    let mut lines = Vec::new();
    for (name, runner) in runners() {
        let mut ran = run(runner, skip, stops, edit);
        counts.push(ran.calls);
        lines.push((name.clone(), std::mem::take(&mut ran.lines)));
        match &serial {
            None => serial = Some(ran),
            Some(want) => {
                assert_eq!(ran.outcomes, want.outcomes, "{name} against serial");
                assert_eq!(ran.mined, want.mined, "{name}'s mined keys");
                assert_eq!(ran.reports, want.reports, "{name}'s abort reports");
                assert!(ran.stored == want.stored, "{name}'s rows differ from serial's");
            }
        }
    }
    let serial = serial.expect("serial ran");
    (serial.outcomes, serial.reports, counts, lines)
}

/// [`ROTATED`]'s commitments, quorum index 0 then 1.
fn rotated_commitments() -> Vec<LlmqCommitment> {
    let p = qc_chain_params();
    let blocks = linked(&Mined::genesis(&p), decode(QC_CHAIN));
    commitments(&blocks[ROTATED as usize - 1].block).into_iter().map(|(_, qc)| qc).collect()
}

/// Core's `-debug=llmq` line for `qc`'s check failing with `reject` (`commitment.cpp` v24:83-84,
/// :96).
fn core_line(qc: &LlmqCommitment, reject: CommitmentReject) -> String {
    format!("CFinalCommitment -- q[{}] {reject}", core_hex(&qc.quorum_hash()))
}

/// BL-843 T4: each queue's lines for [`ROTATED`], whose failing checks log `failing`'s lines.
/// Without threads the queue logs Core's line at `-parbls=1` alone, `first`; with them, at most
/// one line per thread, from `failing`, every one when one fails (BL-843 §2.2). Every line comes
/// before `ConnectBlock`'s failure line, which ends the lines.
fn assert_rotated_lines(lines: &[RunnerLines], failing: &[String], first: &str) {
    for (name, lines) in lines {
        let Some(threads) = name.strip_prefix("queue(").and_then(|n| n.strip_suffix(')')) else {
            continue;
        };
        let threads: usize = threads.parse().expect("a count");
        let (connect, checks) = lines.split_last().expect("lines");
        assert!(connect.starts_with("ConnectBlock(DASH): "), "{name}: {lines:#?}");
        if threads == 0 {
            assert_eq!(checks, [first], "{name}");
            continue;
        }
        super::support::assert_line_rule(name, checks, failing, threads + 1);
    }
}

/// The last block submitted: its height, whether V connected it, and its `BlockChecked` states.
fn last(outcomes: &[Outcome]) -> (u32, bool, &str) {
    let (h, valid, states) = outcomes.last().expect("a block");
    (*h, *valid, states)
}

/// (a) The recording connects whole on every runner, with the same states, rows and mined keys.
/// Each block from DIP0003 makes one runner call, a mining block's with two checks per non-null
/// commitment, the members aggregate and the quorum signature.
#[test]
fn every_runner_connects_the_mining_blocks_as_serial() {
    let (got, _, counts, _) = same_as_serial(false, None, &|_| None);
    assert_eq!(got.len(), END as usize);
    assert!(got.iter().all(|(_, valid, _)| *valid), "{got:?}");
    let want = (END as usize - 1, MINING.len(), 2 + 4 + 2 + 4);
    assert!(counts.iter().all(|&c| c == want), "{counts:?}");
}

/// (b) The signature phase comes before `ProcessCommitment`: a commitment at quorum index 1 whose
/// quorum is index 0's (signatures intact, so `bad-qc-block` alone), and after it one with a
/// bad quorum signature, is `bad-qc-invalid` on every runner.
#[test]
fn a_later_bad_signature_beats_an_earlier_bad_qc_block() {
    let wrong_index = || Change {
        quorum_index: Some(1),
        ..Change::default()
    };
    let _lines = ROTATED_LINES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let control = rotated(wrong_index(), Change::default());
    let (got, _, _, _) = same_as_serial(true, None, &control);
    let (h, valid, states) = last(&got);
    assert!(h == ROTATED && !valid && states.contains("bad-qc-block"), "{:?}", last(&got));
    let bad_sig = Change {
        quorum_sig_from_members: true,
        ..Change::default()
    };
    let edit = rotated(wrong_index(), bad_sig);
    let (got, _, counts, lines) = same_as_serial(true, None, &edit);
    let (h, valid, states) = last(&got);
    assert!(h == ROTATED && !valid && states.contains("bad-qc-invalid"), "{:?}", last(&got));
    assert!(!states.contains("bad-qc-block"), "{states}");
    assert!(counts.iter().all(|&c| c == (227, 2, 2 + 4)), "{counts:?}");
    let qcs = rotated_commitments();
    let quorum = core_line(&qcs[1], CommitmentReject::QuorumSignature);
    assert_rotated_lines(&lines, std::slice::from_ref(&quorum), &quorum);
}

/// (c) Two commitments failing differently, the first's members aggregate and the second's
/// quorum signature: `bad-qc-invalid` on every runner, with step 4 as the node runs it.
#[test]
fn two_failing_commitments_are_bad_qc_invalid() {
    let members = Change {
        members_sig_from_quorum: true,
        ..Change::default()
    };
    let quorum = Change {
        quorum_sig_from_members: true,
        ..Change::default()
    };
    let _lines = ROTATED_LINES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let edit = rotated(members, quorum);
    let (got, _, counts, lines) = same_as_serial(false, None, &edit);
    let (h, valid, states) = last(&got);
    assert!(h == ROTATED && !valid && states.contains("bad-qc-invalid"), "{:?}", last(&got));
    assert!(counts.iter().all(|&c| c == (227, 2, 2 + 4)), "{counts:?}");
    // Core's order at -parbls=1 runs the 4 checks as 2, 3, 1, 0: the second commitment's quorum
    // signature fails first, where push order would reach the first's members aggregate.
    let qcs = rotated_commitments();
    let members = core_line(&qcs[0], CommitmentReject::MembersSignature);
    let quorum = core_line(&qcs[1], CommitmentReject::QuorumSignature);
    assert_rotated_lines(&lines, &[members, quorum.clone()], &quorum);
}

/// (d) An unknown base after a failing commitment is Core's reasonless `return false` before
/// `Wait`, whose control still drains the checks queued so far; rdashd returns before calling the
/// runner, so there is no call at that block. It maps the refusal to a fault that stops V (D5's
/// `qc_failure`), the same report on every runner.
#[test]
fn an_unknown_base_after_a_failing_commitment_stops_v_alike() {
    let _lines = ROTATED_LINES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let bad_sig = Change {
        quorum_sig_from_members: true,
        ..Change::default()
    };
    let unknown = Change {
        quorum_hash: Some(QuorumHash::from([7; 32])),
        ..Change::default()
    };
    let edit = rotated(bad_sig, unknown);
    let (got, reports, counts, _) = same_as_serial(true, Some(ROTATED), &edit);
    assert_eq!(last(&got).0, ROTATED);
    let reported = format!("{reports:?}");
    assert!(reported.contains("quorum block processor"), "{reported}");
    assert!(!reported.contains("bad-qc"), "{reported}");
    assert!(counts.iter().all(|&c| c == (226, 1, 2)), "no call at {ROTATED}: {counts:?}");
}
