//! P3 §5.1's (A − B) Core leg (oracle E1/E6; the plan rev8, OD-P3-8): the fixture's blocks
//! ([`node_amb::fixture`], the node's funded blocks) into two of Core's bench sessions
//! (`SessionMode::Bench`, ABI 34) at mock time `T0`, `check_block_index = false`, no debug
//! categories.
//!
//! - **A:** two outbound peers. S serves `headers`, then a `block` for each `getdata`; H asked
//!   for high-bandwidth compact blocks (`sendcmpct(1, 1)`) before the prefix and then announced
//!   the prefix tip. The prefix goes through S over P2P, uncounted. In the window the caller is
//!   counted only inside its `send_pass` calls (less the calibrated read-pair maximum per call,
//!   rev4), every other thread continuously; `deliver` and `drain` are the caller's gaps.
//! - **B:** the prefix through the session's constructor fixture, the workload staged
//!   (`stage_blocks`) before the window and replayed by `replay_direct` (E6) with PeerManager
//!   unregistered, every thread counted continuously; the caller's own reads at the two ends and
//!   the tip check are inside its count, which raises Core's B and so lowers the figure the node
//!   must beat.
//!
//! A ends when the tip is the last block after a send pass (which ends with
//! `SyncWithValidationInterfaceQueue`) and one further send pass has run with nothing pending
//! (no block in flight or unserved; on S and H no unread bytes, paused receive, queued send or
//! queued announcement) and H's last `cmpctblock` drained. After the window, H must hold exactly
//! one `cmpctblock` per workload block, or the pass is refused. Every session starts with
//! `-forcecompactdb`, so LevelDB's background thread exists before any window. Each
//! measured pass runs in its own child ([`node_amb::sys::supervisor`]). `amb_core_smoke` runs
//! both passes uncounted on a few blocks in every test run; `amb_core_leg` measures (ignored,
//! alone, release):
//! `AMB_OUT=<dir> [AMB_NODE_RECORDS=<the node leg's AMB_OUT>] cargo test -p node-l1 --features
//! oracle --release --config 'profile.release.package.dashd-masternode.debug-assertions=true'
//! --test amb_core amb_core_leg -- --ignored --test-threads=1 --nocapture`. The `--config` is
//! `oracle`'s `dashd-masternode/core-layout`, which refuses an optimized build without it; that
//! crate is not on the counted path.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::collections::BTreeMap;

use dash_consensus::primitives::decode_header;
use dash_oracle_sys::{
    Chain, ConnType, NetAddr, Oracle, PeerOutcome, PeerSession, Pin, Service, SessionConfig,
    SessionMode, SessionOutcome,
};
use node_amb::Refusal;
use node_amb::account::Completion;
use node_amb::compare::{self, Verdict};
use node_amb::fixture::{Fixture, PEER_SERVICES, SMOKE_ROWS, T0};
use node_amb::record::{Conditions, CoreBuild, Identity, Leg, Pass, Record};
use node_amb::sys::meter::{Meter, Plain};
use node_amb::sys::{leg, supervisor};

const CHILD: &str = "amb_core_child";
const THREADS_CHILD: &str = "amb_core_threads_child";
/// version.h `PROTOCOL_VERSION`.
const PROTOCOL_VERSION: i32 = 70242;
/// The sessions' mode: Core with nothing recorded.
const MODE: SessionMode = SessionMode::Bench;
const RNG_SEED: u64 = 0x5033_414d_4231;
/// Send passes a phase may take before the test calls it stuck.
const MAX_PASSES: usize = 100_000;
/// `MSG_BLOCK`.
const MSG_BLOCK: u32 = 2;

type Hash = [u8; 32];

fn service(last: u8) -> Service {
    Service::new(NetAddr::ipv4([10, 3, 0, last]), 19899)
}

fn compact_size(out: &mut Vec<u8>, n: usize) {
    match n {
        0..0xfd => out.push(n as u8),
        _ => {
            out.push(0xfd);
            out.extend(u16::try_from(n).expect("at most 65535 headers").to_le_bytes());
        }
    }
}

fn read_compact_size(r: &mut &[u8]) -> usize {
    let (n, used) = match r[0] {
        n @ 0..0xfd => (n as usize, 1),
        0xfd => (u16::from_le_bytes([r[1], r[2]]) as usize, 3),
        _ => panic!("a large inv count"),
    };
    *r = &r[used..];
    n
}

/// The node's block hash (internal byte order, Core's uint256 bytes).
fn hash_of(block: &[u8]) -> Hash {
    *decode_header(&mut &block[..80]).expect("a header").into_value().hash().as_bytes()
}

fn headers_payload(blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut p = Vec::new();
    compact_size(&mut p, blocks.len());
    for b in blocks {
        p.extend(&b[..80]);
        p.push(0);
    }
    p
}

fn version_payload(start_height: i32, nonce: u64) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend(PROTOCOL_VERSION.to_le_bytes());
    p.extend(PEER_SERVICES.to_le_bytes());
    p.extend(T0.to_le_bytes());
    for _ in 0..2 {
        p.extend([0u8; 8]);
        p.extend([0u8; 10]);
        p.extend([0xff, 0xff, 127, 0, 0, 1]);
        p.extend(19899u16.to_be_bytes());
    }
    p.extend(nonce.to_le_bytes());
    p.push(5);
    p.extend(b"/amb/");
    p.extend(start_height.to_le_bytes());
    p.push(1);
    p
}

/// Core's options beyond the session's own: `-forcecompactdb` compacts each LevelDB as it
/// opens, which starts LevelDB's background thread (started on a first compaction, oracle.h's
/// bench contract) before any window, so none is created inside one.
const ARGS: [&str; 1] = ["-forcecompactdb"];

fn config(blocks: Vec<Vec<u8>>) -> SessionConfig {
    SessionConfig {
        chain: Chain::new("regtest", &ARGS),
        our_services: PEER_SERVICES,
        mocktime: T0,
        rng_seed: RNG_SEED,
        blocks,
        min_chain_work: None,
        max_tip_age: None,
        check_block_index: false,
    }
}

fn oracle() -> Oracle {
    Oracle::new(Pin::V24_0)
}

/// The linked Core build, with `pins.txt`'s row for its pin: the comparison refuses unless the
/// archive is that row's `sha256(plain)`, pinned, of the plain flavor.
fn core_build(oracle: &Oracle) -> CoreBuild {
    let pin = oracle.pin();
    // pins.txt: pin, commit, branch, sha256(plain), sha256(fuzz).
    let row: Vec<&str> = include_str!("../../dash-oracle-sys/pins.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find(|cols| cols.first() == Some(&pin.name()))
        .expect("the pin's row");
    let [_, commit, _, plain_sha256, ..] = row[..] else {
        panic!("a short pins.txt row {row:?}");
    };
    CoreBuild {
        pin: pin.name().into(),
        commit: commit.into(),
        abi: oracle.abi_version(),
        archive_sha256: pin.archive_sha256().unwrap_or("none").into(),
        pinned_sha256: plain_sha256.into(),
        pinned: dash_oracle_sys::ARCHIVES_PINNED,
        flavor: dash_oracle_sys::ARCHIVE_FLAVOR.unwrap_or("none").into(),
    }
}

/// A session in [`MODE`], and whether any of its outcomes recorded a draw or a call.
struct Bench {
    s: PeerSession,
    recorded: bool,
}

impl Bench {
    fn open(oracle: &Oracle, blocks: Vec<Vec<u8>>) -> Bench {
        let s = oracle.peer_session_new_mode(&config(blocks), MODE).expect("a session");
        Bench {
            s: s.expect("the start chain connects"),
            recorded: false,
        }
    }

    fn outcome(&mut self) -> SessionOutcome {
        let o = self.s.session_outcome().expect("an outcome");
        self.recorded |= !o.draws.is_empty() || !o.calls.is_empty();
        o
    }

    fn tip(&mut self) -> Hash {
        self.outcome().tip.hash
    }

    /// The run conditions at a window's start: `bench` holds when the session is in bench mode,
    /// no outcome so far recorded anything, and the log has no output (each pass rechecks the
    /// outcomes after its window).
    fn conditions(&mut self, hb: Option<bool>) -> Conditions {
        let c = self.outcome().conditions;
        Conditions {
            ibd_at_start: c.initial_block_download,
            hb_at_start: hb,
            options: {
                let mut o: Vec<String> =
                    ["-regtest", "-checkblockindex=0"].map(String::from).into();
                o.extend(ARGS.map(String::from));
                o.push(format!("mocktime={T0}"));
                o
            },
            check_block_index: vec![("core".into(), c.check_block_index)],
            log_categories: c.log_categories,
            bench: Some(MODE == SessionMode::Bench && !self.recorded && !c.log_enabled),
            // ChainTestingSetup's `StartScriptCheckWorkerThreads(2)`: counted, as every thread.
            script_check_threads: node_amb::sys::script_check_threads().expect("the census"),
        }
    }
}

/// A scripted peer of a bench session: what its end decoded, by message.
struct Wire {
    id: i64,
    cmpcts: BTreeMap<Hash, usize>,
    wanted: Vec<Hash>,
    /// A block whose `cmpctblock` this peer drops unread (the relay-map test).
    drop: Option<Hash>,
    /// At the last drain, the node had work left for this peer: bytes of its socket unread, its
    /// receive paused, messages queued to send, or blocks queued for announcement.
    pending: bool,
}

impl Wire {
    /// Its end reads what the node wrote (uncounted: the peer's side).
    fn drain(&mut self, s: &mut PeerSession) {
        s.drain(self.id, u64::MAX).expect("a drain");
        let o = s.peer_outcome(self.id).expect("an outcome");
        assert!(o.misbehaving.is_empty(), "{:?}", o.misbehaving);
        self.pending = pending(&o);
        for (cmd, payload) in o.sent {
            match cmd.as_str() {
                "cmpctblock" => {
                    let hash = hash_of(&payload);
                    if self.drop != Some(hash) {
                        *self.cmpcts.entry(hash).or_default() += 1;
                    }
                }
                "getdata" => {
                    let mut r = payload.as_slice();
                    for _ in 0..read_compact_size(&mut r) {
                        let (kind, rest) = r.split_at(4);
                        let (hash, rest) = rest.split_at(32);
                        if u32::from_le_bytes(kind.try_into().expect("4 bytes")) == MSG_BLOCK {
                            self.wanted.push(hash.try_into().expect("32 bytes"));
                        }
                        r = rest;
                    }
                }
                _ => {}
            }
        }
    }
}

/// The node has work left for a peer (Opus M2): bytes of its socket unread, its receive paused,
/// messages queued to send, or blocks queued for announcement.
fn pending(o: &PeerOutcome) -> bool {
    let (inv, headers) = o.peer.as_ref().map_or((&[][..], &[][..]), |p| {
        (&p.blocks_for_inv_relay[..], &p.blocks_for_headers_relay[..])
    });
    Pending {
        unread_bytes: o.peer_unread_bytes,
        pause_recv: o.pause_recv,
        queued_messages: o.queued_messages,
        inv_relay: inv.len(),
        headers_relay: headers.len(),
    }
    .any()
}

/// [`pending`]'s inputs.
#[derive(Clone, Copy, Default)]
struct Pending {
    unread_bytes: u64,
    pause_recv: bool,
    queued_messages: u64,
    inv_relay: usize,
    headers_relay: usize,
}

impl Pending {
    fn any(self) -> bool {
        self.unread_bytes != 0
            || self.pause_recv
            || self.queued_messages != 0
            || self.inv_relay != 0
            || self.headers_relay != 0
    }
}

/// A peer taken through version and verack (the passes are the caller's).
fn add_peer(s: &mut PeerSession, last: u8, start_height: i32) -> Wire {
    let id = s.add_peer(ConnType::OutboundFullRelay, &service(last)).expect("a peer");
    s.deliver(id, "version", &version_payload(start_height, u64::from(last)), &[])
        .expect("version");
    s.deliver(id, "verack", &[], &[]).expect("verack");
    Wire {
        id,
        cmpcts: BTreeMap::new(),
        wanted: Vec::new(),
        drop: None,
        pending: false,
    }
}

/// [`pass_a`]'s test knobs.
#[derive(Default)]
struct Script {
    /// A fixture block past the pass's rows that S announces after the workload and never
    /// serves (the pending-work test).
    withhold: Option<Vec<u8>>,
    /// A workload block whose `cmpctblock` H drops (the relay-map test).
    drop_cmpct: Option<Hash>,
}

/// A pass's record and what the smoke compares.
struct PassOut {
    record: Record,
    tip: Hash,
    cmpcts: BTreeMap<Hash, usize>,
}

/// Core A on `f`, with `script`'s test knobs. After the window, H must hold exactly one
/// `cmpctblock` per workload block, or the pass is refused (SOL-2).
fn pass_a(
    oracle: &Oracle,
    f: &Fixture,
    meter: &mut dyn Meter,
    script: Script,
    revision: String,
) -> Result<PassOut, Refusal> {
    let by_hash: BTreeMap<Hash, &Vec<u8>> =
        f.prefix.iter().chain(&f.workload).map(|b| (hash_of(b), b)).collect();
    let mut b = Bench::open(oracle, Vec::new());
    let total = (f.prefix.len() + f.workload.len()) as i32;
    let mut sp = add_peer(&mut b.s, 1, total);
    let mut hp = add_peer(&mut b.s, 2, f.prefix.len() as i32);
    hp.drop = script.drop_cmpct;
    for _ in 0..4 {
        b.s.send_pass(T0).expect("a pass");
    }
    // H's handshake and HB negotiation come first.
    b.s.deliver(hp.id, "sendcmpct", &[vec![1], 1u64.to_le_bytes().to_vec()].concat(), &[])
        .expect("sendcmpct");
    for _ in 0..2 {
        b.s.send_pass(T0).expect("a pass");
    }
    // Serves `part` through S until the tip is `last`, `pass` running each send pass.
    let serve = |b: &mut Bench,
                 sp: &mut Wire,
                 hp: &mut Wire,
                 part: &[Vec<u8>],
                 pass: &mut dyn FnMut(&mut PeerSession) -> Result<(), Refusal>|
     -> Result<(), Refusal> {
        let last = hash_of(part.last().expect("a block"));
        b.s.deliver(sp.id, "headers", &headers_payload(part), &[]).expect("headers");
        for _ in 0..MAX_PASSES {
            pass(&mut b.s)?;
            sp.drain(&mut b.s);
            hp.drain(&mut b.s);
            for h in std::mem::take(&mut sp.wanted) {
                b.s.deliver(sp.id, "block", by_hash[&h], &[]).expect("a block");
            }
            if b.tip() == last {
                return Ok(());
            }
        }
        panic!("the tip stopped short of {last:?}");
    };
    serve(&mut b, &mut sp, &mut hp, &f.prefix, &mut |s| {
        s.send_pass(T0).expect("a pass");
        Ok(())
    })?;
    // H knows every parent header: the prefix tip.
    b.s.deliver(hp.id, "headers", &headers_payload(&f.prefix[f.prefix.len() - 1..]), &[])
        .expect("headers");
    for _ in 0..4 {
        b.s.send_pass(T0).expect("a pass");
    }
    b.s.sync_validation().expect("quiesce");
    hp.drain(&mut b.s);
    sp.drain(&mut b.s);
    let g = b.outcome();
    assert_eq!(g.tip.hash, hash_of(f.prefix.last().expect("a prefix")), "the prefix tip");
    assert!(g.globals.blocks_in_flight.is_empty(), "blocks in flight after the prefix");
    assert!(!sp.pending && !hp.pending, "the prefix left work pending");
    let h_state = b.s.peer_outcome(hp.id).expect("H").state.expect("H's state");
    let mut conditions = b.conditions(Some(h_state.requested_hb_cmpctblocks));
    hp.cmpcts.clear();

    meter.open(true)?;
    let mut counted_pass = |s: &mut PeerSession| {
        let mut r = Ok(());
        meter.counted(&mut || r = s.send_pass(T0))?;
        r.map_err(|e| Refusal(format!("send_pass: {e:?}")))
    };
    serve(&mut b, &mut sp, &mut hp, &f.workload, &mut counted_pass)?;
    if let Some(extra) = &script.withhold {
        let hash = hash_of(extra);
        b.s.deliver(sp.id, "headers", &headers_payload(std::slice::from_ref(extra)), &[])
            .expect("headers");
        for _ in 0..MAX_PASSES {
            if sp.wanted.contains(&hash) {
                break;
            }
            counted_pass(&mut b.s)?;
            sp.drain(&mut b.s);
        }
        assert!(sp.wanted.contains(&hash), "the withheld block was never asked for");
    }
    // send_pass ends with SyncWithValidationInterfaceQueue; one further pass is the actor's.
    let r = counted_pass(&mut b.s);
    sp.drain(&mut b.s);
    hp.drain(&mut b.s);
    let end = b.outcome();
    let last = hash_of(f.workload.last().expect("a workload"));
    let completion = Completion {
        tip_is_last: end.tip.hash == last,
        signals_barrier: true,
        actor_barrier: Some(r.is_ok()),
        hb_last_cmpct: Some(hp.cmpcts.contains_key(&last)),
        // Nothing pending: no block in flight or unserved, and on S and H no unread socket
        // bytes, paused receive, queued send or queued announcement. Core's received-message
        // queue is not in the peer outcome; every message the peers sent is shown handled by
        // its effect: the headers by S's getdata, each block by an empty in-flight list.
        quiesced: Some(
            end.globals.blocks_in_flight.is_empty()
                && sp.wanted.is_empty()
                && !sp.pending
                && !hp.pending,
        ),
    };
    let counts = meter.close(&completion, true)?;
    hp.drain(&mut b.s);
    compare::one_each(&hp.cmpcts, f.workload.iter().map(|b| hash_of(b)))?;
    let tip = b.tip();
    conditions.bench = conditions.bench.map(|bench| bench && !b.recorded);
    Ok(PassOut {
        record: Record::new(
            Leg::Core,
            Pass::A,
            Identity::of(f, revision, Some(core_build(oracle))),
            conditions,
            completion,
            counts,
        ),
        tip,
        cmpcts: hp.cmpcts,
    })
}

/// Core B on `f`.
fn pass_b(
    oracle: &Oracle,
    f: &Fixture,
    meter: &mut dyn Meter,
    revision: String,
) -> Result<PassOut, Refusal> {
    let mut b = Bench::open(oracle, f.prefix.clone());
    let staged = b.s.stage_blocks(&f.workload).expect("staged");
    assert_eq!(
        staged,
        f.workload.iter().map(|b| hash_of(b)).collect::<Vec<_>>(),
        "the staged hashes"
    );
    b.s.sync_validation().expect("quiesce");
    assert_eq!(b.tip(), hash_of(f.prefix.last().expect("a prefix")), "the prefix tip");
    let mut conditions = b.conditions(None);
    meter.open(false)?;
    let replayed = b.s.replay_direct();
    let completion = Completion {
        tip_is_last: replayed.is_ok() && b.tip() == hash_of(f.workload.last().expect("a workload")),
        signals_barrier: replayed.is_ok(),
        ..Completion::default()
    };
    let counts = meter.close(&completion, false)?;
    let tip = b.tip();
    conditions.bench = conditions.bench.map(|bench| bench && !b.recorded);
    Ok(PassOut {
        record: Record::new(
            Leg::Core,
            Pass::B,
            Identity::of(f, revision, Some(core_build(oracle))),
            conditions,
            completion,
            counts,
        ),
        tip,
        cmpcts: BTreeMap::new(),
    })
}

/// The semantic smoke (no counters, every test run): both passes reach the fixture's last
/// block, B's staged hashes are the fixture's, neither is in IBD at its window's start, H is
/// high-bandwidth and gets each block's `cmpctblock` once, every completion condition held, and
/// no block-index check or debug category is on.
#[test]
fn amb_core_smoke() {
    let (oracle, f) = (oracle(), Fixture::smoke().expect("the fixture"));
    let a = pass_a(&oracle, &f, &mut Plain::default(), Script::default(), "smoke".into()).unwrap();
    let b = pass_b(&oracle, &f, &mut Plain::default(), "smoke".into()).unwrap();
    let last = hash_of(f.workload.last().unwrap());
    assert_eq!((a.tip, b.tip), (last, last));
    for (out, pass_a) in [(&a, true), (&b, false)] {
        let r = &out.record;
        assert!(!r.conditions.ibd_at_start, "{:?}", r.pass);
        assert!(r.completion.check(pass_a).is_ok(), "{:?}", r.completion);
        assert_eq!(r.conditions.check_block_index, [("core".to_owned(), false)]);
        assert_eq!((r.conditions.log_categories, r.conditions.bench), (0, Some(true)));
    }
    assert_eq!(a.record.conditions.hb_at_start, Some(true));
    let once: BTreeMap<Hash, usize> = f.workload.iter().map(|b| (hash_of(b), 1)).collect();
    assert_eq!(a.cmpcts, once, "H's cmpctblocks");
}

/// R2-1's Core half, F5 (3)'s "nothing pending": S announces a fixture block past the pass's
/// rows and never serves it, so it is in flight and unserved at the window's end although the
/// further pass ran, and the pass is refused.
#[test]
fn amb_core_completion_refuses_pending_work() {
    let full = Fixture::load().expect("the fixture");
    let f = full.truncated(SMOKE_ROWS);
    let script = Script {
        withhold: Some(full.workload[SMOKE_ROWS].clone()),
        ..Script::default()
    };
    let e = pass_a(&oracle(), &f, &mut Plain::default(), script, "pending".into())
        .err()
        .expect("pending work is refused");
    assert!(e.0.contains("actor_barrier: Some(true)"), "{e}");
    assert!(e.0.contains("quiesced: Some(false)"), "{e}");
}

/// R2-N3: each of Opus M2's "nothing pending" sub-checks alone marks a peer pending.
#[test]
fn each_pending_sub_check_marks_a_peer_pending() {
    assert!(!Pending::default().any());
    type Case = (&'static str, fn(&mut Pending));
    let cases: [Case; 5] = [
        ("unread bytes", |p| p.unread_bytes = 1),
        ("receive paused", |p| p.pause_recv = true),
        ("queued send", |p| p.queued_messages = 1),
        ("inv relay", |p| p.inv_relay = 1),
        ("headers relay", |p| p.headers_relay = 1),
    ];
    for (name, set) in cases {
        let mut p = Pending::default();
        set(&mut p);
        assert!(p.any(), "{name}");
    }
}

/// SOL-2 on the Core leg: an intermediate workload block's `cmpctblock` never reaches H while
/// the last one does: every in-window condition holds, and the full map after the window
/// refuses the pass.
#[test]
fn amb_core_a_missing_intermediate_relay_is_refused() {
    let f = Fixture::smoke().expect("the fixture");
    let script = Script {
        drop_cmpct: Some(hash_of(&f.workload[f.workload.len() / 2])),
        ..Script::default()
    };
    let e = pass_a(&oracle(), &f, &mut Plain::default(), script, "relay".into())
        .err()
        .expect("a missing relay is refused");
    assert!(e.0.contains("H's cmpctblocks after the window") && e.0.contains("1 of"), "{e}");
}

/// Opus M3: Core's options start LevelDB's background thread as the session's first database
/// opens, before Core's scheduler thread and long before any window. LevelDB's thread takes its
/// creator's name, so it is the one thread other than the caller with the caller's name, and
/// its id is below the scheduler's. (Without [`ARGS`] a later compaction during the session's
/// start creates it, after the scheduler.)
#[test]
fn amb_core_session_starts_leveldb_s_thread_at_its_first_open() {
    let (_, bytes) = supervisor::run(THREADS_CHILD, "threads", &[]).expect("the threads child");
    let names = String::from_utf8(bytes).unwrap();
    let caller = names.split(' ').next().unwrap().to_owned();
    let names: Vec<&str> = names.split(' ').skip(1).collect();
    let me = names.iter().position(|n| *n == caller).expect("the caller");
    let twins: Vec<usize> =
        (0..names.len()).filter(|&i| i != me && names[i] == names[me]).collect();
    let scheduler = names.iter().position(|n| *n == "d-cl-schdlr").expect("the scheduler");
    let [leveldb] = twins[..] else {
        panic!("not one LevelDB thread, by id: {names:?}");
    };
    assert!(leveldb < scheduler, "threads by id: {names:?}");
}

/// [`amb_core_session_starts_leveldb_s_thread_at_its_first_open`]'s child (`AMB_PASS=threads`):
/// opens a bench session with [`ARGS`] and writes the caller's name, then every thread's name,
/// in thread-id order.
#[test]
fn amb_core_threads_child() {
    if supervisor::pass().as_deref() != Some("threads") {
        return;
    }
    let s = oracle().peer_session_new_mode(&config(Vec::new()), MODE);
    let s = s.expect("a session").expect("genesis");
    let comm = |t: i32| {
        let name = std::fs::read_to_string(format!("/proc/self/task/{t}/comm"));
        name.expect("a thread's name").trim().replace(' ', "_")
    };
    let mut out = vec![node_amb::sys::gettid()];
    out.extend(node_amb::sys::census().expect("the census"));
    let names: Vec<String> = out.into_iter().map(comm).collect();
    drop(s);
    supervisor::finish(names.join(" ").as_bytes())
}

/// An observed session (the bench mutant's mode) records its draws, so its conditions say it is
/// no bench session.
#[test]
fn amb_core_observed_session_is_no_bench() {
    let oracle = oracle();
    let s = oracle.peer_session_new_mode(&config(Vec::new()), SessionMode::Observed);
    let mut b = Bench {
        s: s.expect("a session").expect("genesis"),
        recorded: false,
    };
    let mut p = add_peer(&mut b.s, 1, 0);
    for _ in 0..4 {
        b.s.send_pass(T0).expect("a pass");
    }
    p.drain(&mut b.s);
    b.outcome();
    assert!(b.recorded, "an observed session records draws or calls");
    assert_eq!(b.conditions(None).bench, Some(false));
}

/// The measured pass of a child: `AMB_PASS=core:<a|b>:<rows>:<count 0|1>`. A no-op otherwise.
#[test]
fn amb_core_child() {
    leg::child_main("core", false, |mut p| {
        let oracle = oracle();
        let out = if p.pass_a {
            pass_a(&oracle, &p.fixture, &mut p.meter, Script::default(), p.revision)
        } else {
            pass_b(&oracle, &p.fixture, &mut p.meter, p.revision)
        };
        out.map(|o| o.record)
    });
}

/// The child-per-pass lifecycle (rev5), end to end on a few rows: Core A then Core B, each in a
/// fresh child; both records arrive with matching identities from distinct processes, and the
/// leg's report is produced (with a PMU; without one, the comparison refuses the missing count).
#[test]
fn amb_core_child_per_pass_lifecycle() {
    let count = node_amb::sys::counters::available();
    if !count {
        supervisor::no_pmu("amb_core_child_per_pass_lifecycle (counting off)");
    }
    let (a, b) = leg::run(CHILD, "core", 4, count).expect("both passes");
    assert_ne!(a.pid, b.pid);
    assert_ne!(a.pid, std::process::id());
    assert_eq!(a.identity, b.identity);
    assert_eq!(a.identity.core.as_ref().map(|c| c.abi), Some(oracle().abi_version()));
    let report = compare::leg(&a, &b);
    if count {
        let r = report.expect("a report");
        assert!(r.a_instructions > 0 && r.b_instructions > 0, "{r:?}");
    } else {
        assert!(report.unwrap_err().0.contains("no count"));
    }
}

/// The measurement: every fixture row is one block (or `AMB_ROWS` rows, with no gate), each
/// pass in its own child. Prints and saves the leg's report under `AMB_OUT`; with
/// `AMB_NODE_RECORDS` (the directory holding the node leg's `node-a.json` and `node-b.json`), the
/// gate's verdict on the node leg recomputed from them, and fails unless it passes. A debug build
/// prints [`compare::debug_verdict`] instead and runs no gate.
#[test]
#[ignore = "a measurement: run alone, release, with a PMU"]
fn amb_core_leg() {
    let rows = leg::measured_rows();
    let (a, b) = leg::run(CHILD, "core", rows, true).expect("both passes");
    let report = compare::leg(&a, &b).expect("the Core leg");
    println!("{}", leg::save("core", &a, &b, &report).expect("the report"));
    if !leg::is_full(rows) {
        println!("gate: not run, {rows} rows is not the full fixture");
        return;
    }
    let node = std::env::var_os("AMB_NODE_RECORDS").map(|dir| {
        let read = |name: &str| {
            let bytes = std::fs::read(std::path::Path::new(&dir).join(name)).expect(name);
            supervisor::save(name, &bytes);
            Record::decode(&bytes).expect(name)
        };
        compare::leg(&read("node-a.json"), &read("node-b.json")).expect("the node leg")
    });
    if a.identity.profile != "release" {
        let debug = compare::debug_verdict(node.as_ref(), Some(&report)).expect("comparable legs");
        let json = serde_json::to_string(&debug).expect("json");
        supervisor::save("debug-verdict.json", json.as_bytes());
        println!("debug verdict (not the gate): {json}");
        println!("gate: not run, a {} build", a.identity.profile);
        return;
    }
    let verdict = compare::gate(node.as_ref(), Some(&report)).expect("comparable legs");
    let json = serde_json::to_string(&verdict).expect("json");
    supervisor::save("gate.json", json.as_bytes());
    println!("{json}");
    assert!(!matches!(verdict, Verdict::Fail { .. }), "{json}");
}
