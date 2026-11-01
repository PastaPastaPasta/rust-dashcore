//! The completion witnesses (CH2 §7.1 a2, G-R3-M3): blocks far past the budget complete on the
//! presized fallback, and the tagging allocator equals native Σ − R after every op (§6.7). Each
//! prints its numbers for the a2 report: native Σ at the block boundary, its peak over the op
//! boundaries, the allocator's peak inside the ops plus R, the shard doublings and
//! `boundary_state` after the block. Core's meter for the same blocks is reported beside them from
//! a probe on the pinned Core headers; nothing here compares with it.

use std::sync::{Arc, Mutex};

use dash_consensus::coins::{Coin, CoinsView as _};

use super::overlay::{long_input, spending};
use super::*;
use crate::coins::hasher::SaltedHasher;
use crate::coins::{BlockOverlay, CoinSrc, CoinsBoundary, FrozenLayer};

/// The DEC-43 coins share.
const B_DEC43: u64 = 265_027_584;

/// GPT's long-input block: one transaction spending as many [`long_input`] coins as fit 2 MB, at
/// 41 bytes an input with an empty `scriptSig`.
const LONG_INPUTS: u32 = 48_700;

/// The long-input witnesses hold ≈ 1.4 GB each: one at a time.
static LARGE: Mutex<()> = Mutex::new(());

/// One witness's allocator domain and peaks.
struct Meter {
    tagged: Tagged,
    /// Native Σ's peak over the op boundaries.
    sigma_peak: u64,
    ops: u64,
}

impl Meter {
    /// Starts counting on this thread: before the world, so its first layer counts.
    fn start() -> Self {
        let tagged = Tagged::start();
        tagged.reset_peak();
        Self {
            tagged,
            sigma_peak: 0,
            ops: 0,
        }
    }

    /// After an op: the allocator equals native Σ − R.
    #[track_caller]
    fn op(&mut self, u: Usage, what: &str) {
        self.ops += 1;
        self.tagged.check(u, format_args!("after {what} (op {})", self.ops));
        self.sigma_peak = self.sigma_peak.max(u.total());
    }

    fn report(&self, name: &str, w: &World, in_flight: bool) {
        let u = w.cache.usage();
        println!(
            "witness {name}: B = {}, R = {}, sigma at the boundary = {}, peak sigma at op \
             boundaries = {}, allocator peak + R = {}, active doublings = {}, boundary_state = \
             {:?}, ops = {}",
            w.cache.budget().coins_share(),
            u.reserve,
            u.total(),
            self.sigma_peak,
            self.tagged.peak() + u.reserve,
            doublings(w.cache.active()),
            w.cache.boundary_state(0, in_flight),
            self.ops,
        );
    }
}

/// How many times the layer's shards have doubled past their ceiling, summed.
fn doublings(layer: &ShardedLayer) -> u32 {
    let past = |(i, (_, b)): (usize, (usize, usize))| (b / layer.sizes().buckets(i)).max(1).ilog2();
    layer.shard_loads().enumerate().map(past).sum()
}

/// A world at coins share `b`, no mempool allowance, f = 1/2 and R at the default batch size.
fn world(b: u64) -> World {
    World::new(CoinsBudget::new(b, 0, Ratio::HALF, batch_reserve(b)))
}

/// `n` outputs of one transaction.
fn outputs(r: &mut Rng, n: u32) -> Vec<OutPointKey> {
    let txid = random_key(r, 0);
    (0..n)
        .map(|vout| {
            let mut k = txid;
            k.0[32..].copy_from_slice(&vout.to_le_bytes());
            k
        })
        .collect()
}

/// Connects a block of `n` inline P2PKH outputs through an overlay, metering every op.
fn outputs_block(w: &mut World, m: &mut Meter, r: &mut Rng, height: u32, n: u32) {
    let keys = outputs(r, n);
    let mut overlay = BlockOverlay::new(&mut w.cache);
    m.op(overlay.usage(), "open");
    for (i, k) in keys.iter().enumerate() {
        overlay.add(k, CoinSrc::Bytes(p2pkh(i as u64).as_ref()), false).unwrap();
        m.op(overlay.usage(), "add");
    }
    overlay.commit(block(height)).unwrap();
    m.op(w.cache.usage(), "commit");
}

/// A DEC-43 world with F in flight: blocks of 20,000 outputs until THRESHOLD, then the freeze. The
/// fill is metered at its block boundaries only.
fn dec43_with_f_in_flight(m: &mut Meter, r: &mut Rng) -> (World, Arc<FrozenLayer>) {
    let mut w = world(B_DEC43);
    let mut height = 0;
    while w.cache.boundary_state(0, false) == CoinsBoundary::Ok {
        height += 1;
        let mut overlay = BlockOverlay::new(&mut w.cache);
        for (i, k) in outputs(r, 20_000).iter().enumerate() {
            overlay.add(k, CoinSrc::Bytes(p2pkh(i as u64).as_ref()), false).unwrap();
        }
        overlay.commit(block(height)).unwrap();
        m.op(w.cache.usage(), "fill");
    }
    assert_eq!(w.cache.boundary_state(0, false), CoinsBoundary::Threshold);
    let frozen = w.cache.freeze_at(height, 0);
    m.op(w.cache.usage(), "freeze");
    assert_eq!(w.cache.usage().reserve, batch_reserve(B_DEC43));
    (w, frozen)
}

/// Writes F as W would, drops W's handle and releases it.
fn release(w: &mut World, m: &mut Meter, frozen: Arc<FrozenLayer>) {
    w.release(frozen);
    m.op(w.cache.usage(), "release");
}

/// Witness (ii)'s block: [`LONG_INPUTS`] long coins in the database, spent by one transaction, and
/// the block disconnected again from its undo. The undo is written and dropped before the block's
/// boundary is reported, as Core's `ConnectBlock` writes its `CBlockUndo` and drops it before
/// `ConnectTip`'s `FlushStateToDisk` (`validation.cpp:2641`, `:2888`, `:3457`; O3); the disconnect
/// reads it back as plain coins. With `frozen`, F stays in flight throughout; without, the cache
/// flushes after the connect, as a CRITICAL boundary would. Every restored coin is the original, in
/// the cache and then in the database.
fn long_input_block(w: &mut World, m: &mut Meter, r: &mut Rng, frozen: Option<&FrozenLayer>) {
    let keys: Vec<_> = (0..LONG_INPUTS).map(|i| random_key(r, i)).collect();
    {
        let table = CoinsTable::default();
        let mut b = w.db.batch();
        for (i, k) in keys.iter().enumerate() {
            table.put(&mut b, k, long_input(i as u64).as_bytes());
        }
        w.db.write(b, Commit::Buffered).unwrap();
    }
    m.op(w.cache.usage(), "store");
    let tx = spending(&keys);
    let out = random_key(r, 0);
    let undo: Vec<PackedCoin> = {
        let mut overlay = BlockOverlay::new(&mut w.cache);
        assert!(overlay.have_inputs(&tx).unwrap());
        m.op(overlay.usage(), "have_inputs");
        assert!(overlay.child().is_empty(), "no child copy after have_inputs");
        let undo: Vec<_> = keys
            .iter()
            .map(|k| {
                let coin = overlay.spend(k).unwrap().unwrap();
                m.op(overlay.usage(), "spend");
                coin
            })
            .collect();
        assert_eq!(overlay.usage().held_out, u64::from(LONG_INPUTS) * 9_583);
        overlay.add(&out, CoinSrc::Bytes(coin(5).as_ref()), false).unwrap();
        m.op(overlay.usage(), "add");
        overlay.commit(block(1_000)).unwrap();
        undo
    };
    m.op(w.cache.usage(), "commit");
    let undo: Vec<Coin> = undo.into_iter().map(|c| c.as_ref().to_coin()).collect();
    assert_eq!(w.cache.usage().held_out, 0);
    m.op(w.cache.usage(), "undo written");
    let in_flight = frozen.is_some();
    m.report(
        if in_flight {
            "(iii) long-input connect"
        } else {
            "(ii) long-input connect"
        },
        w,
        in_flight,
    );
    if !in_flight {
        let f = w.cache.freeze_at(1_000, 0);
        m.op(w.cache.usage(), "freeze");
        release(w, m, f);
    }
    {
        // DisconnectBlock: the output spent, then each input restored (ApplyTxInUndo), in reverse.
        let mut overlay = BlockOverlay::new(&mut w.cache);
        assert!(overlay.spend(&out).unwrap().is_some());
        m.op(overlay.usage(), "spend");
        for (k, u) in keys.iter().zip(&undo).rev() {
            let clean = !overlay.have_coin(&k.outpoint()).unwrap();
            assert!(clean);
            overlay.add(k, CoinSrc::Bytes(u.as_ref()), !clean).unwrap();
            m.op(overlay.usage(), "add");
        }
        overlay.commit(block(999)).unwrap();
    }
    m.op(w.cache.usage(), "commit");
    drop(undo);
    w.cache.sanity_check().unwrap();
    // The disconnect's boundary, before any flush: `DisconnectTip`'s `FlushStateToDisk` (`:3304`).
    m.report(
        if in_flight {
            "(iii) after the disconnect"
        } else {
            "(ii) after the disconnect"
        },
        w,
        in_flight,
    );
    for (i, k) in keys.iter().enumerate() {
        let got = w.cache.fetch(k).unwrap().map(|c| c.to_coin());
        assert_eq!(got, Some(long_input(i as u64).as_ref().to_coin()), "input {i} restored");
    }
    m.op(w.cache.usage(), "fetches");
    if !in_flight {
        let f = w.cache.freeze_at(999, 0);
        m.op(w.cache.usage(), "freeze");
        release(w, m, f);
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(w.db_get(k).as_ref(), Some(&long_input(i as u64)), "input {i} stored");
        }
    }
}

/// (i) A 50,000-output block at `-dbcache=4` (B = 1.75 MiB): every shard grows past the presize
/// and the block completes.
#[test]
fn witness_i_a_50000_output_block_at_dbcache_4() {
    let mut m = Meter::start();
    let mut w = world(B_4);
    outputs_block(&mut w, &mut m, &mut Rng::new(41), 1, 50_000);
    w.cache.sanity_check().unwrap();
    assert_eq!(w.cache.active().len(), 50_000);
    assert!(doublings(w.cache.active()) >= 64, "every shard grew");
    m.report("(i) 50,000 outputs", &w, false);
}

/// (ii) GPT's long-input connect at `-dbcache=4`, then its disconnect restoring every coin.
#[test]
fn witness_ii_a_long_input_connect_and_disconnect_at_dbcache_4() {
    let _one = LARGE.lock().unwrap_or_else(|e| e.into_inner());
    let mut m = Meter::start();
    let mut w = world(B_4);
    long_input_block(&mut w, &mut m, &mut Rng::new(42), None);
}

/// (iii) Witness (i) at DEC-43 with F in flight: R is in Σ, and the allocator still equals Σ − R.
#[test]
fn witness_iii_a_50000_output_block_at_dec43_with_f_in_flight() {
    let mut m = Meter::start();
    let mut r = Rng::new(43);
    let (mut w, _frozen) = dec43_with_f_in_flight(&mut m, &mut r);
    m.tagged.reset_peak();
    m.sigma_peak = 0;
    outputs_block(&mut w, &mut m, &mut r, 1_000, 50_000);
    w.cache.sanity_check().unwrap();
    m.report("(iii) 50,000 outputs", &w, true);
}

/// (iii) Witness (ii) at DEC-43 with F in flight throughout.
#[test]
fn witness_iii_a_long_input_connect_and_disconnect_at_dec43_with_f_in_flight() {
    let _one = LARGE.lock().unwrap_or_else(|e| e.into_inner());
    let mut m = Meter::start();
    let mut r = Rng::new(44);
    let (mut w, frozen) = dec43_with_f_in_flight(&mut m, &mut r);
    m.tagged.reset_peak();
    m.sigma_peak = 0;
    long_input_block(&mut w, &mut m, &mut r, Some(&frozen));
}

/// (iv) A shard of active doubles past its ceiling mid-block at DEC-43, counted to the byte: the op
/// that doubles it adds exactly the new table less the old to the allocator, and its peak inside
/// the op is the new table on top of the old (hashbrown frees the old one after the move). Below
/// the ceiling the shard grows from its start (CH2c D29).
#[test]
fn witness_iv_a_shard_doubles_mid_block_at_dec43() {
    let mut m = Meter::start();
    let mut w = world(B_DEC43);
    let mut r = Rng::new(45);
    let hasher = SaltedHasher::new(Salt::fixed(7));
    let sizes = w.cache.active().sizes();
    let coin = coin(7);
    let table = CoinsTable::default();
    let db = w.db.clone();
    let mut overlay = BlockOverlay::new(&mut w.cache);
    m.op(overlay.usage(), "open");
    let ceiling = sizes.buckets(0);
    let (_, mut last) = overlay.tip().active().shard_loads().next().unwrap();
    assert!(last < ceiling, "shard 0 starts below its ceiling");
    for round in 0.. {
        // 256 more coins of shard 0, in the database, then one `have_inputs` fetching them. The
        // shard grows by 7/8 of its buckets (O7).
        assert!(round <= ceiling / 256, "shard 0 has not grown: have_inputs caches each input");
        let keys: Vec<_> = std::iter::repeat_with(|| random_key(&mut r, 0))
            .filter(|k| sizes.shard_of(hasher.hash(k)) == 0)
            .take(256)
            .collect();
        let mut b = db.batch();
        for k in &keys {
            table.put(&mut b, k, coin.as_bytes());
        }
        db.write(b, Commit::Buffered).unwrap();
        let tx = spending(&keys);
        let active = overlay.tip().active();
        let shard = active.shard_bytes(0);
        let before = m.tagged.bytes();
        m.tagged.reset_peak();
        assert!(overlay.have_inputs(&tx).unwrap());
        m.op(overlay.usage(), "have_inputs");
        let active = overlay.tip().active();
        let (_, buckets) = active.shard_loads().next().unwrap();
        if buckets == last {
            assert_eq!(m.tagged.bytes(), before, "inline coins in existing buckets");
            continue;
        }
        if buckets <= ceiling {
            last = buckets;
            continue;
        }
        assert_eq!(buckets, 2 * ceiling);
        let grown = active.shard_bytes(0);
        assert_eq!(
            m.tagged.bytes() - before,
            grown - shard,
            "the op adds the new table less the old"
        );
        assert_eq!(m.tagged.peak() - before, grown, "both tables live inside the op");
        println!(
            "witness (iv): shard 0 of {} buckets doubled to {buckets}: table {shard} -> {grown} B, \
             transient {grown} B, sigma {} (B = {B_DEC43})",
            ceiling,
            overlay.usage().total(),
        );
        break;
    }
    overlay.commit(block(1)).unwrap();
    m.op(w.cache.usage(), "commit");
    m.report("(iv)", &w, false);
}
