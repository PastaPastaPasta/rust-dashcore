//! CH4-6 (DEC-835; PLAN-CH4 §CH4-6): E's order under the read-ahead. The `ValidationInterface`
//! calls V makes inline, and those E runs from its queue, each in their order, are the same with
//! the read-ahead off, settled and free-running, over IBD-shaped arrivals and reorgs, with W
//! releasing each flush at once so that E runs `ChainStateFlushed` too; off, they are CH3b's,
//! which its tests pin to Core's. With E held, `LimitValidationInterfaceQueue` (`v:3766`) stops
//! V at the same block with the read-ahead as without it: the reader reads on, but V connects
//! no further ahead of E.

use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::chain::read_ahead::{Counts, DEPTH, Probe};
use crate::chain::tests::flush::ModelW;
use crate::chain::tests::support::branch;
use crate::chain::{ChainLockSig, NewBlock, Queued};

/// The chain's length: more than the read-ahead's depth.
const N: u32 = 100;
const _: () = assert!(N > DEPTH);
/// IBD's download window: blocks arrive a window at a time.
const WINDOW: usize = 16;
/// A bound on a wait that should end at once: a regression fails instead of hanging.
const SOON: Duration = Duration::from_secs(120);

/// Every call V makes on a listener: inline on V's thread, and from E's queue, each in its
/// order. E's calls stop at [`Order::holding`]'s marker until it is let go.
#[derive(Default)]
pub(super) struct Order {
    sync: Mutex<Vec<String>>,
    queued: Mutex<Vec<String>>,
    hold: Option<Hold>,
}

/// Where E stops: at the queued `UpdatedBlockTip` of `at`, which it signals on `entered`
/// before it waits for `go`.
struct Hold {
    at: BlockId,
    entered: mpsc::Sender<()>,
    go: Mutex<mpsc::Receiver<()>>,
}

impl Order {
    /// An [`Order`] whose E stops at the queued `UpdatedBlockTip` of `at`: the receiver hears
    /// that it has, and the sender (or its drop) lets it go on.
    fn holding(at: BlockId) -> (Order, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered, held) = mpsc::channel();
        let (go, gate) = mpsc::channel();
        let hold = Hold {
            at,
            entered,
            go: Mutex::new(gate),
        };
        let order = Order {
            hold: Some(hold),
            ..Order::default()
        };
        (order, held, go)
    }

    /// The calls so far: V's inline ones, and E's.
    pub(super) fn take(&self) -> (Vec<String>, Vec<String>) {
        let take = |m: &Mutex<Vec<String>>| std::mem::take(&mut *m.lock().expect("order"));
        (take(&self.sync), take(&self.queued))
    }

    fn sync(&self, e: String) {
        self.sync.lock().expect("order").push(e);
    }

    fn queued(&self, e: String) {
        self.queued.lock().expect("order").push(e);
    }
}

impl ValidationInterface for Order {
    fn block_checked(&self, block: &Block, state: &BlockState, ibd: bool) {
        self.sync(format!("checked {} {state:?} {ibd}", block.hash()));
    }

    fn new_pow_valid_block(&self, nb: &NewBlock<'_>) {
        self.sync(format!("new pow valid {:?} {}", nb.id, nb.block.hash()));
    }

    fn accepted_block_header(&self, id: BlockId) {
        self.sync(format!("header {id:?}"));
    }

    fn notify_header_tip(&self, id: BlockId, ibd: bool) {
        self.sync(format!("header tip {id:?} {ibd}"));
    }

    fn synchronous_updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        self.sync(format!("tip {new:?} {fork:?} {ibd}"));
    }

    fn initialize_current_block_tip(&self, tip: Option<BlockId>, ibd: bool) {
        self.sync(format!("initial tip {tip:?} {ibd}"));
    }

    fn updated_block_tip(&self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        self.queued(format!("tip {new:?} {fork:?} {ibd}"));
        if let Some(h) = self.hold.as_ref().filter(|h| h.at == new) {
            let _ = h.entered.send(());
            let _ = h.go.lock().expect("hold").recv();
        }
    }

    fn block_connected(&self, block: &Arc<Block>, id: BlockId) {
        self.queued(format!("connected {id:?} {}", block.hash()));
    }

    fn block_disconnected(&self, block: &Arc<Block>, id: BlockId) {
        self.queued(format!("disconnected {id:?} {}", block.hash()));
    }

    fn chain_state_flushed(&self, locator: &[BlockHash]) {
        self.queued(format!("flushed {:?}", locator.first()));
    }

    fn notify_chain_lock(&self, id: BlockId, _: &Arc<ChainLockSig>) {
        self.queued(format!("chainlock {id:?}"));
    }
}

/// What a run left, which must not depend on the read-ahead.
struct Calls {
    sync: Vec<String>,
    queued: Vec<String>,
    reports: Vec<String>,
    coins: Option<BlockHash>,
}

impl Calls {
    /// `w`'s stop, then its calls: the signals thread runs the queue out as the rig drops.
    fn finish(mut w: World, order: &Order) -> Calls {
        w.rig.stop();
        let reports = w.sink.messages();
        let coins = coins_at(&w.coins);
        drop(w);
        let (sync, queued) = order.take();
        Calls {
            sync,
            queued,
            reports,
            coins,
        }
    }

    /// `self` is `want`, field by field.
    fn assert_same(&self, want: &Calls, mode: &str) {
        assert_eq!(self.sync, want.sync, "{mode}: V's inline calls");
        assert_eq!(self.queued, want.queued, "{mode}: E's calls");
        assert_eq!(self.reports, want.reports, "{mode}: the reports");
        assert_eq!(self.coins, want.coins, "{mode}: the coins' block");
    }
}

/// Blocks 1–[`N`] on genesis, each spending its parent's output (block 1 seed 1's).
fn spends(p: &ChainSetup) -> Vec<Mined> {
    let (mut parent, mut prev) = (Mined::genesis(p), seed_op(1));
    let mut blocks = Vec::new();
    for h in 1..=N {
        let t = spend(&[prev], 50 * COIN - Amount::from(h) * 1_000);
        prev = op(&t, 0);
        let m = block_on(p, &parent, vec![t]);
        parent = m.clone();
        blocks.push(m);
    }
    blocks
}

/// A node at genesis with seed 1, which [`spends`] spends, and `order` and `read_ahead`.
fn setup(p: &Arc<ChainSetup>, order: &Arc<Order>, read_ahead: ReadAheadSetup) -> Setup {
    Setup {
        params: Some(p.clone()),
        seeds: vec![seed(1, 50 * COIN, &[OP_TRUE])],
        listener: Some(order.clone()),
        read_ahead,
        ..Setup::default()
    }
}

/// IBD's arrivals: the headers first, then each [`WINDOW`] of blocks with its first block last,
/// so that block lets V connect the window, reading the rest from disk.
fn ibd(rig: &Rig, blocks: &[Mined]) {
    rig.headers(blocks);
    for w in blocks.chunks(WINDOW) {
        for m in w[1..].iter().rev() {
            rig.accept(m);
        }
        let r = rig.submit(&w[0]);
        assert!(r.accepted, "{r:?}");
    }
}

/// `arrivals`, each IBD's way, on a node with `read_ahead` and a W that releases each flush at
/// once, then its stop.
fn run(p: &Arc<ChainSetup>, read_ahead: ReadAheadSetup, arrivals: &[&[Mined]]) -> Calls {
    let order = Arc::new(Order::default());
    let w = world(Setup {
        flush: Some(ModelW::releasing().site()),
        ..setup(p, &order, read_ahead)
    });
    for blocks in arrivals {
        ibd(&w.rig, blocks);
    }
    Calls::finish(w, &order)
}

/// The run with the read-ahead off, then each read-ahead mode's, which must be the same: the
/// off run, and the settled run's reader counts, which xcheck does not change.
fn parity(p: &Arc<ChainSetup>, arrivals: &[&[Mined]]) -> (Calls, Counts) {
    let off = run(p, ReadAheadSetup::off(), arrivals);
    let settled = |xcheck| {
        let probe = Probe::settled();
        let got = run(p, ReadAheadSetup::threaded(xcheck).probed(&probe), arrivals);
        got.assert_same(&off, &format!("settled, xcheck {xcheck}"));
        probe.counts()
    };
    let counts = settled(false);
    assert_eq!(settled(true), counts, "xcheck changes no read");
    for xcheck in [false, true] {
        let got = run(p, ReadAheadSetup::threaded(xcheck), arrivals);
        got.assert_same(&off, &format!("free-running, xcheck {xcheck}"));
    }
    (off, counts)
}

/// E's connects and disconnects, by block.
fn moves(calls: &Calls) -> Vec<String> {
    let block = |e: &String| {
        let (what, rest) = e.split_once(' ')?;
        let hash = rest.rsplit(' ').next()?;
        matches!(what, "connected" | "disconnected").then(|| format!("{what} {hash}"))
    };
    calls.queued.iter().filter_map(block).collect()
}

fn connects(blocks: &[Mined]) -> Vec<String> {
    blocks.iter().map(|m| format!("connected {}", m.hash())).collect()
}

/// E's moves from init's ABC, which connects genesis, through `blocks`' connects.
fn from_genesis(p: &ChainSetup, blocks: &[Mined]) -> Vec<String> {
    let mut moves = connects(&[Mined::genesis(p)]);
    moves.extend(connects(blocks));
    moves
}

fn disconnects(blocks: &[Mined]) -> Vec<String> {
    blocks.iter().rev().map(|m| format!("disconnected {}", m.hash())).collect()
}

/// IBD: windows of blocks, each connected once its first block arrives.
#[test]
fn ibd_arrivals_keep_es_order() {
    let p = regtest();
    let blocks = spends(&p);
    let (off, c) = parity(&p, &[&blocks]);
    // Genesis and every block came from the reader.
    assert_eq!((c.reads, c.hits, c.fallbacks, c.epochs), (N + 1, N + 1, 0, 0), "{c:?}");
    assert_eq!(moves(&off), from_genesis(&p, &blocks));
    assert!(off.queued.iter().any(|e| e.starts_with("flushed")), "{:?}", off.queued);
    assert_eq!(off.coins, Some(blocks[N as usize - 1].hash()));
}

/// A heavier branch off block `N − 20`, arriving IBD's way: V disconnects 20 blocks and connects
/// the branch's 25, read ahead after the disconnects.
#[test]
fn a_reorg_keeps_es_order() {
    let p = regtest();
    let blocks = spends(&p);
    let fork = N as usize - 20;
    let side = branch(&p, &blocks[fork - 1], 25, 0);
    let (off, c) = parity(&p, &[&blocks, &side]);
    // The branch came from the reader too, after an epoch for each disconnect.
    assert_eq!((c.reads, c.hits, c.fallbacks, c.epochs), (N + 26, N + 26, 0, 20), "{c:?}");
    let want = [from_genesis(&p, &blocks), disconnects(&blocks[fork..]), connects(&side)];
    assert_eq!(moves(&off), want.concat());
    assert_eq!(off.coins, Some(side[24].hash()));
}

/// A heavier branch off block `N − 20` whose tenth block spends a coin that does not exist: V
/// disconnects 20 blocks, connects nine, finds the tenth invalid (`InvalidChainFound`) and goes
/// back to the old chain.
#[test]
fn a_reorg_into_an_invalid_block_and_back_keeps_es_order() {
    let p = regtest();
    let blocks = spends(&p);
    let fork = N as usize - 20;
    let mut side = branch(&p, &blocks[fork - 1], 9, 0);
    let bad = block_on(&p, &side[8], vec![spend(&[seed_op(9)], COIN)]);
    side.push(bad.clone());
    side.extend(branch(&p, &bad, 20, 0));
    let (off, c) = parity(&p, &[&blocks, &side]);
    // The reader read the whole branch, and V took its first ten and the old chain's 20 back,
    // after an epoch for each disconnect and the invalid block.
    assert_eq!((c.reads, c.hits, c.fallbacks, c.epochs), (N + 51, N + 31, 0, 30), "{c:?}");
    let want = [
        from_genesis(&p, &blocks),
        disconnects(&blocks[fork..]),
        connects(&side[..9]),
        disconnects(&side[..9]),
        connects(&blocks[fork..]),
    ];
    assert_eq!(moves(&off), want.concat());
    let invalid = format!("checked {} Invalid", bad.hash());
    assert!(off.sync.iter().any(|e| e.starts_with(&invalid)), "{:?}", off.sync);
    assert_eq!(off.coins, Some(blocks[N as usize - 1].hash()));
}

/// V's tip height once `limit_queue` holds it. V serves queries where each ABC cycle starts and
/// in the limit's wait (`v:3766`); with E held, each cycle connects one block between them, so
/// two queries in a row that see the same tip find V waiting at the limit. A V that never waits
/// connects all [`N`] blocks, and two queries then see block `N`.
fn held_tip(rig: &Rig) -> u32 {
    let tip = || {
        rig.v
            .query(|q| q.chain.private_tip().map_or(0, |(t, _)| t.height))
            .recv_timeout(SOON)
            .expect("V serves in time")
            .expect("V serves")
    };
    let mut last = tip();
    loop {
        let now = tip();
        if now == last {
            return now;
        }
        last = now;
    }
}

/// The tip of the markers E is held at, which no block has.
const HELD: u32 = 1_000_000;

/// The read-ahead does not let V outrun E. E is held at a marker of the test's with a second
/// queued behind it, so one callback is pending when block 1 arrives. Each block V connects
/// queues `BlockConnected` and `UpdatedBlockTip`, and V waits once more than 10 are pending
/// (`v:3766`, `validationinterface.cpp`'s `CallbacksPending`): it connects blocks 1 to 5 and
/// waits before block 6, with 11 pending and its barrier. The read-ahead changes neither, and
/// the settled reader has read past block 5.
#[test]
fn limit_queue_holds_v_at_the_same_block_with_the_read_ahead() {
    let p = regtest();
    let blocks = spends(&p);
    let settled = Probe::settled();
    let modes = [
        ("off", ReadAheadSetup::off(), None),
        ("settled", ReadAheadSetup::threaded(false).probed(&settled), Some(&settled)),
        ("free-running", ReadAheadSetup::threaded(false), None),
    ];
    let mut runs = Vec::new();
    for (mode, read_ahead, probe) in modes {
        let (order, entered, go) = Order::holding(BlockId(HELD));
        let order = Arc::new(order);
        // `go` drops before `w`: a panic lets E go on before the rig's drop waits for it.
        let (w, go) = (world(setup(&p, &order, read_ahead)), go);
        w.rig.headers(&blocks);
        for m in blocks[1..].iter().rev() {
            w.rig.accept(m);
        }
        w.rig.sync();
        for i in 0..2 {
            w.rig.signals.enqueue(Queued::UpdatedBlockTip {
                new: BlockId(HELD + i),
                fork: None,
                ibd: false,
            });
        }
        entered.recv_timeout(SOON).expect("E holds");
        // V replies once the ABC that `limit_queue` holds has run out.
        let connected = w.rig.v.process_block(blocks[0].block.clone(), true, None, true);
        let held = held_tip(&w.rig);
        let pending = w.rig.signals.callbacks_pending();
        let read = probe.map(|p| p.counts());
        // E goes on, and V's ABC with it.
        drop(go);
        assert_eq!((held, pending), (5, 12), "{mode}: V's tip and the pending callbacks");
        if let Some(c) = read {
            // V took genesis and blocks 1 to 5 from the reader, which had read `DEPTH` blocks
            // past block 4 before V took block 5.
            assert_eq!(c.hits, held + 1, "{mode}: {c:?}");
            assert!(c.reads >= held + DEPTH, "{mode}: {c:?}");
        }
        let r = connected.recv_timeout(SOON).expect("V replies in time").expect("V replies");
        assert!(r.accepted, "{mode}: {r:?}");
        let calls = Calls::finish(w, &order);
        assert_eq!(calls.coins, Some(blocks[N as usize - 1].hash()), "{mode}");
        runs.push((mode, calls));
    }
    let (_, off) = &runs[0];
    for (mode, calls) in &runs[1..] {
        calls.assert_same(off, mode);
    }
}
