//! The MNHF manager's rules on crafted blocks (EV3-3): the activation check, signal extraction,
//! the cache's LRU, rows and the cache entry before V20, and the undo. Core's answers are
//! `tests/mnhf_step_vectors.rs` and node-l1's `diff_mnhf_step`.

use std::sync::Arc;

use dash_consensus::TxRef;
use dash_num::{Arith256, CompactTarget};
use dash_params::{Bip9Deployment, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut};
use dash_types::Numeric as _;

use super::*;
use crate::store::{EvoDelta, MemEvoStore};

/// Regtest's bits (chainparams.cpp:842-867): testdummy is not an EHF deployment, v24 is.
const TESTDUMMY: u8 = 28;
const V24: u8 = 13;
const T: i64 = 1_700_000_000;

fn params(v20_height: u32) -> ChainParams {
    let mut p = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .build()
        .expect("regtest params");
    p.consensus.v20_height = v20_height.into();
    p
}

fn tx(version: i16, tx_type: u16, payload: Vec<u8>) -> TxRef {
    Arc::new(Tx::new(Transaction {
        version,
        tx_type: TxType::new(tx_type),
        inputs: vec![TxIn {
            prevout: OutPoint {
                hash: TxHash::from([7; 32]),
                index: 0,
            },
            script_sig: Default::default(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value: 0,
            script_pubkey: vec![0x51].into(),
        }],
        lock_time: 0,
        extra_payload: payload,
    }))
}

/// An `MNHFTxPayload` (version 1, `bit`, a null quorum hash and signature), then `trailing`.
fn signal_payload(bit: u8, trailing: usize) -> Vec<u8> {
    let mut p = vec![1, bit];
    p.extend([0; 32 + 96]);
    p.extend(vec![0; trailing]);
    p
}

fn signal(bit: u8) -> TxRef {
    tx(3, 7, signal_payload(bit, 0))
}

fn block(signals: &[u8]) -> Block {
    let genesis = params(0).genesis;
    let mut txs = vec![tx(1, 0, vec![])];
    txs.extend(signals.iter().map(|&b| signal(b)));
    Block::new(dash_consensus::Header::new(genesis.header), txs)
}

/// A line of blocks from genesis, block `h` with hash `[h, 0xe0]` and median time past `T + h`.
struct Line(Vec<HeaderCtx>);

impl Line {
    fn new(tip: u32) -> Self {
        Self(
            (0..=tip)
                .map(|height| {
                    let mut hash = [0u8; 32];
                    hash[..4].copy_from_slice(&height.to_le_bytes());
                    hash[31] = 0xe0;
                    HeaderCtx {
                        height,
                        hash: BlockHash::from(hash),
                        time: 0,
                        bits: CompactTarget::new(0x207f_ffff),
                        version: 4,
                        mtp: T + i64::from(height),
                        chain_work: Arith256::ZERO,
                    }
                })
                .collect(),
        )
    }

    fn hash(&self, h: u32) -> BlockHash {
        self.0[h as usize].hash
    }
}

impl ChainAncestry for Line {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.0[id.0 as usize]
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

fn row(line: &Line, h: u32, signals: Option<EhfSignals>) -> EvoDelta {
    EvoDelta {
        block: line.hash(h),
        height: h,
        dml: None,
        mined_commitments: Vec::new(),
        credit_data: None,
        credit_pool: None,
        mnhf_signals: signals,
        erased_mined: Vec::new(),
        evo_tip: None,
    }
}

fn signals(pairs: &[(u8, i32)]) -> EhfSignals {
    pairs.iter().copied().collect()
}

fn reason<T: std::fmt::Debug, E: std::fmt::Debug>(r: Result<T, MnhfError<E>>) -> String {
    r.expect_err("a reject").reject_reason().expect("a verdict").to_owned()
}

// Core: chainparams.cpp:109-130
#[test]
fn activation_by_window_and_use_ehf() {
    let mut p = params(0);
    let window = |p: &mut ChainParams, i: usize, bit: i32, start: i64, timeout: i64, ehf: bool| {
        let d: &mut Bip9Deployment = &mut p.consensus.deployments[i];
        (d.bit, d.start_time, d.timeout, d.use_ehf) = (bit, start, timeout, ehf);
    };
    // An unknown bit, and any bit outside every window on it, is valid (quirk 31).
    assert!(is_valid_mn_activation(&p, 5, T));
    window(&mut p, 0, 9, T, T + 10, false);
    window(&mut p, 1, 9, T + 5, T + 20, true);
    window(&mut p, 2, 9, T + 30, T + 40, false);
    // The first deployment whose window holds the time decides; bounds are inclusive.
    assert!(!is_valid_mn_activation(&p, 9, T));
    assert!(!is_valid_mn_activation(&p, 9, T + 10));
    assert!(is_valid_mn_activation(&p, 9, T + 11));
    assert!(is_valid_mn_activation(&p, 9, T + 20));
    assert!(is_valid_mn_activation(&p, 9, T + 25));
    assert!(!is_valid_mn_activation(&p, 9, T + 30));
    assert!(is_valid_mn_activation(&p, 9, T - 1));
}

#[test]
#[should_panic(expected = "IsValidMNActivation")]
fn activation_asserts_the_bit_range() {
    is_valid_mn_activation(&params(0), 29, T);
}

// Core: evo/mnhftx.cpp:176-208
#[test]
fn signals_extract_after_the_coinbase() {
    assert_eq!(extract_signals(&block(&[V24, 5])), Ok(vec![V24, 5]));
    let mut b = block(&[V24]).txs().to_vec();
    // The coinbase is skipped even as a signal; a non-special version, another type and a
    // payload with a trailing byte carry no signal.
    b[0] = signal(14);
    b.push(tx(2, 7, signal_payload(15, 0)));
    b.push(tx(3, 8, signal_payload(16, 0)));
    b.push(tx(3, 7, signal_payload(17, 1)));
    let b = Block::new(*block(&[]).header(), b);
    assert_eq!(extract_signals(&b), Ok(vec![V24]));
    let err = extract_signals(&block(&[V24, 5, V24])).unwrap_err();
    assert_eq!(err.reject_reason(), "bad-mnhf-duplicates-in-block");
}

// Core: evo/mnhftx.cpp:210-251
#[test]
fn checks_run_per_bit_in_order() {
    let p = params(0);
    let run = |bits: &[u8], stage: &[(u8, i32)], just_check: bool| {
        process_signals(&block(bits), 40, T, &p, just_check, || Ok::<_, StoreError>(signals(stage)))
    };
    assert_eq!(run(&[V24, 5], &[], false), Ok(signals(&[(5, 40), (V24, 40)])));
    assert_eq!(reason(run(&[V24], &[(V24, 30)], false)), "bad-mnhf-duplicate");
    assert_eq!(reason(run(&[TESTDUMMY, V24], &[(V24, 30)], false)), "bad-mnhf-non-mn-fork");
    assert_eq!(reason(run(&[V24, TESTDUMMY], &[(V24, 30)], false)), "bad-mnhf-duplicate");
    // The duplicate in the block comes before the stage is read.
    let read = std::cell::Cell::new(false);
    let r = process_signals(&block(&[V24, V24]), 40, T, &p, false, || {
        read.set(true);
        Ok::<_, StoreError>(EhfSignals::new())
    });
    assert_eq!((reason(r), read.get()), ("bad-mnhf-duplicates-in-block".to_owned(), false));
    // A bit CheckMNHFTx refuses: its reason, before the block's duplicates and the stage.
    assert_eq!(reason(run(&[V24, V24, 29], &[], false)), "bad-mnhf-nbit-out-of-bounds");
    assert_eq!(reason(run(&[u8::MAX], &[], true)), "bad-mnhf-nbit-out-of-bounds");
    // A bit out of range in transactions that carry no signal is not checked: a payload with a
    // trailing byte, a non-special version, another type.
    let mut b = block(&[V24]).txs().to_vec();
    b.push(tx(3, 7, signal_payload(30, 1)));
    b.push(tx(2, 7, signal_payload(30, 0)));
    b.push(tx(3, 8, signal_payload(30, 0)));
    let b = Block::new(*block(&[]).header(), b);
    let r = process_signals(&b, 40, T, &p, false, || Ok::<_, StoreError>(EhfSignals::new()));
    assert_eq!(r, Ok(signals(&[(V24, 40)])));
    // A stage the store cannot read is its fault, not a verdict.
    let fault = StoreError::inconsistent("a fault");
    let r = process_signals(&block(&[5]), 40, T, &p, false, || Err(fault.clone()));
    assert_eq!(r, Err(MnhfError::Store(fault)));
    // With fJustCheck, the stage alone.
    assert_eq!(run(&[5], &[(V24, 30)], true), Ok(signals(&[(V24, 30)])));
}

// Core: unordered_lru_cache.h:39-117
#[test]
fn the_cache_keeps_the_most_recent_thousand() {
    let key = |i: u32| {
        let mut h = [0u8; 32];
        h[..4].copy_from_slice(&i.to_le_bytes());
        BlockHash::from(h)
    };
    let mut cache = SignalsCache::default();
    for i in 0..2000 {
        cache.insert(key(i), signals(&[(1, i as i32)]));
    }
    assert_eq!(cache.map.len(), 2000);
    // A hit restamps; a miss does not count.
    assert_eq!(cache.get(&key(0)), Some(signals(&[(1, 0)])));
    assert_eq!(cache.get(&key(5000)), None);
    cache.insert(key(2000), EhfSignals::new());
    assert_eq!(cache.map.len(), CACHE_SIZE);
    assert!(cache.map.contains_key(&key(0)) && cache.map.contains_key(&key(2000)));
    assert!(cache.map.contains_key(&key(1002)) && !cache.map.contains_key(&key(1001)));
    assert_eq!(cache.access_counter, 2002);
}

// Core: evo/mnhftx.cpp:210-262, :323-383 (O2a-MNHF D13, D16)
#[test]
fn rows_from_v20_and_the_cache_entry_below_it() {
    let line = Line::new(12);
    let p = params(10);
    let store = MemEvoStore::new();
    let evo = store.view(&line);
    let view_of = |manager| MnhfView {
        manager,
        params: &p,
        chain: &line,
        store: &evo,
    };
    let manager = MnhfManager::new();
    let view = view_of(&manager);
    // Below V20 the stage is empty and nothing is stored; the signals stay in the cache.
    let at9 = view.process_block(&block(&[V24]), BlockId(9), false).unwrap();
    assert_eq!((at9.signals, at9.row), (signals(&[(V24, 9)]), None));
    // The first V20 block reads them from the cache (DeploymentActiveAfter(9, V20)).
    assert_eq!(view.signals_stage(Some(BlockId(9))).unwrap(), signals(&[(V24, 9)]));
    let at10 = view.process_block(&block(&[5]), BlockId(10), false).unwrap();
    assert_eq!(at10.row, Some(signals(&[(5, 10), (V24, 9)])));
    // A fresh manager (a restart) has no entry for block 9: Core reads the empty map.
    let restarted = MnhfManager::new();
    assert_eq!(view_of(&restarted).signals_stage(Some(BlockId(9))).unwrap(), EhfSignals::new());
    // Just checking caches only what reading the stage cached (block 9's empty map) and
    // stores no row.
    let fresh = MnhfManager::new();
    let checked = view_of(&fresh).process_block(&block(&[V24]), BlockId(10), true).unwrap();
    assert_eq!(
        (checked, fresh.cached()),
        (
            MnhfProcessed {
                signals: EhfSignals::new(),
                row: None,
            },
            1
        )
    );
}

/// D15, D17: rows come from the store; a missing one is a fault; the undo checks the row.
#[test]
fn rows_from_the_store_and_the_undo() {
    let line = Line::new(12);
    let p = params(10);
    let mut store = MemEvoStore::new();
    store.apply(&row(&line, 10, Some(signals(&[(V24, 10)])))).unwrap();
    store.apply(&row(&line, 11, Some(signals(&[(V24, 10), (5, 11)])))).unwrap();
    let evo = store.view(&line);
    let manager = MnhfManager::new();
    let view = MnhfView {
        manager: &manager,
        params: &p,
        chain: &line,
        store: &evo,
    };
    assert_eq!(view.signals_stage(Some(BlockId(11))).unwrap(), signals(&[(5, 11), (V24, 10)]));
    assert_eq!(reason(view.process_block(&block(&[5]), BlockId(12), false)), "bad-mnhf-duplicate");
    assert!(matches!(view.get_for_block(BlockId(12)), Err(StoreError::Inconsistent { .. })));

    assert_eq!(view.undo_block(&block(&[5]), BlockId(11)), Ok(true));
    assert_eq!(view.undo_block(&block(&[]), BlockId(12)), Ok(true));
    assert_eq!(view.undo_block(&block(&[5, 5]), BlockId(11)), Ok(false));
    assert!(matches!(
        view.undo_block(&block(&[14]), BlockId(11)),
        Err(StoreError::Inconsistent { .. })
    ));
}

/// D21: a missing parent row reaches `process_block`'s caller as the store's fault; only
/// versionbits' source panics on it (CS4's contract).
#[test]
fn a_missing_row_is_a_store_fault() {
    let line = Line::new(12);
    let p = params(10);
    let store = MemEvoStore::new();
    let evo = store.view(&line);
    let manager = MnhfManager::new();
    let view = MnhfView {
        manager: &manager,
        params: &p,
        chain: &line,
        store: &evo,
    };
    assert!(matches!(view.signals_stage(Some(BlockId(11))), Err(StoreError::Inconsistent { .. })));
    for just_check in [false, true] {
        let r = view.process_block(&block(&[5]), BlockId(12), just_check);
        assert!(matches!(r, Err(MnhfError::Store(StoreError::Inconsistent { .. }))), "{r:?}");
        assert_eq!(r.unwrap_err().reject_reason(), None);
    }
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        view.signals_for_block(BlockId(11))
    }));
    assert!(panicked.is_err());
}

/// Quirk 31's bit parts on the real networks: a bit no deployment uses (the historic 10, 11, 12
/// signals) is valid at any time and never expires from the stage, so a re-signal stays a
/// duplicate. The signer and request-id parts are `CheckMNHFTx` (EV1).
// Core: chainparams.cpp:109-130, evo/mnhftx.cpp:61-92
#[test]
fn unknown_and_historic_bits_on_mainnet_and_testnet() {
    for network in [NetworkKind::Mainnet, NetworkKind::Testnet] {
        let mut p = ChainParamsBuilder::new(network, Release::V24_0).build().expect("params");
        for bit in [5, 10, 11, 12] {
            for time in [0, T, i64::MAX] {
                assert!(is_valid_mn_activation(&p, bit, time), "{network:?} bit {bit}");
            }
        }
        p.consensus.v20_height = 10u32.into();
        let line = Line::new(12);
        let mut store = MemEvoStore::new();
        let historic = signals(&[(10, 1), (11, 2), (12, 3)]);
        store.apply(&row(&line, 10, Some(historic))).unwrap();
        store.apply(&row(&line, 11, Some(historic))).unwrap();
        let evo = store.view(&line);
        let manager = MnhfManager::new();
        let view = MnhfView {
            manager: &manager,
            params: &p,
            chain: &line,
            store: &evo,
        };
        assert_eq!(view.signals_stage(Some(BlockId(11))).unwrap(), historic, "{network:?}");
        for bit in [10, 11, 12] {
            let r = view.process_block(&block(&[bit]), BlockId(12), true);
            assert_eq!(reason(r), "bad-mnhf-duplicate", "{network:?} bit {bit}");
        }
        let at12 = view.process_block(&block(&[5]), BlockId(12), false).unwrap();
        assert_eq!(at12.row, Some(signals(&[(5, 12), (10, 1), (11, 2), (12, 3)])));
    }
}
