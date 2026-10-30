//! `oracle_mnhf_step` (ABI 26; the tip-cache seed ABI 28): `CMNHFManager::ProcessBlock()` for one
//! block, the stage the next block reads, and `UndoBlock()`, on crafted regtest chains. Every
//! expected reason is the string Core's own code writes at the pin (`evo/mnhftx.cpp:190-262`):
//! `bad-mnhf-duplicates-in-block` (`extractSignals()`), then per new bit in block order
//! `bad-mnhf-duplicate` (a bit the stage holds) and `bad-mnhf-non-mn-fork`
//! (`CChainParams::IsValidMNActivation()`, `chainparams.cpp:109-130`). Expiry is
//! `GetSignalsStage()` (`evo/mnhftx.cpp:61-92`).
//!
//! The crafted chains are regtest headers 600 s apart from `T0`, so the median time past of
//! the block at height `h >= 10` is `T0 + 600 * (h - 5)`. Signal transactions carry a null BLS
//! signature: `ProcessBlock()` reads only their version bits (`CheckMNHFTx()` is the special-tx
//! entries' business). The Core-produced chain of `feature_mnehf.py` is `mnhf_regtest.rs`.

mod common;

use common::mnhf_vectors::{VectorBlock, context, reject};
use dash_oracle_sys::{
    Chain, ErrorKind, MnhfConnected, MnhfContext, MnhfStep, Oracle, SignalChange, Verdict,
};

/// The first header's time.
const T0: u32 = 1_417_713_337;
/// The crafted chains' tip.
const TIP: i32 = 39;
/// The stepped block's height.
const H: i32 = TIP + 1;
/// Regtest deployments (chainparams.cpp:842-867): testdummy (bit 28) is not an EHF deployment
/// and is in its window from time 0 on; v24 (13) and evo_shares (14) are EHF deployments.
const TESTDUMMY: u8 = 28;
const V24: u8 = 13;
const EVO_SHARES: u8 = 14;
/// `TRANSACTION_MNHF_SIGNAL`.
const MNHF_TYPE: u32 = 7;

/// Regtest with V20 and MN_RR active from `height`, plus `extra`.
fn regtest_from(height: i32, extra: &[&str]) -> Chain {
    let v20 = format!("-testactivationheight=v20@{height}");
    let mn_rr = format!("-testactivationheight=mn_rr@{height}");
    let mut args = vec!["-dip3params=2:2", v20.as_str(), mn_rr.as_str()];
    args.extend_from_slice(extra);
    Chain::new("regtest", &args)
}

/// Regtest with V20 and MN_RR active from height 2, plus `extra`.
fn regtest(extra: &[&str]) -> Chain {
    regtest_from(2, extra)
}

/// v24's window from `start` on (its other parameters as regtest's).
fn v24_from(start: i64) -> String {
    format!("-vbparams=v24:{start}:999999999999:0:250:200:150:5:1")
}

/// The median time past of the crafted block at `height >= 10`.
fn mtp(height: i32) -> i64 {
    i64::from(T0) + 600 * i64::from(height - 5)
}

fn headers(oracle: &Oracle, n: i32) -> Vec<[u8; 80]> {
    let mut out = Vec::new();
    let mut prev = [0u8; 32];
    for i in 0..n {
        let mut h = common::header(T0 + 600 * i as u32, 0x207f_ffff);
        h[0..4].copy_from_slice(&4i32.to_le_bytes());
        h[4..36].copy_from_slice(&prev);
        prev = oracle.header_decode(&h).unwrap().hash;
        out.push(h);
    }
    out
}

fn compact(out: &mut Vec<u8>, n: usize) {
    assert!(n < 0xfd);
    out.push(n as u8);
}

/// A coinbase-like first transaction (`extractSignals()` skips it).
fn coinbase() -> Vec<u8> {
    let mut tx = 1i32.to_le_bytes().to_vec();
    compact(&mut tx, 1);
    tx.extend([0u8; 32]);
    tx.extend(u32::MAX.to_le_bytes());
    tx.extend([2, 0x01, 0x28]);
    tx.extend(u32::MAX.to_le_bytes());
    compact(&mut tx, 1);
    tx.extend(0i64.to_le_bytes());
    tx.extend([1, 0x51]);
    tx.extend(0u32.to_le_bytes());
    tx
}

/// An MNHF signal transaction for `bit` (version 3, type 7, an `MNHFTxPayload` of version 1, a
/// null quorum hash and a null signature). `salt` makes otherwise equal signals distinct.
fn signal(bit: u8, salt: u8) -> Vec<u8> {
    let mut tx = (3 | (MNHF_TYPE << 16)).to_le_bytes().to_vec();
    compact(&mut tx, 0);
    compact(&mut tx, 0);
    tx.extend(u32::from(salt).to_le_bytes());
    let mut payload = vec![1u8, bit];
    payload.extend([0u8; 32]);
    payload.extend([0u8; 96]);
    compact(&mut tx, payload.len());
    tx.extend(payload);
    tx
}

/// A block on `tip_header` with a coinbase, then `txs`.
fn block(oracle: &Oracle, tip_header: &[u8; 80], txs: &[Vec<u8>]) -> Vec<u8> {
    let tip_time = common::header_time(tip_header);
    let mut h = common::header(tip_time + 600, 0x207f_ffff);
    h[0..4].copy_from_slice(&4i32.to_le_bytes());
    h[4..36].copy_from_slice(&oracle.header_decode(tip_header).unwrap().hash);
    let mut out = h.to_vec();
    compact(&mut out, txs.len() + 1);
    out.extend(coinbase());
    for tx in txs {
        out.extend(tx);
    }
    out
}

fn signals_at(height: i32, signals: &[(u8, i32)]) -> Vec<SignalChange> {
    vec![SignalChange {
        height,
        signals: signals.to_vec(),
    }]
}

struct Case<'a> {
    name: &'a str,
    chain: Chain,
    rows: Vec<SignalChange>,
    bits: Vec<u8>,
    just_check: bool,
}

impl Case<'_> {
    fn run(&self, oracle: &Oracle) -> MnhfStep {
        let all = headers(oracle, TIP + 1);
        let txs: Vec<_> = self.bits.iter().enumerate().map(|(i, &b)| signal(b, i as u8)).collect();
        let block = block(oracle, all.last().unwrap(), &txs);
        let mnhf = MnhfContext {
            just_check: self.just_check,
            signals: self.rows.clone(),
            seed_tip_cache: false,
        };
        oracle
            .mnhf_step(&self.chain, &block, &context(&all, 0), &mnhf)
            .unwrap_or_else(|e| panic!("{}: {e:?}", self.name))
    }

    /// The step as a dash-dml vector line.
    fn vector(&self, oracle: &Oracle) -> String {
        let block = VectorBlock {
            name: self.name,
            height: H,
            mtp: mtp(H),
            bits: &self.bits,
            just_check: self.just_check,
        };
        common::mnhf_vectors::line(oracle, &self.chain, &block, &self.run(oracle))
    }
}

/// The step of a block signalling `bits` on a tip at `H` (headers 0 ..= `H`, the context's
/// from `first`), with V20 from `H + 1`, so the tip is at `V20Height - 1`, over `rows`; with
/// `seed` the context is format 2 and seeds the tip's cache entry.
fn on_last_tip_before_v20(
    oracle: &Oracle,
    rows: Vec<SignalChange>,
    first: i32,
    bits: &[u8],
    seed: bool,
) -> Result<MnhfStep, dash_oracle_sys::Error> {
    let all = headers(oracle, H + 1);
    let txs: Vec<_> = bits.iter().enumerate().map(|(i, &b)| signal(b, i as u8)).collect();
    let block = block(oracle, all.last().unwrap(), &txs);
    let mnhf = MnhfContext {
        just_check: false,
        signals: rows,
        seed_tip_cache: seed,
    };
    oracle.mnhf_step(&regtest_from(H + 1, &[]), &block, &context(&all, first), &mnhf)
}

/// The seeded step on that tip, its change `tip_signals`, as a dash-dml vector line.
fn tip_cache_vector(oracle: &Oracle, name: &str, tip_signals: &[(u8, i32)], bits: &[u8]) -> String {
    let step = on_last_tip_before_v20(oracle, signals_at(H, tip_signals), 0, bits, true).unwrap();
    let vector = VectorBlock {
        name,
        height: H + 1,
        mtp: mtp(H + 1),
        bits,
        just_check: false,
    };
    common::mnhf_vectors::line(oracle, &regtest_from(H + 1, &[]), &vector, &step)
}

fn connected(after: &[(u8, i32)], undone: &[(u8, i32)]) -> Option<MnhfConnected> {
    Some(MnhfConnected {
        stage_after: after.to_vec(),
        undo_ok: true,
        stage_undone: undone.to_vec(),
    })
}

#[test]
fn process_block_verdicts_and_signals() {
    for oracle in Oracle::all() {
        let case = |name, rows: Vec<SignalChange>, bits: &[u8]| Case {
            name,
            chain: regtest(&[]),
            rows,
            bits: bits.to_vec(),
            just_check: false,
        };

        let r = case("no signals", vec![], &[]).run(&oracle);
        assert_eq!(r.verdict, Verdict::Accept);
        assert_eq!(r.stage_before, vec![]);
        assert_eq!(r.signals, Some(vec![]));
        assert_eq!(r.connected, connected(&[], &[]));

        // An EHF deployment in its window, and a bit no deployment uses (valid as unknown).
        for bit in [V24, 5] {
            let r = case("one new signal", vec![], &[bit]).run(&oracle);
            assert_eq!(r.verdict, Verdict::Accept, "bit {bit}");
            assert_eq!(r.signals, Some(vec![(bit, H)]));
            assert_eq!(r.connected, connected(&[(bit, H)], &[]));
        }

        // A deployment in its window that is not an EHF one.
        let r = case("non-EHF bit", vec![], &[TESTDUMMY]).run(&oracle);
        assert_eq!(r.verdict, reject("bad-mnhf-non-mn-fork"));
        assert_eq!((r.signals, r.connected), (None, None));

        // A bit twice in the block comes first, before the stage and activation checks.
        for bits in [[V24, V24], [TESTDUMMY, TESTDUMMY]] {
            let r = case("twice in the block", vec![], &bits).run(&oracle);
            assert_eq!(r.verdict, reject("bad-mnhf-duplicates-in-block"), "{bits:?}");
        }

        // A bit the stage holds; another bit beside it is added at the block's height.
        let held = signals_at(30, &[(V24, 30)]);
        let r = case("held bit", held.clone(), &[V24]).run(&oracle);
        assert_eq!(r.verdict, reject("bad-mnhf-duplicate"));
        assert_eq!(r.stage_before, vec![(V24, 30)]);
        let r = case("new bit beside a held one", held.clone(), &[EVO_SHARES]).run(&oracle);
        assert_eq!(r.verdict, Verdict::Accept);
        assert_eq!(r.signals, Some(vec![(V24, 30), (EVO_SHARES, H)]));
        assert_eq!(r.connected, connected(&[(V24, 30), (EVO_SHARES, H)], &[(V24, 30)]));

        // Per bit in block order: the first failing check decides.
        let r = case("non-EHF then held", held.clone(), &[TESTDUMMY, V24]).run(&oracle);
        assert_eq!(r.verdict, reject("bad-mnhf-non-mn-fork"));
        let r = case("held then non-EHF", held.clone(), &[V24, TESTDUMMY]).run(&oracle);
        assert_eq!(r.verdict, reject("bad-mnhf-duplicate"));
        // A later bit is checked as the first is.
        let r = case("valid then non-EHF", vec![], &[V24, 5, TESTDUMMY]).run(&oracle);
        assert_eq!(r.verdict, reject("bad-mnhf-non-mn-fork"));
        let r = case("valid then held", held.clone(), &[EVO_SHARES, V24]).run(&oracle);
        assert_eq!(r.verdict, reject("bad-mnhf-duplicate"));

        // fJustCheck (TestBlockValidity): Core returns the stage without the new bits.
        let r = Case {
            just_check: true,
            ..case("just check", held, &[EVO_SHARES])
        }
        .run(&oracle);
        assert_eq!(r.verdict, Verdict::Accept);
        assert_eq!((r.signals, r.connected), (Some(vec![(V24, 30)]), None));
    }
}

#[test]
fn expired_signals_leave_the_stage_and_free_their_bit() {
    for oracle in Oracle::all() {
        // v24's window starts after the old signal's block: the old signal is expired at the
        // tip, and its bit is signalled again.
        let rows = signals_at(30, &[(V24, 30), (5, 30)]);

        // The new signal's block is inside the window: it stays in the stage.
        let inside = v24_from(mtp(H) - 1);
        let r = Case {
            name: "re-signal in the window",
            chain: regtest(&[&inside]),
            rows: rows.clone(),
            bits: vec![V24],
            just_check: false,
        }
        .run(&oracle);
        assert_eq!(r.verdict, Verdict::Accept);
        assert_eq!(r.stage_before, vec![(5, 30)], "the old v24 signal is expired");
        assert_eq!(r.signals, Some(vec![(5, 30), (V24, H)]));
        assert_eq!(r.connected, connected(&[(5, 30), (V24, H)], &[(5, 30)]));

        // The new signal's block is before the window too: valid (no deployment on the bit in its
        // window), stored in the block's row, and out of the next block's stage.
        let before = v24_from(mtp(H) + 1);
        let r = Case {
            name: "re-signal before the window",
            chain: regtest(&[&before]),
            rows,
            bits: vec![V24],
            just_check: false,
        }
        .run(&oracle);
        assert_eq!(r.verdict, Verdict::Accept);
        assert_eq!(r.signals, Some(vec![(5, 30), (V24, H)]));
        assert_eq!(r.connected, connected(&[(5, 30)], &[(5, 30)]));
    }
}

#[test]
fn before_v20_the_stage_is_empty() {
    for oracle in Oracle::all() {
        let r = Case {
            name: "before V20",
            chain: regtest_from(100, &[]),
            rows: vec![],
            bits: vec![V24],
            just_check: false,
        }
        .run(&oracle);
        assert_eq!(r.verdict, Verdict::Accept);
        assert_eq!(r.signals, Some(vec![(V24, H)]));
        assert_eq!(r.connected, connected(&[], &[]));
    }
}

/// Core stores no row below V20: the signals of the block at `V20Height - 1` are in its
/// manager's cache alone (`CMNHFManager::AddToCache()`, `GetFromCache()`). The connecting step
/// reads them in the next block's stage; a step on a tip at `V20Height - 1` reads them only when
/// its context seeds the tip's cache entry, and otherwise refuses a change there (D13, BL-211).
#[test]
fn signals_below_v20_are_not_stored() {
    for oracle in Oracle::all() {
        let r = Case {
            name: "the block before V20",
            chain: regtest_from(H + 1, &[]),
            rows: vec![],
            bits: vec![V24],
            just_check: false,
        }
        .run(&oracle);
        assert_eq!(r.verdict, Verdict::Accept);
        assert_eq!(r.connected, connected(&[(V24, H)], &[]), "from the manager's cache");

        let first = Case {
            name: "the first block with V20",
            chain: regtest_from(H, &[]),
            rows: vec![],
            bits: vec![V24],
            just_check: false,
        };
        let r = first.run(&oracle);
        assert_eq!(r.stage_before, vec![], "no row at the tip");
        assert_eq!(r.verdict, Verdict::Accept);
        assert_eq!(r.connected, connected(&[(V24, H)], &[]));

        // A change at the tip, where V20 is not active: Core would hold it in the cache alone,
        // which the step does not seed, so it is refused rather than dropped. An empty one is not.
        let all = headers(&oracle, TIP + 1);
        let block = block(&oracle, all.last().unwrap(), &[signal(V24, 0)]);
        let step = |rows| {
            let mnhf = MnhfContext {
                just_check: false,
                signals: rows,
                seed_tip_cache: false,
            };
            oracle.mnhf_step(&first.chain, &block, &context(&all, 0), &mnhf)
        };
        let err = step(signals_at(TIP, &[(V24, TIP)])).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{err:?}");
        assert_eq!(step(signals_at(TIP, &[])).unwrap(), r);
    }
}

/// The seeded tip (BL-211): the step on a tip at `V20Height - 1` whose cache entry holds the
/// tip's signals reads, as the next block's stage, what the connecting step left there.
#[test]
fn a_seeded_tip_cache_is_the_connecting_nodes() {
    for oracle in Oracle::all() {
        // Connecting the block at V20Height - 1 (H, V20 from H + 1) leaves its signal in the
        // manager's cache: the next block's stage.
        let connecting = Case {
            name: "the block before V20",
            chain: regtest_from(H + 1, &[]),
            rows: vec![],
            bits: vec![V24],
            just_check: false,
        }
        .run(&oracle);
        let cached = connecting.connected.unwrap().stage_after;
        assert_eq!(cached, vec![(V24, H)]);

        // The next block on that tip, seeded with what the connecting step cached.
        let step = |rows: &[(u8, i32)], first, bits: &[u8], seed| {
            on_last_tip_before_v20(&oracle, signals_at(H, rows), first, bits, seed)
        };
        let held = step(&cached, 0, &[V24], true).unwrap();
        assert_eq!(held.stage_before, cached, "the stage is the connecting step's after");
        assert_eq!(held.verdict, reject("bad-mnhf-duplicate"));
        let beside = step(&cached, 0, &[EVO_SHARES], true).unwrap();
        assert_eq!(beside.verdict, Verdict::Accept);
        assert_eq!(beside.signals, Some(vec![(V24, H), (EVO_SHARES, H + 1)]));
        assert_eq!(beside.connected, connected(&[(V24, H), (EVO_SHARES, H + 1)], &cached));

        // Unseeded, the change at the tip is refused (format 1, D14); an empty one, seeded or
        // not, is the empty entry Core's manager inserts below V20.
        let err = step(&cached, 0, &[V24], false).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{err:?}");
        let empty = step(&[], 0, &[V24], false).unwrap();
        assert_eq!(step(&[], 0, &[V24], true).unwrap(), empty);
        assert_eq!(empty.stage_before, vec![]);

        // The tip's entry holds the tip's own bits at its height: a seeded signal from an
        // earlier block is refused, and so is a seeded change below the tip.
        let err = step(&[(V24, H - 1)], 0, &[], true).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{err:?}");
        let below = signals_at(H - 1, &[(V24, H - 1)]);
        let err = on_last_tip_before_v20(&oracle, below, 0, &[], true).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{err:?}");

        // A seeded signal's median time past is read: its block and the ten below it.
        assert_eq!(step(&cached, H - 9, &[], true).unwrap_err().kind, ErrorKind::Context);
        assert_eq!(step(&cached, H - 10, &[], true).unwrap().stage_before, cached);

        // Where V20 is active at the tip the seed has no effect: the tip's row is what the
        // cache would hold.
        let at_v20 = |seed| {
            let mnhf = MnhfContext {
                just_check: false,
                signals: signals_at(TIP, &[(V24, 30)]),
                seed_tip_cache: seed,
            };
            let all = headers(&oracle, TIP + 1);
            let signalled = block(&oracle, all.last().unwrap(), &[signal(EVO_SHARES, 0)]);
            oracle.mnhf_step(&regtest(&[]), &signalled, &context(&all, 0), &mnhf).unwrap()
        };
        assert_eq!(at_v20(true), at_v20(false));
        assert_eq!(at_v20(true).stage_before, vec![(V24, 30)]);
    }
}

/// MNHF contexts the binding never writes: Core's deserializer and the shim's checks refuse
/// them (`ORACLE_E_ARG`), and format 2 without the seed is format 1.
#[test]
fn mnhf_context_bytes() {
    for oracle in Oracle::all() {
        let chain = regtest(&[]);
        let all = headers(&oracle, TIP + 1);
        let block = block(&oracle, all.last().unwrap(), &[signal(V24, 0)]);
        let run = |bytes: &[u8]| oracle.mnhf_step_raw(&chain, &block, &context(&all, 0), bytes);
        let rows = signals_at(30, &[(EVO_SHARES, 30)]);
        let format1 = MnhfContext {
            just_check: false,
            signals: rows.clone(),
            seed_tip_cache: false,
        }
        .serialize();
        let format2 = MnhfContext {
            seed_tip_cache: true,
            signals: rows,
            just_check: false,
        }
        .serialize();
        let unseeded = [&format2[..format2.len() - 1], &[0]].concat();
        let expected = run(&format1).unwrap();
        assert_eq!(expected.stage_before, vec![(EVO_SHARES, 30)]);
        assert_eq!(run(&unseeded).unwrap(), expected, "format 2 with seed 0");
        assert_eq!(run(&format2).unwrap(), expected, "the seed has no effect where V20 is active");
        let mut refused = vec![
            ([&format2[..format2.len() - 1], &[2]].concat(), "a seed flag of 2"),
            (format2[..format2.len() - 1].to_vec(), "format 2 without its flag"),
            ([&format1[..], &[0]].concat(), "format 1 with a byte after it"),
            ([&format2[..], &[0]].concat(), "format 2 with a byte after it"),
        ];
        for format in [0, 3] {
            let mut bytes = format1.clone();
            bytes[0] = format;
            refused.push((bytes, "another format"));
        }
        for (bytes, what) in refused {
            let err = run(&bytes).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Argument, "{what}: {err:?}");
        }
    }
}

#[test]
fn refusals_and_missing_context() {
    for oracle in Oracle::all() {
        let chain = regtest(&[]);
        let all = headers(&oracle, TIP + 1);
        let tip = all.last().unwrap();
        let run = |block: &[u8], first: i32, rows: Vec<SignalChange>| {
            let mnhf = MnhfContext {
                just_check: false,
                signals: rows,
                seed_tip_cache: false,
            };
            oracle.mnhf_step(&chain, block, &context(&all, first), &mnhf)
        };
        let with = |bits: &[u8]| {
            let txs: Vec<_> = bits.iter().map(|&b| signal(b, 0)).collect();
            block(&oracle, tip, &txs)
        };

        // IsValidMNActivation() asserts on a bit at or above VERSIONBITS_NUM_BITS.
        assert_eq!(run(&with(&[29]), 0, vec![]).unwrap_err().kind, ErrorKind::Argument);
        // A block that does not extend the tip.
        let other = block(&oracle, &all[TIP as usize - 1], &[]);
        assert_eq!(run(&other, 0, vec![]).unwrap_err().kind, ErrorKind::Argument);
        // Blocks CheckBlock() rejects before ProcessBlock(): no transactions, or a signal first.
        let mut empty = with(&[]);
        empty.truncate(81);
        empty[80] = 0;
        assert_eq!(run(&empty, 0, vec![]).unwrap_err().kind, ErrorKind::Argument);
        let mut no_coinbase = with(&[])[..80].to_vec();
        no_coinbase.push(1);
        no_coinbase.extend(signal(V24, 0));
        assert_eq!(run(&no_coinbase, 0, vec![]).unwrap_err().kind, ErrorKind::Argument);
        // Signal changes outside the version-bits rules: below the headers, a signal above its change.
        assert_eq!(run(&with(&[]), 20, signals_at(10, &[])).unwrap_err().kind, ErrorKind::Argument);
        assert_eq!(
            run(&with(&[]), 0, signals_at(30, &[(V24, 31)])).unwrap_err().kind,
            ErrorKind::Argument
        );

        // The block's median time past needs the tip and the nine below it, once it has a signal.
        assert_eq!(run(&with(&[V24]), 31, vec![]).unwrap_err().kind, ErrorKind::Context);
        assert_eq!(run(&with(&[V24]), 30, vec![]).unwrap().verdict, Verdict::Accept);
        assert_eq!(run(&with(&[]), TIP, vec![]).unwrap().verdict, Verdict::Accept);
        // Not when extractSignals() rejects first. (A first bit the stage holds also returns
        // before the read, but a held signal's own read already needs these headers.)
        assert_eq!(
            run(&with(&[V24, V24]), TIP, vec![]).unwrap().verdict,
            reject("bad-mnhf-duplicates-in-block")
        );
        // A held signal's median time past needs its block and the ten below it.
        let rows = signals_at(TIP, &[(V24, 30)]);
        assert_eq!(run(&with(&[]), 21, rows.clone()).unwrap_err().kind, ErrorKind::Context);
        assert_eq!(run(&with(&[]), 20, rows).unwrap().stage_before, vec![(V24, 30)]);
    }
}

/// The crafted cases as dash-dml's L0 vectors (`dash-dml/tests/data/mnhf/crafted.txt`): every
/// reject reason, the bit order, `fJustCheck`, expiry on both sides of a window start, window
/// timeouts, an unknown bit, and blocks before V20.
#[test]
fn crafted_cases_are_dash_dml_vectors() {
    for oracle in Oracle::all() {
        let held = || signals_at(30, &[(V24, 30)]);
        let v24_until = |timeout: i64| format!("-vbparams=v24:0:{timeout}:0:250:200:150:5:1");
        let dummy = |start: i64, timeout: i64| format!("-vbparams=testdummy:{start}:{timeout}");
        let expiring = || signals_at(30, &[(V24, 30), (5, 30)]);
        let cases = [
            ("no signals", vec![], vec![], vec![], false),
            ("an EHF bit in its window", vec![], vec![], vec![V24], false),
            ("an unknown bit", vec![], vec![], vec![5], false),
            ("two new bits", vec![], vec![], vec![EVO_SHARES, V24], false),
            ("a non-EHF bit in its window", vec![], vec![], vec![TESTDUMMY], false),
            ("a bit twice", vec![], vec![], vec![V24, 5, V24], false),
            ("a non-EHF bit twice", vec![], vec![], vec![TESTDUMMY, TESTDUMMY], false),
            ("a held bit", vec![], held(), vec![V24], false),
            // The stage check comes before IsValidMNActivation() (evo/mnhftx.cpp:232, :237).
            (
                "a held non-EHF bit",
                vec![],
                signals_at(30, &[(TESTDUMMY, 30)]),
                vec![TESTDUMMY],
                false,
            ),
            // duplicates-in-block comes before the stage check.
            ("a held bit twice", vec![], held(), vec![V24, V24], false),
            ("beside a held bit", vec![], held(), vec![EVO_SHARES], false),
            ("non-EHF before held", vec![], held(), vec![TESTDUMMY, V24], false),
            ("held before non-EHF", vec![], held(), vec![V24, TESTDUMMY], false),
            ("valid before non-EHF", vec![], vec![], vec![V24, 5, TESTDUMMY], false),
            ("valid before held", vec![], held(), vec![EVO_SHARES, V24], false),
            ("just check accepted", vec![], held(), vec![EVO_SHARES], true),
            ("just check rejected", vec![], held(), vec![V24], true),
            ("just check no signals", vec![], held(), vec![], true),
            ("re-signal in the window", vec![v24_from(mtp(H) - 1)], expiring(), vec![V24], false),
            ("re-signal at the window start", vec![v24_from(mtp(H))], expiring(), vec![V24], false),
            (
                "re-signal before the window",
                vec![v24_from(mtp(H) + 1)],
                expiring(),
                vec![V24],
                false,
            ),
            ("expired, no signals", vec![v24_from(mtp(H) + 1)], expiring(), vec![], false),
            ("an EHF bit after its timeout", vec![v24_until(mtp(H) - 1)], vec![], vec![V24], false),
            ("an EHF bit at its timeout", vec![v24_until(mtp(H))], vec![], vec![V24], false),
            // Out of its window a non-EHF deployment's bit is unknown, so valid.
            (
                "a non-EHF bit after its timeout",
                vec![dummy(0, mtp(H) - 1)],
                vec![],
                vec![TESTDUMMY],
                false,
            ),
            (
                "a non-EHF bit at its timeout",
                vec![dummy(0, mtp(H))],
                vec![],
                vec![TESTDUMMY],
                false,
            ),
            (
                "a non-EHF bit before its start",
                vec![dummy(mtp(H) + 1, 1 << 40)],
                vec![],
                vec![TESTDUMMY],
                false,
            ),
            (
                "a non-EHF bit at its start",
                vec![dummy(mtp(H), 1 << 40)],
                vec![],
                vec![TESTDUMMY],
                false,
            ),
        ];
        let mut lines = Vec::new();
        for (name, extra, rows, bits, just_check) in cases {
            let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
            let case = Case {
                name,
                chain: regtest(&extra),
                rows,
                bits,
                just_check,
            };
            lines.push(case.vector(&oracle));
        }
        let before_v20 = Case {
            name: "before V20",
            chain: regtest_from(100, &[]),
            rows: vec![],
            bits: vec![V24],
            just_check: false,
        };
        lines.push(before_v20.vector(&oracle));
        // `after` comes from the manager's cache (D13).
        let last_before_v20 = Case {
            name: "V20 from the next block",
            chain: regtest_from(H + 1, &[]),
            ..before_v20
        };
        lines.push(last_before_v20.vector(&oracle));
        // The next block on that tip, its cache entry seeded with what it cached (BL-211).
        let cached = [(V24, H)];
        for (name, bit) in [
            ("on V20 from the tip's cache: the cached bit", V24),
            ("on V20 from the tip's cache: a new bit", EVO_SHARES),
        ] {
            lines.push(tip_cache_vector(&oracle, name, &cached, &[bit]));
        }
        common::mnhf_vectors::check_or_write(&oracle, "crafted.txt", "tests/mnhf.rs", &lines);
    }
}

#[test]
fn signal_payloads_are_ones_core_decodes() {
    for oracle in Oracle::all() {
        let tx = signal(V24, 0);
        let decoded = oracle.tx_decode(&tx).unwrap();
        assert_eq!((decoded.consumed, decoded.reencoded.as_slice()), (tx.len(), tx.as_slice()));
        let payload = common::extra_payload(&tx);
        let p = oracle.payload_decode(MNHF_TYPE as u16, false, &payload).unwrap();
        assert!(p.consumed_all && p.reencoded == payload, "Core re-encodes the payload unchanged");
    }
}
