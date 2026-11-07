//! MN0-3 OD-CP-135 (5), (6): the template's read faults on a real node, through the node's
//! template source, against connect's on the same fault. setup-dashd's masternode network, cold,
//! so the template's reads of historical lists go to the engine.

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
    keys: Vec<Vec<u8>>,
    serving: bool,
    shutdown: bool,
}

fn outcome(sut: &NodeSut, answer: Result<(), String>) -> Outcome {
    let keys = sut.read_fault.keys();
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
        keys,
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

/// A block of the network mining a rotated commitment (cycle base 288, work list 280).
const H: usize = 300;

/// V at `H`'s parent with `H`'s commitment back in the mineable pool, its caches cold: blocks 1 to
/// `H` connected, restarted, then `H` invalidated, whose undo re-adds the commitment.
fn pooled(dir: &Path, blocks: &[Arc<Block>]) -> NodeSut {
    let sut = cold(dir, blocks, H);
    let chain = sut.chain().expect("V runs").clone();
    chain.invalidate_block(blocks[H - 1].hash()).recv().expect("V").expect("in the index");
    sut
}

/// OD-CP-135 (5), (6) on a node: a template mining a pool commitment computes its members over
/// work lists the list cache does not hold, so its own turn reads their `mn_diff` rows from the
/// engine. A row there that does not decode, or a decodable one at another height, is no engine
/// read error (no `CoreAbortSink::read_failed`): it is V's fault, as connect's reading of the same
/// row is (DEC-1073/D5). The template answers `Failed`, and V reports once and stops, its latch
/// open, as the connect of `H` from the same cold caches does.
// Core: v24.0.0-rc.5 evo/deterministicmns.cpp:826-939 (GetListForBlockInternal)
#[test]
fn a_templates_historical_list_fault_stops_v_as_connect_does() {
    let blocks = blocks();
    let mn_diff = CHAINSTATE_SCHEMA.cf("mn_diff").expect("the family");
    let (key, other) = {
        let dir = tempfile::tempdir().unwrap();
        let sut = pooled(dir.path(), &blocks);
        sut.read_fault.watch(mn_diff, &[]);
        let t = template(&sut);
        assert_eq!(t.answer, Ok(()), "{t:?}");
        assert!(t.reports.is_empty() && t.serving, "{t:?}");
        let first = t.keys.first().cloned().expect("the template reads mn_diff from the engine");
        let other = t.keys.iter().find(|k| **k != first).expect("two rows");
        let other = sut.stored_row("mn_diff", other).expect("a stored row");
        (first, other)
    };
    for (fault, bytes, error) in [
        ("undecodable", vec![0xff; 3], "undecodable mn_diff row"),
        ("another height's", other, "Found EvoDB inconsistency"),
    ] {
        let t = {
            let dir = tempfile::tempdir().unwrap();
            let sut = pooled(dir.path(), &blocks);
            sut.read_fault.arm(mn_diff, &key, bytes.clone());
            template(&sut)
        };
        let c = {
            let dir = tempfile::tempdir().unwrap();
            let sut = cold(dir.path(), &blocks, H - 1);
            sut.read_fault.arm(mn_diff, &key, bytes);
            connect(&sut, &blocks[H - 1])
        };
        let answer = t.answer.as_ref().expect_err(fault);
        assert!(
            answer.starts_with("Failed(\"CreateNewBlock: BuildNewListFromBlock: "),
            "{fault}: {t:?}"
        );
        assert!(answer.contains(error), "{fault}: {t:?}");
        for (path, o) in [("template", &t), ("connect", &c)] {
            assert_eq!(
                o.keys,
                std::slice::from_ref(&key),
                "{fault}, {path}: the armed row read once"
            );
            assert_eq!(o.reports.len(), 1, "{fault}, {path}: {o:?}");
            assert!(!o.latched && !o.serving && o.shutdown, "{fault}, {path}: {o:?}");
        }
    }
}
