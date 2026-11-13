//! CH5-5: the regtest live-copy contract (`docs/node-program/designs/CH5-notes.md` §6.3–§6.5).
//!
//! - The reopen oracle's self-tests (§6.3.4, phase 4): twin against twin, the damage cases, and
//!   an undamaged stopped-node copy on both fixtures. The evo fixture needs dashd v24 (it cannot
//!   be skipped) and is built once per test binary; its tests copy the stopped template.
//! - The process tests (§6.5, phase 5): copies of a running source with the live-copy hooks,
//!   each through the oracle and the §2.2 counter check.
#![cfg(target_os = "linux")]

mod common;
#[path = "live_copy/fixture.rs"]
mod fixture;
#[path = "live_copy/live.rs"]
mod live;
#[path = "live_copy/oracle.rs"]
mod oracle;
#[path = "live_copy/workload.rs"]
mod workload;

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use dash_dml::MnList;
use dash_dml::codec::encode_snapshot_record;
use dash_primitives::BlockHash;
use dash_testkit::livecopy::{edit_rows, read_cf};
use serde_json::json;

use common::*;
use fixture::{
    Evo, Expect, Fixture, built_list, codec, internal, net_info, plain, rdashd, row, snapshot, twin,
};
use live::{ARGS, Source, delta, dir_state, moved};
use oracle::{check, step1, step2, step3};

/// The evo fixture, built by the first test that needs it.
fn evo() -> &'static Evo {
    static EVO: OnceLock<Evo> = OnceLock::new();
    EVO.get_or_init(fixture::evo)
}

/// The fingerprints do not depend on flush history: a twin flushed after every submission
/// equals one flushed only at its stop.
fn twins_agree(f: &Fixture) {
    let flushed = twin(&f.log, &f.expect.tip, f.extra, true);
    let at_stop = twin(&f.log, &f.expect.tip, f.extra, false);
    let (a, b) = (step1(&flushed).expect("rows"), step1(&at_stop).expect("rows"));
    assert_eq!(a.tip.as_deref(), Some(f.expect.tip.as_str()));
    step3(&a, &b).expect("twin against twin");
}

#[test]
fn twin_against_twin_plain() {
    twins_agree(&plain());
}

#[test]
fn twin_against_twin_evo() {
    twins_agree(&evo().fixture);
}

#[test]
fn an_undamaged_copy_passes_plain() {
    let f = plain();
    check(&f.source.copy(), &f).expect("the oracle");
}

#[test]
fn an_undamaged_copy_passes_evo() {
    let f = &evo().fixture;
    check(&f.source.copy(), f).expect("the oracle");
}

/// A coin row's value split into its height (from the leading VARINT, `height * 2 + coinbase`)
/// and its compressed txout.
// Core: coins.h:66-73, serialize.h:430-447
fn split_coin(value: &[u8]) -> (u64, &[u8]) {
    let mut n = 0u64;
    for (i, b) in value.iter().enumerate() {
        n = (n << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return (n >> 1, &value[i + 1..]);
        }
        n += 1;
    }
    panic!("a truncated VARINT")
}

/// Coins rows of blocks at most `max` high, below the startup check's six blocks.
fn coins_below(f: &Fixture, copy: &fixture::Datadir, max: u64) -> Vec<(Vec<u8>, Vec<u8>)> {
    let rows = read_cf(&copy.datadir, "coins").expect("coins");
    assert_eq!(rows.len() as u64, f.expect.txouts);
    rows.into_iter().filter(|(_, v)| split_coin(v).0 <= max).collect()
}

/// Two coins rows' values swapped: step 3's `coins` fingerprint and step 2's
/// `hash_serialized_2` differ. The two txouts differ: the legacy hash writes 1 for every
/// transaction's height code (quirk 33), so a swap of coins that differ only in height would
/// not change it. The plain fixture's coinbases all pay 500 DASH to `raw(51)`; the evo
/// fixture's outputs differ.
#[test]
fn swapped_coins_fail_steps_2_and_3() {
    let f = &evo().fixture;
    let copy = f.source.copy();
    let coins = coins_below(f, &copy, evo().heights.t - 6);
    let (a, b) = coins
        .iter()
        .flat_map(|a| coins.iter().map(move |b| (a, b)))
        .find(|(a, b)| split_coin(&a.1).1 != split_coin(&b.1).1)
        .expect("two coins with different txouts");
    edit_rows(&copy.datadir, &[("coins", &a.0, Some(&b.1)), ("coins", &b.0, Some(&a.1))])
        .expect("the swap");
    let rows = step1(&copy).expect("step 1 passes: the rows are well formed");
    let e = step3(&rows, &step1(&f.twin).expect("rows")).expect_err("step 3 fails");
    assert_eq!(e, r#"step 3: ["coins"] differ"#);
    let e = step2(&copy, f, &rows).expect_err("step 2 fails");
    assert!(e.contains("hash_serialized_2") && !e.contains("txouts"), "{e}");
}

/// One coins row deleted: step 3's count and step 2's `txouts` differ.
#[test]
fn a_deleted_coin_fails_steps_2_and_3() {
    let f = plain();
    let copy = f.source.copy();
    let coin = coins_below(&f, &copy, 20).remove(0);
    edit_rows(&copy.datadir, &[("coins", &coin.0, None)]).expect("the delete");
    let rows = step1(&copy).expect("step 1");
    let twin = step1(&f.twin).expect("rows");
    assert_eq!(rows.cf("coins").count + 1, twin.cf("coins").count);
    assert_eq!(step3(&rows, &twin).expect_err("step 3 fails"), r#"step 3: ["coins"] differ"#);
    let e = step2(&copy, &f, &rows).expect_err("step 2 fails");
    assert!(e.contains(&format!("txouts {} != {}", f.expect.txouts - 1, f.expect.txouts)), "{e}");
}

/// A fresh rdashd on `copy`, and its answer to dashd's continuation block.
fn submit_next(copy: &fixture::Datadir) -> (RpcNode, String) {
    let f = &evo().fixture;
    let node = rdashd(copy, f.extra, None);
    let answer = f.next.as_ref().expect("Core's continuation").to(&node);
    (node, answer)
}

/// D3-2, the continuation gate: the tip's populated `mn_snapshot` row replaced by an empty list at
/// its own position, keeping its counter. The node's list at the tip is that row (its walk stops at
/// a stored snapshot), so it is empty where the undamaged copy's holds the masternode; the
/// fingerprint differs, Core's next block is refused with `bad-cbtx-mnmerkleroot`, and the node
/// stays at the tip. An undamaged copy accepts the block.
#[test]
fn an_empty_snapshot_at_the_tip_fails_the_continuation() {
    let e = evo();
    let (f, h) = (&e.fixture, &e.heights);
    let (control, damaged) = (f.source.copy(), f.source.copy());
    let tip = &f.hashes[h.t as usize];
    let at = BlockHash::from(internal(tip));
    let original = snapshot(&f.source.datadir, tip);
    let height = i32::try_from(h.t).expect("a height");
    let empty = MnList::new(at, height, original.total_registered());
    let empty = encode_snapshot_record(&empty, &net_info()).expect("encodes");
    let empty = empty.as_bytes().to_vec();
    let decoded = codec().decode_snapshot(&empty, &at).expect("the damage decodes");
    assert!(decoded.is_empty() && decoded.height() == height, "{decoded:?}");
    edit_rows(&damaged.datadir, &[("mn_snapshot", &internal(tip), Some(&empty))]).expect("edit");
    let undamaged = built_list(&control.datadir, &f.hashes, h).expect("the undamaged list");
    assert_eq!(undamaged.len(), 1, "the undamaged list at the tip holds the masternode");
    let mn = undamaged.iter().next().expect("the entry");
    assert_eq!(mn.pro_tx_hash.as_bytes(), &internal(&e.pro_tx));
    let service = dash_dml::netinfo::primary(&mn.cold.net_info).to_string();
    assert_eq!(service, format!("127.0.0.2:{}", e.port));
    let built = built_list(&damaged.datadir, &f.hashes, h).expect("the list at the tip builds");
    assert!(built.is_empty(), "the damaged list at the tip is empty: {built:?}");

    let (node, answer) = submit_next(&control);
    assert_eq!(answer, "null", "the control accepts {}", h.next);
    node.stop();

    let rows = step1(&damaged).expect("step 1");
    let e = step3(&rows, &step1(&f.twin).expect("rows")).expect_err("step 3 fails");
    assert_eq!(e, r#"step 3: ["mn_snapshot"] differ"#);
    let (node, answer) = submit_next(&damaged);
    assert_eq!(answer, r#""bad-cbtx-mnmerkleroot""#);
    let tip = fixture::call(&node, "getbestblockhash", json!([]));
    assert_eq!(tip, json!(f.expect.tip));
    assert_eq!(fixture::call(&node, "getblockcount", json!([])), json!(h.t));
    node.stop();
}

/// Storage corruption, not the continuation gate: under the tip's snapshot key, the first DIP3
/// block's row verbatim, an empty list stamped with that block's hash. Its block-identity check
/// fails ("mn_snapshot of another block"), and the node fail-stops through AbortNode at start's
/// tip read, before any block reaches it. The read runs on the block-import thread after init's
/// warmup ends, so a call may still be answered first.
#[test]
fn a_foreign_snapshot_aborts_the_node() {
    let e = evo();
    let (f, h) = (&e.fixture, &e.heights);
    let copy = f.source.copy();
    let tip = &f.hashes[h.t as usize];
    let foreign = row(&f.source.datadir, "mn_snapshot", &f.hashes[h.dip3 as usize]);
    let e =
        codec().decode_snapshot(&foreign, &BlockHash::from(internal(tip))).expect_err("foreign");
    assert!(e.to_string().contains("mn_snapshot of another block"), "{e}");
    edit_rows(&copy.datadir, &[("mn_snapshot", &internal(tip), Some(&foreign))]).expect("edit");
    // The abort follows "Done loading" closely, so the start may see either a running node or
    // its exit.
    let (served, out) = match fixture::try_rdashd(&copy, f.extra, None) {
        Ok(node) => {
            let reply = try_rpc(node.port, &cookie(&copy.datadir), "getblockcount", "[]");
            (reply.map(|r| (r.status, r.body)), node.node.wait())
        }
        Err(out) => (Err("exited during init".into()), out),
    };
    let log = read(&debug_log(&copy.datadir));
    assert_eq!(out.status.code(), Some(1), "{served:?} {log}");
    let throw = "GetListAtChainTip: Found EvoDB inconsistency, you must reindex: mn_snapshot of \
                 another block";
    assert!(log.contains(throw), "{log}");
    assert!(log.contains("A fatal internal error occurred"), "{log}");
}

/// The `mn_diff` row at the update height deleted. Step 3 differs in `mn_diff` only. Startup's
/// undo of 576 walks 575's diffs back to 112, which has none and is taken as an initial empty
/// snapshot, as Core does; 113's diff then fails with "can't find an updated masternode".
/// Core fails its start as "Corrupted block database detected" (EV3 D21, OD-CP-278).
// Core: evo/deterministicmns.cpp:389, :422, :864-887; validation.cpp:5120-5129 at v24.0.0
#[test]
fn a_deleted_mn_diff_row_fails_the_oracle() {
    let e = evo();
    let f = &e.fixture;
    let copy = f.source.copy();
    let updated = &f.hashes[e.heights.updated as usize];
    edit_rows(&copy.datadir, &[("mn_diff", &internal(updated), None)]).expect("the delete");
    let rows = step1(&copy).expect("step 1");
    let three = step3(&rows, &step1(&f.twin).expect("rows")).expect_err("step 3 fails");
    assert_eq!(three, r#"step 3: ["mn_diff"] differ"#);
    let two = step2(&copy, f, &rows).expect_err("step 2 fails");
    assert!(two.starts_with("step 2: the start failed (exit status: 1)"), "{two}");
    let walk = two.find("Verifying last 6 blocks at level 3").expect(&two);
    let stop = "*** GetListForBlock: Found EvoDB inconsistency, you must reindex: mn_diff row for \
                block ";
    let stopped = two.find(stop).expect(&two);
    assert!(walk < stopped, "{two}");
    assert!(two.contains("ApplyDiff: can't find an updated masternode, id=0"), "{two}");
}

// Phase 5: the process tests (§6.5).

/// The expected state at a copy point, from its reply.
fn expect_of(reply: &serde_json::Value) -> Expect {
    Expect {
        tip: fixture::text(&reply["bestblock"]),
        height: reply["height"].as_u64().expect("height"),
        hash_serialized_2: fixture::text(&reply["hash_serialized_2"]),
        txouts: reply["txouts"].as_u64().expect("txouts"),
    }
}

/// I0, M1's killer (D2-5): a dirty tip at the copy point, copied at once. The copy point's own
/// flush is the only flush between the durable baseline (10) and the copy, so the copy holds T
/// (20) only if the copy point flushed.
#[test]
fn i0_the_copy_point_flushes_the_dirty_tip() {
    let mut s = Source::start(ARGS);
    let jobs = |l: &serde_json::Value| l["events"]["flush_jobs_done"].as_u64().expect("jobs");
    // 1. Leave IBD: the LEFT_IBD job.
    let j_pre = s.jobs_done();
    s.mine(1);
    wait_until("the end of IBD", || {
        s.call("getblockchaininfo", json!([]))["initialblockdownload"] == json!(false)
    });
    assert!(s.wait_live(Duration::from_secs(30), |l| jobs(l) > j_pre), "no LEFT_IBD job");
    let j0 = s.jobs_done();
    // 2. The durable baseline at 10.
    s.mine(9);
    s.flush();
    let baseline = s.tip();
    let j1 = s.jobs_done();
    assert_eq!(j1, j0 + 1, "only the scripted job");
    // 3. Dirty to 20, with no flush.
    s.mine(10);
    let t = s.tip();
    assert_eq!(s.jobs_done(), j1, "no flush while the tip is dirty");
    assert_ne!(baseline, t);
    // 4. The copy point, and the copy at once; no RPC to the source meanwhile.
    let r = s.copy_point();
    let copy = s.list().now();
    let acked = s.wait_live(Duration::from_secs(10), |l| jobs(l) == j1 + 1);
    let live = s.live();
    // 5. (a), (b), (c).
    assert_eq!((&r["height"], &r["bestblock"]), (&json!(20), &json!(t)), "(a) the reply");
    assert!(acked && jobs(&live) == j1 + 1, "the copy point's job: {}", jobs(&live));
    let rows = step1(&copy).expect("step 1");
    assert_eq!(rows.tip.as_deref(), Some(t.as_str()), "(b) the copy's tip");
    let f = s.fixture_at(expect_of(&r));
    step3(&rows, &step1(&f.twin).expect("rows")).expect("(c) the twin at T");
    assert_eq!(moved(&r, &live), Vec::<String>::new(), "the counter check");
    check(&copy, &f).expect("the oracle");
}

/// Whether `file` is RocksDB's MANIFEST or CURRENT.
fn manifest(file: &Path) -> bool {
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    name.starts_with("MANIFEST-") || name == "CURRENT"
}

fn in_dir(file: &Path, dir: &str) -> bool {
    file.starts_with(Path::new("regtest").join(dir))
}

/// I1's interval: 30 blocks, the copy point, the listing, 5 more blocks (beyond A2′, in its safe
/// direction), a scripted flush, `quiesce`, then the copy in `order`. Returns the copy, the reply,
/// `livecopycounters` after the copy, whether `chainstate/`'s `dir_state` held, and the oracle's
/// expectations at the source's tip (a copy point read after the copy).
fn i1_interval(
    order: impl FnOnce(&mut Vec<std::path::PathBuf>),
) -> (fixture::Datadir, serde_json::Value, serde_json::Value, bool, Fixture) {
    let mut s = Source::start(ARGS);
    s.mine(30);
    let r = s.copy_point();
    let state = dir_state(&s.chainstate());
    let listed = s.list();
    s.mine(5);
    let done = s.flush();
    assert!(done.lines().next().unwrap().ends_with("Release,AfterBk"), "{done}");
    s.quiesce();
    let copy = listed.run(order, |_| ());
    let live = s.live();
    let held = dir_state(&s.chainstate()) == state;
    let f = s.fixture_at(Expect::of(&s.node));
    (copy, r, live, held, f)
}

/// I1, M3's killer: an ordinary flush in the interval appends only. `chainstate/` (without its
/// MANIFEST and CURRENT) is copied first, then `blocks/`, then the MANIFEST and CURRENT.
#[test]
fn i1_an_ordinary_flush_in_the_interval_appends_only() {
    let (copy, r, live, held, f) = i1_interval(|files| chainstate_first(files));
    appends_only(held, &r, &live);
    assert_eq!(delta(&r, &live, "activations"), 5);
    assert!(delta(&r, &live, "flatfile_mutations") > 0);
    assert_eq!(delta(&r, &live, "disconnects"), 0);
    assert_eq!(f.expect.height, 35);
    check(&copy, &f).expect("the oracle at the post-flush tip");
}

/// I1's blocks-first variant (F3): `blocks/` before `chainstate/`. Its outcome is recorded, not
/// asserted.
#[test]
fn i1_blocks_first_is_recorded() {
    let (copy, r, live, held, f) = i1_interval(|files| files.sort_by_key(|f| !in_dir(f, "blocks")));
    eprintln!(
        "CH5-5 I1 blocks-first (F3): dir_state held {held}; counters moved {:?}; oracle {:?}",
        moved(&r, &live),
        check(&copy, &f)
    );
}

/// I3 (OD-CH5-25): a second copy point while the copy is paused, with no write between. It is
/// the next explicit flush point, so C1's interval ends at it: the tripwire holds and only meta's
/// super version moves, but the files change by one WAL switch and at most one meta SST, and the
/// paused copy fails on the switched WAL (pinned: if it ever passes, look). A fresh copy after
/// the second reply passes the oracle and the counter check against that reply.
#[test]
fn i3_a_second_copy_point_while_paused() {
    let mut s = Source::start(ARGS);
    s.mine(30);
    let r1 = s.copy_point();
    let events = |s: &Source| s.live()["events"].clone();
    let (before, e1) = (dir_state(&s.chainstate()), events(&s));
    let blocks = dir_state(&net(&s.dir.datadir).join("blocks"));
    let mut r2 = None;
    let paused = s.list().try_run(
        |_| (),
        |p| {
            if matches!(p, common::copier::Pause::Listed) {
                r2 = Some(s.copy_point());
            }
        },
    );
    let r2 = r2.expect("the second copy point");
    // 1. The same tripwire; only meta's super version moves; one job and one valid verdict more.
    let at2 = &r2["livecopy_counters"];
    let moved2 = moved(&r1, at2);
    assert_eq!(moved2.len(), 1, "{moved2:?}");
    assert!(moved2[0].starts_with("super version meta "), "{moved2:?}");
    let e2 = events(&s);
    let n = |e: &serde_json::Value, k: &str| e[k].as_u64().expect(k);
    assert_eq!(n(&e2, "flush_jobs_done"), n(&e1, "flush_jobs_done") + 1, "flush_jobs_done");
    assert_eq!(n(&e2, "verdicts"), n(&e1, "verdicts") + 1, "verdicts");
    let last = (n(&e2, "verdicts") - 1).to_string();
    let lines = s.ask("verdicts", "");
    let r2_verdict: Vec<&str> =
        lines.lines().filter(|l| l.split(' ').next() == Some(last.as_str())).collect();
    assert!(!r2_verdict.is_empty(), "{lines}");
    assert!(r2_verdict.iter().all(|l| l.contains(" valid=true ")), "{r2_verdict:?}");
    // 2. The files: one WAL switch and at most one new SST, meta's; nothing else created,
    // renamed or deleted (the MANIFEST grows in place); the block files unchanged.
    let after = dir_state(&s.chainstate());
    let (created, deleted, changed) = live::dir_diff(&before, &after);
    let wals = |v: &[String]| v.iter().filter(|n| n.ends_with(".log")).count();
    let ssts: Vec<&String> = created.iter().filter(|n| n.ends_with(".sst")).collect();
    assert_eq!((wals(&created), wals(&deleted), deleted.len()), (1, 1, 1), "{deleted:?}");
    assert_eq!(created.len(), 1 + ssts.len(), "{created:?}");
    assert!(ssts.len() <= 1, "{ssts:?}");
    for sst in &ssts {
        let cf = live::sst_cf(&s.chainstate().join(sst));
        assert_eq!(cf.as_deref(), Some("meta"), "{sst}");
    }
    assert!(changed.iter().all(|n| n.starts_with("MANIFEST-")), "{changed:?}");
    assert_eq!(dir_state(&net(&s.dir.datadir).join("blocks")), blocks, "the block files");
    println!("I3 files: created {created:?}, deleted {deleted:?}, changed {changed:?}");
    // 3. The copy paused across r2 is the known outcome: it fails on the WAL r2 switched away.
    let err = paused.err().expect("the paused copy failed");
    assert!(err.ends_with(".log vanished during the copy"), "{err}");
    assert!(deleted.iter().any(|w| err.contains(w.as_str())), "{err}");
    // 4. A fresh copy after r2's reply passes the oracle and the counter check against r2.
    let copy = s.list().now();
    assert_eq!(moved(&r2, &s.live()), Vec::<String>::new(), "the counter check");
    let f = s.fixture_at(expect_of(&r2));
    check(&copy, &f).expect("the oracle");
}

/// Core's functional-test key, regtest's spork signer (`dashd-node/src/sut.rs`).
const SPORK_KEY: &str = "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";
const I5_ARGS: &[&str] = &[live::MOCKTIME, SPORK_KEY];

/// I5 steps 1–5: S mines A1..A10; T takes A1..A5 and mines C6..C9 to `raw(52)` (less work);
/// they go to S, stored and not connected, and are logged; ChainLocks go on, and the ChainLock
/// handler's 5 s task (`cl-schdlr`, BL-626) runs the off→on reset; then the copy point, and the
/// copy is listed. T takes A1..A5 by `submitblock`
/// rather than a P2P sync: the same chain, without the network (a mechanics note).
fn i5_steps_1_to_5() -> (Source, serde_json::Value, live::Copying, Vec<String>) {
    let mut s = Source::start(I5_ARGS);
    let a = s.mine(10);
    let t_dir = fixture::Datadir::new();
    let t = rdashd(&t_dir, ARGS, None);
    for sub in &s.log[..5] {
        assert_eq!(sub.to(&t), "null", "T's A block {}", sub.hash);
    }
    let mined = fixture::call(&t, "generatetodescriptor", json!([4, "raw(52)"]));
    let c: Vec<String> = mined.as_array().expect("hashes").iter().map(fixture::text).collect();
    let c_blocks: Vec<fixture::Submission> =
        c.iter().map(|h| fixture::Submission::block(&t, h)).collect();
    t.stop();
    for mut sub in c_blocks {
        sub.result = sub.to(&s.node);
        s.log.push(sub);
    }
    assert_eq!(s.tip(), a[9], "S keeps A10");
    let tips = s.call("getchaintips", json!([]));
    let fork = tips.as_array().expect("tips").iter().find(|t| t["hash"] == c[3].as_str());
    let status = fork.map(|t| fixture::text(&t["status"]));
    assert!(
        matches!(status.as_deref(), Some("valid-headers" | "valid-fork")),
        "C9 is stored, not connected: {tips}"
    );
    assert!(s.ask("spork-update", "SPORK_19_CHAINLOCKS_ENABLED 0").starts_with("hash="));
    assert!(
        s.wait_live(Duration::from_secs(30), |l| l["events"]["cl_turned_on_reset"] == 1),
        "the off->on reset: {}",
        s.live()
    );
    let reply = s.copy_point();
    assert_eq!(fixture::text(&reply["bestblock"]), a[9]);
    let copying = s.list();
    (s, reply, copying, c)
}

/// I5 (the D2-1 boundary, outside A2'; M16's killer): late enforcement of a lock on C9 in the
/// interval reorganizes A6..A10 away. The §2.2 check fires; the reopen outcome is recorded.
#[test]
fn i5_late_enforcement_fires_the_counter_check() {
    let (s, reply, copying, c) = i5_steps_1_to_5();
    let enforced = s.live()["events"]["cl_enforced"].as_u64().expect("cl_enforced");
    assert!(s.ask("cl-floor", &format!("{} 9", c[3])).starts_with("relay="));
    let floor = |l: &serde_json::Value| {
        l["events"]["cl_enforced"].as_u64() > Some(enforced)
            && l["events"]["cl_enforced_floor"] == c[3].as_str()
    };
    assert!(s.wait_live(Duration::from_secs(30), floor), "enforcement: {}", s.live());
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while s.tip() != c[3] {
        assert!(std::time::Instant::now() < deadline, "the tip never reached C9");
        std::thread::sleep(Duration::from_millis(50));
    }
    let live = s.live();
    assert_eq!(delta(&reply, &live, "disconnects"), 5, "disconnects");
    assert_eq!(delta(&reply, &live, "activations"), 4, "activations");
    assert_eq!(delta(&reply, &live, "undo_writes"), 4, "undo_writes");
    assert!(delta(&reply, &live, "flatfile_mutations") > 0, "flatfile_mutations");
    s.flush();
    s.quiesce();
    let copy = copying.try_run(|_| (), |_| ());
    let moved = moved(&reply, &s.live());
    for field in ["disconnects ", "activations ", "undo_writes "] {
        assert!(moved.iter().any(|m| m.starts_with(field)), "{field}: {moved:?}");
    }
    println!("I5 the counter check fired: {moved:?}");
    let outcome = copy.and_then(|copy| {
        let f = s.fixture_through(expect_of(&reply), &c[3]);
        check(&copy, &f)
    });
    println!("I5 the reopen outcome (recorded): {outcome:?}");
    assert_eq!(s.live()["events"]["cl_turned_on_reset"], 1, "no second reset in the interval");
}

/// I5's positive control: steps 1–5 and 7, with no lock and a 12 s pause for step 6, which
/// covers at least 2 runs of the 5 s task. Its enforcement returns with no best lock after the
/// reset, so the floor stays unset; the §2.2 check holds, and the oracle passes at A10 against a
/// twin that replays the full log, C6..C9 included.
#[test]
fn i5_control_without_a_lock_holds() {
    let (s, reply, copying, c) = i5_steps_1_to_5();
    let floor = s.live()["events"]["cl_enforced_floor"].clone();
    std::thread::sleep(Duration::from_secs(12));
    assert_eq!(s.live()["events"]["cl_enforced_floor"], floor, "the floor");
    s.flush();
    s.quiesce();
    let copy = copying.now();
    assert_eq!(moved(&reply, &s.live()), Vec::<String>::new(), "the counter check");
    let f = s.fixture_through(expect_of(&reply), &c[3]);
    check(&copy, &f).expect("the oracle");
    assert_eq!(s.live()["events"]["cl_turned_on_reset"], 1, "no second reset in the interval");
}

/// W and B, the live profile's per-CF numbers (§3.2, Q4).
const W: i64 = dashd_store::LiveCopy::REGTEST.write_buffer_bytes as i64;
const B: i64 = dashd_store::LiveCopy::REGTEST.bound_bytes as i64;
const MIB: i64 = 1 << 20;

/// A barrier verdict: per column family, `(hwm, valid)`.
type Verdict = std::collections::BTreeMap<String, (i64, bool)>;

/// The barrier verdict `index` (the `verdicts` hook).
fn verdict(s: &Source, index: u64) -> Verdict {
    let lines = s.ask("verdicts", "");
    let at = index.to_string();
    let field = |line: &str, key: &str| -> String {
        let word = line.split(' ').find_map(|w| w.strip_prefix(key)).unwrap_or_else(|| {
            panic!("{key} in {line}");
        });
        word.to_owned()
    };
    let cfs: Verdict = lines
        .lines()
        .filter(|l| l.split(' ').next() == Some(at.as_str()))
        .map(|l| {
            let cf = l.split(' ').nth(1).expect("cf").to_owned();
            (cf, (field(l, "hwm=").parse().expect("hwm"), field(l, "valid=") == "true"))
        })
        .collect();
    assert_eq!(cfs.len(), 11, "verdict {index}: {lines}");
    cfs
}

/// The latest barrier verdict.
fn last_verdict(s: &Source) -> Verdict {
    let n = s.live()["events"]["verdicts"].as_u64().expect("verdicts");
    verdict(s, n - 1)
}

/// A column family's allocation in a verdict's interval: its `hwm` less the empty memtable's,
/// which is the smallest `hwm` of the verdict (a column family no batch wrote; every column
/// family has the same W, so the same arena). Asserts the interval was valid for every family.
fn allocation(v: &Verdict, cf: &str) -> i64 {
    assert!(v.values().all(|(_, valid)| *valid), "an invalid interval: {v:?}");
    let empty = v.values().map(|(hwm, _)| *hwm).min().expect("11 families");
    v[cf].0 - empty
}

/// What [`i2_interval`] measured: the copy point's reply, the counters after the copy, whether
/// `dir_state(chainstate)` held, and the verdict of the interval.
struct Interval {
    reply: serde_json::Value,
    live: serde_json::Value,
    held: bool,
    verdict: Verdict,
}

/// I2's interval (§6.5): `before` runs before the copy point; the copy point and the listing;
/// `workload` in the interval; the forced `flush`; the follow-up write (one header, then another
/// `flush`); `quiesce`; the copy in I1's order (chainstate without its MANIFEST and CURRENT,
/// then blocks, then those). Then the §2.2 check, a second copy point whose verdict measures the
/// interval, and the oracle at that copy point against a twin of the whole log.
fn i2_interval(
    args: &'static [&'static str],
    before: impl FnOnce(&mut Source),
    workload: impl FnOnce(&mut Source),
) -> Interval {
    let mut s = Source::start(args);
    before(&mut s);
    let reply = s.copy_point();
    let state = dir_state(&s.chainstate());
    let listed = s.list();
    workload(&mut s);
    s.flush();
    workload::follow_up(&mut s);
    s.flush();
    s.quiesce();
    let copy = listed.run(|files| chainstate_first(files), |_| ());
    let live = s.live();
    let held = dir_state(&s.chainstate()) == state;
    let r2 = s.copy_point();
    let verdict = last_verdict(&s);
    // The twin replays the whole log: the headers come after the tip's block.
    let last = s.log.last().expect("a log").hash.clone();
    let f = s.fixture_through(expect_of(&r2), &last);
    check(&copy, &f).expect("the oracle");
    Interval {
        reply,
        live,
        held,
        verdict,
    }
}

/// I2's assertions after the window: `dir_state` and every super version unchanged.
fn i2_held(i: &Interval) {
    appends_only(i.held, &i.reply, &i.live);
}

/// I1's and I2's copy order: `chainstate/` without its MANIFEST and CURRENT, then `blocks/`, then
/// those two.
fn chainstate_first(files: &mut [std::path::PathBuf]) {
    files.sort_by_key(|f| match (in_dir(f, "chainstate"), manifest(f)) {
        (true, false) => 0,
        (false, _) => 1,
        (true, true) => 2,
    });
}

/// The interval appended only: `dir_state(chainstate)` held and no super version moved from the
/// copy point's `reply` to `live`.
fn appends_only(held: bool, reply: &serde_json::Value, live: &serde_json::Value) {
    assert!(held, "dir_state(chainstate) changed in the interval");
    let svs: Vec<_> = moved(reply, live).into_iter().filter(|m| m.starts_with("super")).collect();
    assert_eq!(svs, Vec::<String>::new(), "super versions");
}

/// I2a, M4/M5/M8's killer (§6.4's coins workload): 570 000 coins rows in the interval, then the
/// follow-up write. Expected deltas: `activations` +3, `flatfile_mutations` > 0, `disconnects` 0.
#[test]
fn i2a_the_coins_workload_appends_only() {
    let i = i2_interval(ARGS, |s| drop(s.mine(160)), workload::coins);
    let coins = allocation(&i.verdict, "coins");
    println!("I2a: coins allocation {coins} B; verdict {:?}", i.verdict);
    assert!((16 * MIB + MIB..=B).contains(&coins), "S + 1 MiB <= {coins} <= B");
    i2_held(&i);
    assert_eq!(delta(&i.reply, &i.live, "activations"), 3);
    assert!(delta(&i.reply, &i.live, "flatfile_mutations") > 0);
    assert_eq!(delta(&i.reply, &i.live, "disconnects"), 0);
}

/// I2b's nodes (the source, its twin, the copy): regtest's default `-checkblockindex` walks the
/// whole index on every header, quadratic over 50 000 (17 headers/s and falling); it checks
/// memory only, never the files (a mechanics note).
const NO_CHECKBLOCKINDEX: &[&str] = &[live::MOCKTIME, "-checkblockindex=0"];

/// I2b, M8's killer (§6.4's headers workload): 50 000 headers-only records in the interval,
/// then the follow-up write. Expected deltas: none (headers only: no activation, no block file).
#[test]
fn i2b_the_headers_workload_appends_only() {
    let i = i2_interval(
        NO_CHECKBLOCKINDEX,
        |s| drop(s.mine(30)),
        |s| {
            workload::headers(s, 10, 5_000);
        },
    );
    let block_index = allocation(&i.verdict, "block_index");
    println!("I2b: block_index allocation {block_index} B; verdict {:?}", i.verdict);
    assert!((5 * MIB..=B).contains(&block_index), "5 MiB <= {block_index} <= B");
    i2_held(&i);
    assert_eq!(moved(&i.reply, &i.live), Vec::<String>::new(), "the counter check");
}

/// I4 (supplementary; M2 and M7's): A1 ~ 0.55 W of coins rows at the first copy point, whose
/// barrier flushes the memtable; then A2 in the interval with A1 + A2 >= W + 1 MiB and A2 <= B,
/// the follow-up write and `quiesce`: `dir_state` and the super versions unchanged. A1 is 54
/// fan-out transactions; A2's count is set from A1's measured allocation per transaction.
#[test]
fn i4_cumulative_volume_needs_the_barrier_flush() {
    const A1_TXS: u64 = 54;
    let mut a1 = 0;
    let mut a2_txs = 0;
    let i = i2_interval(
        ARGS,
        |s| {
            s.mine(220);
            workload::fan_outs(s, 1..=A1_TXS);
        },
        |s| {
            // The copy point's verdict measured A1.
            a1 = allocation(&last_verdict(s), "coins");
            let per_tx = a1 / A1_TXS as i64;
            a2_txs = ((W + MIB - a1) / per_tx + 2) as u64;
            workload::fan_outs(s, A1_TXS + 1..=A1_TXS + a2_txs);
        },
    );
    let a2 = allocation(&i.verdict, "coins");
    println!("I4: A1 {a1} B ({A1_TXS} txs), A2 {a2} B ({a2_txs} txs), W {W}, B {B}");
    assert!((W / 2..=W * 6 / 10).contains(&a1), "A1 ~ 0.55 W: {a1}");
    assert!(a1 + a2 >= W + MIB, "A1 + A2 >= W + 1 MiB");
    assert!(a2 <= B, "A2 <= B");
    i2_held(&i);
}

/// The nightly's receipt of its known transaction K, from the source's net log (Sol r2 C-2,
/// option (iii)). `received:` lines name no txid, so the receipt rests on K being the only
/// transaction the source was ever offered: exactly one transaction inv, K's (`ids` holds both
/// byte orders of its txid), and exactly one transaction body, after that inv and from the same
/// peer. The nightly's traffic starts only once this holds, so no other body can stand in for K's.
fn tx_receipt(log: &str, ids: [&str; 2]) -> Result<(), String> {
    let lines: Vec<&str> = log.lines().collect();
    let tx =
        |l: &&str, kind: &str| ["tx", "dstx"].iter().any(|c| l.contains(&format!("{kind}{c}")));
    let invs: Vec<usize> = (0..lines.len()).filter(|&i| tx(&lines[i], "got inv: ")).collect();
    let bodies: Vec<usize> = (0..lines.len())
        .filter(|&i| ["tx", "dstx"].iter().any(|c| lines[i].contains(&format!("received: {c} ("))))
        .collect();
    let &[inv] = &invs[..] else {
        return Err(format!("{} transaction invs, not exactly K's", invs.len()));
    };
    if !ids.iter().any(|id| lines[inv].contains(id)) {
        return Err(format!("the only transaction inv is not K's: {}", lines[inv]));
    }
    let peer = lines[inv].rsplit_once(" peer=").map(|(_, p)| p).unwrap_or("?");
    match bodies[..] {
        [] => Err("K's body has not arrived".into()),
        [body] if body > inv && lines[body].ends_with(&format!(" peer={peer}")) => Ok(()),
        _ => Err(format!("transaction bodies at lines {bodies:?}, K's inv at {inv}")),
    }
}

/// [`tx_receipt`] fails unless K's own body arrived: no body; a body before K's inv; a body from
/// another peer; another transaction's inv and body before K's (Sol r2 C-2's case).
#[test]
fn the_known_tx_receipt_needs_ks_own_body() {
    let k = "aa".repeat(32);
    let x = "bb".repeat(32);
    let inv = |id: &str, peer: u32| format!("got inv: tx:{id}  new peer={peer}");
    let body = |peer: u32| format!("received: tx (226 bytes) peer={peer}");
    let ids = [k.as_str(), k.as_str()];
    let receipt = |lines: &[String]| tx_receipt(&lines.join("\n"), ids);
    assert_eq!(receipt(&[inv(&k, 0), body(0)]), Ok(()));
    let failing = [
        vec![inv(&k, 0)],
        vec![body(0), inv(&k, 0)],
        vec![inv(&k, 0), body(1)],
        vec![inv(&x, 0), inv(&k, 0), body(0)],
        vec![inv(&k, 0), body(0), body(0)],
        vec![inv(&x, 0), body(0)],
    ];
    for lines in failing {
        assert!(receipt(&lines).is_err(), "{lines:?}");
    }
}

/// The nightly's source: its net log shows what reached it (`got inv:`, `received:`).
/// rdashd defaults to `-blocksonly=1` until P5 (DEC-322), whose `version` asks peers not to
/// relay transactions; the nightly's source takes them, so its peer's relay reaches it.
const NIGHTLY_ARGS: &[&str] = &[live::MOCKTIME, "-debug=net", "-blocksonly=0"];

/// The nightly (§6.7, supplementary; `contrib/node/livecopy/nightly.sh`): a copy of a live source
/// while a dashd peer relays transactions to it, through the oracle and the §2.2 check; with
/// `blocks`, the report-only variant, where the peer also mines blocks during the copy and the
/// deltas and the oracle's outcome are reported, not asserted. Each file of the copy waits 20 ms,
/// so the copy overlaps the traffic. `getqrinfo` and `getmnlistd` are P6b deviations at this
/// head, so no quorum-row request can reach the source's snapshot writer (R17); the traffic is
/// relay (and, in the variant, blocks) only.
///
/// Sol r1 C-2: every traffic call must succeed (an error fails the test), and the counts are of
/// successful calls only. At the copier's first pause, with the traffic held, a known
/// transaction is sent and must reach the source inside the interval: the source logs its inv
/// and then receives a transaction body (the source has no mempool at this head, P5; it
/// requests and drops the body). The peer's clock steps 1 s per call (`setmocktime`), so its
/// inventory trickle runs under the fixed mocktime.
fn nightly(blocks: bool) {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    let dashd = require_dashd();
    let core_dir = fixture::Datadir::new();
    let p2p = free_port();
    // An explicit bind, and no onion service: without them, a listening dashd also binds a fixed
    // onion-target port, which another listening dashd on the machine may hold.
    let bind = format!("-bind=127.0.0.1:{p2p}");
    let core = RpcNode::start_on(
        &dashd,
        core_dir.home.path(),
        &core_dir.datadir,
        &[live::MOCKTIME, "-listen=1", bind.as_str(), "-listenonion=0", "-fallbackfee=0.0002"],
    );
    fixture::call(&core, "createwallet", json!(["w"]));
    let to = fixture::text(&fixture::call(&core, "getnewaddress", json!([])));
    fixture::call(&core, "generatetoaddress", json!([110, to]));
    let core_tip = fixture::text(&fixture::call(&core, "getbestblockhash", json!([])));
    let mut s = Source::start(NIGHTLY_ARGS);
    s.call("addnode", json!([format!("127.0.0.1:{p2p}"), "onetry"]));
    wait_until("the source's sync", || s.tip() == core_tip);
    for h in 1..=110 {
        let hash = fixture::text(&s.call("getblockhash", json!([h])));
        let sub = fixture::Submission::block(&s.node, &hash);
        s.log.push(sub);
    }
    let start: u64 = live::MOCKTIME.trim_start_matches("-mocktime=").parse().expect("mocktime");
    let clock = AtomicU64::new(start);
    // One traffic call on the peer, its clock stepped first; an error is the test's failure.
    let peer = |method: &str, params: serde_json::Value| -> Result<serde_json::Value, String> {
        let t = clock.fetch_add(1, Ordering::SeqCst) + 1;
        fixture::try_call(&core, "setmocktime", json!([t]))?;
        fixture::try_call(&core, method, params)
    };
    let source_log = debug_log(&s.dir.datadir);
    let reply = s.copy_point();
    let listed = s.list();
    let done = AtomicBool::new(false);
    let received = AtomicBool::new(false);
    let failure: Mutex<Option<String>> = Mutex::new(None);
    let (sent, mined) = (AtomicU64::new(0), AtomicU64::new(0));
    let mut held: Option<Result<String, String>> = None;
    let copy = std::thread::scope(|scope| {
        scope.spawn(|| {
            // The traffic starts once the known transaction's receipt holds, so no transaction
            // is in flight before it (Sol r2 C-2, option (iii)) and no block mines it first.
            while !received.load(Ordering::SeqCst) {
                if done.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let step = || -> Result<(), String> {
                fixture::text(&peer("sendtoaddress", json!([to, 0.01]))?);
                let n = sent.fetch_add(1, Ordering::SeqCst) + 1;
                if blocks && n % 10 == 0 {
                    let hashes = peer("generatetoaddress", json!([1, to]))?;
                    if hashes.as_array().map(Vec::len) != Some(1) {
                        return Err(format!("generatetoaddress: {hashes}"));
                    }
                    mined.fetch_add(1, Ordering::SeqCst);
                }
                Ok(())
            };
            while !done.load(Ordering::SeqCst) {
                if let Err(e) = step() {
                    *failure.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        // Before any traffic: the known transaction, then its receipt ([`tx_receipt`]).
        let known = || -> Result<String, String> {
            let txid = fixture::text(&peer("sendtoaddress", json!([to, 0.01]))?);
            let internal: String = txid
                .as_bytes()
                .chunks(2)
                .rev()
                .map(|c| std::str::from_utf8(c).expect("hex"))
                .collect();
            let deadline = std::time::Instant::now() + Duration::from_secs(60);
            loop {
                match tx_receipt(&read(&source_log), [&txid, &internal]) {
                    Ok(()) => {
                        received.store(true, Ordering::SeqCst);
                        return Ok(txid);
                    }
                    Err(e) if std::time::Instant::now() > deadline => {
                        return Err(format!("{txid} never reached the source: {e}"));
                    }
                    Err(_) => {}
                }
                peer("getblockcount", json!([]))?;
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        let copy = listed.try_run(
            |_| (),
            |p| {
                if matches!(p, common::copier::Pause::Before(_)) {
                    if held.is_none() {
                        held = Some(known());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            },
        );
        done.store(true, Ordering::SeqCst);
        copy
    });
    if let Some(e) = failure.into_inner().unwrap_or_else(|p| p.into_inner()) {
        panic!("a traffic call failed: {e}");
    }
    let known = held.expect("the copier paused").expect("the known transaction");
    let live = s.live();
    let moved = moved(&reply, &live);
    let (sent, mined) = (sent.into_inner(), mined.into_inner());
    assert!(sent > 0, "no transaction was sent during the copy");
    let f = s.fixture_at(expect_of(&reply));
    let outcome = copy.and_then(|copy| check(&copy, &f));
    println!(
        "CH5-5 nightly (blocks {blocks}): {sent} sent and {mined} mined successfully; {known} \
         reached the source while the copier was held; moved {moved:?}; the oracle {outcome:?}"
    );
    if !blocks {
        assert_eq!(moved, Vec::<String>::new(), "the counter check");
        outcome.expect("the oracle");
    }
    // The peer is dashd: `RpcNode::stop` expects rdashd's reply.
    core.node.stop_ok(libc::SIGTERM);
}

#[test]
#[ignore = "nightly: contrib/node/livecopy/nightly.sh"]
fn nightly_copy_under_relay() {
    nightly(false);
}

#[test]
#[ignore = "nightly: contrib/node/livecopy/nightly.sh"]
fn nightly_report_only_blocks_arrive() {
    nightly(true);
}
