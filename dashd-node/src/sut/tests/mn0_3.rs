//! MN0-3 OD-CP-135 (5): the template's read faults on a real node, against connect's on the
//! same fault. setup-dashd's masternode network, cold at a rotation cycle base, so the template's
//! reads of the evo rows go to the engine.

use dash_consensus::primitives::{Block, decode_block};
use dashd_mining::{BlockTemplateSource as _, ChainTemplateSource, TemplateTxs};

use super::*;

/// setup-dashd's masternode network, recorded by dashd v23.1.0 (blocks 1 to 406).
const QC_CHAIN: &str = include_str!("../../../../dashd-chainstate/tests/data/mn0-qc/blocks.txt");
const QC_ARGS: [&str; 5] = [
    "-regtest",
    "-printtoconsole=0",
    "-dip3params=2:2",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
];

fn blocks() -> Vec<Arc<Block>> {
    QC_CHAIN
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let bytes: Vec<u8> = (0..l.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&l[i..i + 2], 16).expect("hex"))
                .collect();
            Arc::new(decode_block(&mut bytes.as_slice()).expect("a block").into_value())
        })
        .collect()
}

/// A node on `dir` with blocks 1 to `tip` connected, restarted so its caches start empty.
fn cold(dir: &Path, blocks: &[Arc<Block>], tip: usize) -> NodeSut {
    let last = blocks.iter().map(|b| i64::from(b.header().time())).max().expect("blocks");
    let mut sut =
        NodeSut::with_harness(dir, &QC_ARGS, last, ui(), Harness::default()).expect("the SUT");
    let chain = sut.chain().expect("V runs").clone();
    for block in &blocks[..tip] {
        let r = chain.process_block(Arc::clone(block), true, None, true).recv();
        assert!(r.expect("V").accepted, "V refused a recorded block");
    }
    sut.restart(Teardown::Clean);
    assert!(sut.down().is_none(), "{:?}", sut.down());
    sut
}

/// What one faulted call did: its answer, the abort reports, whether the latch tripped, the
/// armed reads, whether V still serves, and whether shutdown was requested.
#[derive(Debug)]
struct Outcome {
    answer: Result<(), String>,
    reports: Vec<String>,
    latched: bool,
    hits: u32,
    serving: bool,
    shutdown: bool,
}

fn outcome(sut: &NodeSut, answer: Result<(), String>) -> Outcome {
    let hits = sut.read_fault.hits();
    sut.read_fault.disarm();
    let chain = sut.chain().expect("V ran");
    let serving = chain.view_handle().query(|_| ()).recv().is_ok();
    let abort = sut.steps.abort();
    Outcome {
        answer,
        reports: (0..abort.reports())
            .filter_map(|n| abort.report(n))
            .map(|r| format!("{r:?}"))
            .collect(),
        latched: sut.steps.chainstate().latched(),
        hits,
        serving,
        shutdown: sut.shutdown.shutdown_requested(),
    }
}

fn template(sut: &NodeSut) -> Outcome {
    let chain = sut.chain().expect("V runs").clone();
    let clock: Arc<dyn Clock> = sut.clock().clone();
    let source = ChainTemplateSource::new(chain, Arc::new(sut.params().clone()), clock, None);
    let answer =
        source.create_new_block(&[0x51], TemplateTxs::None).map(drop).map_err(|e| format!("{e:?}"));
    outcome(sut, answer)
}

fn connect(sut: &NodeSut, block: &Arc<Block>) -> Outcome {
    let chain = sut.chain().expect("V runs").clone();
    let answer = match chain.process_block(Arc::clone(block), true, None, true).recv() {
        Ok(r) if r.accepted && r.state.is_valid() => Ok(()),
        Ok(r) => Err(format!("{:?}", r.state)),
        Err(_) => Err("V stopped".to_owned()),
    };
    outcome(sut, answer)
}

/// The rotation cycle base whose template computes the rotated members.
const BASE: usize = 312;

/// OD-CP-135 (5): on a node, the template's only engine reads are the snapshot rows, at a
/// rotation cycle base, and none reaches V's fault boundary. Restarted at 311, V holds every
/// other evo row (lists, diffs, mined commitments, the credit pool, the EHF signals) and the
/// tip's coins, so no armed fault meets them; an engine read error aborts the process first
/// (`CoreAbortSink::read_failed`, Core's `dbwrapper_error`), so it never reaches V either. An
/// undecodable snapshot row reads as none on both paths, as Core's `Read` returning false
/// does: the template and the connect each recompute it, with no report, and V serves on. The
/// boundary's faults are `proposal_faults`' (chainstate) and `evo_dashd`'s (the list build).
// Core: v24.0.0-rc.5 llmq/snapshot.cpp:301-317 (GetSnapshotForBlock), dbwrapper.h:322-326
#[test]
fn a_cold_templates_engine_reads_meet_no_fault_boundary() {
    let blocks = blocks();
    let dir = tempfile::tempdir().unwrap();
    let mut sut = cold(dir.path(), &blocks, BASE - 1);
    for name in [
        "mn_diff",
        "mn_snapshot",
        "credit_pool",
        "mnhf",
        "llmq_mined",
        "llmq_mined_h",
        "coins",
        "llmq_snapshot",
    ] {
        sut.read_fault.watch(CHAINSTATE_SCHEMA.cf(name).expect("the family"), &[]);
        let t = template(&sut);
        assert_eq!(t.answer, Ok(()), "{name}: {t:?}");
        let want = if name == "llmq_snapshot" {
            3
        } else {
            0
        };
        assert_eq!(t.hits, want, "{name}: engine reads");
        sut.restart(Teardown::Clean);
    }

    let snapshot = CHAINSTATE_SCHEMA.cf("llmq_snapshot").expect("the family");
    let garbage = vec![0xff; 3];
    let t = {
        let dir = tempfile::tempdir().unwrap();
        let sut = cold(dir.path(), &blocks, BASE - 1);
        sut.read_fault.arm(snapshot, &[], garbage.clone());
        template(&sut)
    };
    let dir = tempfile::tempdir().unwrap();
    let sut = cold(dir.path(), &blocks, BASE - 1);
    sut.read_fault.arm(snapshot, &[], garbage);
    let c = connect(&sut, &blocks[BASE - 1]);
    for (path, o) in [("template", &t), ("connect", &c)] {
        assert_eq!(o.answer, Ok(()), "{path}: {o:?}");
        assert!(o.hits > 0, "{path}: the row read: {o:?}");
        assert!(o.reports.is_empty() && !o.latched && o.serving && !o.shutdown, "{path}: {o:?}");
    }
}
