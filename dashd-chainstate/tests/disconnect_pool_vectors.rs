//! The disconnect pool against Dash Core (CH3b b1; CH3 §4.3, D25): every row of the vectors
//! `test-vectors/disconnect-pool/` holds, which Core's `DisconnectedBlockTransactions`, its
//! memory accounting and libstdc++'s `std::unordered_map` computed (`PROVENANCE.md` there). Each
//! file's header names its columns.

use std::collections::HashMap;
use std::sync::Arc;

use dash_consensus::{Tx, TxRef, decode_tx_exact};
use dash_core_vectors::hex::{hex_str, parse_hex, uint256_to_hex};
use dash_core_vectors::read_vector_text;
use dash_sha256::Sha256;
use dash_types::Numeric as _;
use dashd_chainstate::chain::DisconnectPool;
use dashd_chainstate::reorg::buckets::Buckets;
use dashd_chainstate::reorg::memusage::{
    SCRIPT_INLINE, SIZEOF_LIST_NODE, SIZEOF_TX_IN, SIZEOF_TX_OUT, SIZEOF_UNORDERED_NODE,
    malloc_usage, recursive_dynamic_usage,
};
use dashd_chainstate::reorg::{DisconnectedBlockTransactions, MAX_DISCONNECTED_TX_POOL_SIZE};

/// A file's rows, each split on whitespace, without its comments.
fn rows(file: &str) -> Vec<Vec<String>> {
    read_vector_text(&format!("disconnect-pool/{file}"))
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split_whitespace().map(str::to_owned).collect())
        .collect()
}

fn num(field: &str) -> u64 {
    field.parse().unwrap_or_else(|e| panic!("{field}: {e}"))
}

/// The generator's runs: `a-b` from `a` to `b` in steps of one, up or down; `a`; comma-separated;
/// `-` for none.
fn runs(field: &str) -> Vec<u64> {
    if field == "-" {
        return Vec::new();
    }
    let mut out = Vec::new();
    for run in field.split(',') {
        match run.split_once('-').map(|(a, b)| (num(a), num(b))) {
            Some((a, b)) if a <= b => out.extend(a..=b),
            Some((a, b)) => out.extend((b..=a).rev()),
            None => out.push(num(run)),
        }
    }
    out
}

/// `buckets.txt`: `_M_next_bkt` and the `_M_next_resize` it leaves, on runs that cover every
/// `size_t`. Each run is checked at its ends, and every `n` below 2^20 one by one.
#[test]
fn next_bkt_matches_libstdcxx_for_every_size() {
    let probe = |n: u64| {
        let mut buckets = Buckets::default();
        let count = buckets.next_bkt(n);
        (count, buckets.next_resize())
    };
    let mut next_first = Some(0);
    for row in rows("buckets.txt") {
        assert_eq!(row[0], "run");
        let (first, last, expected) = (num(&row[1]), num(&row[2]), (num(&row[3]), num(&row[4])));
        assert_eq!(Some(first), next_first, "the runs are contiguous");
        for n in [first, last] {
            assert_eq!(probe(n), expected, "_M_next_bkt({n})");
        }
        for n in first..=last.min((1 << 20) - 1) {
            assert_eq!(probe(n), expected, "_M_next_bkt({n})");
        }
        next_first = last.checked_add(1);
    }
    assert_eq!(next_first, None, "the runs reach 2^64 - 1");
}

/// `container.txt`'s `grow` rows: the pool's map type on its own, one insert at a time to 2^21
/// entries.
#[test]
fn the_maps_bucket_count_grows_as_libstdcxx() {
    let all = rows("container.txt");
    let mut grow =
        all.iter().filter(|r| r[0] == "grow").map(|r| (num(&r[1]), num(&r[2]))).peekable();
    let mut buckets = Buckets::default();
    let mut count = 0;
    for len in 0..=1 << 21 {
        if len > 0 {
            buckets.insert(len - 1);
        }
        if let Some((_, changed)) = grow.next_if(|(size, _)| *size == len) {
            count = changed;
        }
        assert_eq!(buckets.count(), count, "grow to {len}");
    }
    assert!(grow.next().is_none());
}

/// `container.txt`'s sequences: the pool's map type through random `reserve`, inserts, repeated
/// keys, erases and `clear`.
#[test]
fn the_maps_bucket_count_follows_libstdcxx_through_random_calls() {
    let all = rows("container.txt");
    let mut seqs = 0;
    let (mut buckets, mut len) = (Buckets::default(), 0);
    for row in all.iter().filter(|r| r[0] != "grow") {
        if row[0] == "seq" {
            (buckets, len, seqs) = (Buckets::default(), 0, seqs + 1);
            continue;
        }
        let n = num(&row[1]);
        match row[0].as_str() {
            "reserve" => buckets.reserve(len, len + n),
            "insert" => {
                for _ in 0..n {
                    buckets.insert(len);
                    len += 1;
                }
            }
            "dup" => {}
            "erase" => len -= n,
            "clear" => len = 0,
            op => panic!("unknown op {op}"),
        }
        assert_eq!((len, buckets.count()), (num(&row[2]), num(&row[3])), "seq {seqs}: {row:?}");
    }
    assert_eq!(seqs, 64);
}

/// The vectors' transactions by index: `pool.txt`'s `tx` rows, then its `bulk` ranges, each
/// checked against Core's `GetHash()` and `RecursiveDynamicUsage`.
fn corpus(all: &[Vec<String>]) -> HashMap<u64, TxRef> {
    let mut txs = HashMap::new();
    for row in all.iter().filter(|r| r[0] == "tx") {
        let tx = decode(&parse_hex(&row[4]));
        assert_eq!(uint256_to_hex(&tx.txid().to_lendian()), row[2], "tx {} GetHash()", row[1]);
        assert_eq!(
            recursive_dynamic_usage(&tx) as u64,
            num(&row[3]),
            "tx {} RecursiveDynamicUsage",
            row[1]
        );
        txs.insert(num(&row[1]), tx);
    }
    for row in all.iter().filter(|r| r[0] == "bulk") {
        let [first, count, n_in, n_out, sig_len, spk_len, usage] =
            [1, 2, 3, 4, 5, 6, 7].map(|i| num(&row[i]));
        let mut hashes = Sha256::new();
        for k in first..first + count {
            let tx = decode(&bulk(k, n_in, n_out, sig_len as usize, spk_len as usize));
            assert_eq!(recursive_dynamic_usage(&tx) as u64, usage, "bulk {k}");
            hashes.update(&tx.txid().to_lendian());
            txs.insert(k, tx);
        }
        assert_eq!(hex_str(&hashes.finalize()), row[8], "bulk {first}: the GetHash() digest");
    }
    txs
}

/// The vectors' indices of `txs`.
fn indices<'a>(index: &HashMap<*const Tx, u64>, txs: impl Iterator<Item = &'a TxRef>) -> Vec<u64> {
    txs.map(|tx| index[&Arc::as_ptr(tx)]).collect()
}

fn decode(bytes: &[u8]) -> TxRef {
    Arc::new(decode_tx_exact(bytes).unwrap_or_else(|e| panic!("{e:?}")).into_value())
}

/// The generator's `Bulk(k, …)`: version 1, input `j` spending `(LE64(k) ‖ LE32(j) ‖ 0^20, j)`
/// with `sig_len` bytes of `0x51`, each output `k` duffs to `spk_len` bytes of `0x51`.
fn bulk(k: u64, n_in: u64, n_out: u64, sig_len: usize, spk_len: usize) -> Vec<u8> {
    let compact = |bytes: &mut Vec<u8>, n: u64| {
        assert!(n < 0xfd);
        bytes.push(n as u8);
    };
    let mut bytes = 1i32.to_le_bytes().to_vec();
    compact(&mut bytes, n_in);
    for j in 0..n_in as u32 {
        bytes.extend(k.to_le_bytes());
        bytes.extend(j.to_le_bytes());
        bytes.extend([0; 20]);
        bytes.extend(j.to_le_bytes());
        compact(&mut bytes, sig_len as u64);
        bytes.extend(std::iter::repeat_n(0x51, sig_len));
        bytes.extend(u32::MAX.to_le_bytes());
    }
    compact(&mut bytes, n_out);
    for _ in 0..n_out {
        bytes.extend(k.to_le_bytes());
        compact(&mut bytes, spk_len as u64);
        bytes.extend(std::iter::repeat_n(0x51, spk_len));
    }
    bytes.extend(0u32.to_le_bytes());
    bytes
}

/// `pool.txt`'s `const` rows: Core's object sizes and `MallocUsage`.
#[test]
fn sizes_and_malloc_usage_match_core() {
    let all = rows("pool.txt");
    let consts: HashMap<&str, u64> =
        all.iter().filter(|r| r[0] == "const").map(|r| (r[1].as_str(), num(&r[2]))).collect();
    assert_eq!(consts["sizeof_CTxIn"], SIZEOF_TX_IN as u64);
    assert_eq!(consts["sizeof_CTxOut"], SIZEOF_TX_OUT as u64);
    assert_eq!(consts["sizeof_list_node"], SIZEOF_LIST_NODE as u64);
    assert_eq!(consts["sizeof_unordered_node"], SIZEOF_UNORDERED_NODE as u64);
    // A prevector<28>: its inline bytes and its size field.
    assert_eq!(consts["sizeof_CScript"], SCRIPT_INLINE as u64 + 4);
    assert_eq!(consts["MAX_DISCONNECTED_TX_POOL_SIZE"], MAX_DISCONNECTED_TX_POOL_SIZE as u64);
    for n in 0..=100 {
        assert_eq!(consts[format!("MallocUsage({n})").as_str()], malloc_usage(n) as u64);
    }
}

/// `pool.txt`'s transactions: Core's `GetHash()` (the stable txid of a version-2 asset unlock) and
/// `RecursiveDynamicUsage`, for 21 script lengths (27, 28 and 29 either side of the inline 28
/// bytes among them), inputs and outputs from none to 1,000, special transactions whose payload is
/// not counted, and the bulk ranges.
#[test]
fn txids_and_recursive_dynamic_usage_match_core() {
    let all = rows("pool.txt");
    let txs = corpus(&all);
    assert_eq!(txs.len(), all.iter().filter(|r| r[0] == "tx").count() + 61_200);
}

/// `pool.txt`'s scenarios: after every `AddTransactionsFromBlock`, `removeForBlock` and `take()`,
/// the pool's `DynamicMemoryUsage`, bucket count, size, `cachedInnerUsage`, the evicted or taken
/// transactions in order, and the queue front first.
#[test]
fn every_pool_call_matches_core() {
    let all = rows("pool.txt");
    let txs = corpus(&all);
    let index: HashMap<*const Tx, u64> = txs.iter().map(|(i, tx)| (Arc::as_ptr(tx), *i)).collect();
    let refs = |field: &str| runs(field).iter().map(|i| Arc::clone(&txs[i])).collect::<Vec<_>>();

    let mut scenarios = 0;
    let mut current: Option<(String, DisconnectedBlockTransactions)> = None;
    for row in all.iter().skip_while(|r| r[0] != "scenario") {
        if row[0] == "scenario" {
            current =
                Some((row[1].clone(), DisconnectedBlockTransactions::new(num(&row[2]) as usize)));
            scenarios += 1;
            continue;
        }
        if row[0] == "end" {
            let (name, pool) = current.take().expect("a scenario");
            assert!(pool.is_empty(), "{name} ends empty");
            continue;
        }
        let (name, pool) = current.as_mut().expect("a scenario");
        // Each row: the op, its transactions, the pool's state (2..6), then the op's own fields.
        let queue = match row[0].as_str() {
            "add" => {
                let evicted = pool.add_from_block(&refs(&row[1]));
                assert_eq!(
                    indices(&index, evicted.iter()),
                    runs(&row[6]),
                    "{name}: evicted, {row:?}"
                );
                Some(&row[7])
            }
            "remove" => {
                pool.remove_for_block(&refs(&row[1]));
                Some(&row[6])
            }
            "take" => {
                let taken = pool.take();
                assert_eq!(indices(&index, taken.iter()), runs(&row[1]), "{name}: taken");
                None
            }
            op => panic!("unknown op {op}"),
        };
        let state = [
            pool.dynamic_memory_usage() as u64,
            pool.bucket_count(),
            pool.len() as u64,
            pool.inner_usage() as u64,
        ];
        assert_eq!(state, std::array::from_fn(|i| num(&row[2 + i])), "{name}: {row:?}");
        if let Some(queue) = queue.filter(|q| *q != "*") {
            assert_eq!(
                indices(&index, pool.queued()),
                runs(queue),
                "{name}: the queue after {row:?}"
            );
        }
    }
    assert!(current.is_none());
    assert_eq!(scenarios, 61);
}
