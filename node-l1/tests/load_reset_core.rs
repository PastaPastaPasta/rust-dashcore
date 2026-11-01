//! Core's load-time `ResetBlockFailureFlags(nullptr)` (CH1-node-LR, O2b D32, CH1-node D64) on
//! chain sessions: the restarts that `dashd-chainstate`'s `chain::tests::verify` runs on V, with
//! Core's `getchaintips` after each. VerifyLoadedChainstate runs in the session's load as in
//! init (`-checklevel`, `-checkblocks` read the same way).

use dash_oracle_sys::{
    ChainOp, ChainReport, ChainSession, ChainStart, FlushMode, Oracle, Pin, RestartKind,
};
use dashd_chainstate::chain::verify::FUTURE_TIP;
use node_l1::chain::build::{
    BlockPlan, Built, GENESIS_TIME, REGTEST_BITS, SPACING, VERSION_BITS_TOP, coinbase,
    genesis_hash, subsidy,
};
use node_l1::chain::core::Schedule;

/// An hour after a ten-block chain's top.
const NOW: i64 = GENESIS_TIME as i64 + 10 * SPACING as i64 + 3600;

/// `n` blocks on `prev` (at `height`), tagged from `tag`.
fn chain(prev: [u8; 32], height: u32, n: u32, tag: u64) -> Vec<Built> {
    let mut prev = prev;
    (1..=n)
        .map(|i| {
            let h = height + i;
            let b = BlockPlan {
                prev,
                version: VERSION_BITS_TOP,
                time: GENESIS_TIME + h * SPACING + tag as u32 % 97,
                bits: REGTEST_BITS,
                txs: vec![coinbase(h, subsidy(h), tag + u64::from(i))],
                merkle_override: None,
            }
            .build();
            prev = b.hash;
            b
        })
        .collect()
}

fn session(extra: &[&str]) -> ChainSession {
    let mut args = Schedule::S0.args();
    args.extend(extra);
    match Oracle::new(Pin::V24_0).chain_session(&args, NOW, None).expect("a session") {
        ChainStart::Started(s) => s,
        ChainStart::Refused(m) => panic!("genesis load refused: {m}"),
    }
}

fn step(s: &mut ChainSession, op: &ChainOp) -> ChainReport {
    let r = s.step(op).expect("the step runs");
    assert!(!r.post.down, "{op:?} left the session down");
    r
}

/// `submitblock` of each block; one that does not become the tip is `inconclusive`. The tip
/// after the last.
fn connect(s: &mut ChainSession, blocks: &[Built]) -> i32 {
    let mut top = 0;
    for b in blocks {
        let r = step(s, &ChainOp::Block(b.bytes.clone()));
        let reply = r.reply.outcome.expect("submitblock replies");
        assert!(reply.is_null() || reply == "inconclusive", "submitblock: {reply}");
        top = r.post.tip.expect("tip").0;
    }
    top
}

fn rpc_hash(b: &Built) -> String {
    b.hash.iter().rev().map(|x| format!("{x:02x}")).collect()
}

/// `getchaintips` as `(height, hash, status)`.
fn tips(r: &ChainReport) -> Vec<(i64, String, String)> {
    r.post
        .tips
        .as_ref()
        .expect("a live session's tips")
        .iter()
        .map(|t| {
            let height = t["height"].as_i64().expect("height");
            let hash = t["hash"].as_str().expect("hash").to_owned();
            let status = t["status"].as_str().expect("status").to_owned();
            (height, hash, status)
        })
        .collect()
}

fn tip(b: &Built, height: i64, status: &str) -> (i64, String, String) {
    (height, rpc_hash(b), status.to_owned())
}

/// `invalidateblock` of the tip, then a restart: at the default level Core's load resets it and
/// the ABC reconnects it, whether the restart is clean or a kill after a full flush.
#[test]
fn a_restart_reconsiders_an_invalidated_tip() {
    let main = chain(genesis_hash(), 0, 3, 0x100);
    for kind in [RestartKind::Clean, RestartKind::Kill] {
        let mut s = session(&[]);
        connect(&mut s, &main);
        let r = step(&mut s, &ChainOp::Invalidate(main[2].hash));
        assert_eq!(r.post.tip.expect("tip").0, 2);
        step(
            &mut s,
            &ChainOp::Flush {
                mode: FlushMode::Always,
                label: String::new(),
            },
        );
        let r = step(&mut s, &ChainOp::Restart(kind));
        let failed = &r.post.persisted.as_ref().expect("the persisted image").failed;
        assert_eq!(failed.len(), 1, "{kind:?}: main[2] was stored failed");
        assert_eq!(tips(&r), vec![tip(&main[2], 3, "active")], "{kind:?}");
    }
}

/// An invalidated ancestor of the tip: the reset clears the old tip and its failed ancestors.
#[test]
fn a_restart_reconsiders_an_invalidated_branch() {
    let main = chain(genesis_hash(), 0, 3, 0x200);
    let mut s = session(&[]);
    connect(&mut s, &main);
    assert_eq!(step(&mut s, &ChainOp::Invalidate(main[1].hash)).post.tip.expect("tip").0, 1);
    let r = step(&mut s, &ChainOp::Restart(RestartKind::Clean));
    assert_eq!(tips(&r), vec![tip(&main[2], 3, "active")]);
}

/// The reset reads `-checklevel` as `GetIntArg` gives it, before VerifyDB's narrowing to `int`.
#[test]
fn the_reset_reads_the_level_before_its_narrowing() {
    let main = chain(genesis_hash(), 0, 2, 0x300);
    for (arg, resets) in [
        ("-checklevel=-4294967293", false),
        ("-checklevel=-1", false),
        ("-checklevel=0", false),
        ("-checklevel=2", false),
        ("-checklevel=3", true),
        ("-checklevel=4", true),
        ("-checklevel=5", true),
        ("-checklevel=4294967298", true),
    ] {
        let mut s = session(&[arg]);
        connect(&mut s, &main);
        step(&mut s, &ChainOp::Invalidate(main[1].hash));
        let r = step(&mut s, &ChainOp::Restart(RestartKind::Clean));
        let want = if resets {
            vec![tip(&main[1], 2, "active")]
        } else {
            vec![tip(&main[1], 2, "invalid"), tip(&main[0], 1, "active")]
        };
        assert_eq!(tips(&r), want, "{arg}");
    }
}

/// With an empty coins view (an index-only flush, then a kill) Core's load skips VerifyDB and
/// the reset: the failed block stays failed.
#[test]
fn an_empty_coins_view_skips_the_reset() {
    let main = chain(genesis_hash(), 0, 2, 0x400);
    let mut s = session(&[]);
    connect(&mut s, &main);
    step(&mut s, &ChainOp::Invalidate(main[1].hash));
    step(
        &mut s,
        &ChainOp::Flush {
            mode: FlushMode::IndexOnly,
            label: String::new(),
        },
    );
    let r = step(&mut s, &ChainOp::Restart(RestartKind::Kill));
    let persisted = r.post.persisted.as_ref().expect("the persisted image");
    assert_eq!(persisted.best_block, None, "the coins were never written");
    assert_eq!(persisted.failed.len(), 1, "the index was");
    assert_eq!(tips(&r), vec![tip(&main[1], 2, "invalid"), tip(&main[0], 1, "active")]);
}

/// The best known invalid block on another branch does not descend from the tip: no reset.
#[test]
fn an_invalid_side_branch_stays_invalid() {
    let main = chain(genesis_hash(), 0, 2, 0x500);
    let side = chain(genesis_hash(), 0, 3, 0x600);
    let mut s = session(&[]);
    connect(&mut s, &main);
    assert_eq!(connect(&mut s, &side), 3);
    assert_eq!(step(&mut s, &ChainOp::Invalidate(side[0].hash)).post.tip.expect("tip").0, 2);
    let r = step(&mut s, &ChainOp::Restart(RestartKind::Clean));
    assert_eq!(tips(&r), vec![tip(&side[2], 3, "invalid"), tip(&main[1], 2, "active")]);
}

/// A loaded tip more than two hours ahead of the clock fails the load with the text V gives
/// ([`FUTURE_TIP`]); at two hours it loads.
#[test]
fn a_tip_from_the_future_fails_the_load_with_cores_text() {
    let main = chain(genesis_hash(), 0, 2, 0x700);
    let top = i64::from(u32::from_le_bytes(main[1].header[68..72].try_into().expect("nTime")));
    for (ahead, refused) in [(7_200, false), (7_201, true)] {
        let mut s = session(&[]);
        connect(&mut s, &main);
        step(&mut s, &ChainOp::SetTime(top - ahead));
        let r = s.step(&ChainOp::Restart(RestartKind::Clean)).expect("the step runs");
        assert_eq!(r.post.down, refused, "{ahead} s ahead");
        if refused {
            let reply = r.reply.outcome.expect("the load's reply");
            assert_eq!(reply["status"], "failure");
            assert_eq!(reply["message"], FUTURE_TIP);
        }
    }
}
