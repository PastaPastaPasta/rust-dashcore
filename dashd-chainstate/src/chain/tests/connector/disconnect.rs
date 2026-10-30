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
use dashd_store::flatfile::undo_checksum;

use super::*;
use crate::chain::seams::HeldChainLocks;
use crate::chain::{EvoTxn, ScratchCoins, ScratchView};
use crate::coins::{PackedCoin, PrefetchBatch};
use crate::connect::UNDO_READ_FAILED;

/// `ActivateBestChainStep`'s `AbortNode` after a failed `DisconnectTip` (`v:3621`).
const STEP_FAILED: &str = "Failed to disconnect block; see debug.log for details";

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
pub(super) fn spent(n: u8, mut setup: Setup) -> (World, Mined, Arc<Tx>) {
    setup.seeds.push(seed1(n, 50 * COIN));
    let w = world(setup);
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
    levels_on(v, &format!("DisconnectTip(): DisconnectBlock {} failed", m.hash())).len()
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
        let chainlocks = HeldChainLocks::new(&chain, -1);
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
/// their prefetch, the tip or the evo store, and neither runs H1, which is `DisconnectTip`'s
/// (`v:3272-3273`, `:3320`).
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
        assert_eq!(evo_steps(&w), [EvoStep::UndoSpecial, EvoStep::ListChanged(true)]);
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
    assert_eq!(evo_steps(&w), []);
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
    const TX_SIZES: &str = "DisconnectBlock(): transaction and undo data inconsistent";
    const H9: &str = "DisconnectBlock(): UndoSpecialTxsInBlock failed";
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
        (38, None, false, vec![H9.to_owned()], steps),
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
    assert_eq!(fatal_text(&exit), Some(STEP_FAILED));
    assert_eq!(w.sink.messages(), [STEP_FAILED], "one report");
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
        assert_eq!(fatal_text(&exit), Some(STEP_FAILED), "complete {complete}");
        assert_eq!(
            w.sink.reports(),
            [first, (STEP_FAILED.to_owned(), None)],
            "complete {complete}"
        );
        assert_eq!(exit.chain.private_tip().map(|(t, _)| t.hash), Some(b1.hash()));
        assert_eq!(coins_at(&w.coins), Some(b1.hash()), "complete {complete}");
        assert_eq!(bracket(&w), NOT_CONNECTED, "complete {complete}");
        assert_eq!(evo_steps(&w), [EvoStep::BlsCtx], "complete {complete}");
        assert_eq!(evo_best(&w), Some(b1.hash()), "complete {complete}");
    }
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
        let other = OutPoint {
            hash: seed_op(n).hash,
            index: 1,
        };
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
        let other = OutPoint {
            hash: seed_op(n).hash,
            index: 1,
        };
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
            assert_eq!(fatal_text(&exit), Some(STEP_FAILED));
            assert_eq!(w.sink.messages(), [STEP_FAILED], "one report");
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
    let other = OutPoint {
        hash: seed_op(62).hash,
        index: 1,
    };
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
