//! MN0-2b-2's proposal-path read-fault sweep (PLAN-MN0 §10.9, OD-CP-49/51): a proposal past DIP3
//! on setup-dashd's masternode network, each of its store reads faulted cold.

use std::sync::Arc;

use dash_consensus::Block;

use super::evo_chain::{Kept, OpenSeams, QC_CHAIN, decode, linked, node_config, qc_chain_params};
use super::read_faults::Fault;
use super::support::{Mined, Rig, RigConfig};
use crate::chain::BlockState;
use crate::connect::NodeHelper;
use crate::evo::cl::AttemptClVerifier;
use crate::evo::codec::cf;
use crate::evo::ev1::DmlChecks;

/// The tip the evo rows' proposals build on: the next block mines a signed `llmq_test_dip0024`
/// commitment, past V20 and MN_RR (100) and the first CbTx ChainLock (237).
const TIP: u32 = 299;

/// The tip P2's proposal builds on: the next block spends three coins (a masternode's
/// registration), the recording's first spend past DIP0003.
const SPEND_TIP: u32 = 125;

/// A recorded rig on the masternode network at `tip`, restarted on its stores with every evo row
/// and coin flushed and the next block's inputs out of the coins cache, so V's caches start empty
/// and the next block's reads go to the engine.
struct Cold {
    rig: Rig,
    kept: Kept,
    main: Vec<Mined>,
    tip: u32,
}

fn seams() -> OpenSeams<DmlChecks> {
    OpenSeams {
        ev1: DmlChecks,
        cl: Box::new(AttemptClVerifier::default()),
        helper: Box::new(NodeHelper),
        blocks: Box::new(|b| b),
    }
}

impl Cold {
    fn at_tip() -> Cold {
        Cold::at(TIP)
    }

    fn at(tip: u32) -> Cold {
        Cold::open(tip, true)
    }

    /// The rig at `tip`; `cold` flushes and restarts it, else V keeps its layer and caches.
    fn open(tip: u32, cold: bool) -> Cold {
        let p = qc_chain_params();
        let blocks = decode(QC_CHAIN);
        let last = blocks.iter().map(|b| i64::from(b.header().time())).max().expect("blocks");
        let after_genesis = last - i64::from(Mined::genesis(&p).time()) + 86_400;
        let s = seams();
        let (cfg, kept) =
            node_config(p, after_genesis, RigConfig::default(), s.ev1, s.cl, s.helper, s.blocks);
        let mut rig = Rig::with(cfg);
        let main = linked(&rig.genesis, blocks);
        for m in &main[..tip as usize] {
            rig.accept(m);
        }
        if !cold {
            return Cold {
                rig,
                kept,
                main,
                tip,
            };
        }
        let at = rig.id(&main[tip as usize - 1]);
        let exit = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        kept.flush_evo();
        kept.flush_coins();
        let mut cfg = kept.reopen(RigConfig::default(), tip + 1, seams());
        let files = cfg.files.take().expect("the node's block files");
        kept.uncache_inputs(&main[tip as usize].block);
        rig = Rig::restart_with(exit, files, at, cfg).expect("V starts");
        kept.assert_no_abort(&rig);
        Cold {
            rig,
            kept,
            main,
            tip,
        }
    }

    fn next(&self) -> Arc<Block> {
        self.main[self.tip as usize].block.clone()
    }
}

/// What a faulted proposal did: V's answer (`None` when V stopped), the abort reports, whether
/// the latch tripped, the matching reads, whether shutdown was requested, and what the next
/// block then met: connected, or rejected as invalid (neither when V has stopped).
#[derive(Debug)]
struct Outcome {
    answer: Option<String>,
    reports: usize,
    tripped: bool,
    hits: u32,
    shutdown: bool,
    next_connects: bool,
    next_invalid: bool,
}

fn probe(cf_name: &str, fault: Option<Fault>) -> Outcome {
    probe_on(
        &Cold::at(if cf_name == "coins" {
            SPEND_TIP
        } else {
            TIP
        }),
        cf_name,
        fault,
    )
}

fn probe_on(c: &Cold, cf_name: &str, fault: Option<Fault>) -> Outcome {
    match fault {
        Some(f) => c.kept.faults().arm(cf(cf_name), &[], f),
        None => c.kept.faults().watch(cf(cf_name), &[]),
    }
    propose(c)
}

/// The next block proposed under the armed fault, which is then disarmed.
fn propose(c: &Cold) -> Outcome {
    propose_with(c, |_| ())
}

/// [`propose`], with `seen` called on the faults before they are disarmed.
fn propose_with(c: &Cold, seen: impl FnOnce(&super::read_faults::Faults)) -> Outcome {
    let faults = c.kept.faults();
    let answer = c.rig.v.test_block_validity(c.next(), false, true).recv().ok().map(|r| match r {
        Ok(BlockState::Valid) => "Valid".to_owned(),
        Ok(other) => format!("{other:?}"),
        Err(not) => format!("NotTested {not:?}"),
    });
    let hits = faults.hits();
    seen(faults);
    faults.disarm();
    let (reports, tripped) = (c.kept.reports().len(), c.kept.tripped());
    let next = c.rig.v.process_block(c.next(), true, None, true).recv().ok();
    Outcome {
        answer,
        reports,
        tripped,
        hits,
        shutdown: c.kept.shutdown_requested(),
        next_connects: next.as_ref().is_some_and(|r| r.accepted && r.state.is_valid()),
        next_invalid: next.is_some_and(|r| matches!(r.state, BlockState::Invalid(_))),
    }
}

/// The template's evo reads at V's tip (MN0-2b-2 commit 2's `tip_block`), under one fault: the
/// tip list, the credit pool and `with_qc`'s qc hashes, each as its error text.
fn template_reads(c: &Cold) -> [Result<(), String>; 3] {
    let p = c.rig.p.clone();
    c.rig
        .v
        .view_handle()
        .query(move |q| {
            let view = q.evo_view();
            let tip = view.tip.expect("a tip");
            let list = view.tip_list().map(drop).map_err(|e| e.to_string());
            let pool = view.credit_pool().map(drop).map_err(|e| e.to_string());
            let bls = dash_consensus::ctx::bls_ctx_for_block(&p, TIP + 1);
            let qc = view
                .with_qc(bls, |qc| {
                    let mut cache = dash_dml::blockprocessor::QcHashCache::new();
                    dash_dml::blockprocessor::qc_hashes(&qc.llmq, qc.mined, &mut cache, tip)
                        .map(drop)
                        .map_err(|e| format!("{e:?}"))
                })
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            [list, pool, qc]
        })
        .recv()
        .expect("V serves")
}

/// B-2 (OD-CP-20) at the template's reads, cold. An engine read error is reported once, where
/// the engine read it, and trips the latch; the read answers its error to the template, which
/// throws it as `CreateNewBlock` does. A row that does not decode is a P-row's class: the list's
/// (`mn_diff`, P4) is the evo store's read error, unreported, and the node runs on (OD-CP-49);
/// the block processor's (`llmq_mined`, P3, reached through `GetQcHashes`) is a corrupt row,
/// reported once as it trips the latch (DEC-1185 (1), OD-CP-99), and no template is served nor
/// any later block rejected.
#[test]
fn the_templates_evo_reads_fault_in_b2s_classes() {
    for (name, read) in [("mn_diff", 0), ("llmq_mined", 2)] {
        let c = Cold::at_tip();
        c.kept.faults().arm(cf(name), &[], Fault::Io);
        let err = template_reads(&c)[read].clone().expect_err("the read fails");
        assert!(err.contains("injected read failure"), "{name}: {err}");
        assert_eq!((c.kept.reports().len(), c.kept.tripped()), (1, true), "{name}: reported once");
        assert!(c.rig.v.view_handle().query(|_| ()).recv().is_ok(), "{name}: V serves");
    }

    let c = Cold::at_tip();
    c.kept.faults().arm(cf("mn_diff"), &[], Fault::Garbage);
    let err = template_reads(&c)[0].clone().expect_err("the read fails");
    assert!(err.contains("undecodable mn_diff row"), "{err}");
    c.kept.faults().disarm();
    assert_eq!((c.kept.reports().len(), c.kept.tripped()), (0, false), "mn_diff: unreported");
    c.rig.accept(&c.main[TIP as usize]);
    c.kept.assert_no_abort(&c.rig);

    let c = Cold::at_tip();
    c.kept.faults().arm(cf("llmq_mined"), &[], Fault::Garbage);
    let err = template_reads(&c)[2].clone().expect_err("no template");
    assert!(err.contains("undecodable llmq_mined row"), "{err}");
    c.kept.faults().disarm();
    let reports = c.kept.reports();
    assert_eq!(reports.len(), 1, "llmq_mined: reported once: {reports:?}");
    assert!(reports[0].0.contains("(Corruption): corrupt record llmq_mined/"), "{reports:?}");
    assert!(c.kept.tripped(), "llmq_mined: the latch");
    let next = c.rig.v.process_block(c.next(), true, None, true).recv().ok();
    assert!(
        !next.as_ref().is_some_and(|r| matches!(r.state, BlockState::Invalid(_))),
        "no later reject: {:?}",
        next.map(|r| r.state)
    );
    assert_eq!(c.kept.reports().len(), 1, "still one report");
}

/// Row P2 (PLAN-MN0 §10.10): the proposal's coins reads, cold. An engine read error stops V
/// with one report, as Core's `CCoinsViewErrorCatcher` aborts (rc.5 `coins.cpp:356-369`); an
/// undecodable coin stops it the same way, the tip path's DEC-78 fail-stop where Core reads the
/// coin as absent (`dbwrapper.h:322-326`). The tip path's class either way.
#[test]
fn p2_a_faulted_coins_read_stops_v_once() {
    for fault in [Fault::Io, Fault::Garbage] {
        let o = probe("coins", Some(fault));
        assert!(o.answer.is_none(), "{fault:?}: V stopped: {o:?}");
        assert_eq!((o.reports, o.tripped), (1, true), "{fault:?}: one report: {o:?}");
        assert!(o.hits > 0, "{fault:?}: an engine read: {o:?}");
    }
}

/// OD-CP-49 #4's warm controls (OD-CP-95 c3): with the rig warm (no flush, no restart), each
/// stored row the proposal reads is answered from V's layer and caches, and no engine read meets
/// it. Garbage armed on every row is never decoded, so the proposal passes. The rows V holds whole
/// (the height index, the lists, the MNHF signals, the credit pool) are not read at all, so an
/// armed I/O error is never met. The mined rows and the coins still look up absent keys in the
/// store, as Core's positive-only `mapMinedCommitmentBlockCache` and coins cache do, but no
/// resident key: neither a mined row V's layer holds nor an input the block spends.
#[test]
fn warm_reads_meet_no_armed_fault() {
    let held = ["llmq_mined_h", "mn_snapshot", "mn_diff", "mnhf", "credit_pool"];
    for (tip, name) in
        held.iter().chain(&["llmq_mined"]).map(|n| (TIP, *n)).chain([(SPEND_TIP, "coins")])
    {
        for fault in [Fault::Garbage, Fault::Io] {
            if fault == Fault::Io && !held.contains(&name) {
                continue;
            }
            let c = Cold::open(tip, false);
            let resident = match name {
                "llmq_mined" => c.kept.resident_mined(),
                "coins" => c.next().txs()[1..]
                    .iter()
                    .flat_map(|tx| tx.inputs().iter().map(|i| i.prevout()))
                    .map(|op| {
                        let mut buf = [0; crate::coins::DB_KEY_MAX];
                        let n = crate::coins::OutPointKey::new(&op).db_key(&mut buf);
                        buf[..n].to_vec()
                    })
                    .collect(),
                _ => Vec::new(),
            };
            c.kept.faults().arm(cf(name), &[], fault);
            let o = propose_keeping_keys(&c);
            assert_eq!(o.0.answer.as_deref(), Some("Valid"), "{name} {fault:?}: {:?}", o.0);
            assert_eq!((o.0.reports, o.0.tripped), (0, false), "{name} {fault:?}: {:?}", o.0);
            assert!(o.0.next_connects, "{name} {fault:?}: {:?}", o.0);
            if held.contains(&name) {
                assert_eq!(o.0.hits, 0, "{name} {fault:?}: no store read: {:?}", o.0);
            } else {
                assert!(!resident.is_empty(), "{name}: resident keys to check");
                let met: Vec<_> = o.1.iter().filter(|k| resident.contains(k)).collect();
                assert!(met.is_empty(), "{name} {fault:?}: resident keys read: {met:?}");
                let cold = Cold::at(tip);
                cold.kept.faults().watch(cf(name), &[]);
                let (_, read) = propose_keeping_keys(&cold);
                assert!(
                    read.iter().any(|k| resident.contains(k)),
                    "{name}: cold, the same keys are read from the engine"
                );
            }
        }
    }
}

/// [`propose`], with the keys the armed reads met.
fn propose_keeping_keys(c: &Cold) -> (Outcome, Vec<Vec<u8>>) {
    let keys = std::cell::RefCell::new(Vec::new());
    let o = propose_with(c, |f| *keys.borrow_mut() = f.read_keys());
    (o, keys.into_inner())
}

/// The tip list's size, read through V's query.
fn tip_list_len(c: &Cold) -> Result<usize, String> {
    c.rig
        .v
        .view_handle()
        .query(|q| q.evo_view().tip_list().map(|l| l.len()).map_err(|e| e.to_string()))
        .recv()
        .expect("V serves")
}

/// Row P4, absent (OD-CP-49 #2, DEC-1064, BL-650): the tip's `mn_diff` row hidden, cold, on a
/// single chainstate. The walk takes Core's initial snapshot at the tip, an empty list (rc.5
/// `deterministicmns.cpp:863-889`), so the proposal's CbTx list root is wrong and it answers
/// Core's `bad-cbtx-mnmerkleroot` (`specialtxman.cpp:995`) with no report and the node running.
/// With the row back, the tip list is the real one again (no fallback list cached, OD-CP-46)
/// and the block connects.
#[test]
fn p4_an_absent_parent_diff_takes_the_initial_snapshot() {
    use crate::evo::codec::EvoCf;
    let c = Cold::at_tip();
    let parent = c.main[TIP as usize - 1].hash();
    let real = tip_list_len(&c).expect("the tip list");
    assert!(real > 0, "masternodes registered by {TIP}");
    let c = Cold::at_tip();
    let faults = c.kept.faults();
    faults.arm(EvoCf::MnDiff.cf(), EvoCf::MnDiff.key(&parent).as_bytes(), Fault::Absent);
    assert_eq!(tip_list_len(&c), Ok(0), "the initial snapshot: an empty list");
    faults.arm(EvoCf::MnDiff.cf(), EvoCf::MnDiff.key(&parent).as_bytes(), Fault::Absent);
    let o = propose(&c);
    assert_eq!(
        o.answer.as_deref(),
        Some(
            r#"Invalid(ValidationError { result: Consensus, reason: "bad-cbtx-mnmerkleroot", debug: "" })"#
        ),
        "{o:?}"
    );
    assert_eq!((o.reports, o.tripped, o.hits), (0, false, 1), "{o:?}");
    assert_eq!(tip_list_len(&c), Ok(real), "no initial-snapshot list left cached");
    assert!(o.next_connects, "{o:?}");
    c.kept.assert_no_abort(&c.rig);
}

/// Row P9 (PLAN-MN0 §10.10): the proposal's deployment reads go through `ProposalDeployments`,
/// answered from the parent; a question about the dummy's child would end the attempt in
/// `Fatal::assert` (`connect/proposal.rs`, C-2) and stop V. Past V20 and MN_RR, with a signed
/// commitment mined, cold and warm, the proposal asks none: it is valid, nothing is reported,
/// and the block then connects. The EHF signals behind V24's versionbits are row P7's reads.
#[test]
fn p9_a_proposal_asks_nothing_of_the_dummys_child() {
    for cold in [true, false] {
        let o = propose(&Cold::open(TIP, cold));
        assert_eq!(o.answer.as_deref(), Some("Valid"), "cold {cold}: {o:?}");
        assert_eq!((o.reports, o.tripped), (0, false), "cold {cold}: {o:?}");
        assert!(o.next_connects, "cold {cold}: {o:?}");
    }
}

/// OD-CP-96 F1: an evo row's engine read error is reported once, by the guarded engine as it
/// trips the latch, on the proposal path and on the tip path alike; V's stop does not report it
/// again (D5, DEC-77 Q1: the node stops where Core's catch-all rejects, `specialtxman.cpp:1053-1055`).
#[test]
fn an_evo_read_error_is_reported_once() {
    for name in ["mn_diff", "mn_snapshot", "llmq_mined", "llmq_mined_h"] {
        let o = probe(name, Some(Fault::Io));
        assert!(o.answer.is_none(), "{name}: V stopped: {o:?}");
        assert_eq!((o.reports, o.tripped), (1, true), "{name}: one report: {o:?}");
        assert!(o.hits > 0, "{name}: an engine read: {o:?}");

        let c = Cold::at_tip();
        c.kept.faults().arm(cf(name), &[], Fault::Io);
        let r = c.rig.v.process_block(c.next(), true, None, true).recv();
        c.kept.faults().disarm();
        assert!(r.is_err() || r.is_ok_and(|r| !r.accepted), "{name}: the tip path stops");
        let reports = c.kept.reports();
        assert_eq!(reports.len(), 1, "{name}: tip path: {reports:?}");
        assert!(reports[0].0.contains("injected read failure"), "{name}: {reports:?}");
        assert!(c.kept.tripped(), "{name}");
    }
}

/// P3 and P4 decode on the proposal path, cold: a row that does not decode stops the node with
/// one report and no reject, where Core reads it as missing (`dbwrapper.h:322-326`). P3's rows
/// (`llmq_mined`, `llmq_mined_h`; DEC-1185 (1), Core's later `bad-qc-missing` or quorum-root
/// reject at `llmq/blockprocessor.cpp:680-682`, `:718`) are the guarded engine's corrupt rows:
/// the latch trips as they are reported. P4's (`mn_diff`, `mn_snapshot`; OD-CP-51, DEC-1073) are
/// V's fault, reported by V's stop: the latch stays open, and the stop is V's shutdown request and
/// its ended thread, so the next block is never answered.
#[test]
fn p3_p4_an_undecodable_row_stops_the_node_once() {
    for (name, latched) in
        [("llmq_mined", true), ("llmq_mined_h", true), ("mn_diff", false), ("mn_snapshot", false)]
    {
        let o = probe(name, Some(Fault::Garbage));
        assert!(o.answer.is_none(), "{name}: V stopped: {o:?}");
        assert_eq!((o.reports, o.tripped), (1, latched), "{name}: one report: {o:?}");
        assert!(o.shutdown, "{name}: shutdown requested: {o:?}");
        assert!(!o.next_connects && !o.next_invalid, "{name}: no later answer: {o:?}");
        assert!(o.hits > 0, "{name}: an engine read: {o:?}");
    }
}

/// The tip's credit pool, as the template reads it, in its debug form.
fn tip_pool(c: &Cold) -> String {
    c.rig
        .v
        .view_handle()
        .query(|q| format!("{:?}", q.evo_view().credit_pool().expect("the pool")))
        .recv()
        .expect("V serves")
}

/// Row P8 (OD-CP-96 F6), warm and rebuilt: the recording ends before the first multiple of 576,
/// the only heights Core and V read the `credit_pool` row at (`creditpool.cpp:137`), so the pool
/// is rebuilt from blocks. Warm, the tip's pool is answered with no engine read of the row;
/// cold, the rebuilt pool is the same; and block 300's CbTx, which carries the pool's balance,
/// proposes valid. The cold row waits for a recording past 576.
#[test]
fn p8_the_credit_pool_is_answered_warm_and_rebuilt_cold() {
    let warm = Cold::open(TIP, false);
    warm.kept.faults().watch(cf("credit_pool"), &[]);
    let pool = tip_pool(&warm);
    assert_eq!(warm.kept.faults().hits(), 0, "no credit_pool read");
    assert!(pool.starts_with("Some("), "a pool past V20: {pool}");
    let o = propose(&warm);
    assert_eq!((o.answer.as_deref(), o.reports, o.hits), (Some("Valid"), 0, 0), "{o:?}");
    assert_eq!(tip_pool(&Cold::at_tip()), pool, "the rebuilt pool");
}

/// OD-CP-101 (1): an evo read fault on the disconnect path is reported once. With V cold at 300
/// (whose block mines a signed commitment), invalidating 300 walks the list back over `mn_diff`
/// and `mn_snapshot` rows from the engine: under an I/O error the guarded engine reports and
/// trips the latch, and V's stop adds nothing. The quorum processor's `UndoBlock` erases the
/// block's mined rows by the keys the block names and reads none of them from the engine, so a
/// P3 fault cannot reach this path.
#[test]
fn an_evo_read_error_undoing_a_block_is_reported_once() {
    for name in ["llmq_mined", "llmq_mined_h"] {
        let c = Cold::at(TIP + 1);
        c.kept.faults().watch(cf(name), &[]);
        assert_eq!(c.rig.v.invalidate_block(c.main[TIP as usize].hash()).recv(), Ok(Ok(())));
        assert_eq!(c.kept.faults().hits(), 0, "{name}: the undo reads no mined row");
        c.kept.assert_no_abort(&c.rig);
    }
    for name in ["mn_diff", "mn_snapshot"] {
        let c = Cold::at(TIP + 1);
        c.kept.faults().arm(cf(name), &[], Fault::Io);
        let _ = c.rig.v.invalidate_block(c.main[TIP as usize].hash()).recv();
        let hits = c.kept.faults().hits();
        c.kept.faults().disarm();
        let reports = c.kept.reports();
        assert!(hits > 0, "{name}: the undo read the row");
        assert_eq!(reports.len(), 1, "{name}: {reports:?}");
        assert!(reports[0].0.contains("injected read failure"), "{name}: {reports:?}");
        assert!(c.kept.tripped(), "{name}");
    }
}

/// A list's content, whatever built it: its place, its counter and each masternode.
fn fingerprint(list: &dash_dml::MnList) -> (String, i32, u32, Vec<String>) {
    let mns = list.iter().map(|mn| format!("{mn:?}")).collect();
    (list.block_hash().to_string(), list.height(), list.total_registered(), mns)
}

/// The list after the tip, as V reads it.
fn tip_fingerprint(c: &Cold) -> (String, i32, u32, Vec<String>) {
    c.rig
        .v
        .view_handle()
        .query(|q| fingerprint(&q.evo_view().tip_list().expect("the tip list")))
        .recv()
        .expect("V serves")
}

/// Row P4 absent's residue (OD-CP-100, Core parity): the proposal's walk over the hidden row
/// marks the tip as an initial snapshot, as Core's `GetListForBlockInternal` sets
/// `m_initial_snapshot_index` on the same `TestBlockValidity` walk (rc.5
/// `deterministicmns.cpp:882-888`), and keeps no list (OD-CP-46; Core also caches the empty
/// one). With the row back, connecting 300 writes the extra snapshot the mark asks for (DEC-1064,
/// `evo/node.rs` `extra_snapshot`): the real list at 300, as a rebuild from the rows gives it.
/// Without the hidden row, with or without a valid proposal first, 300 gets no snapshot.
#[test]
fn p4_the_initial_snapshot_mark_writes_the_real_list() {
    use crate::evo::codec::EvoCf;
    let (parent, at) = {
        let c = Cold::at_tip();
        (c.main[TIP as usize - 1].hash(), c.main[TIP as usize].hash())
    };

    let plain = Cold::at_tip();
    assert!(plain.kept.snapshot_at(&plain.main[1].hash()).is_some(), "DIP0003's block has one");
    plain.rig.accept(&plain.main[TIP as usize]);
    assert!(plain.kept.snapshot_at(&at).is_none(), "no proposal: no snapshot at 300");
    let real = tip_fingerprint(&plain);
    assert_eq!(real.3.len(), 4, "four masternodes at 300");

    let valid = Cold::at_tip();
    assert_eq!(propose(&valid).answer.as_deref(), Some("Valid"));
    assert!(valid.kept.snapshot_at(&at).is_none(), "a valid proposal: no snapshot at 300");

    let c = Cold::at_tip();
    c.kept.faults().arm(EvoCf::MnDiff.cf(), EvoCf::MnDiff.key(&parent).as_bytes(), Fault::Absent);
    let o = propose(&c);
    assert!(o.answer.as_deref().is_some_and(|a| a.contains("bad-cbtx-mnmerkleroot")), "{o:?}");
    assert!(o.next_connects, "{o:?}");
    let written = c.kept.snapshot_at(&at).expect("the extra snapshot at 300");
    assert_eq!(fingerprint(&written), real, "the real list at 300");
    assert_eq!(tip_fingerprint(&c), real);
    c.kept.assert_no_abort(&c.rig);
}
