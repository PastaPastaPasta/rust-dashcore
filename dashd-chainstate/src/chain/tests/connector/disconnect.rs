//! CH3a's `DisconnectBlock` through V (CH3 §2.12, §2.10.2, §7.1; slice a5): a disconnect restores
//! the coins the block spent and the parent as best block, in Core's reverse orders; an unclean
//! one is the tip's failure and commits nothing, while `VerifyDB`'s scratch view keeps it; each
//! failed undo read and each inconsistent record is `DISCONNECT_FAILED` with Core's lines; H2 and
//! G1 report through the abort path, and `ActivateBestChainStep` reports again (`v:3621`).
//!
//! Each test's blocks spend their own seed, so the lines naming them are the test's own.
//! `v:N` is `validation.cpp` line N at v24.0.0-rc.3.

use std::cell::RefCell;
use std::os::unix::fs::FileExt as _;
use std::path::PathBuf;

use dash_chain::BlockRef;
use dash_consensus::coins::BlockUndo;
use dashd_store::MemoryEngine;
use dashd_store::flatfile::undo_checksum;

use super::*;
use crate::chain::abc::DISCONNECT_FAILED;
use crate::chain::seams::HeldChainLocks;
use crate::chain::{EvoTxn, ScratchCoins, ScratchView, WaitError};
use crate::coins::{PackedCoin, PrefetchBatch};
use crate::connect::UNDO_READ_FAILED;
use crate::evo::RocksEvoStore;
use crate::store::CHAINSTATE_SCHEMA;

/// A disconnect that commits: the connect's bracket (`v:3281-3288`).
const DISCONNECTED: [Call; 6] = CONNECTED;

/// A disconnect that fails after H6: everything but the commit.
const UNCLEAN: [Call; 6] = [
    Call::BeginAttempt,
    Call::BeginTx,
    Call::SetBestBlock,
    Call::FinishAttempt,
    Call::EndAttempt,
    Call::RollbackTx,
];

/// `DisconnectBlock`'s line for a failed `UndoSpecialTxsInBlock` (`v:2322` at v24.0.0).
pub(super) const UNDO_SPECIAL_FAILED: &str = "DisconnectBlock(): UndoSpecialTxsInBlock failed";

/// `DisconnectBlock`'s line for a transaction whose undo has another number of coins than inputs
/// (`v:2361` at v24.0.0).
const TX_SIZES: &str = "DisconnectBlock(): transaction and undo data inconsistent";

/// A tip disconnect's evo steps (`v:3272-3273`, `:2309-2313`, `:2368-2371`, `:3320`).
const TIP_STEPS: [EvoStep; 4] =
    [EvoStep::BlsCtx, EvoStep::UndoSpecial, EvoStep::ListChanged(true), EvoStep::CommitBls];

/// A seeded, non-coinbase coin of `value` at height 1: an undo coin of height 0 is the legacy
/// format (`v:2259-2266`), which only [`a_legacy_undo_entry_takes_its_transactions_metadata`]
/// wants.
fn seed1(n: u8, value: Amount) -> (OutPoint, TxOut, u32, bool) {
    (seed_op(n), out(value, &[OP_TRUE]), 1, false)
}

/// A world whose block 1 spends seed `n` (50 DASH at height 1) into one output of 49 DASH.
pub(super) fn spent(n: u8, setup: Setup) -> (World, Mined, Arc<Tx>) {
    spent_with(n, setup, |cfg| cfg)
}

/// [`spent`] in [`world_with`]'s world.
fn spent_with(
    n: u8,
    mut setup: Setup,
    rig: impl FnOnce(RigConfig) -> RigConfig,
) -> (World, Mined, Arc<Tx>) {
    setup.seeds.push(seed1(n, 50 * COIN));
    let w = world_with(setup, rig);
    let t1 = spend(&[seed_op(n)], 49 * COIN);
    let b1 = block_on(&w.rig.p.clone(), &w.rig.genesis, vec![t1.clone()]);
    w.rig.accept(&b1);
    (w, b1, t1)
}

/// The first output of `m`'s coinbase.
fn cb_op(m: &Mined) -> OutPoint {
    op(&m.block.txs()[0], 0)
}

/// The active coins' best block and their coins at `ops`.
fn snapshot(w: &World, ops: &[OutPoint]) -> (Option<BlockHash>, Vec<Option<Coin>>) {
    let mut coins = w.coins.lock().expect("coins");
    let held = ops
        .iter()
        .map(|o| coins.fetch(&OutPointKey::new(o)).expect("reads").map(|c| c.to_coin()))
        .collect();
    (coins.best_block().map(|b| b.hash), held)
}

/// Changes the active coins at their best block, outside any attempt.
pub(super) fn edit(w: &World, f: impl FnOnce(&mut BlockOverlay<'_, ProbeEngine>)) {
    let mut coins = w.coins.lock().expect("coins");
    let best = coins.best_block().expect("connected");
    let mut ov = BlockOverlay::new(&mut coins);
    f(&mut ov);
    ov.commit(best).expect("commits");
}

/// Admits a coin at `k` to the active coins' prefetch map only.
fn prefetch(w: &World, k: &OutPointKey) {
    let mut coins = w.coins.lock().expect("coins");
    let coin = PackedCoin::from_output(&out(COIN, &[OP_TRUE]), 1, false).expect("packs");
    let batch = PrefetchBatch {
        fetch_gen: coins.released_gen(),
        epoch: coins.db().write_epoch(),
        entries: vec![(*k, coin)],
    };
    assert_eq!(coins.admit_prefetch(batch, 0), 1);
}

/// Whether the active coins hold a coin at `k`, the prefetch map included.
fn has(w: &World, k: &OutPointKey) -> bool {
    w.coins.lock().expect("coins").fetch(k).expect("reads").is_some()
}

/// `invalidateblock` of `m`, which returns: a failed `DisconnectTip` only ends its walk (`v:3966`).
pub(super) fn invalidate(w: &World, m: &Mined) {
    assert_eq!(w.rig.v.invalidate_block(m.hash()).recv(), Ok(Ok(())));
    w.rig.sync();
}

fn tip(w: &World) -> Option<BlockHash> {
    w.rig.v.tip_watch().get()
}

fn evo_steps(w: &World) -> Vec<EvoStep> {
    w.seen.lock().expect("seen").evo_steps.clone()
}

fn evo_best(w: &World) -> Option<BlockHash> {
    w.seen.lock().expect("seen").evo_best
}

/// `DisconnectTip`'s line for a failed `DisconnectBlock` of `m` (`v:3281-3284`), as often as the V
/// thread `v` logged it: a test running at the same time can disconnect the same block, since the
/// same seed builds the same `b1`.
fn tip_failed_lines(v: std::thread::ThreadId, m: &Mined) -> usize {
    levels_on(v, &tip_failed_line(m)).len()
}

/// `DisconnectTip`'s line for a failed `DisconnectBlock` of `m` (`v:3293` at v24.0.0).
fn tip_failed_line(m: &Mined) -> String {
    format!("DisconnectTip(): DisconnectBlock {} failed", m.hash())
}

/// `DisconnectBlock`'s `DISCONNECT_FAILED` from the undo's catch, as `DisconnectTip` meets it:
/// "UndoSpecialTxsInBlock failed" (`v:2322` at v24.0.0), then `DisconnectTip`'s line for `m`
/// (`v:3293`), once each on the V thread `v`.
pub(super) fn assert_tip_failed_after_the_catch(v: std::thread::ThreadId, m: &Mined, case: &str) {
    assert_eq!(levels_on(v, UNDO_SPECIAL_FAILED), [tracing::Level::ERROR], "{case}");
    assert_eq!(tip_failed_lines(v, m), 1, "{case}: DisconnectTip's line");
    let lines = lines_on(v);
    let at = |line: &str| lines.iter().position(|l| l == line).expect("logged");
    let tip = tip_failed_line(m);
    assert!(at(UNDO_SPECIAL_FAILED) < at(&tip), "{case}: the catch's line, then DisconnectTip's");
}

/// BL-778: one of Core's asserts after a failed `DisconnectTip` of `m` whose undo met a kept fault
/// (CH7 D102, D103). Core's `false` leaves its caller's fresh state valid, so its line `line` reads
/// `Valid` (`consensus/validation.h:105-108` at v24.0.0): logged once on the V thread `v`, at
/// INFO, after `DisconnectTip`'s line (`v:3293`) and after the fault's one report `report` (CH3b
/// D214: the line follows the report).
fn assert_valid_line_after_the_tip(
    v: std::thread::ThreadId,
    m: &Mined,
    report: &str,
    line: &str,
    case: &str,
) {
    assert_eq!(levels_on(v, line), [tracing::Level::INFO], "{case}: the assert's line");
    assert_eq!(tip_failed_lines(v, m), 1, "{case}: DisconnectTip's line");
    let lines = lines_on(v);
    let at = |l: &str| lines.iter().position(|x| x == l).expect("logged");
    assert!(at(&tip_failed_line(m)) < at(line), "{case}: DisconnectTip's line, then the assert's");
    assert_logged_before(&lines, &format!("*** {report}"), line);
}

/// The rev file holding `m`'s undo record, and the record's position.
pub(super) fn undo_record(w: &World, m: &Mined) -> (PathBuf, u64) {
    let e = w.rig.v.reader().entry(w.rig.id(m)).expect("indexed");
    assert_eq!(e.file, Some(0), "one rev file");
    (w.stores.dir.path().join("blocks").join("rev00000.dat"), u64::from(e.undo_pos))
}

/// The record's size field (`blockstorage.cpp:530`).
pub(super) fn record_len(path: &Path, at: u64) -> u64 {
    let mut size = [0; 4];
    std::fs::File::open(path).expect("opens").read_exact_at(&mut size, at - 4).expect("reads");
    u64::from(u32::from_le_bytes(size))
}

pub(super) fn write_at(path: &Path, at: u64, bytes: &[u8]) {
    let file = std::fs::OpenOptions::new().write(true).open(path).expect("opens");
    file.write_all_at(bytes, at).expect("writes");
}

/// `v:2325-2362` in Core's orders: the transactions from last to first, and each one's outputs
/// spent before its inputs are restored, so the coin a transaction makes and a later one in the
/// block spends is restored by the later one's undo, then spent by its maker's outputs; an
/// unspendable output is never looked for. Each disconnect commits with the parent as the best
/// block of the coins and of evo, runs H1 for the tip around H9 and H7 with its undo flag, and
/// leaves the coins as they were at the parent.
#[test]
fn a_disconnect_restores_the_coins_in_cores_reverse_orders() {
    let (w, b1, t1) = spent(1, Setup::default());
    let p = w.rig.p.clone();
    let t2 = spend(&[op(&t1, 0)], 48 * COIN);
    let t3 = tx(
        1,
        0,
        vec![input(op(&t2, 0), Vec::new(), u32::MAX)],
        vec![out(47 * COIN, &[OP_TRUE]), out(0, &[0x6a])],
    );
    let b2 = block_on(&p, &b1, vec![t2.clone(), t3.clone()]);
    let ops = [seed_op(1), op(&t1, 0), cb_op(&b1), op(&t2, 0), op(&t3, 0), cb_op(&b2)];
    let at_b1 = snapshot(&w, &ops);
    let coin = |value, height, coinbase| Some(Coin::new(value, &[OP_TRUE], height, coinbase));
    assert_eq!(
        at_b1,
        (
            Some(b1.hash()),
            vec![None, coin(49 * COIN, 1, false), coin(COIN, 1, true), None, None, None]
        )
    );
    w.rig.accept(&b2);
    assert_eq!(evo_best(&w), Some(b2.hash()));

    invalidate(&w, &b2);
    assert_eq!(tip(&w), Some(b1.hash()));
    assert_eq!(snapshot(&w, &ops), at_b1);
    assert_eq!(bracket(&w), DISCONNECTED);
    assert_eq!(evo_steps(&w), TIP_STEPS);
    assert_eq!(evo_best(&w), Some(b1.hash()));
    assert!(w.rig.status(&b2).has(Status::FAILED_VALID));

    invalidate(&w, &b1);
    assert_eq!(tip(&w), Some(w.rig.genesis.hash()));
    let at_genesis =
        (Some(w.rig.genesis.hash()), vec![coin(50 * COIN, 1, false), None, None, None, None, None]);
    assert_eq!(snapshot(&w, &ops), at_genesis);
    assert_eq!(evo_best(&w), Some(w.rig.genesis.hash()));
    w.rig.assert_no_abort();
    assert!(w.sink.messages().is_empty(), "{:?}", w.sink.messages());
}

/// `v:2333-2343` and `ApplyTxInUndo` (`v:2252-2278`): a block output missing or other than the
/// block made it, or a spent coin found unspent, is `DISCONNECT_UNCLEAN`, on which `DisconnectTip`
/// fails without flushing its view (`v:3281-3284`). Nothing commits: the tip, the coins and the
/// evo best block stay at the block, the evo transaction rolls back after H6, H1's commit never
/// runs (`:3320`), and the block is not marked failed (`InvalidateBlock` returns false, `:3966`).
#[test]
fn an_unclean_tip_disconnect_commits_nothing() {
    install_accept_log();
    for (n, case) in [(10, "output missing"), (11, "output differs"), (12, "spent coin unspent")] {
        let (w, b1, t1) = spent(n, Setup::default());
        let made = op(&t1, 0);
        edit(&w, |ov| match case {
            "output missing" => {
                assert!(ov.spend(&OutPointKey::new(&made)).expect("reads").is_some());
            }
            "output differs" => {
                let other = out(48 * COIN, &[OP_TRUE]);
                let src = CoinSrc::Output {
                    out: &other,
                    height: 1,
                    coinbase: false,
                };
                ov.add(&OutPointKey::new(&made), src, true).expect("adds");
            }
            _ => {
                let (op, out, height, coinbase) = seed1(n, 50 * COIN);
                let src = CoinSrc::Output {
                    out: &out,
                    height,
                    coinbase,
                };
                ov.add(&OutPointKey::new(&op), src, false).expect("adds");
            }
        });
        let ops = [seed_op(n), made, cb_op(&b1)];
        let before = snapshot(&w, &ops);
        invalidate(&w, &b1);
        assert_eq!(tip(&w), Some(b1.hash()), "{case}");
        assert_eq!(snapshot(&w, &ops), before, "{case}");
        assert_eq!(bracket(&w), UNCLEAN, "{case}");
        assert_eq!(evo_steps(&w), TIP_STEPS[..3], "{case}");
        assert_eq!(evo_best(&w), Some(b1.hash()), "{case}");
        assert!(!w.rig.status(&b1).has(Status::FAILED_MASK), "{case}");
        assert_eq!(tip_failed_lines(w.rig.v_thread(), &b1), 1, "{case}");
        assert!(w.sink.messages().is_empty(), "{case}: {:?}", w.sink.messages());
    }
}

/// CH2 §2.4.3 item 4: a committed tip disconnect clears the active coins' prefetch map; a failed
/// one commits nothing and leaves it.
#[test]
fn only_a_committed_tip_disconnect_clears_the_prefetch() {
    for (n, clean) in [(15, true), (16, false)] {
        let (w, b1, t1) = spent(n, Setup::default());
        if !clean {
            edit(&w, |ov| {
                assert!(ov.spend(&OutPointKey::new(&op(&t1, 0))).expect("reads").is_some());
            });
        }
        let k = OutPointKey::new(&seed_op(99));
        prefetch(&w, &k);
        invalidate(&w, &b1);
        let tip_at = if clean {
            &w.rig.genesis
        } else {
            &b1
        };
        assert_eq!(tip(&w), Some(tip_at.hash()), "clean {clean}");
        assert_eq!(has(&w, &k), !clean, "clean {clean}");
    }
}

/// `VerifyDB`'s scratch view (CH3 D36) as CH5 builds it, reduced to what the connector calls: a
/// coins layer over an engine of its own, holding the coins a test puts in it, and an evo
/// transaction.
pub(super) struct Scratch {
    coins: CoinsCache<ProbeEngine>,
    evo: NoTxn,
}

struct NoTxn;

impl EvoTxn for NoTxn {}

impl ScratchView for Scratch {
    fn split(&mut self) -> (ScratchCoins<'_>, &mut dyn EvoTxn) {
        (ScratchCoins::new(&mut self.coins), &mut self.evo)
    }
}

impl Scratch {
    /// A view at `best` holding `coins`: (outpoint, output, height, coinbase).
    pub(super) fn at(best: BlockRef, coins: &[(OutPoint, TxOut, u32, bool)]) -> Scratch {
        let db = Arc::new(GuardedEngine::probe(Arc::new(RecordingSink::default())));
        let budget = CoinsBudget::new(8 << 20, 0, Ratio::HALF, 0);
        let mut layer = CoinsCache::new(db, budget, Salt::fixed(9), None);
        let mut ov = BlockOverlay::new(&mut layer);
        for (op, out, height, coinbase) in coins {
            let src = CoinSrc::Output {
                out,
                height: *height,
                coinbase: *coinbase,
            };
            ov.add(&OutPointKey::new(op), src, false).expect("adds");
        }
        ov.commit(best).expect("commits");
        Scratch {
            coins: layer,
            evo: NoTxn,
        }
    }

    fn coin(&mut self, op: &OutPoint) -> Option<Coin> {
        self.coins.fetch(&OutPointKey::new(op)).expect("reads").map(|c| c.to_coin())
    }
}

/// `VerifyDB`'s `DisconnectBlock(block, pindex, coins)` of `m` into `scratch` (`v:5113`), on V.
fn scratch_disconnect(w: &World, scratch: Scratch, m: &Mined) -> (DisconnectResult, Scratch) {
    let (result, scratch) = disconnect_on_v(w, Some(scratch), m);
    (result.expect("disconnects"), scratch.expect("the view"))
}

/// Runs `f` on V with a `ConnectCx` of `origin` over V's state, and V's connector and context. V
/// does not act on what `f` returns.
pub(super) fn on_v_cx<R: Send + 'static>(
    w: &World,
    origin: AttemptOrigin,
    f: impl FnOnce(&ConnectCx<'_>, &mut dyn BlockConnector, &mut VContext) -> R + Send + 'static,
) -> R {
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    w.rig.on_v(move |v| {
        let deployments = v.deployments.view();
        let chain = RefCell::new(&mut v.chain);
        let best_header = chain.borrow().best_header();
        let chainlocks = HeldChainLocks::new(&chain, None);
        let cx = ConnectCx {
            params: &v.params,
            reader: &v.reader,
            deployments: &deployments,
            best_header,
            min_chain_work: v.reader.min_chain_work(),
            // No step these tests run reads it.
            is_ibd: false,
            chainlocks: &chainlocks,
            guard: v.guard,
            origin,
        };
        *put.lock().expect("out") = Some(f(&cx, &mut *v.connector, &mut v.vcx));
        Ok(())
    });
    out.lock().expect("out").take().expect("ran")
}

/// `DisconnectBlock` of `m` on V, called on the connector: into `scratch` as `VerifyDB`'s, or with
/// none into the active coins as `DisconnectTip`'s. V does not act on the result, so a `Fatal`
/// does not stop it.
pub(super) fn disconnect_on_v(
    w: &World,
    mut scratch: Option<Scratch>,
    m: &Mined,
) -> (Result<DisconnectResult, Fatal>, Option<Scratch>) {
    let (id, block) = (w.rig.id(m), m.block.clone());
    let origin = match scratch {
        Some(_) => AttemptOrigin::VerifyDb,
        None => AttemptOrigin::Tip,
    };
    on_v_cx(w, origin, move |cx, connector, vcx| {
        let target = match &mut scratch {
            Some(view) => ViewTarget::Scratch(view),
            None => ViewTarget::Active,
        };
        (connector.disconnect_block(cx, vcx, target, &block, id), scratch)
    })
}

/// `VerifyDB`'s disconnect into its own view (`v:5113-5122`): the view keeps a clean and an
/// unclean disconnect alike, with the parent as its best block. Neither touches the active coins,
/// their prefetch, the tip or the evo store. Both run H1, the block's scheme, as VerifyDB's guard
/// gives the undo it (`v:5055`, `stm:1065-1088`; CH5-1a), and neither commits one.
#[test]
fn a_scratch_disconnect_keeps_even_an_unclean_result_in_its_view() {
    for (n, clean) in [(20, true), (21, false)] {
        let (w, b1, t1) = spent(n, Setup::default());
        let t1_out = (op(&t1, 0), t1.outputs()[0].clone(), 1, false);
        let cb_out = (cb_op(&b1), b1.block.txs()[0].outputs()[0].clone(), 1, true);
        // Unclean: the coinbase's output is missing from the view.
        let held = if clean {
            vec![t1_out, cb_out]
        } else {
            vec![t1_out]
        };
        let best = BlockRef {
            id: w.rig.id(&b1),
            hash: b1.hash(),
            height: 1,
        };
        let ops = [seed_op(n), op(&t1, 0), cb_op(&b1)];
        let before = snapshot(&w, &ops);
        let k = OutPointKey::new(&seed_op(99));
        prefetch(&w, &k);

        let (result, mut scratch) = scratch_disconnect(&w, Scratch::at(best, &held), &b1);
        let want = if clean {
            DisconnectResult::Ok
        } else {
            DisconnectResult::Unclean
        };
        assert_eq!(result, want);
        let at = scratch.coins.best_block().map(|b| (b.id, b.hash, b.height));
        assert_eq!(at, Some((w.rig.id(&w.rig.genesis), w.rig.genesis.hash(), 0)), "clean {clean}");
        let restored = Coin::new(50 * COIN, &[OP_TRUE], 1, false);
        assert_eq!(scratch.coin(&seed_op(n)), Some(restored), "clean {clean}");
        assert_eq!((scratch.coin(&op(&t1, 0)), scratch.coin(&cb_op(&b1))), (None, None));

        assert_eq!(snapshot(&w, &ops), before, "clean {clean}");
        assert!(has(&w, &k), "clean {clean}: the prefetch stays");
        assert_eq!(tip(&w), Some(b1.hash()));
        assert_eq!(evo_best(&w), Some(b1.hash()), "clean {clean}");
        assert_eq!(bracket(&w), DISCONNECTED, "clean {clean}");
        let steps = [EvoStep::BlsCtx, EvoStep::UndoSpecial, EvoStep::ListChanged(true)];
        assert_eq!(evo_steps(&w), steps);
        w.rig.assert_no_abort();
    }
}

/// A failed `VerifyDB` disconnect (`v:5114-5117`) leaves its view as it was: `DISCONNECT_FAILED`
/// commits nothing, whatever the target (CH3a D106).
#[test]
fn a_failed_scratch_disconnect_leaves_its_view() {
    install_accept_log();
    let (w, b1, t1) = spent(22, Setup::default());
    let (path, at) = undo_record(&w, &b1);
    write_at(&path, at + record_len(&path, at), &[0; 32]);
    let held = [
        (op(&t1, 0), t1.outputs()[0].clone(), 1, false),
        (cb_op(&b1), b1.block.txs()[0].outputs()[0].clone(), 1, true),
    ];
    let best = BlockRef {
        id: w.rig.id(&b1),
        hash: b1.hash(),
        height: 1,
    };
    let (result, mut scratch) = scratch_disconnect(&w, Scratch::at(best, &held), &b1);
    assert_eq!(result, DisconnectResult::Failed);
    assert_eq!(scratch.coins.best_block().map(|b| b.hash), Some(b1.hash()));
    assert_eq!(scratch.coin(&seed_op(22)), None);
    assert!(scratch.coin(&op(&t1, 0)).is_some());
    assert!(scratch.coin(&cb_op(&b1)).is_some());
    assert_eq!(tip(&w), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
    assert_eq!(bracket(&w), NOT_CONNECTED);
    // H1 runs before the undo read fails (`v:3272-3273` precedes `v:2298-2302`).
    assert_eq!(evo_steps(&w), [EvoStep::BlsCtx]);
    w.rig.assert_no_abort();
}

/// How a test damages a block's undo record.
#[derive(Clone)]
enum Damage {
    /// The record's last byte, its output script's: it still decodes.
    FlipRecord,
    FlipChecksum,
    /// The file ends one byte into the record's last.
    CutRecord,
    /// The file ends inside the checksum.
    CutChecksum,
    Unlink,
    /// These bytes over the record's first.
    Overwrite(Vec<u8>),
    /// A well-formed record of this undo, with its checksum, in the record's place.
    Replace(BlockUndo),
}

/// One failed undo read (`node/blockstorage.cpp:539-575`) or inconsistent record (`v:2304-2307`,
/// `:2309-2313`, `:2350-2352`), each `DISCONNECT_FAILED` (CH3 §2.10.2) with Core's lines, on which
/// `DisconnectTip` fails (`v:3281-3284`): `InvalidateBlock`'s walk ends with the tip, the coins and
/// the evo best block at the block and nothing reported (`:3966`). A failure before H6 commits
/// nothing and runs no H6; the evo steps stop where Core's do. In a reorg the same failure is
/// `ActivateBestChainStep`'s `AbortNode` (`v:3621`), its one report.
#[test]
fn a_failed_undo_read_fails_the_disconnect_with_cores_lines() {
    install_accept_log();
    const EOF: &str = "UndoReadFromDisk: Deserialize or I/O error - AutoFile::read: end of file: \
                       iostream error";
    const CHECKSUM: &str = "UndoReadFromDisk: Checksum mismatch";
    const SIZES: &str = "DisconnectBlock(): block and undo data inconsistent";
    let read = |line: &'static str| vec![line.to_owned(), UNDO_READ_FAILED.to_owned()];
    let no_coins = BlockUndo {
        txs: vec![TxUndo {
            coins: vec![],
        }],
    };
    let steps = &TIP_STEPS[..2];
    // (seed, damage, H9's answer, the lines, the evo steps)
    type Case<'s> = (u8, Option<Damage>, bool, Vec<String>, &'s [EvoStep]);
    let cases: Vec<Case<'_>> = vec![
        (30, Some(Damage::FlipRecord), true, read(CHECKSUM), &steps[..1]),
        (31, Some(Damage::FlipChecksum), true, read(CHECKSUM), &steps[..1]),
        (32, Some(Damage::CutRecord), true, read(EOF), &steps[..1]),
        (33, Some(Damage::CutChecksum), true, read(EOF), &steps[..1]),
        (34, Some(Damage::Unlink), true, vec![], &steps[..1]),
        (
            35,
            Some(Damage::Overwrite(vec![0xfd, 0x01, 0x00])),
            true,
            read(
                "UndoReadFromDisk: Deserialize or I/O error - non-canonical ReadCompactSize(): \
                 iostream error",
            ),
            &steps[..1],
        ),
        (
            36,
            Some(Damage::Replace(BlockUndo {
                txs: vec![],
            })),
            true,
            vec![SIZES.to_owned()],
            &steps[..1],
        ),
        (37, Some(Damage::Replace(no_coins)), true, vec![TX_SIZES.to_owned()], steps),
        (38, None, false, vec![UNDO_SPECIAL_FAILED.to_owned()], steps),
    ];
    for (n, damage, h9, mut lines, want_steps) in cases {
        let (w, b1, _) = spent(
            n,
            Setup {
                steps: StepsCfg {
                    undo_special: h9,
                    ..StepsCfg::default()
                },
                ..Setup::default()
            },
        );
        let (path, at) = undo_record(&w, &b1);
        let len = record_len(&path, at);
        match damage.clone() {
            Some(Damage::FlipRecord | Damage::FlipChecksum) => {
                let byte = if matches!(damage, Some(Damage::FlipRecord)) {
                    at + len - 1
                } else {
                    at + len
                };
                let mut b = [0];
                std::fs::File::open(&path)
                    .expect("opens")
                    .read_exact_at(&mut b, byte)
                    .expect("reads");
                write_at(&path, byte, &[b[0] ^ 1]);
            }
            Some(Damage::CutRecord) => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .expect("opens")
                    .set_len(at + len - 1)
                    .expect("cuts");
            }
            Some(Damage::CutChecksum) => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .expect("opens")
                    .set_len(at + len + 16)
                    .expect("cuts");
            }
            Some(Damage::Unlink) => {
                std::fs::remove_file(&path).expect("unlinks");
                lines = vec![
                    format!("Unable to open file {}", path.display()),
                    "UndoReadFromDisk: OpenUndoFile failed".to_owned(),
                    UNDO_READ_FAILED.to_owned(),
                ];
            }
            Some(Damage::Overwrite(bytes)) => write_at(&path, at, &bytes),
            Some(Damage::Replace(undo)) => {
                let bytes = undo.core_bytes();
                let checksum = undo_checksum(w.rig.genesis.hash().as_bytes(), bytes.as_slice());
                write_at(&path, at, &[bytes.as_slice(), &checksum].concat());
            }
            None => {}
        }
        let ops = [seed_op(n), cb_op(&b1)];
        let coins = snapshot(&w, &ops);
        invalidate(&w, &b1);
        assert_eq!(tip(&w), Some(b1.hash()), "{n}");
        assert_eq!(snapshot(&w, &ops), coins, "{n}");
        assert_eq!(evo_best(&w), Some(b1.hash()), "{n}");
        assert_eq!(bracket(&w), NOT_CONNECTED, "{n}");
        assert_eq!(evo_steps(&w), want_steps, "{n}");
        assert!(!w.rig.status(&b1).has(Status::FAILED_MASK), "{n}");
        // Each line once, on this world's V: other tests log the same lines beside it.
        let logged: Vec<usize> =
            lines.iter().map(|l| levels_on(w.rig.v_thread(), l).len()).collect();
        assert_eq!(logged, vec![1; lines.len()], "{n}: {lines:?}");
        assert_eq!(tip_failed_lines(w.rig.v_thread(), &b1), 1, "{n}");
        // CH3a D191: a line held after H9 comes out once the log applies, before DisconnectTip's.
        if let Some(held) =
            lines.iter().find(|l| [TX_SIZES, UNDO_SPECIAL_FAILED].contains(&l.as_str()))
        {
            assert_in_order(w.rig.v_thread(), &[held, &tip_failed_line(&b1)], &n.to_string());
        }
        assert!(w.sink.messages().is_empty(), "{n}: {:?}", w.sink.messages());
    }

    // The reorg: a heavier branch from genesis disconnects the block, whose undo is damaged.
    let (mut w, b1, _) = spent(39, Setup::default());
    let (path, at) = undo_record(&w, &b1);
    let len = record_len(&path, at);
    write_at(&path, at + len, &[0; 32]);
    let p = w.rig.p.clone();
    let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
    let s2 = block_on(&p, &s1, vec![]);
    assert!(w.rig.submit(&s1).accepted);
    let v = w.rig.v_thread();
    w.rig.submit_stopping(&s2, "reorg");
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(DISCONNECT_FAILED));
    assert_eq!(w.sink.messages(), [DISCONNECT_FAILED], "one report");
    assert_eq!(levels_on(v, CHECKSUM).len(), 1);
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
}

/// The corruption matrix's size field (CH3 §7.1): Core reads the record from its position and
/// never the size field before it (`node/blockstorage.cpp:550-563`), and dashd's `invalidateblock`
/// disconnects cleanly with the field at 0 and at `0xffffffff` (CH3a D107). ST2 takes the field as a
/// hint only (D104), so no value of it, in the file's bounds or past them, changes the disconnect.
#[test]
fn a_damaged_size_field_changes_nothing() {
    // (seed, the case, the field from the record's length and the file's)
    type Size = fn(u64, u64) -> u64;
    let sizes: [(u8, &str, Size); 5] = [
        (70, "zero", |_, _| 0),
        (71, "one short", |len, _| len - 1),
        (72, "one long", |len, _| len + 1),
        (73, "past the file", |_, file| file + 1),
        (74, "the largest", |_, _| u64::from(u32::MAX)),
    ];
    for (n, case, size) in sizes {
        let (w, b1, _) = spent(n, Setup::default());
        let (path, at) = undo_record(&w, &b1);
        let file = std::fs::metadata(&path).expect("a rev file").len();
        let size = u32::try_from(size(record_len(&path, at), file)).expect("a size field");
        write_at(&path, at - 4, &size.to_le_bytes());
        invalidate(&w, &b1);
        assert_eq!(tip(&w), Some(w.rig.genesis.hash()), "{case}");
        let seed = Some(Coin::new(50 * COIN, &[OP_TRUE], 1, false));
        assert_eq!(snapshot(&w, &[seed_op(n), cb_op(&b1)]).1, [seed, None], "{case}");
        assert_eq!(bracket(&w), DISCONNECTED, "{case}");
        assert_eq!(tip_failed_lines(w.rig.v_thread(), &b1), 0, "{case}");
        assert!(w.sink.messages().is_empty(), "{case}: {:?}", w.sink.messages());
    }
}

/// The checks before the undo read (CH3 §2.12): G1 without complete hooks, then H2 (`v:2286-2292`).
/// Each is an `AbortNode` through the abort path and `DISCONNECT_FAILED`; in a reorg,
/// `ActivateBestChainStep` reports its own after it (`v:3621`), Core's two reports on that path.
/// Nothing commits and no undo is read.
#[test]
fn g1_and_h2_report_and_fail_the_disconnect() {
    let dip3 = Some(Deployment::Buried(BuriedDeployment::Dip0003));
    for (n, complete) in [(50, false), (51, true)] {
        let (mut w, b1, _) = spent(
            n,
            Setup {
                steps: StepsCfg {
                    complete,
                    verify_best: false,
                    ..StepsCfg::default()
                },
                disconnect_extra: dip3,
                ..Setup::default()
            },
        );
        let p = w.rig.p.clone();
        let first = if complete {
            (EVODB_INCONSISTENCY.to_owned(), None)
        } else {
            let text = refusal(&p, &b1, &g1_capability("DIP0003"));
            (text.clone(), Some(text))
        };
        let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
        let s2 = block_on(&p, &s1, vec![]);
        assert!(w.rig.submit(&s1).accepted);
        w.rig.submit_stopping(&s2, &format!("complete {complete}"));
        let exit = w.rig.stop();
        assert_eq!(fatal_text(&exit), Some(DISCONNECT_FAILED), "complete {complete}");
        assert_eq!(
            w.sink.reports(),
            [first, (DISCONNECT_FAILED.to_owned(), None)],
            "complete {complete}"
        );
        assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
        assert_eq!(coins_at(&w.coins), Some(b1.hash()), "complete {complete}");
        assert_eq!(bracket(&w), NOT_CONNECTED, "complete {complete}");
        assert_eq!(evo_steps(&w), [EvoStep::BlsCtx], "complete {complete}");
        assert_eq!(evo_best(&w), Some(b1.hash()), "complete {complete}");
    }
}

/// A disconnect whose log fails to apply (CH7b-2 C7, OD-CP-14, OD-CP-18): the log is the evo undo's
/// writes, which Core makes inside `UndoSpecialTxsInBlock`, whose catch logs its scheme and the
/// failure and fails the disconnect (`specialtxman.cpp:1086-1090`, `validation.cpp:2321-2323` at
/// v24.0.0). In a reorg `ActivateBestChainStep`'s "Failed to disconnect block" (`:3632`) is then
/// the one report: for an undo's snapshot write that fails in the node's rows (an injected I/O
/// failure in a guarded engine on the node's latch and sink), which trips the latch and only logs,
/// and for any other failure. Nothing commits.
#[test]
fn a_failed_log_application_fails_the_disconnect() {
    install_accept_log();
    for (n, store) in [(52, false), (53, true)] {
        let (mut w, b1, _) = spent(n, Setup::default());
        let v = w.rig.v_thread();
        let p = w.rig.p.clone();
        let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
        let s2 = block_on(&p, &s1, vec![]);
        assert!(w.rig.submit(&s1).accepted);
        let latch = w.seen.lock().expect("seen").latch.clone();
        let rows = store.then(|| {
            let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
            engine.inject_failure(dashd_store::ErrorKind::Io);
            let db = Arc::new(GuardedEngine::new(engine, latch.clone(), w.sink.clone()));
            let rows = RocksEvoStore::new(db, crate::evo::testkit::codec(), w.rig.genesis.hash());
            Arc::new(RuntimeRows::new(Arc::new(rows), Arc::default()))
        });
        {
            let mut seen = w.seen.lock().expect("seen");
            seen.fail_next_apply = true;
            seen.undo_rows = rows;
        }
        w.rig.submit_stopping(&s2, &format!("store {store}"));
        let exit = w.rig.stop();
        assert_eq!(fatal_text(&exit), Some(DISCONNECT_FAILED), "store {store}");
        assert_eq!(
            w.sink.reports(),
            [(DISCONNECT_FAILED.to_owned(), None)],
            "store {store}: one report"
        );
        assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
        assert_eq!(coins_at(&w.coins), Some(b1.hash()), "store {store}");
        assert_eq!(evo_best(&w), Some(b1.hash()), "store {store}");
        let what = match latch.tripped() {
            Some(f) if store => {
                assert_eq!(f.site, "chainstate raw write", "the undo write's latch");
                format!("System error while flushing: {}", f.message)
            }
            _ => APPLY_FAILED.to_owned(),
        };
        let caught = format!("CSpecialTxProcessor::UndoSpecialTxsInBlock -- FAILURE! {what}");
        assert_eq!(levels_on(v, &caught), [tracing::Level::ERROR], "store {store}");
        assert_eq!(levels_on(v, UNDO_SPECIAL_FAILED), [tracing::Level::ERROR], "store {store}");
        assert_eq!(levels_on(v, UNDO_CAUGHT_SCHEME), [tracing::Level::INFO], "store {store}");
        assert!(
            lines_on(v).windows(2).any(|w| w[0] == UNDO_CAUGHT_SCHEME && w[1] == caught),
            "store {store}: the scheme line, then FAILURE"
        );
        assert_eq!(tip_failed_lines(v, &b1), 1, "store {store}");
    }
}

/// BL-764: the catch's scheme line is the scheme the undo entered with, under `DisconnectTip`'s
/// guard the block's (`validation.cpp:3283-3284` at v24.0.0). With V19 at height 2 that is b1's
/// legacy, which the committed scheme, basic, is not.
#[test]
fn a_failed_log_application_logs_the_tip_blocks_scheme() {
    install_accept_log();
    let setup = Setup {
        params: Some(v19_at_2()),
        ..Setup::default()
    };
    let (mut w, b1, _) = spent(154, setup);
    let v = w.rig.v_thread();
    let p = w.rig.p.clone();
    let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
    let s2 = block_on(&p, &s1, vec![]);
    assert!(w.rig.submit(&s1).accepted);
    w.seen.lock().expect("seen").fail_next_apply = true;
    w.rig.submit_stopping(&s2, "a failed log application at a tip without V19");
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(DISCONNECT_FAILED));
    assert_eq!(levels_on(v, UNDO_CAUGHT_LEGACY), [tracing::Level::INFO], "b1's legacy");
    assert!(levels_on(v, UNDO_CAUGHT_SCHEME).is_empty(), "not the committed basic");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
}

/// OD-CP-29 (CH7 D102): a failed snapshot write is a store fault with one report, its store's
/// (EV3 D21, CH3b D221), whoever reads the latch next. Another route's write fails in the node's
/// rows (an injected I/O failure in a guarded engine on the node's latch and sink): their store
/// latches it and reports it. The disconnect's undo then writes a snapshot row, which the latch
/// refuses: the undo takes the failure as reported, so the disconnect fails at it and keeps it,
/// and ABC adds no report. Nothing commits. Core's write throws on LevelDB's kept error
/// (`dbwrapper.cpp:200-208`, `:261-268` at v24.0.0) into the undo's catch, so its scheme line and
/// FAILURE are logged here too (`specialtxman.cpp:1088-1089`), and the catch's `false` is
/// `DISCONNECT_FAILED`, which `DisconnectTip` logs (`validation.cpp:2322`, `:3293`; BL-775).
/// ABC's `AbortNode` then sets Core's state (`:3632`), so V stops on ABC's text, reported
/// already, and `ProcessNewBlock`'s line reads it (`:4912`, BL-780).
#[test]
fn an_undo_write_the_latch_refuses_adds_no_report() {
    install_accept_log();
    let (mut w, b1, _) = spent(54, Setup::default());
    let v = w.rig.v_thread();
    let p = w.rig.p.clone();
    let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
    let s2 = block_on(&p, &s1, vec![]);
    assert!(w.rig.submit(&s1).accepted);
    let latch = w.seen.lock().expect("seen").latch.clone();
    let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
    engine.inject_failure(dashd_store::ErrorKind::Io);
    let db = Arc::new(GuardedEngine::new(engine, latch.clone(), w.sink.clone()));
    let rows = RocksEvoStore::new(db, crate::evo::testkit::codec(), w.rig.genesis.hash());
    {
        let mut seen = w.seen.lock().expect("seen");
        seen.fail_next_apply = true;
        seen.undo_rows = Some(Arc::new(RuntimeRows::new(Arc::new(rows), Arc::default())));
        seen.undo_after_a_report = true;
    }
    w.rig.submit_stopping(&s2, "a refused undo write");
    let exit = w.rig.stop();
    let failure = latch.tripped().expect("the store latched it").clone();
    assert_eq!(failure.site, "chainstate raw write");
    let text = format!("System error while flushing: {}", failure.message);
    assert_eq!(fatal_text(&exit), Some(DISCONNECT_FAILED), "ABC's text");
    assert!(exit.fatal.as_ref().is_some_and(Fatal::is_abort_node), "an AbortNode, not an assert");
    assert_eq!(w.sink.reports(), [(text.clone(), None)], "the store's report, and only it");
    assert_pnb_line_after_the_tip(v, &b1, &text, "a refused undo write");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
    let caught = "CSpecialTxProcessor::UndoSpecialTxsInBlock -- FAILURE! ";
    assert_eq!(levels_on(v, UNDO_CAUGHT_SCHEME), [tracing::Level::INFO], "the catch's scheme line");
    assert!(
        lines_on(v).windows(2).any(|w| w[0] == UNDO_CAUGHT_SCHEME && w[1].starts_with(caught)),
        "the scheme line, then FAILURE"
    );
    assert_tip_failed_after_the_catch(v, &b1, "a refused undo write");
}

/// BL-629: H9 fails after the undo's log holds its snapshot write, and the write is the first
/// store fault (the guard latches it, the store only logs). Core's write comes first and fails the
/// undo (`specialtxman.cpp:1086-1090`), so H9's failure is never reached: the disconnect fails at
/// the write, and in a reorg `ActivateBestChainStep`'s "Failed to disconnect block" (`v:3632` at
/// v24.0.0) is the one report. H9's `Fatal` is logged at debug only. Nothing commits.
#[test]
fn an_undo_failure_after_its_log_write_failed_has_abcs_one_report() {
    install_accept_log();
    let (mut w, b1, _) = spent(150, Setup::default());
    let v = w.rig.v_thread();
    let p = w.rig.p.clone();
    let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
    let s2 = block_on(&p, &s1, vec![]);
    assert!(w.rig.submit(&s1).accepted);
    let latch = arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    w.seen.lock().expect("seen").fail_next_undo = true;
    w.rig.submit_stopping(&s2, "an undo failure after its log write failed");
    let exit = w.rig.stop();
    assert_eq!(fatal_text(&exit), Some(DISCONNECT_FAILED));
    assert_eq!(
        w.sink.reports(),
        [(DISCONNECT_FAILED.to_owned(), None)],
        "ABC's report, and only it"
    );
    let failure = latch.tripped().expect("the store latched it").clone();
    assert_eq!(failure.site, "chainstate raw write", "the undo write's latch");
    let caught = format!(
        "CSpecialTxProcessor::UndoSpecialTxsInBlock -- FAILURE! System error while flushing: {}",
        failure.message
    );
    assert_eq!(levels_on(v, &caught), [tracing::Level::ERROR]);
    assert_eq!(levels_on(v, UNDO_SPECIAL_FAILED), [tracing::Level::ERROR]);
    let dropped = format!("DisconnectBlock: {UNDO_FATAL}");
    assert_eq!(levels_on(v, &dropped), [tracing::Level::DEBUG], "H9's failure, at debug");
    assert_eq!(tip_failed_lines(v, &b1), 1);
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
}

/// BL-629 with OD-CP-29 (CH7 D102): H9 fails, and the undo's snapshot write is then refused, as
/// another route's write in the node's rows failed first and their store reported it. Core's write
/// throws first on LevelDB's kept error into the undo's catch (`specialtxman.cpp:1086-1090` at
/// v24.0.0, OD-CP-149), so H9's failure is never reached (BL-772): the disconnect fails at the
/// write with its scheme line, FAILURE and "UndoSpecialTxsInBlock failed", keeps the write's
/// reported failure, and `DisconnectTip` logs its line (`validation.cpp:2322`, `:3293`; BL-775);
/// the store's report is the one. V stops on ABC's text, reported already, which
/// `ProcessNewBlock`'s line reads (`:3632`, `:4912`; BL-780). H9's `Fatal` is logged at debug
/// only. Nothing commits.
#[test]
fn an_undo_failure_whose_log_write_was_refused_fails_at_the_write() {
    install_accept_log();
    let (mut w, b1, _) = spent(152, Setup::default());
    let v = w.rig.v_thread();
    let p = w.rig.p.clone();
    let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
    let s2 = block_on(&p, &s1, vec![]);
    assert!(w.rig.submit(&s1).accepted);
    let latch = arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    {
        let mut seen = w.seen.lock().expect("seen");
        seen.undo_after_a_report = true;
        seen.fail_next_undo = true;
    }
    w.rig.submit_stopping(&s2, "an undo failure whose log write was refused");
    let exit = w.rig.stop();
    let text = reported_once(&latch, &w.sink);
    assert_eq!(fatal_text(&exit), Some(DISCONNECT_FAILED), "ABC's text");
    assert!(exit.fatal.as_ref().is_some_and(Fatal::is_abort_node), "an AbortNode, not an assert");
    assert_pnb_line_after_the_tip(v, &b1, &text, "an undo failure whose log write was refused");
    assert!(levels_on(v, &format!("finish_attempt: {text}")).is_empty(), "not the guard's arm");
    let caught = format!("CSpecialTxProcessor::UndoSpecialTxsInBlock -- FAILURE! {text}");
    assert_eq!(levels_on(v, &caught), [tracing::Level::ERROR]);
    assert_eq!(levels_on(v, UNDO_CAUGHT_SCHEME), [tracing::Level::INFO], "the catch's scheme line");
    assert!(
        lines_on(v).windows(3).any(|w| {
            w[0] == UNDO_CAUGHT_SCHEME && w[1] == caught && w[2] == UNDO_SPECIAL_FAILED
        }),
        "the scheme line, FAILURE, then UndoSpecialTxsInBlock failed"
    );
    assert_tip_failed_after_the_catch(v, &b1, "an undo failure whose log write was refused");
    let dropped = format!("DisconnectBlock: {UNDO_FATAL}");
    assert_eq!(levels_on(v, &dropped), [tracing::Level::DEBUG], "H9's failure, at debug");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
}

/// BL-772: H9 fails with an `AbortNode`, which V reports latched or not (H9's `Fatal::abort_node`
/// class), and the undo's snapshot write is then refused, as another route's write failed
/// first and their store reported it. Core's write throws first (`specialtxman.cpp:1086-1090` at
/// v24.0.0), so H9's failure is never reached: the disconnect fails at the write and keeps its
/// reported failure, `DisconnectTip` logs its line (`validation.cpp:3293`, BL-775), and the
/// store's report is the one (OD-CP-29, CH7 D102). V stops on ABC's text, reported already, which
/// `ProcessNewBlock`'s line reads (`validation.cpp:3632`, `:4912`; BL-780). Nothing commits. No
/// `undo_block` verdict gives H9 this class today; the test pins the guard for a future one
/// (OD-CP-153).
#[test]
fn an_abort_node_undo_failure_whose_log_write_was_refused_has_one_report() {
    install_accept_log();
    let (mut w, b1, _) = spent(156, Setup::default());
    let v = w.rig.v_thread();
    let p = w.rig.p.clone();
    let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
    let s2 = block_on(&p, &s1, vec![]);
    assert!(w.rig.submit(&s1).accepted);
    let latch = arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    {
        let mut seen = w.seen.lock().expect("seen");
        seen.undo_after_a_report = true;
        seen.fail_next_undo_always = true;
    }
    w.rig.submit_stopping(&s2, "an AbortNode undo failure whose log write was refused");
    let exit = w.rig.stop();
    let text = reported_once(&latch, &w.sink);
    assert_eq!(fatal_text(&exit), Some(DISCONNECT_FAILED), "ABC's text");
    assert!(exit.fatal.as_ref().is_some_and(Fatal::is_abort_node), "an AbortNode, not an assert");
    assert_tip_failed_after_the_catch(v, &b1, "an AbortNode undo failure");
    assert_pnb_line_after_the_tip(v, &b1, &text, "an AbortNode undo failure");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
}

/// BL-775 (Sol r1, CH3b D231): W panics, and the latch then refuses the undo's snapshot write,
/// which carries W's panic as the assert it is, reported. Core's flush is sequential, so its
/// assert ended the process before the undo wrote: nothing after it logs, neither the catch's
/// lines nor `DisconnectTip`'s, and ABC adds no report. W's report is the one, the assert's class
/// and text kept, with H9's own failure, if any, at debug only. Nothing commits. W panics after
/// the undo's log holds the write, with H9 succeeding or failing, or in H9, so that the latch had
/// tripped before the attempt ended (L-3): the assert still wins over H9's failure.
#[test]
fn an_undo_write_refused_after_an_assert_logs_nothing_after_it() {
    install_accept_log();
    for (n, case) in [(164, "H9 succeeds"), (165, "H9 fails"), (167, "W panics in H9")] {
        let (mut w, b1, _) = spent(n, Setup::default());
        let v = w.rig.v_thread();
        let p = w.rig.p.clone();
        let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
        let s2 = block_on(&p, &s1, vec![]);
        assert!(w.rig.submit(&s1).accepted);
        if case == "W panics in H9" {
            arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
            w.seen.lock().expect("seen").fail_next_undo_after_a_panic = true;
        } else {
            arm_undo_refused_after_an_assert(&w);
            w.seen.lock().expect("seen").fail_next_undo = case == "H9 fails";
        }
        w.rig.submit_stopping(&s2, case);
        let exit = w.rig.stop();
        assert_nothing_after_the_assert(&w, &exit, v, &b1, case);
        let dropped = levels_on(v, &format!("DisconnectBlock: {UNDO_FATAL}"));
        let debug = (case != "H9 succeeds").then_some(tracing::Level::DEBUG);
        assert_eq!(dropped, Vec::from_iter(debug), "{case}: H9's failure, at debug only");
        assert_eq!(evo_best(&w), Some(b1.hash()), "{case}");
    }
}

/// Arms the next attempt's log to meet W's panic before the undo's snapshot write in the node's
/// rows, which the latch then refuses with the panic's assert (BL-586).
pub(super) fn arm_undo_refused_after_an_assert(w: &World) {
    arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    w.seen.lock().expect("seen").undo_after_an_assert = Some(w.sink.clone());
}

/// After [`arm_undo_refused_after_an_assert`]'s disconnect of `m`, which V on thread `v` stopped
/// on: W's panic is the one report and V's fatal, an assert; V logged no line of the undo's catch,
/// `DisconnectBlock`'s, `finish_attempt`'s, `DisconnectTip`'s or VerifyDB's after it; and the
/// tip and the active coins are still at `m`.
pub(super) fn assert_nothing_after_the_assert(
    w: &World,
    exit: &VExit,
    v: std::thread::ThreadId,
    m: &Mined,
    case: &str,
) {
    let latched = w.seen.lock().expect("seen").latch.tripped().map(|f| f.site);
    assert_eq!(latched, Some(W_PANIC_SITE), "{case}: W's panic latched first");
    assert_eq!(w.sink.reports(), [(W_PANICKED.to_owned(), None)], "{case}: W's report only");
    assert_eq!(fatal_text(exit), Some(W_PANICKED), "{case}: the assert's text");
    assert!(exit.fatal.as_ref().is_some_and(|f| !f.is_abort_node()), "{case}: an assert");
    let after = [
        "CSpecialTxProcessor::UndoSpecialTxsInBlock --",
        UNDO_SPECIAL_FAILED,
        "finish_attempt:",
        "DisconnectTip(): DisconnectBlock",
        "Verification error: irrecoverable inconsistency",
    ];
    let logged: Vec<_> =
        lines_on(v).into_iter().filter(|l| after.iter().any(|a| l.starts_with(a))).collect();
    assert!(logged.is_empty(), "{case}: nothing after the assert: {logged:?}");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(m.hash()), "{case}");
    assert_eq!(coins_at(&w.coins), Some(m.hash()), "{case}");
}

/// BL-771, BL-773: H9 answers `false` (`v:2321-2323`) after the undo's log holds its snapshot
/// write, and the log then fails to apply: a wiring failure, or the undo write's own store fault.
/// Core's write comes first and fails the undo in its catch (`specialtxman.cpp:1086-1090` at
/// v24.0.0), so `DisconnectBlock` logs "UndoSpecialTxsInBlock failed" once, after the catch's
/// lines, and H9's `false` is unreached (CH3a D191); in a reorg `ActivateBestChainStep`'s "Failed
/// to disconnect block" (`v:3632`) is the one report.
#[test]
fn a_false_undo_whose_log_fails_logs_its_line_once() {
    install_accept_log();
    for (n, store) in [(157, false), (158, true)] {
        let setup = Setup {
            steps: StepsCfg {
                undo_special: false,
                ..StepsCfg::default()
            },
            ..Setup::default()
        };
        let (mut w, b1, _) = spent(n, setup);
        let v = w.rig.v_thread();
        let p = w.rig.p.clone();
        let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
        let s2 = block_on(&p, &s1, vec![]);
        assert!(w.rig.submit(&s1).accepted);
        if store {
            arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
        } else {
            w.seen.lock().expect("seen").fail_next_apply = true;
        }
        w.rig.submit_stopping(&s2, &format!("store {store}"));
        let exit = w.rig.stop();
        assert_eq!(fatal_text(&exit), Some(DISCONNECT_FAILED), "store {store}");
        assert_eq!(
            w.sink.reports(),
            [(DISCONNECT_FAILED.to_owned(), None)],
            "store {store}: one report"
        );
        let once = levels_on(v, UNDO_SPECIAL_FAILED);
        assert_eq!(once, [tracing::Level::ERROR], "store {store}: once");
        let caught = "CSpecialTxProcessor::UndoSpecialTxsInBlock -- FAILURE! ";
        let failures = lines_on(v).iter().filter(|l| l.starts_with(caught)).count();
        assert_eq!(failures, 1, "store {store}: one FAILURE");
        assert_eq!(levels_on(v, UNDO_CAUGHT_SCHEME), [tracing::Level::INFO], "store {store}");
        assert_eq!(tip_failed_lines(v, &b1), 1, "store {store}");
        assert_catch_then_tip(v, &b1, &format!("store {store}"));
        assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
        assert_eq!(coins_at(&w.coins), Some(b1.hash()), "store {store}");
        assert_eq!(evo_best(&w), Some(b1.hash()), "store {store}");
    }
}

/// The undo catch's lines and `DisconnectTip`'s for `m` on the V thread `v`, in Core's order once
/// its snapshot write threw: the scheme line, FAILURE, "UndoSpecialTxsInBlock failed" and
/// `v:3293` (`specialtxman.cpp:1088-1089`, `validation.cpp:2322`, `:3293` at v24.0.0), each once.
fn assert_catch_then_tip(v: std::thread::ThreadId, m: &Mined, case: &str) {
    let tip = tip_failed_line(m);
    assert_in_order(v, &[UNDO_CAUGHT_SCHEME, UNDO_FAILURE, UNDO_SPECIAL_FAILED, &tip], case);
}

/// The lines on the V thread `v` that start with one of `of` are exactly `of`, in its order.
pub(super) fn assert_in_order(v: std::thread::ThreadId, of: &[&str], case: &str) {
    let lines = lines_on(v);
    let logged: Vec<&str> =
        lines.iter().filter_map(|l| of.iter().copied().find(|p| l.starts_with(p))).collect();
    assert_eq!(logged, of, "{case}");
}

/// CH3a D191 (BL-768): a disconnect's scheme guard is `DisconnectTip`'s (`validation.cpp:3283-3284`
/// at v24.0.0), so its exit, whose revert line is the destructor's (`:162-167`), comes once, after
/// `DisconnectTip`'s line (`:3293`), as it returns: after a failed undo read, and after the
/// catch's lines when the undo's log fails. After one of Core's asserts the exit logs nothing
/// (CH3b D231). The rig's hooks log each exit with its `log`.
#[test]
fn a_disconnects_scheme_guard_exits_after_disconnect_tips_line() {
    install_accept_log();
    let exit_lines = |v, log: bool| levels_on(v, &format!("{GUARD_EXIT}{log}")).len();
    let logs = format!("{GUARD_EXIT}true");

    // `invalidateblock` of b1, whose undo record's checksum is damaged.
    let (w, b1, _) = spent(176, Setup::default());
    let (path, at) = undo_record(&w, &b1);
    write_at(&path, at + record_len(&path, at), &[0; 32]);
    invalidate(&w, &b1);
    let v = w.rig.v_thread();
    assert_in_order(v, &[UNDO_READ_FAILED, &tip_failed_line(&b1), &logs], "a failed undo read");
    assert_eq!((exit_lines(v, true), exit_lines(v, false)), (1, 0), "a failed undo read");

    // A reorg whose undo answers `false` and whose log then fails.
    for (n, case) in [(177, "the catch"), (178, "an assert")] {
        let setup = Setup {
            steps: StepsCfg {
                undo_special: false,
                ..StepsCfg::default()
            },
            ..Setup::default()
        };
        let (mut w, b1, _) = spent(n, setup);
        let v = w.rig.v_thread();
        let p = w.rig.p.clone();
        let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
        let s2 = block_on(&p, &s1, vec![]);
        assert!(w.rig.submit(&s1).accepted);
        if case == "an assert" {
            arm_undo_refused_after_an_assert(&w);
        } else {
            w.seen.lock().expect("seen").fail_next_apply = true;
        }
        w.rig.submit_stopping(&s2, case);
        drop(w.rig.stop());
        if case == "an assert" {
            assert_eq!((exit_lines(v, true), exit_lines(v, false)), (0, 1), "{case}");
        } else {
            let tip = tip_failed_line(&b1);
            assert_in_order(v, &[UNDO_FAILURE, UNDO_SPECIAL_FAILED, &tip, &logs], case);
            assert_eq!((exit_lines(v, true), exit_lines(v, false)), (1, 0), "{case}");
        }
    }
}

/// BL-773: an undo record whose transaction has fewer coins than inputs (`v:2359-2362`), where
/// the undo's log holds its snapshot write and then fails to apply, a wiring failure or the undo
/// write's own store fault. Core's write throws before the coins (`llmq/snapshot.cpp:327`,
/// `specialtxman.cpp:1086-1090` at v24.0.0), so "transaction and undo data inconsistent" is
/// unreached: the disconnect logs the catch's lines and `DisconnectTip`'s alone (CH3a D191), and
/// "Failed to disconnect block" (`v:3632`) is the one report.
#[test]
fn an_inconsistent_undo_whose_log_fails_logs_only_the_catch() {
    install_accept_log();
    for (n, store) in [(174, false), (175, true)] {
        let (mut w, b1, _) = spent(n, Setup::default());
        let (path, at) = undo_record(&w, &b1);
        let no_coins = BlockUndo {
            txs: vec![TxUndo {
                coins: vec![],
            }],
        };
        let bytes = no_coins.core_bytes();
        let checksum = undo_checksum(w.rig.genesis.hash().as_bytes(), bytes.as_slice());
        write_at(&path, at, &[bytes.as_slice(), &checksum].concat());
        let v = w.rig.v_thread();
        let p = w.rig.p.clone();
        let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
        let s2 = block_on(&p, &s1, vec![]);
        assert!(w.rig.submit(&s1).accepted);
        if store {
            arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
        } else {
            w.seen.lock().expect("seen").fail_next_apply = true;
        }
        let case = format!("store {store}");
        w.rig.submit_stopping(&s2, &case);
        let exit = w.rig.stop();
        assert_eq!(fatal_text(&exit), Some(DISCONNECT_FAILED), "{case}");
        assert_eq!(w.sink.reports(), [(DISCONNECT_FAILED.to_owned(), None)], "{case}: one report");
        assert!(levels_on(v, TX_SIZES).is_empty(), "{case}: Core's unreached line");
        assert_catch_then_tip(v, &b1, &case);
        assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()), "{case}");
        assert_eq!(coins_at(&w.coins), Some(b1.hash()), "{case}");
    }
}

/// BL-629: H9 fails after the node latch tripped with a failure its store reported (as a guarded
/// read's does), and the log then fails to apply, a fault nobody reported. The latch had its
/// report before the log was applied (L-3), so the disconnect keeps H9's failure: V stops on it
/// and adds nothing, and ABC adds no "Failed to disconnect block". One report. Nothing commits.
#[test]
fn an_undo_failure_latched_before_its_log_keeps_its_one_report() {
    install_accept_log();
    let (mut w, b1, _) = spent(153, Setup::default());
    let v = w.rig.v_thread();
    let p = w.rig.p.clone();
    let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
    let s2 = block_on(&p, &s1, vec![]);
    assert!(w.rig.submit(&s1).accepted);
    {
        let mut seen = w.seen.lock().expect("seen");
        seen.fail_next_undo_latched = true;
        seen.fail_next_apply = true;
    }
    w.rig.submit_stopping(&s2, "an undo failure latched before its log");
    let exit = w.rig.stop();
    assert_eq!(w.sink.reports(), [(UNDO_READ_REPORTED.to_owned(), None)], "the read's report only");
    assert_eq!(fatal_text(&exit), Some(UNDO_FATAL), "H9's own failure");
    assert_eq!(levels_on(v, &format!("finish_attempt: {APPLY_FAILED}")), [tracing::Level::ERROR]);
    let caught = "CSpecialTxProcessor::UndoSpecialTxsInBlock -- FAILURE!";
    assert!(lines_on(v).iter().all(|l| !l.starts_with(caught)), "no FAILURE line");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
}

/// Arms the next attempt's log to fail on an undo's own snapshot write in the node's rows (an
/// injected I/O failure in a guarded engine on the node's latch and `sink`): the first store
/// fault, which the guard latches and the store only logs (OD-CP-18). The node's latch.
pub(super) fn arm_undo_store_fault(
    seen: &Mutex<Seen>,
    sink: &Arc<RecordingAbort>,
    genesis: BlockHash,
) -> GlobalFailStop {
    let mut seen = seen.lock().expect("seen");
    let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
    engine.inject_failure(dashd_store::ErrorKind::Io);
    let db = Arc::new(GuardedEngine::new(engine, seen.latch.clone(), sink.clone()));
    let rows = RocksEvoStore::new(db, crate::evo::testkit::codec(), genesis);
    seen.fail_next_apply = true;
    seen.undo_rows = Some(Arc::new(RuntimeRows::new(Arc::new(rows), Arc::default())));
    seen.latch.clone()
}

/// BL-780: `ProcessNewBlock`'s line after a failed `DisconnectTip` of `m` whose undo met a kept
/// fault reported already (CH7 D102). Core's state reads ABC's text (`v:3632`, `v:4912` at
/// v24.0.0, `consensus/validation.h:92-97`), not the fault's: logged once on the V thread `v`, at
/// ERROR, after `DisconnectTip`'s line (`v:3293`) and after the fault's one report `report`.
fn assert_pnb_line_after_the_tip(v: std::thread::ThreadId, m: &Mined, report: &str, case: &str) {
    let line = format!("ProcessNewBlock: ActivateBestChain failed ({DISCONNECT_FAILED})");
    assert_eq!(levels_on(v, &line), [tracing::Level::ERROR], "{case}: ProcessNewBlock's line");
    let lines = lines_on(v);
    let fault = format!("failed ({report})");
    assert!(!lines.iter().any(|l| l.contains(&fault)), "{case}: {lines:#?}");
    assert_logged_before(&lines, &tip_failed_line(m), &line);
    assert_logged_before(&lines, &format!("*** {report}"), &line);
}

/// OD-CP-30 (CH7 D103): the store fault's one report is its store's text, the latched raw row
/// write's: `sink`'s whole list. The text.
pub(super) fn reported_once(latch: &GlobalFailStop, sink: &RecordingAbort) -> String {
    let failure = latch.tripped().expect("the store latched it").clone();
    assert_eq!(failure.site, "chainstate raw write");
    let text = format!("System error while flushing: {}", failure.message);
    assert_eq!(sink.reports(), [(text.clone(), None)], "the store's report, and only it");
    text
}

/// OD-CP-30 (CH7 D103), `invalidateblock`: the disconnect's undo writes a snapshot row, and the
/// write is the first store fault (the guard latches it, the store only logs). Core's
/// `InvalidateBlock` returns `false` with no report (`v:3965-3972`), but the latch keeps the fault
/// and the node's flush side then takes it as reported; so V stops on it where invalidation
/// fails, and that stop is its one report, the store's text. Nothing commits.
#[test]
fn an_invalidation_whose_undo_write_fails_first_has_the_stores_report() {
    install_accept_log();
    let setup = Setup {
        node_stop: true,
        ..Setup::default()
    };
    let (mut w, b1, _) = spent(55, setup);
    let latch = arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    let replied = w.rig.v.invalidate_block(b1.hash()).recv();
    let exit = w.rig.stop();
    let text = reported_once(&latch, &w.sink);
    assert_eq!(fatal_text(&exit), Some(text.as_str()), "the store's text");
    assert_eq!(replied, Err(WaitError::Disconnected), "V stops in the invalidation");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
}

/// BL-775 (CH7 D102), `invalidateblock`: another route's snapshot write fails first, reported by
/// their store, and the disconnect's undo write is then refused. Core's write throws into the
/// undo's catch (`specialtxman.cpp:1086-1090` at v24.0.0), so `DisconnectTip` logs its line
/// (`validation.cpp:3293`) and `InvalidateBlock` returns `false` (`:3977`). V stops on the refused
/// write's reported failure and adds nothing: the store's report is the one. Nothing commits.
#[test]
fn an_invalidation_whose_undo_write_was_refused_has_the_stores_report() {
    install_accept_log();
    let setup = Setup {
        node_stop: true,
        ..Setup::default()
    };
    let (mut w, b1, _) = spent(159, setup);
    let v = w.rig.v_thread();
    let latch = arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    w.seen.lock().expect("seen").undo_after_a_report = true;
    let replied = w.rig.v.invalidate_block(b1.hash()).recv();
    let exit = w.rig.stop();
    let text = reported_once(&latch, &w.sink);
    assert_eq!(fatal_text(&exit), Some(text.as_str()), "the store's text");
    assert_eq!(replied, Err(WaitError::Disconnected), "V stops in the invalidation");
    assert_tip_failed_after_the_catch(v, &b1, "a refused undo write in an invalidation");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
}

/// OD-CP-30 (CH7 D103), ChainLock enforcement: a lock on s1 marks the tied b1 conflicting, and
/// `MarkConflictingBlock`'s `DisconnectTip` of b1 meets the undo's snapshot write as the first
/// store fault. V stops on it where the mark fails, its one report the store's text, and
/// EnforceBlock's assert (`v:4075-4078` at v24.0.0) logs Core's line after it, reporting nothing.
/// Core's `false` leaves `dummy_state` valid (`chainlock/handler.cpp:289`), so the line reads
/// `Valid` (BL-778).
#[test]
fn a_chainlock_mark_whose_undo_write_fails_first_has_the_stores_report() {
    use super::super::locks::{intake, lock, record};

    install_accept_log();
    let setup = Setup {
        node_stop: true,
        ..Setup::default()
    };
    let (mut w, b1, s1) = tied_chainlock_world(Some(56), setup);
    let v = w.rig.v_thread();
    intake(&w.rig);
    let latch = arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    assert!(record(&w.rig, &lock(&s1)));
    let served = served_next(&w);
    let exit = w.rig.stop();
    let report = reported_once(&latch, &w.sink);
    let line = "Chainstate::EnforceBlock -- MarkConflictingBlock failed: Valid";
    assert_eq!(fatal_text(&exit), Some(line), "EnforceBlock's assert");
    assert!(exit.fatal.as_ref().is_some_and(|f| !f.is_abort_node()), "the assert's class");
    assert_eq!(served, Some(Err(WaitError::Disconnected)), "V stops in the mark");
    assert_valid_line_after_the_tip(
        v,
        &b1,
        &report,
        line,
        "an undo write failing first in the mark",
    );
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
}

/// BL-775 (CH7 D102), ChainLock enforcement: a lock on s1 marks the tied b1 conflicting, another
/// route's snapshot write fails first, reported by their store, and the undo write of
/// `MarkConflictingBlock`'s `DisconnectTip` of b1 is then refused. Core's write throws into the
/// undo's catch (`specialtxman.cpp:1086-1090` at v24.0.0), so `DisconnectTip` logs its line
/// (`validation.cpp:3293`) before the mark's `false` (`:4114-4116`) and EnforceBlock's assert
/// (`:4076-4078`), whose line reads `dummy_state`'s `Valid` (`chainlock/handler.cpp:289`;
/// BL-778). V stops on the refused write's reported failure, and the assert reports nothing: the
/// store's report is the one.
#[test]
fn a_chainlock_mark_whose_undo_write_was_refused_has_the_stores_report() {
    use super::super::locks::{intake, lock, record};

    install_accept_log();
    let setup = Setup {
        node_stop: true,
        ..Setup::default()
    };
    let (mut w, b1, s1) = tied_chainlock_world(Some(160), setup);
    let v = w.rig.v_thread();
    intake(&w.rig);
    let latch = arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    w.seen.lock().expect("seen").undo_after_a_report = true;
    assert!(record(&w.rig, &lock(&s1)));
    let served = served_next(&w);
    let exit = w.rig.stop();
    let report = reported_once(&latch, &w.sink);
    let line = "Chainstate::EnforceBlock -- MarkConflictingBlock failed: Valid";
    assert_eq!(fatal_text(&exit), Some(line), "EnforceBlock's assert");
    assert!(exit.fatal.as_ref().is_some_and(|f| !f.is_abort_node()), "the assert's class");
    assert_eq!(served, Some(Err(WaitError::Disconnected)), "V stops in the mark");
    assert_tip_failed_after_the_catch(v, &b1, "a refused undo write in the mark");
    assert_valid_line_after_the_tip(v, &b1, &report, line, "a refused undo write in the mark");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
}

/// InstantSend's store, scripted: one conflict, in the block set, so `ResolveBlockConflicts`
/// invalidates it (`is:608-631`), and with the lock's transaction known (`has_tx`) passes it by
/// `ActivateBestChain` (`is:634-641`) instead of resetting it.
#[derive(Clone, Default)]
struct ConflictIn {
    block: Arc<Mutex<Option<BlockHash>>>,
    has_tx: bool,
}

impl crate::chain::IsConflicts for ConflictIn {
    fn retrieve_conflicts(&mut self, _: &Hash256) -> Vec<BlockHash> {
        self.block.lock().expect("conflict").iter().copied().collect()
    }

    fn has_tx_for_lock(&mut self, _: &Hash256) -> bool {
        self.has_tx
    }

    fn clear_conflicting(&mut self, _: &Hash256, _: &BlockHash) {}

    fn remove_conflicting_lock(&mut self, _: &Hash256) {}
}

/// OD-CP-30 (CH7 D103), InstantSend's conflict: `ResolveBlockConflicts` invalidates b1, which holds
/// the conflict (`is:621-624` at v24.0.0), through `invalidateblock`'s `InvalidateBlock`, whose
/// disconnect meets the undo's snapshot write as the first store fault. V stops on it there, its
/// one report the store's text, and the resolution's assert logs Core's line after it. Core's
/// `false` leaves the resolution's fresh state valid (`instantsend/net_instantsend.cpp:618` at
/// v24.0.0), so the line reads `Valid` (BL-778).
#[test]
fn an_instantsend_conflicts_invalidation_whose_undo_write_fails_first_has_the_stores_report() {
    instantsend_conflict_undo_write_fault(57, false);
}

/// BL-778 (CH7 D102), InstantSend's conflict: `ResolveBlockConflicts` invalidates b1, another
/// route's snapshot write fails first, reported by their store, and the undo write of b1's
/// disconnect is then refused. Core's write throws into the undo's catch
/// (`specialtxman.cpp:1086-1090` at v24.0.0), so `DisconnectTip` logs its line
/// (`validation.cpp:3293`) before `InvalidateBlock`'s `false` (`:3977`) and the resolution's
/// assert (`is:622-624`), whose line reads the fresh state's `Valid` (`is:618`). V stops on the
/// refused write's reported failure, and the assert reports nothing: the store's report is the one.
#[test]
fn an_instantsend_conflicts_invalidation_whose_undo_write_was_refused_has_the_stores_report() {
    instantsend_conflict_undo_write_fault(168, true);
}

/// `ResolveBlockConflicts` for a conflict in b1 of seed `n`'s world, whose disconnect's undo
/// write fails as the first store fault, or with `refused` is refused after another route's write
/// failed first: the store's one report, then the resolution's assert with Core's `Valid` line.
fn instantsend_conflict_undo_write_fault(n: u8, refused: bool) {
    install_accept_log();
    let setup = Setup {
        node_stop: true,
        ..Setup::default()
    };
    let (mut w, b1) = conflict_world(Some(n), setup, false);
    let v = w.rig.v_thread();
    let latch = arm_undo_store_fault(&w.seen, &w.sink, w.rig.genesis.hash());
    w.seen.lock().expect("seen").undo_after_a_report = refused;
    let replied = resolve_conflict(&w);
    let exit = w.rig.stop();
    let report = reported_once(&latch, &w.sink);
    let line = "NetInstantSend::ResolveBlockConflicts -- InvalidateBlock failed: Valid";
    assert_eq!(fatal_text(&exit), Some(line), "the resolution's assert");
    assert!(exit.fatal.as_ref().is_some_and(|f| !f.is_abort_node()), "the assert's class");
    assert_eq!(replied, Some(WaitError::Disconnected), "V stops in the invalidation");
    let case = if refused {
        "a refused undo write"
    } else {
        "an undo write failing first"
    };
    if refused {
        assert_tip_failed_after_the_catch(v, &b1, case);
    }
    assert_valid_line_after_the_tip(v, &b1, &report, line, case);
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(coins_at(&w.coins), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
}

/// BL-781 (#12): every read of the active coins' database fails from now on with an engine I/O
/// error, so a coin the cache does not hold is a coins read fault. A seed a connect spent is one:
/// FRESH, it left the cache when spent. The guard latches the fault and reports it to the coins'
/// own sink, which this world keeps apart from the node's.
fn fail_coins_reads(w: &World) {
    let coins = w.coins.lock().expect("coins");
    coins.db().inner().fail_reads(Some(dashd_store::ErrorKind::Io));
}

/// BL-781 (#12): Core's coins read ends its process inside the coins error catcher
/// (`coins.cpp:356-369` at v24.0.0), so no caller logs a line. The read latched its store, which
/// reported it; V reports nothing more; no line the V thread `v` logged starts with one of
/// `callers`; and V's exit carries the read's text.
fn assert_no_caller_line_after_a_read(
    w: &World,
    v: std::thread::ThreadId,
    exit: &VExit,
    callers: &[&str],
    case: &str,
) {
    let coins = w.coins.lock().expect("coins");
    let read = coins.db().latch().tripped().expect("the read latched its store").clone();
    assert_eq!(read.site, "chainstate read", "{case}");
    assert!(w.sink.reports().is_empty(), "{case}: the store's report is the one");
    let lines = lines_on(v);
    for caller in callers {
        assert!(!lines.iter().any(|l| l.starts_with(caller)), "{case}: {caller}: {lines:#?}");
    }
    let text = fatal_text(exit).expect("V stopped on it");
    assert!(text.starts_with("coins read: "), "{case}: the read's text, not {text:?}");
}

/// [`assert_no_caller_line_after_a_read`] under the mark's or InstantSend's assert, which ends V
/// as Core's assert would (D62a), with no `DisconnectTip` line for `m` either: the read never
/// returns to `DisconnectBlock` in Core.
fn assert_no_assert_line_after_a_read(
    w: &World,
    v: std::thread::ThreadId,
    exit: &VExit,
    m: &Mined,
    caller: &str,
    case: &str,
) {
    assert_no_caller_line_after_a_read(w, v, exit, &[caller], case);
    assert!(exit.fatal.as_ref().is_some_and(|f| f.is_reported_assert()), "{case}: the class");
    assert_eq!(tip_failed_lines(v, m), 0, "{case}: DisconnectTip's line");
}

/// The mark's assert line's prefix (`v:4076` at v24.0.0).
const MARK_FAILED: &str = "Chainstate::EnforceBlock -- MarkConflictingBlock failed";

/// InstantSend's assert line's prefix after `InvalidateBlock` (`is:622` at v24.0.0).
const IS_INVALIDATE_FAILED: &str =
    "NetInstantSend::ResolveBlockConflicts -- InvalidateBlock failed";

/// A ChainLock world whose block 1 spends `seed`, or holds only its coinbase, with s1 tied to it:
/// the world, b1, s1.
fn tied_chainlock_world(seed: Option<u8>, setup: Setup) -> (World, Mined, Mined) {
    let (w, b1) = b1_world(seed, setup, |cfg| RigConfig {
        chainlocks: true,
        ..cfg
    });
    let s1 = tie_b1(&w, &b1);
    (w, b1, s1)
}

/// A world whose block 1 spends `seed`, or holds only its coinbase: the world, b1.
fn b1_world(
    seed: Option<u8>,
    setup: Setup,
    rig: impl FnOnce(RigConfig) -> RigConfig,
) -> (World, Mined) {
    if let Some(n) = seed {
        let (w, b1, _) = spent_with(n, setup, rig);
        return (w, b1);
    }
    let w = world_with(setup, rig);
    let b1 = block_on(&w.rig.p.clone(), &w.rig.genesis, vec![]);
    w.rig.accept(&b1);
    (w, b1)
}

/// s1, a coinbase-only block on genesis that ties b1, which stays the tip.
fn tie_b1(w: &World, b1: &Mined) -> Mined {
    let p = w.rig.p.clone();
    let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
    assert!(w.rig.submit(&s1).accepted);
    assert_eq!(tip(w), Some(b1.hash()), "s1 ties b1");
    s1
}

/// Whether V served the next command. A recorded enforcement it deferred runs first.
fn served_next(w: &World) -> Option<Result<(), WaitError>> {
    w.rig.v.query(|_| ()).recv_timeout(std::time::Duration::from_secs(120))
}

/// A lock on s1 recorded, and the enforcement V deferred then run: whether V served the next
/// command.
fn enforce_lock_on(
    w: &World,
    s1: &Mined,
    arm: impl FnOnce(&World),
) -> Option<Result<(), WaitError>> {
    use super::super::locks::{intake, lock, record};

    intake(&w.rig);
    arm(w);
    assert!(record(&w.rig, &lock(s1)));
    served_next(w)
}

/// An InstantSend world whose block 1 spends `seed`, or holds only its coinbase, with the
/// conflict in b1 and the lock's transaction known (`has_tx`): the world, b1.
fn conflict_world(seed: Option<u8>, setup: Setup, has_tx: bool) -> (World, Mined) {
    let conflict = ConflictIn {
        has_tx,
        ..ConflictIn::default()
    };
    let (w, b1) = b1_world(seed, setup, |cfg| RigConfig {
        is: Some(Box::new(conflict.clone())),
        ..cfg
    });
    *conflict.block.lock().expect("conflict") = Some(b1.hash());
    (w, b1)
}

/// `ResolveBlockConflicts` for the world's conflict: V's reply's error, if any.
fn resolve_conflict(w: &World) -> Option<WaitError> {
    let (lock, txid) = (Hash256::from_bendian([7; 32]), Hash256::from_bendian([8; 32]));
    w.rig.v.is_conflict_invalidate(lock, txid).recv().err()
}

/// BL-781 (#12), ChainLock enforcement: a lock on s1 marks the tied b1 conflicting, and
/// `MarkConflictingBlock`'s `DisconnectTip` of b1 reads a coin the cache does not hold, which
/// fails. Core's process aborts in the read (`coins.cpp:356-369` at v24.0.0), so neither
/// `DisconnectTip`'s line (`v:3293`) nor EnforceBlock's (`v:4076`) follows. V ends as at the
/// assert, with the store's one report.
#[test]
fn a_chainlock_mark_whose_disconnect_read_fails_logs_no_line() {
    install_accept_log();
    let setup = Setup {
        node_stop: true,
        ..Setup::default()
    };
    let (mut w, b1, s1) = tied_chainlock_world(Some(169), setup);
    let v = w.rig.v_thread();
    let served = enforce_lock_on(&w, &s1, fail_coins_reads);
    let exit = w.rig.stop();
    assert_eq!(served, Some(Err(WaitError::Disconnected)), "V stops in the mark");
    assert_no_assert_line_after_a_read(&w, v, &exit, &b1, MARK_FAILED, "a read in the mark");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
}

/// BL-781 (#12), InstantSend's conflict: `ResolveBlockConflicts` invalidates b1, whose disconnect
/// reads a coin the cache does not hold, which fails. Core's process aborts in the read
/// (`coins.cpp:356-369` at v24.0.0), so neither `DisconnectTip`'s line (`v:3293`) nor the
/// resolution's (`is:622`) follows. V ends as at the assert, with the store's one report.
#[test]
fn an_instantsend_conflicts_invalidation_whose_disconnect_read_fails_logs_no_line() {
    install_accept_log();
    let setup = Setup {
        node_stop: true,
        ..Setup::default()
    };
    let (mut w, b1) = conflict_world(Some(170), setup, false);
    let v = w.rig.v_thread();
    fail_coins_reads(&w);
    let replied = resolve_conflict(&w);
    let exit = w.rig.stop();
    assert_eq!(replied, Some(WaitError::Disconnected), "V stops in the invalidation");
    let case = "a read in the invalidation";
    assert_no_assert_line_after_a_read(&w, v, &exit, &b1, IS_INVALIDATE_FAILED, case);
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
    assert_eq!(evo_best(&w), Some(b1.hash()));
}

/// `DisconnectBlock`'s EvoDB check fails (H2, `v:2299-2302` at v24.0.0): with DIP3 active at b1,
/// the complete hooks' evo best block is not b1. Its `AbortNode` reports through the connector's
/// abort path, which trips the node latch, and the disconnect is a plain `DISCONNECT_FAILED`.
fn evodb_inconsistency() -> Setup {
    Setup {
        node_stop: true,
        steps: StepsCfg {
            complete: true,
            verify_best: false,
            ..StepsCfg::default()
        },
        disconnect_extra: Some(Deployment::Buried(BuriedDeployment::Dip0003)),
        ..Setup::default()
    }
}

/// BL-781 (R-2), ChainLock enforcement: a lock on s1 marks the tied b1 conflicting, and
/// `MarkConflictingBlock`'s `DisconnectTip` of b1 fails at the EvoDB check, whose `AbortNode`
/// (`v:2301` at v24.0.0) reports and trips the latch. The mark's `false` leaves `dummy_state`
/// valid (`chainlock/handler.cpp:289`), so EnforceBlock logs `Valid` (`v:4076`) after
/// `DisconnectTip`'s line (`v:3293`), then asserts: V logs the line though the latch has
/// tripped, and reports nothing more.
#[test]
fn a_chainlock_mark_failing_after_a_report_logs_its_valid_line() {
    install_accept_log();
    let (mut w, b1, s1) = tied_chainlock_world(Some(171), evodb_inconsistency());
    let v = w.rig.v_thread();
    let served = enforce_lock_on(&w, &s1, |_| {});
    let exit = w.rig.stop();
    assert_eq!(w.sink.reports(), [(EVODB_INCONSISTENCY.to_owned(), None)], "H2's report alone");
    let line = "Chainstate::EnforceBlock -- MarkConflictingBlock failed: Valid";
    assert_eq!(fatal_text(&exit), Some(line), "EnforceBlock's assert");
    assert!(exit.fatal.as_ref().is_some_and(|f| !f.is_abort_node()), "the assert's class");
    assert_eq!(served, Some(Err(WaitError::Disconnected)), "V stops in the mark");
    let case = "the mark after the EvoDB check's report";
    assert_valid_line_after_the_tip(v, &b1, EVODB_INCONSISTENCY, line, case);
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
}

/// BL-781 (R-2), InstantSend's conflict: `ResolveBlockConflicts` invalidates b1, whose disconnect
/// fails at the EvoDB check, whose `AbortNode` (`v:2301` at v24.0.0) reports and trips the latch.
/// `InvalidateBlock`'s `false` leaves the fresh state valid (`is:618`), so the resolution logs
/// `Valid` (`is:622`) after `DisconnectTip`'s line (`v:3293`), then asserts: V logs the line
/// though the latch has tripped, and reports nothing more.
#[test]
fn an_instantsend_conflicts_invalidation_failing_after_a_report_logs_its_valid_line() {
    install_accept_log();
    let (mut w, b1) = conflict_world(Some(172), evodb_inconsistency(), false);
    let v = w.rig.v_thread();
    let replied = resolve_conflict(&w);
    let exit = w.rig.stop();
    assert_eq!(w.sink.reports(), [(EVODB_INCONSISTENCY.to_owned(), None)], "H2's report alone");
    let line = "NetInstantSend::ResolveBlockConflicts -- InvalidateBlock failed: Valid";
    assert_eq!(fatal_text(&exit), Some(line), "the resolution's assert");
    assert!(exit.fatal.as_ref().is_some_and(|f| !f.is_abort_node()), "the assert's class");
    assert_eq!(replied, Some(WaitError::Disconnected), "V stops in the invalidation");
    let case = "the invalidation after the EvoDB check's report";
    assert_valid_line_after_the_tip(v, &b1, EVODB_INCONSISTENCY, line, case);
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
}

/// BL-781 (#12), `submitblock`: b2's `ConnectBlock` reads a coin the cache does not hold, its
/// coinbase's BIP30 probe (`v:2603-2614` at v24.0.0), which fails. Core's process aborts in the
/// read (`coins.cpp:356-369`), so neither `ConnectTip`'s line (`v:3442-3448`) nor
/// `ProcessNewBlock`'s (`v:4912`) follows. V stops with the store's one report.
#[test]
fn a_connect_whose_coins_read_fails_logs_no_caller_line() {
    install_accept_log();
    let (mut w, b1, _) = spent(173, Setup::default());
    let v = w.rig.v_thread();
    let b2 = block_on(&w.rig.p.clone(), &b1, vec![]);
    fail_coins_reads(&w);
    w.rig.submit_stopping(&b2, "a read in b2's connect");
    let exit = w.rig.stop();
    let callers = ["ConnectTip:", "ProcessNewBlock: ActivateBestChain failed"];
    assert_no_caller_line_after_a_read(&w, v, &exit, &callers, "a read in b2's connect");
    assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
}

/// BL-781 (#12), ChainLock enforcement: a lock on s1 marks the tied b1 conflicting, and the mark
/// disconnects b1 (its coinbase alone, so the cache holds every coin it reads). The same
/// enforcement then runs `EnforceBestChainLock`'s `ActivateBestChain` (`h:299-303` at v24.0.0),
/// which connects s1, whose `ConnectBlock` reads a coin the cache does not hold, which fails.
/// Core's process aborts in the read (`coins.cpp:356-369`), so neither `ConnectTip`'s line nor
/// `EnforceBestChainLock`'s (`h:303`) follows.
#[test]
fn a_chainlock_activation_whose_coins_read_fails_logs_no_caller_line() {
    install_accept_log();
    let (mut w, _, s1) = tied_chainlock_world(None, Setup::default());
    let v = w.rig.v_thread();
    // The activation's first release point may serve the first command before s1's connect.
    let stopped = match enforce_lock_on(&w, &s1, fail_coins_reads) {
        Some(Ok(())) => served_next(&w),
        served => served,
    };
    let exit = w.rig.stop();
    assert_eq!(stopped, Some(Err(WaitError::Disconnected)), "V stops in the activation");
    let genesis = Some(w.rig.genesis.hash());
    let at = exit.chain.private_tip().map(|(t, _)| t.hash);
    assert_eq!(at, genesis, "the mark disconnected b1, and s1 did not connect");
    let callers =
        ["ConnectTip:", "ChainlockHandler::EnforceBestChainLock -- ActivateBestChain", MARK_FAILED];
    assert_no_caller_line_after_a_read(&w, v, &exit, &callers, "a read in the activation");
}

/// BL-781 (#12), InstantSend's conflict with the lock's transaction known:
/// `ResolveBlockConflicts` invalidates b1 (its coinbase alone, so the cache holds every coin its
/// disconnect reads), and its `ActivateBestChain` (`is:634-641` at v24.0.0) then connects the tied
/// s1, whose `ConnectBlock` reads a coin the cache does not hold, which fails. Core's process
/// aborts in the read (`coins.cpp:356-369`), so neither `ConnectTip`'s line nor the resolution's
/// (`is:638`) follows; V ends as at the resolution's assert, with the store's one report.
#[test]
fn an_instantsend_activation_whose_coins_read_fails_logs_no_caller_line() {
    install_accept_log();
    let (mut w, b1) = conflict_world(None, Setup::default(), true);
    let v = w.rig.v_thread();
    tie_b1(&w, &b1);
    fail_coins_reads(&w);
    let replied = resolve_conflict(&w);
    let exit = w.rig.stop();
    assert_eq!(replied, Some(WaitError::Disconnected), "V stops in the activation");
    let callers = [
        "ConnectTip:",
        "NetInstantSend::ResolveBlockConflicts -- ActivateBestChain",
        IS_INVALIDATE_FAILED,
    ];
    assert_no_caller_line_after_a_read(&w, v, &exit, &callers, "a read in the activation");
    assert!(exit.fatal.as_ref().is_some_and(|f| f.is_reported_assert()), "the assert's class");
}

/// `ApplyTxInUndo`'s legacy entry (`v:2259-2266`): an undo coin of height 0 takes the height and
/// coinbase flag of another unspent output of its transaction (`AccessByTxid`), and with none the
/// disconnect is `DISCONNECT_FAILED`, which only `DisconnectTip` logs.
#[test]
fn a_legacy_undo_entry_takes_its_transactions_metadata() {
    install_accept_log();
    for (n, sibling) in [(60, true), (61, false)] {
        // Height 0: the spend's undo entry is in the legacy format.
        let mut seeds = vec![seed(n, 50 * COIN, &[OP_TRUE])];
        let other = OutPoint::new(seed_op(n).txid(), 1);
        if sibling {
            seeds.push((other, out(COIN, &[OP_TRUE]), 7, true));
        }
        let w = world(Setup {
            seeds,
            ..Setup::default()
        });
        let t1 = spend(&[seed_op(n)], 49 * COIN);
        let b1 = block_on(&w.rig.p.clone(), &w.rig.genesis, vec![t1]);
        w.rig.accept(&b1);
        invalidate(&w, &b1);
        let restored = snapshot(&w, &[seed_op(n)]).1.remove(0);
        if sibling {
            assert_eq!(tip(&w), Some(w.rig.genesis.hash()));
            assert_eq!(restored, Some(Coin::new(50 * COIN, &[OP_TRUE], 7, true)));
            assert_eq!(bracket(&w), DISCONNECTED);
        } else {
            assert_eq!(tip(&w), Some(b1.hash()));
            assert_eq!(restored, None);
            assert_eq!(bracket(&w), NOT_CONNECTED);
            assert_eq!(evo_steps(&w), TIP_STEPS[..2]);
        }
        assert_eq!(
            tip_failed_lines(w.rig.v_thread(), &b1),
            usize::from(!sibling),
            "sibling {sibling}"
        );
        assert!(w.sink.messages().is_empty(), "sibling {sibling}: {:?}", w.sink.messages());
    }
}

/// CH3 §7.5's legacy-undo reorg: a heavier branch from genesis disconnects a block whose spend
/// has a legacy undo entry. With a sibling output the entry takes its metadata, as in
/// [`a_legacy_undo_entry_takes_its_transactions_metadata`], and the reorg completes; with none
/// the disconnect fails, and in a reorg `ActivateBestChainStep` aborts (`v:3621`). The expected
/// coins are that connector fixture's, not Core's: no Core chain has a legacy entry after import
/// (CH2 §3.5.2). The branch's `debug_assert` is compiled out of this crate's test builds only.
#[test]
fn a_reorg_over_a_legacy_undo_entry() {
    install_accept_log();
    for (n, sibling) in [(70, true), (71, false)] {
        let mut seeds = vec![seed(n, 50 * COIN, &[OP_TRUE])];
        let other = OutPoint::new(seed_op(n).txid(), 1);
        if sibling {
            seeds.push((other, out(COIN, &[OP_TRUE]), 7, true));
        }
        let mut w = world(Setup {
            seeds,
            ..Setup::default()
        });
        let p = w.rig.p.clone();
        let t1 = spend(&[seed_op(n)], 49 * COIN);
        let t1_out = op(&t1, 0);
        let b1 = block_on(&p, &w.rig.genesis, vec![t1]);
        w.rig.accept(&b1);
        let s1 = with_coinbase(&p, &w.rig.genesis, coinbase_tx(1, 1, 0, COIN - 1), vec![]);
        let s2 = block_on(&p, &s1, vec![]);
        assert!(w.rig.submit(&s1).accepted, "sibling {sibling}");
        let v = w.rig.v_thread();
        if sibling {
            w.rig.accept(&s2);
            w.rig.sync();
            assert_eq!(tip(&w), Some(s2.hash()));
            // The spent coin is back and b1's output is gone.
            assert_eq!(
                snapshot(&w, &[seed_op(n), other, t1_out]),
                (
                    Some(s2.hash()),
                    vec![
                        Some(Coin::new(50 * COIN, &[OP_TRUE], 7, true)),
                        Some(Coin::new(COIN, &[OP_TRUE], 7, true)),
                        None
                    ]
                )
            );
            assert!(w.sink.messages().is_empty(), "{:?}", w.sink.messages());
        } else {
            w.rig.submit_stopping(&s2, "no sibling");
            let exit = w.rig.stop();
            assert_eq!(fatal_text(&exit), Some(DISCONNECT_FAILED));
            assert_eq!(w.sink.messages(), [DISCONNECT_FAILED], "one report");
        }
        assert_eq!(tip_failed_lines(v, &b1), usize::from(!sibling), "sibling {sibling}");
    }
}

/// Inputs are restored last to first (`v:2346-2352`), which only the legacy entry shows: a
/// transaction spending a legacy entry, then its sibling, gets the sibling back first, so the
/// entry takes the sibling's metadata. First to last, the entry would find no sibling and fail.
#[test]
fn inputs_are_restored_last_to_first() {
    install_accept_log();
    let other = OutPoint::new(seed_op(62).txid(), 1);
    let w = world(Setup {
        seeds: vec![seed(62, 50 * COIN, &[OP_TRUE]), (other, out(COIN, &[OP_TRUE]), 7, false)],
        ..Setup::default()
    });
    let t1 = spend(&[seed_op(62), other], 50 * COIN);
    let b1 = block_on(&w.rig.p.clone(), &w.rig.genesis, vec![t1]);
    w.rig.accept(&b1);
    invalidate(&w, &b1);
    assert_eq!(tip(&w), Some(w.rig.genesis.hash()));
    assert_eq!(
        snapshot(&w, &[seed_op(62), other]).1,
        [
            Some(Coin::new(50 * COIN, &[OP_TRUE], 7, false)),
            Some(Coin::new(COIN, &[OP_TRUE], 7, false))
        ]
    );
    assert_eq!(bracket(&w), DISCONNECTED);
    assert_eq!(tip_failed_lines(w.rig.v_thread(), &b1), 0);
    assert!(w.sink.messages().is_empty(), "{:?}", w.sink.messages());
}
