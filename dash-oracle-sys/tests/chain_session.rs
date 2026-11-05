//! Chain sessions (ABI 22, node-program O2b-2): the binding's report decoding, and Core-only
//! determinism from Rust: one program run in two sessions in sequence gives byte-identical
//! reports, and so does its suffix after a save/restore round trip.
//!
//! Blocks are built before a session starts (the binding refuses the codecs on a session's
//! thread): regtest blocks with an OP_TRUE coinbase, ground with the oracle's X11 header hash.

use dash_oracle_sys::{
    BlockAt, Chain, ChainHash, ChainOp, ChainReport, ChainSession, ChainStart, CheckResult,
    CoreImage, Error, ErrorKind, FlushMode, Oracle, Pin, RawReport, RestartKind, SaveOutcome,
    SporkState,
};
use dash_statedigest::{Header, Record};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Command;

const GENESIS_TIME: i64 = 1417713337;
const REGTEST_BITS: u32 = 0x207fffff;
const COIN: i64 = 100_000_000;
const SPORK_19: i32 = 10018;
const SPORK_OFF: i64 = 4070908800;

/// Schedule S0's options (O2b §2.5) with a large cache.
const ARGS: &[&str] = &[
    "-dip3params=2000:2000",
    "-testactivationheight=v20@2000",
    "-testactivationheight=mn_rr@2000",
    "-vbparams=testdummy:0:999999999999:0:12:9",
    "-par=1",
    "-fastprune",
    "-sporkaddr=yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW",
    "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK",
    "-dbcache=300",
];

/// The prefix's height (past coinbase maturity, 100); programs run an hour after its last block.
const PREFIX: i32 = 110;
const T0: i64 = GENESIS_TIME + PREFIX as i64 * 150 + 3600;

fn oracle() -> Oracle {
    Oracle::new(Pin::V24_0)
}

fn genesis() -> [u8; 32] {
    rpc_order("000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e")
}

fn rpc_order(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[31 - i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

fn compact(out: &mut Vec<u8>, n: usize) {
    assert!(n < 253);
    out.push(n as u8);
}

/// A version-1 transaction spending `ins` (outpoint, scriptSig) to `outs` (value, script).
fn tx(ins: &[([u8; 32], u32, Vec<u8>)], outs: &[(i64, Vec<u8>)]) -> Vec<u8> {
    let mut t = 1i32.to_le_bytes().to_vec();
    compact(&mut t, ins.len());
    for (txid, n, script_sig) in ins {
        t.extend(txid);
        t.extend(n.to_le_bytes());
        compact(&mut t, script_sig.len());
        t.extend(script_sig);
        t.extend(u32::MAX.to_le_bytes());
    }
    compact(&mut t, outs.len());
    for (value, script) in outs {
        t.extend(value.to_le_bytes());
        compact(&mut t, script.len());
        t.extend(script);
    }
    t.extend(0u32.to_le_bytes());
    t
}

/// BIP34's height push: `CScript() << height`.
fn height_push(height: i32) -> Vec<u8> {
    if (1..=16).contains(&height) {
        return vec![0x50 + height as u8];
    }
    let mut num = Vec::new();
    let mut v = height;
    while v > 0 {
        num.push(v as u8);
        v >>= 8;
    }
    if num.last().is_some_and(|b| b & 0x80 != 0) {
        num.push(0);
    }
    let mut out = vec![num.len() as u8];
    out.extend(num);
    out
}

#[derive(Clone, Debug)]
struct Block {
    hash: [u8; 32],
    height: i32,
    header: [u8; 80],
    bytes: Vec<u8>,
    coinbase_txid: [u8; 32],
}

impl Block {
    fn at(&self) -> BlockAt {
        BlockAt(self.height, ChainHash(self.hash))
    }

    /// `submitblock` of the block.
    fn submit(&self) -> ChainOp {
        ChainOp::Block(self.bytes.clone())
    }
}

/// The heights of the blocks built for one test.
#[derive(Default)]
struct Blocks {
    height: HashMap<[u8; 32], i32>,
}

impl Blocks {
    /// A block at `prev`'s height + 1 paying one OP_TRUE output (`tag` makes it unique), then
    /// `txs`, ground to regtest's bits.
    fn make(&mut self, oracle: &Oracle, prev: [u8; 32], tag: u32, txs: &[Vec<u8>]) -> Block {
        let height = if prev == genesis() {
            1
        } else {
            self.height[&prev] + 1
        };
        let mut script_sig = height_push(height);
        script_sig.push(4);
        script_sig.extend(tag.to_le_bytes());
        let coinbase = tx(&[([0; 32], u32::MAX, script_sig)], &[(COIN, vec![0x51])]);
        let all: Vec<&Vec<u8>> = std::iter::once(&coinbase).chain(txs).collect();
        let ids: Vec<[u8; 32]> = all.iter().map(|t| oracle.tx_decode(t).unwrap().txid).collect();
        let (root, mutated) = oracle.merkle_root(&ids).unwrap();
        assert!(!mutated);
        let time = (GENESIS_TIME + i64::from(height) * 150 + i64::from(tag % 97)) as u32;
        let mut header = [0u8; 80];
        header[..4].copy_from_slice(&0x2000_0000i32.to_le_bytes());
        header[4..36].copy_from_slice(&prev);
        header[36..68].copy_from_slice(&root);
        header[68..72].copy_from_slice(&time.to_le_bytes());
        header[72..76].copy_from_slice(&REGTEST_BITS.to_le_bytes());
        let hash = (0u32..)
            .find_map(|nonce| {
                header[76..].copy_from_slice(&nonce.to_le_bytes());
                let hash = oracle.header_decode(&header).unwrap().hash;
                // 0x207fffff: below 0x7fffff followed by 29 zero bytes (big-endian).
                let big_endian: Vec<u8> = hash.iter().rev().copied().collect();
                (big_endian.as_slice() <= [0x7f, 0xff, 0xff].as_slice()).then_some(hash)
            })
            .unwrap();
        let mut bytes = header.to_vec();
        compact(&mut bytes, all.len());
        all.iter().for_each(|t| bytes.extend(*t));
        let block = Block {
            hash,
            height,
            header,
            bytes,
            coinbase_txid: ids[0],
        };
        self.height.insert(hash, height);
        block
    }

    /// `n` blocks on `from`, tagged from `tag`.
    fn chain(&mut self, oracle: &Oracle, from: [u8; 32], n: usize, tag: u32) -> Vec<Block> {
        let mut prev = from;
        (0..n as u32)
            .map(|i| {
                let b = self.make(oracle, prev, tag + i, &[]);
                prev = b.hash;
                b
            })
            .collect()
    }
}

fn session(image: Option<&CoreImage>) -> ChainSession {
    match oracle().chain_session(ARGS, T0, image).unwrap() {
        ChainStart::Started(s) => s,
        ChainStart::Refused(message) => panic!("the load was refused: {message}"),
    }
}

fn tip(r: &ChainReport) -> BlockAt {
    r.post.tip.expect("a live session's tip")
}

fn ok_result(r: &ChainReport) -> &Value {
    r.reply
        .outcome
        .as_ref()
        .unwrap_or_else(|e| panic!("{e:?}: {}", String::from_utf8_lossy(&r.raw.reply)))
}

/// The prefix chain (heights 1 ..= PREFIX) and a program over it that reaches every op: a fork
/// announced headers first that overtakes a main extension, invalidate / reconsider / precious, a
/// spend of a mature coinbase, a ChainLock recorded and enforced, sporks, the flush modes (one
/// labelled), a proposal, both restarts and an activation.
struct World {
    prefix: Vec<Block>,
    /// Three blocks on the prefix's top.
    main: Vec<Block>,
    /// Five blocks on the prefix's second-to-top block: one more than `main` at its top.
    fork: Vec<Block>,
    /// On `main`'s top (equal work with `fork`'s), spending the first coinbase.
    spending: Block,
    /// On `spending`, after its ChainLock: the active chain's work is the most (no tie at a load).
    after: Block,
    program: Vec<ChainOp>,
}

fn world() -> World {
    let oracle = oracle();
    let mut blocks = Blocks::default();
    let prefix = blocks.chain(&oracle, genesis(), PREFIX as usize, 0);
    let main = blocks.chain(&oracle, prefix[PREFIX as usize - 1].hash, 3, 0x100);
    let fork = blocks.chain(&oracle, prefix[PREFIX as usize - 2].hash, 5, 0x200);
    let spend = tx(&[(prefix[0].coinbase_txid, 0, Vec::new())], &[(COIN - 1000, vec![0x51])]);
    let spending = blocks.make(&oracle, main[2].hash, 0x300, &[spend]);
    let after = blocks.make(&oracle, spending.hash, 0x400, &[]);
    let proposal = blocks.make(&oracle, after.hash, 0x500, &[]);
    let mut program: Vec<ChainOp> = main.iter().map(Block::submit).collect();
    program.extend(fork.iter().map(|b| ChainOp::Header(b.header)));
    program.extend(fork.iter().map(Block::submit));
    program.extend([
        ChainOp::Invalidate(fork[1].hash),
        ChainOp::Reconsider {
            hash: fork[1].hash,
            ignore_chainlocks: false,
        },
        ChainOp::Precious(main[2].hash),
        ChainOp::Spork {
            id: SPORK_19,
            value: 0,
        },
        spending.submit(),
        ChainOp::ClRecord {
            height: spending.height,
            hash: spending.hash,
            sig: [0; 96],
        },
        ChainOp::ClEnforce,
        after.submit(),
        ChainOp::SetTime(T0 + 60),
        ChainOp::Flush {
            mode: FlushMode::IfNeeded,
            label: String::new(),
        },
        ChainOp::Flush {
            mode: FlushMode::IndexOnly,
            label: "mirror".into(),
        },
        ChainOp::Proposal(proposal.bytes.clone()),
        ChainOp::Restart(RestartKind::Clean),
        ChainOp::Flush {
            mode: FlushMode::Always,
            label: String::new(),
        },
        ChainOp::Restart(RestartKind::Kill),
        ChainOp::Spork {
            id: SPORK_19,
            value: SPORK_OFF,
        },
        ChainOp::Activate,
    ]);
    World {
        prefix,
        main,
        fork,
        spending,
        after,
        program,
    }
}

/// Connect the prefix to a session at genesis: every report.
fn connect_prefix(s: &mut ChainSession, w: &World) -> Vec<RawReport> {
    w.prefix
        .iter()
        .map(|b| {
            let r = s.step(&b.submit()).unwrap();
            assert_eq!(ok_result(&r), &Value::Null, "prefix block {}", b.height);
            assert_eq!(tip(&r), b.at());
            r.raw
        })
        .collect()
}

/// A fresh session from genesis with the prefix connected, then the program: every report.
fn run_from_genesis(w: &World) -> Vec<RawReport> {
    let mut s = session(None);
    let mut out = connect_prefix(&mut s, w);
    out.extend(w.program.iter().map(|op| s.step(op).unwrap().raw));
    out
}

/// The reports up to (not including) the first tainted one (O2b D30).
fn untainted(reports: &[RawReport]) -> Vec<RawReport> {
    reports.iter().take_while(|r| !ChainReport::decode((*r).clone()).tainted()).cloned().collect()
}

#[test]
fn one_program_in_two_sessions_gives_identical_reports() {
    let w = world();
    let first = run_from_genesis(&w);
    let second = run_from_genesis(&w);
    assert_eq!(first.len(), second.len());
    let n = untainted(&first).len();
    assert_eq!(n, untainted(&second).len(), "the taint is at the same step");
    // The program is built so that no tie-break decides it.
    if let Some(r) = first.get(n) {
        panic!("step {n} was tainted: {}", String::from_utf8_lossy(&r.post));
    }
    for (i, (a, b)) in first.iter().zip(&second).enumerate() {
        assert_eq!(a, b, "step {i} differs");
    }
}

#[test]
fn reports_decode_into_the_ops_effects() {
    let w = world();
    let reports: Vec<ChainReport> =
        run_from_genesis(&w).into_iter().map(ChainReport::decode).collect();
    let program = &reports[PREFIX as usize..];
    let by_op = |pick: fn(&ChainOp) -> bool| -> Vec<&ChainReport> {
        w.program.iter().zip(program).filter(|(op, _)| pick(op)).map(|(_, r)| r).collect()
    };
    // submitblock: null on acceptance, the BlockChecked signal, records in O8's grammar, the tip.
    let first = &program[0];
    assert_eq!(ok_result(first), &Value::Null);
    assert_eq!(first.reply.checked.len(), 1);
    assert_eq!(
        (first.reply.checked[0].0, first.reply.checked[0].1),
        (ChainHash(w.main[0].hash), CheckResult::Valid)
    );
    assert!(!first.records.is_empty());
    assert_eq!(tip(first), w.main[0].at());
    assert!(first.events.iter().any(|e| e.name == "UpdatedBlockTip"), "{:?}", first.events);
    // submitheader: null; the best header is the fork's once it has more work than the tip.
    let headers = by_op(|op| matches!(op, ChainOp::Header(_)));
    assert!(headers.iter().all(|r| ok_result(r).is_null()));
    assert_eq!(headers[4].post.best_header, Some(Some(w.fork[4].at())));
    let blocks = by_op(|op| matches!(op, ChainOp::Block(_)));
    assert_eq!(tip(blocks[7]), w.fork[4].at(), "the fork overtakes");
    let [invalidate] = by_op(|op| matches!(op, ChainOp::Invalidate(_)))[..] else {
        panic!()
    };
    assert_eq!(tip(invalidate), w.main[2].at());
    let [reconsider] = by_op(|op| matches!(op, ChainOp::Reconsider { .. }))[..] else {
        panic!()
    };
    assert_eq!(tip(reconsider), w.fork[4].at());
    // The spend matures (height PREFIX + 4 > 100) but only ties the fork's work: kept aside.
    assert_eq!(ok_result(blocks[8]), &json!("inconclusive"));
    assert_eq!(tip(blocks[8]), w.fork[4].at());
    // A spork: signed, and its state in POST.
    let sporks = by_op(|op| matches!(op, ChainOp::Spork { .. }));
    assert_eq!(ok_result(sporks[0]), &json!(true));
    assert!(sporks[0].post.sporks.as_ref().unwrap().contains(&SporkState(SPORK_19, 0, true)));
    // The ChainLock on the spend is the best one, and enforcing it moves the tip there.
    let [enforce] = by_op(|op| matches!(op, ChainOp::ClEnforce))[..] else {
        panic!()
    };
    assert_eq!(enforce.post.best_cl, Some(Some(w.spending.at())));
    assert_eq!(tip(enforce), w.spending.at());
    // The labelled index-only flush is booked under its label.
    let flushes = by_op(|op| matches!(op, ChainOp::Flush { .. }));
    assert_eq!(
        flushes[1].post.flush_delta.index_only.get("mirror"),
        Some(&1),
        "{:?}",
        flushes[1].post.flush_delta
    );
    // A proposal on the tip.
    let [proposal] = by_op(|op| matches!(op, ChainOp::Proposal(_)))[..] else {
        panic!()
    };
    assert_eq!(ok_result(proposal), &Value::Null, "{:?}", proposal.reply);
    // Restarts report the storage they load and come back at the tip.
    for r in by_op(|op| matches!(op, ChainOp::Restart(_))) {
        let persisted = r.post.persisted.as_ref().expect("a restart's persisted image");
        assert!(persisted.index_entries > PREFIX as u64);
        assert_eq!(ok_result(r)["status"], json!("success"));
        assert_eq!(tip(r), w.after.at());
    }
    // Every live POST carries the full state.
    for r in &reports {
        assert!(!r.post.down);
        assert!(r.post.utxo.unwrap().txouts > 0);
        assert!(r.post.tips.as_ref().is_some_and(|t| !t.is_empty()));
        assert!(r.post.deployments.as_ref().is_some_and(Value::is_object));
        assert!(r.post.ibd.is_some() && r.post.index.is_some() && r.post.flushes.is_some());
    }
}

#[test]
fn a_restored_image_replays_the_suffix_identically() {
    let w = world();
    // Saved once the ChainLocked chain has the most work (no equal-work rival for a reloaded
    // index, whose blocks all share sequence id 0, to tie on); the suffix sets the clock first.
    let program = &w.program;
    let split = program.iter().position(|op| *op == w.after.submit()).unwrap() + 1;
    assert!(matches!(program[split], ChainOp::SetTime(_)));
    let mut s = session(None);
    connect_prefix(&mut s, &w);
    for op in &program[..split] {
        s.step(op).unwrap();
    }
    let SaveOutcome::Saved(image) = s.save().unwrap() else {
        panic!("the save reloads")
    };
    // Both restores run at one clock, so that their reports can be equal byte for byte.
    let restore = |s: &mut ChainSession| -> ChainReport {
        s.step(&ChainOp::SetTime(T0)).unwrap();
        let restored = s.restore(&image).unwrap();
        assert!(restored.post.persisted.is_some());
        assert_eq!(ok_result(&restored)["status"], json!("success"));
        restored
    };
    let suffix = |s: &mut ChainSession| -> Vec<RawReport> {
        program[split..].iter().map(|op| s.step(op).unwrap().raw).collect()
    };
    let spork_19 = |r: &ChainReport| {
        *r.post.sporks.as_ref().unwrap().iter().find(|s| s.0 == SPORK_19).unwrap()
    };
    let saved = restore(&mut s);
    assert_eq!(spork_19(&saved), SporkState(SPORK_19, 0, true), "the image holds spork 19 on");
    let first = suffix(&mut s);
    if let Some(r) = first.get(untainted(&first).len()) {
        panic!("a suffix step was tainted: {}", String::from_utf8_lossy(&r.post));
    }
    // Storage durably differs from the image (Sol r1 #1): the suffix ends with spork 19 off,
    // and a CLEAN restart stores that.
    let stored = s.step(&ChainOp::Restart(RestartKind::Clean)).unwrap();
    assert_eq!(spork_19(&stored), SporkState(SPORK_19, SPORK_OFF, false));
    // The restore loads the image, not the storage: the same report as the first restore, so
    // spork 19 is on again.
    let restored = restore(&mut s);
    assert_eq!(spork_19(&restored), SporkState(SPORK_19, 0, true), "the restore ignored the image");
    assert_eq!(restored.raw, saved.raw, "two restores of one image differ");
    let second = suffix(&mut s);
    for (i, (a, b)) in first.iter().zip(&second).enumerate() {
        assert_eq!(a, b, "suffix step {i} after the restore differs");
    }
    drop(s);
    // A new session on the image starts at the image's tip.
    let mut fresh = session(Some(&image));
    let r = fresh.step(&ChainOp::Activate).unwrap();
    assert_eq!(tip(&r), w.after.at());
}

/// Core reports a tie once, on the op that fired it (or the first after a load that had no
/// report): the session's latch and the image's stamp keep it (O2b-2 Opus-h r1 F1).
#[test]
fn a_tie_taints_the_session_until_a_clean_restore() {
    let w = world();
    let mut s = session(None);
    connect_prefix(&mut s, &w);
    let SaveOutcome::Saved(clean) = s.save().unwrap() else {
        panic!("the save reloads")
    };
    assert!(!clean.tainted());
    // Through the spend: it ties the fork's top in work, and arrived later, so in the session
    // the fork stays the tip by sequence id.
    let tied = w.program.iter().position(|op| *op == w.spending.submit()).unwrap() + 1;
    for op in &w.program[..tied] {
        assert!(!s.step(op).unwrap().tainted());
    }
    assert_eq!(s.tainted_since(), None);
    // A reloaded index gives every block sequence id 0: the address order picks the tip.
    let restart = s.step(&ChainOp::Restart(RestartKind::Clean)).unwrap();
    assert!(restart.post.tie.contains(&2), "{:?}", restart.post.tie);
    let since = PREFIX as u64 + tied as u64;
    assert_eq!(s.tainted_since(), Some(since), "the restart's report");
    assert!(!s.step(&ChainOp::Activate).unwrap().tainted(), "Core reports the tie once");
    assert_eq!(s.tainted_since(), Some(since), "the latch holds");
    let SaveOutcome::Saved(tied_image) = s.save().unwrap() else {
        panic!("the save reloads")
    };
    assert!(tied_image.tainted());
    assert!(s.step(&ChainOp::Activate).unwrap().tainted(), "the save's reload carried its ties");
    // A refused frame keeps the carried ties, and the latch.
    let SaveOutcome::Saved(_) = s.save().unwrap() else {
        panic!("the save reloads")
    };
    assert_eq!(s.step_frame(&[14]).unwrap_err().kind, ErrorKind::Argument);
    assert!(s.step(&ChainOp::Activate).unwrap().tainted(), "carried past a refused frame");
    // A restore reports its own load's ties; only a clean image's clean restore clears the latch.
    let restored = s.restore(&tied_image).unwrap();
    assert!(restored.post.tie.contains(&2), "{:?}", restored.post.tie);
    assert!(s.tainted_since().is_some());
    // Once the spend's chain has the most work, a load ties no more, but the state is still
    // what the tie decided: the image is stamped, and its clean restore keeps the latch. (The
    // block's own step may tie or not, as the address order of the restore's tip picked.)
    s.step(&w.after.submit()).unwrap();
    let SaveOutcome::Saved(later) = s.save().unwrap() else {
        panic!("the save reloads")
    };
    assert!(later.tainted());
    let restored = s.restore(&later).unwrap();
    assert!(!restored.tainted(), "{:?}", restored.post.tie);
    assert!(s.tainted_since().is_some(), "a tainted image's restore");
    assert!(!s.restore(&clean).unwrap().tainted());
    assert_eq!(s.tainted_since(), None);
    drop(s);
    // A session from the tied image: latched from its start; its first report carries the
    // load's ties, the next says nothing.
    let mut fresh = session(Some(&tied_image));
    assert_eq!(fresh.tainted_since(), Some(0));
    assert!(fresh.step(&ChainOp::Activate).unwrap().tainted());
    assert!(!fresh.step(&ChainOp::Activate).unwrap().tainted());
    assert_eq!(fresh.tainted_since(), Some(0));
    drop(fresh);
    // A session from the clean image is not.
    let mut fresh = session(Some(&clean));
    assert!(!fresh.step(&ChainOp::Activate).unwrap().tainted());
    assert_eq!(fresh.tainted_since(), None);
}

#[test]
fn a_refused_load_leaves_the_session_down_until_a_good_restart() {
    let w = world();
    let mut s = session(None);
    connect_prefix(&mut s, &w);
    let SaveOutcome::Saved(image) = s.save().unwrap() else {
        panic!()
    };
    // VerifyLoadedChainstate refuses a tip more than 2 h ahead of the clock.
    s.step(&ChainOp::SetTime(T0 - 4 * 3600)).unwrap();
    let refused = s.step(&ChainOp::Restart(RestartKind::Clean)).unwrap();
    assert!(refused.post.down);
    assert_eq!(ok_result(&refused)["status"], json!("failure"));
    let restart_message = ok_result(&refused)["message"].as_str().expect("the load's message");
    assert_eq!(refused.post.tip, None);
    let down = s.step(&w.prefix[0].submit()).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&down.raw.reply),
        r#"{"checked":[],"error":{"code":null,"message":"session-down"}}"#
    );
    let error = down.reply.outcome.unwrap_err();
    assert_eq!((error.code, error.message.as_str()), (None, "session-down"));
    assert!(matches!(s.save().unwrap(), SaveOutcome::Down));
    s.step(&ChainOp::SetTime(T0)).unwrap();
    let back = s.step(&ChainOp::Restart(RestartKind::Kill)).unwrap();
    assert!(!back.post.down);
    assert_eq!(tip(&back).0, PREFIX);
    // A save whose reload is refused still gives the image, and leaves the session down; a
    // restore of it at a good clock brings it back.
    s.step(&ChainOp::SetTime(T0 - 4 * 3600)).unwrap();
    let SaveOutcome::ReloadRefused(refused_image, message) = s.save().unwrap() else {
        panic!("the reload is refused")
    };
    // The load's own refusal, as RESTART and chain_session's Refused give it (Sol r1 #2):
    // the same tip and clock, the same text.
    assert!(message.contains("future"), "{message}");
    assert_eq!(message, restart_message);
    assert!(s.step(&ChainOp::Activate).unwrap().post.down);
    s.step(&ChainOp::SetTime(T0)).unwrap();
    let restored = s.restore(&refused_image).unwrap();
    assert!(!restored.post.down);
    assert_eq!(tip(&restored).0, PREFIX);
    drop(s);
    // oracle_chain_new: the load's message, and no session is left behind.
    match oracle().chain_session(ARGS, T0 - 4 * 3600, Some(&image)).unwrap() {
        ChainStart::Refused(message) => assert_eq!(message, restart_message),
        ChainStart::Started(_) => panic!("the load was accepted"),
    }
    drop(session(Some(&image)));
}

#[test]
fn frames_the_shim_refuses_run_nothing() {
    let mut s = session(None);
    let kind = |frame: &[u8], s: &mut ChainSession| s.step_frame(frame).unwrap_err().kind;
    assert_eq!(kind(&[14], &mut s), ErrorKind::Argument, "an unknown tag");
    assert_eq!(kind(&[7, 0], &mut s), ErrorKind::Trailing, "bytes after ACTIVATE");
    assert_eq!(kind(&[4, 1, 2], &mut s), ErrorKind::Decode, "a short hash");
    let label = ChainOp::Flush {
        mode: FlushMode::Always,
        label: "\u{e9}".into(),
    };
    assert_eq!(s.step(&label).unwrap_err().kind, ErrorKind::Argument, "a label that is not ASCII");
    assert_eq!(s.step(&ChainOp::SetTime(0)).unwrap_err().kind, ErrorKind::Argument);
    // The session is still usable, and nothing ran: no flush was booked.
    let r = s.step(&ChainOp::Activate).unwrap();
    assert_eq!(tip(&r).0, 0);
    assert!(r.post.flushes.unwrap().full.is_empty());
}

#[test]
fn a_session_excludes_every_other_call_on_its_thread() {
    let oracle = oracle();
    // The network selected before the session, for the session to make Core forget.
    assert!(oracle.chainparams_json(&Chain::regtest()).unwrap().contains("regtest"));
    let s = session(None);
    let live = |e: Error| assert_eq!(e.kind, ErrorKind::SessionLive, "{e}");
    live(oracle.tx_decode(&[0]).unwrap_err());
    live(oracle.chainparams_json(&Chain::regtest()).unwrap_err());
    live(oracle.chain_session(ARGS, T0, None).unwrap_err());
    // Another thread's call waits until the session is dropped.
    let (done, finished) = std::sync::mpsc::channel();
    let other = std::thread::spawn(move || {
        let root = oracle.merkle_root(&[[1; 32]]);
        done.send(()).unwrap();
        root
    });
    assert!(
        finished.recv_timeout(std::time::Duration::from_millis(500)).is_err(),
        "a call ran during the session"
    );
    drop(s);
    finished
        .recv_timeout(std::time::Duration::from_secs(300))
        .expect("the call runs once the session is dropped");
    assert_eq!(other.join().unwrap().unwrap(), ([1; 32], false));
    // The session forgot the network: a chain call selects it again.
    assert!(oracle.chainparams_json(&Chain::regtest()).unwrap().contains("regtest"));
}

#[test]
fn the_shim_refuses_options_a_session_does_not_model() {
    let without_par: Vec<&str> = ARGS.iter().copied().filter(|a| !a.starts_with("-par=")).collect();
    let cases = [
        [&without_par[..], &["-par=0"]].concat(),
        [ARGS, &["-reindex"]].concat(),
        [ARGS, &["-txindex=0", "-disablegovernance", "-prune=1"]].concat(),
        [ARGS, &["-stopatheight=5"]].concat(),
    ];
    for args in cases {
        let e = oracle().chain_session(&args, T0, None).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Argument, "{:?}: {e}", args.last());
    }
    // A message is read whole, however long (Core's own refusal of an unknown option names it).
    let unknown = format!("-{}", "x".repeat(3000));
    let e = oracle().chain_session(&[ARGS, &[unknown.as_str()]].concat(), T0, None).unwrap_err();
    assert!(e.message.ends_with(&unknown[1..]), "{} bytes", e.message.len());
    // Core's own parameter interaction refuses -prune first while -txindex or governance is on
    // (Dash's defaults).
    let e = oracle().chain_session(&[ARGS, &["-prune=550"]].concat(), T0, None).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Exception, "{e}");
    assert!(e.message.contains("-txindex"), "{e}");
    // Nothing is left behind.
    drop(session(None));
}

#[test]
fn the_version_normalizes_out_of_a_report() {
    let mut s = session(None);
    let r = s.step(&ChainOp::Restart(RestartKind::Kill)).unwrap();
    let Some(Record::Header(header)) = r.records.first() else {
        panic!(
            "a load's report opens with the StateDigest header: {}",
            String::from_utf8_lossy(&r.raw.records)
        );
    };
    assert!(!header.ver.is_empty() && header.ver != b"*");
    let normalized = r.raw.normalized();
    let again = ChainReport::decode(normalized.clone());
    let Some(Record::Header(n)) = again.records.first() else {
        panic!()
    };
    assert_eq!(
        *n,
        Header {
            ver: b"*".to_vec(),
            ..header.clone()
        },
        "the version alone"
    );
    assert_eq!(again.records[1..], r.records[1..], "only the header's version changes");
    assert_eq!(
        (&normalized.reply, &normalized.events, &normalized.post),
        (&r.raw.reply, &r.raw.events, &r.raw.post)
    );
    assert_eq!(normalized.normalized(), normalized, "idempotent");
}

/// The child of [`a_failed_teardown_poisons_the_pin`]: runs only with `O2B_POISON_CHILD` set.
#[test]
fn a_failed_teardown_poisons_the_pin_child() {
    let Ok(path) = std::env::var("O2B_POISON_CHILD") else {
        return;
    };
    let oracle = oracle();
    if path == "free" {
        // The fault is read at teardown only: the session starts.
        drop(session(None));
    } else {
        // A refused option's start is torn down too.
        let args = [ARGS, &["-reindex"]].concat();
        let e = oracle.chain_session(&args, T0, None).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Exception, "{e}");
        assert!(e.message.starts_with("teardown failed"), "{e}");
    }
    let poisoned = |e: Error| assert_eq!(e.kind, ErrorKind::Poisoned, "{e}");
    poisoned(oracle.merkle_root(&[[1; 32]]).unwrap_err());
    poisoned(oracle.chain_session(ARGS, T0, None).unwrap_err());
    println!("POISONED");
    // The leaked session's script-check threads may still run: end without Core's static
    // destructors.
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    unsafe extern "C" {
        fn _exit(status: i32) -> !;
    }
    unsafe { _exit(0) }
}

/// A teardown that fails (at drop, or after a start) leaves Core's state unknown: every later
/// call of the pin fails with `Poisoned`.
#[test]
fn a_failed_teardown_poisons_the_pin() {
    for path in ["free", "start"] {
        // The leaked session's data directory goes into a directory removed afterwards.
        let tmp = std::env::temp_dir().join(format!("o2b-poison-{}-{path}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "a_failed_teardown_poisons_the_pin_child", "--nocapture"])
            .env("O2B_POISON_CHILD", path)
            .env("DASH_ORACLE_FAULT_TEARDOWN", "1")
            .env("TMPDIR", &tmp)
            .output()
            .unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
        let stdout = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success(),
            "{path}: {stdout}{}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(stdout.contains("POISONED"), "{path}: {stdout}");
    }
}
