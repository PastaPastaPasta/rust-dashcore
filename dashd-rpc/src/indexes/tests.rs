//! Unit checks of `getrawtransaction` and `getindexinfo` over fakes; the DIFF against dashd is
//! in rdashd/tests/rpc_diff.rs.

use std::sync::{Arc, Mutex};

use dash_consensus::coins::BlockUndo;
use dash_primitives::{BlockHash, Transaction, TxHash};

use super::{BlockFilterRead, IndexSeams, IndexSummary, RetrievedTx, TxLookup};
use crate::UniValue;
use crate::blockchain::tests::{
    HASH_7, Live, at, block_with, call_method, facts, hash, index_server, keys, method_err, reply,
    spend_hex, spend_undo,
};
use crate::protocol::{
    RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_MISC_ERROR,
    RPC_TYPE_ERROR,
};
use crate::rawtransaction::IsLockView;
use crate::server::RpcServer;

/// The mempool, the txindex (absent with `ready` `None`) and the running indexes, recording
/// each call.
#[derive(Default)]
struct Fake {
    ready: Option<bool>,
    mempool: Vec<Arc<Transaction>>,
    indexed: Vec<(Arc<Transaction>, BlockHash)>,
    summaries: Vec<IndexSummary>,
    locked: Vec<TxHash>,
    /// The basic filter index is running.
    filters: bool,
    /// `BlockUntilSyncedToCurrentChain` for a block lookup.
    filter_ready: bool,
    /// The one block the index holds: its hash, encoded filter and header hex.
    filter_row: Option<(BlockHash, Vec<u8>, String)>,
    calls: Mutex<Vec<String>>,
}

fn txid_of(tx: &Transaction) -> TxHash {
    dash_consensus::Tx::new(tx.clone()).txid()
}

impl TxLookup for Fake {
    fn txindex_ready(&self) -> Option<bool> {
        self.calls.lock().unwrap().push("ready".into());
        self.ready
    }

    fn get_transaction(&self, block: Option<&BlockHash>, txid: &TxHash) -> Option<RetrievedTx> {
        let at = block.map_or("none".into(), ToString::to_string);
        self.calls.lock().unwrap().push(format!("lookup {at}"));
        if block.is_none()
            && let Some(tx) = self.mempool.iter().find(|tx| txid_of(tx) == *txid)
        {
            return Some((Arc::clone(tx), None));
        }
        self.ready?;
        let (tx, hash) = self.indexed.iter().find(|(tx, _)| txid_of(tx) == *txid)?;
        block.is_none_or(|b| b == hash).then(|| (Arc::clone(tx), Some(*hash)))
    }

    fn summaries(&self) -> Vec<IndexSummary> {
        self.summaries.clone()
    }

    fn block_filter(&self, name: Option<&str>, block: Option<&BlockHash>) -> BlockFilterRead {
        let label = name.unwrap_or("default");
        let at = block.map_or_else(|| "none".to_owned(), ToString::to_string);
        self.calls.lock().unwrap().push(format!("filter {label} {at}"));
        if name.is_some_and(|n| n != "basic") {
            return BlockFilterRead::Unknown;
        }
        if !self.filters {
            return BlockFilterRead::Disabled {
                name: "basic",
            };
        }
        let Some(hash) = block else {
            return BlockFilterRead::Enabled;
        };
        let row = self
            .filter_row
            .as_ref()
            .filter(|(h, _, _)| h == hash)
            .map(|(_, filter, header)| (filter.clone(), header.clone()));
        BlockFilterRead::Looked {
            ready: self.filter_ready,
            row,
        }
    }
}

impl IsLockView for Fake {
    fn is_locked(&self, txid: &TxHash) -> bool {
        self.locked.contains(txid)
    }
    fn lock_count(&self) -> usize {
        self.locked.len()
    }
}

const GENESIS_COINBASE: [u8; 32] = [9; 32];

fn server(fake: Fake) -> (Arc<RpcServer>, Arc<Mutex<Live>>, Arc<Fake>) {
    let fake = Arc::new(fake);
    let (server, live) = index_server(IndexSeams {
        lookup: Arc::clone(&fake) as _,
        islocks: Arc::clone(&fake) as _,
        genesis_coinbase: TxHash::from(GENESIS_COINBASE),
    });
    (server, live, fake)
}

fn spend() -> Arc<Transaction> {
    let block = block_with(&[spend_hex()]);
    Arc::new(block.txs()[1].to_mutable())
}

fn coinbase() -> Arc<Transaction> {
    let block = block_with(&[spend_hex()]);
    Arc::new(block.txs()[0].to_mutable())
}

fn shown(tx: &Transaction) -> String {
    txid_of(tx).to_string()
}

fn hex_of(tx: &Transaction) -> String {
    crate::descriptor::hex(&dash_consensus::CoreSerialize::core_serialize(tx))
}

fn get(server: &RpcServer, params: &str) -> UniValue {
    call_method(server, "getrawtransaction", params).unwrap()
}

fn err(server: &RpcServer, params: &str) -> (i32, String) {
    method_err(server, "getrawtransaction", params)
}

fn miss(message: &str) -> (i32, String) {
    reply(
        RPC_INVALID_ADDRESS_OR_KEY,
        &format!("{message}. Use gettransaction for wallet transactions."),
    )
}

/// A chain whose block 1 holds the spend, with its undo data.
fn with_block(live: &Arc<Mutex<Live>>) {
    let mut live = live.lock().unwrap();
    live.indexed = true;
    live.have_data = true;
    live.facts = Some(facts(1));
    live.block = Some(Ok((facts(1), block_with(&[spend_hex()]))));
    live.undo = Some(spend_undo(7));
}

/// A txindex hit: the hex at verbosity 0, `false` or below; `TxToJSON` at 1 or `true`, with the
/// block's fields and no undo read; at 2 or more also the `fee` and each `prevout`.
// Core: v24.0.0 src/rpc/rawtransaction.cpp:420-429, :467-507
#[test]
fn an_indexed_transaction_in_each_verbosity() {
    let tx = spend();
    let (server, live, _) = server(Fake {
        ready: Some(true),
        indexed: vec![(Arc::clone(&tx), hash(1))],
        ..Fake::default()
    });
    with_block(&live);
    let id = shown(&tx);
    for v in ["", ",0", ",false", ",-1"] {
        assert_eq!(get(&server, &format!(r#"["{id}"{v}]"#)), UniValue::from(hex_of(&tx)), "{v}");
    }
    for v in [",1", ",true"] {
        live.lock().unwrap().calls.clear();
        let json = get(&server, &format!(r#"["{id}"{v}]"#));
        assert_eq!(
            keys(&json),
            [
                "txid",
                "version",
                "type",
                "size",
                "locktime",
                "vin",
                "vout",
                "hex",
                "blockhash",
                "height",
                "confirmations",
                "time",
                "blocktime",
                "instantlock",
                "instantlock_internal",
                "chainlock"
            ],
            "{v}"
        );
        assert_eq!(at(&json, "blockhash"), UniValue::from(hash(1).to_string()));
        assert_eq!(at(&json, "hex"), UniValue::from(hex_of(&tx)));
        assert_eq!(at(&json, "height").write(), "1");
        assert_eq!(at(&json, "confirmations").write(), "1");
        assert_eq!(at(&json, "time").write(), "1296688602");
        assert_eq!(live.lock().unwrap().calls, [format!("header {}", hash(1))], "{v}");
    }
    for v in [",2", ",3"] {
        live.lock().unwrap().calls.clear();
        let json = get(&server, &format!(r#"["{id}"{v}]"#));
        assert_eq!(at(&json, "fee").write(), "0.00010000", "{v}");
        assert_eq!(at(&json, "vin.0.prevout.height").write(), "7", "{v}");
        assert_eq!(keys(&at(&json, "vin.0")), ["txid", "vout", "scriptSig", "prevout", "sequence"]);
        let h = hash(1);
        assert_eq!(
            live.lock().unwrap().calls,
            [format!("header {h}"), format!("undo {h}"), format!("block {h}")],
            "{v}"
        );
    }
    // A ChainLock on the block locks it.
    live.lock().unwrap().facts.as_mut().unwrap().chainlock = true;
    let json = get(&server, &format!(r#"["{id}",1]"#));
    assert_eq!(at(&json, "instantlock"), true.into());
    assert_eq!(at(&json, "instantlock_internal"), false.into());
    assert_eq!(at(&json, "chainlock"), true.into());
}

/// Verbosity 2 prints no `prevout` or `fee` for a coinbase, a block without undo data or one
/// whose block is not read; short undo data is `vtxundo.at`'s out-of-range error.
// Core: v24.0.0 src/rpc/rawtransaction.cpp:485-507
#[test]
fn verbosity_2_reads_undo_data_only_where_core_does() {
    let (tx, cb) = (spend(), coinbase());
    let (server, live, _) = server(Fake {
        ready: Some(true),
        indexed: vec![(Arc::clone(&tx), hash(1)), (Arc::clone(&cb), hash(1))],
        ..Fake::default()
    });
    with_block(&live);
    let json = get(&server, &format!(r#"["{}",2]"#, shown(&cb)));
    assert!(at(&json, "fee").is_null() && at(&json, "vin.0.prevout").is_null());
    assert!(!live.lock().unwrap().calls.iter().any(|c| c.starts_with("undo")), "no undo read");
    let id = shown(&tx);
    live.lock().unwrap().undo_pruned = true;
    let json = get(&server, &format!(r#"["{id}",2]"#));
    assert!(at(&json, "fee").is_null(), "pruned");
    live.lock().unwrap().undo_pruned = false;
    live.lock().unwrap().block = Some(Err(crate::blockchain::BlockReadError::NotOnDisk));
    let json = get(&server, &format!(r#"["{id}",2]"#));
    assert!(at(&json, "fee").is_null(), "block not read");
    live.lock().unwrap().block = Some(Ok((facts(1), block_with(&[]))));
    let json = get(&server, &format!(r#"["{id}",2]"#));
    assert!(at(&json, "fee").is_null(), "not in the block read");
    live.lock().unwrap().block = Some(Ok((facts(1), block_with(&[spend_hex()]))));
    live.lock().unwrap().undo = Some(BlockUndo {
        txs: vec![],
    });
    let out_of_range = "vector::_M_range_check: __n (which is 0) >= this->size() (which is 0)";
    assert_eq!(err(&server, &format!(r#"["{id}",2]"#)), reply(RPC_MISC_ERROR, out_of_range));
    // An entry short of a coin per input fails the same way (D41).
    live.lock().unwrap().undo = Some(BlockUndo {
        txs: vec![dash_consensus::coins::TxUndo {
            coins: vec![],
        }],
    });
    assert_eq!(err(&server, &format!(r#"["{id}",2]"#)), reply(RPC_MISC_ERROR, out_of_range));
    // An unindexed block has no undo read at all.
    live.lock().unwrap().facts = None;
    let json = get(&server, &format!(r#"["{id}",2]"#));
    assert_eq!(at(&json, "blockhash"), UniValue::from(hash(1).to_string()));
    assert!(at(&json, "height").is_null() && at(&json, "fee").is_null());
}

/// A mempool transaction has no block fields, and its lock is InstantSend's alone.
// Core: v24.0.0 src/node/transaction.cpp:128-131, src/rpc/rawtransaction.cpp:149-166
#[test]
fn a_mempool_transaction_has_no_block_and_its_own_lock() {
    let tx = spend();
    let (server, live, _) = server(Fake {
        ready: Some(true),
        mempool: vec![Arc::clone(&tx)],
        indexed: vec![(Arc::clone(&tx), hash(1))],
        locked: vec![txid_of(&tx)],
        ..Fake::default()
    });
    with_block(&live);
    for v in ["1", "2"] {
        let json = get(&server, &format!(r#"["{}",{v}]"#, shown(&tx)));
        let k = keys(&json);
        assert_eq!(&k[k.len() - 4..], ["hex", "instantlock", "instantlock_internal", "chainlock"]);
        assert!(at(&json, "blockhash").is_null() && at(&json, "fee").is_null(), "{v}");
        assert_eq!(at(&json, "instantlock"), true.into());
        assert_eq!(at(&json, "instantlock_internal"), true.into());
        assert_eq!(at(&json, "chainlock"), false.into());
    }
    assert!(live.lock().unwrap().calls.is_empty(), "no block is read");
}

/// The txindex's readiness is read before the lookup and answers only a miss: a hit while it
/// syncs is served, a miss names why the lookup could not find it.
// Core: v24.0.0 src/rpc/rawtransaction.cpp:441-466
#[test]
fn a_miss_names_where_the_lookup_looked() {
    let tx = spend();
    let other = format!(r#"["{HASH_7}"]"#);
    for (ready, want) in [
        (
            None,
            "No such mempool transaction. Use -txindex or provide a block hash to enable blockchain transaction queries",
        ),
        (
            Some(false),
            "No such mempool transaction. Blockchain transactions are still in the process of being indexed",
        ),
        (Some(true), "No such mempool or blockchain transaction"),
    ] {
        let (server, _, fake) = server(Fake {
            ready,
            indexed: vec![(Arc::clone(&tx), hash(1))],
            ..Fake::default()
        });
        assert_eq!(err(&server, &other), miss(want), "{ready:?}");
        assert_eq!(*fake.calls.lock().unwrap(), ["ready", "lookup none"], "{ready:?}");
        if ready.is_some() {
            let found = get(&server, &format!(r#"["{}"]"#, shown(&tx)));
            assert_eq!(found, UniValue::from(hex_of(&tx)), "a hit is served: {ready:?}");
        }
    }
}

/// With a block hash: "Block hash not found" for an unknown block, the block's own transaction
/// read when the txindex finds none in it, `in_active_chain` first, and its misses; no mempool
/// lookup and no readiness read.
// Core: v24.0.0 src/rpc/rawtransaction.cpp:431-439, :449-455, :472-475;
// src/node/transaction.cpp:132-156
#[test]
fn a_given_block_is_looked_in() {
    let tx = spend();
    let (server, live, fake) = server(Fake {
        ready: Some(true),
        mempool: vec![Arc::clone(&tx)],
        indexed: vec![(Arc::clone(&tx), hash(2))],
        ..Fake::default()
    });
    let id = shown(&tx);
    let in_block = |v: &str| format!(r#"["{id}",{v},"{}"]"#, hash(1));
    assert_eq!(
        err(&server, &in_block("1")),
        reply(RPC_INVALID_ADDRESS_OR_KEY, "Block hash not found")
    );
    with_block(&live);
    live.lock().unwrap().calls.clear();
    let json = get(&server, &in_block("1"));
    assert_eq!(keys(&json)[0], "in_active_chain");
    assert_eq!(at(&json, "in_active_chain"), true.into());
    assert_eq!(at(&json, "blockhash"), UniValue::from(hash(1).to_string()));
    let h = hash(1);
    assert_eq!(
        live.lock().unwrap().calls,
        [format!("lookup {h}"), format!("block {h}"), format!("header {h}")]
    );
    assert_eq!(*fake.calls.lock().unwrap(), [format!("lookup {h}")], "no readiness read");
    assert_eq!(get(&server, &in_block("0")), UniValue::from(hex_of(&tx)));
    let json = get(&server, &in_block("2"));
    assert_eq!(at(&json, "fee").write(), "0.00010000");
    // Off the active chain.
    live.lock().unwrap().facts.as_mut().unwrap().confirmations = -1;
    let json = get(&server, &in_block("1"));
    assert_eq!(at(&json, "in_active_chain"), false.into());
    assert_eq!(
        (at(&json, "height").write(), at(&json, "confirmations").write()),
        ("-1".into(), "0".into())
    );
    assert!(at(&json, "time").is_null());
    // The txindex's own block.
    live.lock().unwrap().facts.as_mut().unwrap().confirmations = 1;
    live.lock().unwrap().calls.clear();
    let json = get(&server, &format!(r#"["{id}",1,"{}"]"#, hash(2)));
    assert_eq!(at(&json, "blockhash"), UniValue::from(hash(2).to_string()));
    assert!(!live.lock().unwrap().calls.iter().any(|c| c.starts_with("block")), "no block read");
    // Not in the block, with and without its data.
    live.lock().unwrap().block = Some(Ok((facts(1), block_with(&[]))));
    let missing = miss("No such transaction found in the provided block");
    assert_eq!(err(&server, &in_block("1")), missing);
    live.lock().unwrap().have_data = false;
    assert_eq!(err(&server, &in_block("1")), reply(RPC_MISC_ERROR, "Block not available"));
}

/// The genesis coinbase is refused before the verbosity or block is read; the parameters are
/// Core's `ParseHashV` names, and the verbosity is a bool or an `int`.
// Core: v24.0.0 src/rpc/rawtransaction.cpp:412-429
#[test]
fn the_genesis_coinbase_and_the_arguments_are_refused_as_core() {
    let (server, _, fake) = server(Fake::default());
    let genesis = TxHash::from(GENESIS_COINBASE).to_string();
    assert_eq!(
        err(&server, &format!(r#"["{genesis}","x","zz"]"#)),
        reply(
            RPC_INVALID_ADDRESS_OR_KEY,
            "The genesis block coinbase is not considered an ordinary transaction and cannot be retrieved"
        )
    );
    assert!(fake.calls.lock().unwrap().is_empty());
    assert_eq!(
        err(&server, r#"["zz"]"#),
        reply(RPC_INVALID_PARAMETER, "parameter 1 must be of length 64 (not 2, for 'zz')")
    );
    assert_eq!(
        err(&server, &format!(r#"["{HASH_7}",0,"zz"]"#)),
        reply(RPC_INVALID_PARAMETER, "parameter 3 must be of length 64 (not 2, for 'zz')")
    );
    let (code, _) = err(&server, &format!(r#"["{HASH_7}","1"]"#));
    assert_eq!(code, RPC_TYPE_ERROR);
}

/// No chain: Core's `EnsureChainman` before anything is read.
#[test]
fn without_a_chain_the_chainman_is_missing() {
    let server = crate::blockchain::tests::unloaded_index_server(Arc::new(Fake::default()));
    assert_eq!(
        method_err(&server, "getrawtransaction", r#"["zz"]"#),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
}

fn summary(name: &'static str, synced: bool, height: u32) -> IndexSummary {
    IndexSummary {
        name,
        synced,
        best_block_height: height,
    }
}

/// `getindexinfo`: every running index in Core's order, or the one named; `{}` for none.
// Core: v24.0.0 src/rpc/node.cpp:1166-1232
#[test]
fn getindexinfo_lists_the_running_indexes() {
    let (empty, _, _) = server(Fake::default());
    let info = |params: &str| call_method(&empty, "getindexinfo", params).unwrap().write();
    assert_eq!(info("[]"), "{}");
    assert_eq!(info(r#"["txindex"]"#), "{}");
    let (server, _, _) = server(Fake {
        summaries: vec![
            summary("txindex", true, 5),
            summary("basic block filter index", false, 3),
            summary("coinstatsindex", false, 0),
        ],
        ..Fake::default()
    });
    let info = |params: &str| call_method(&server, "getindexinfo", params).unwrap().write();
    let all = r#"{"txindex":{"synced":true,"best_block_height":5},"coinstatsindex":{"synced":false,"best_block_height":0},"basic block filter index":{"synced":false,"best_block_height":3}}"#;
    assert_eq!(info("[]"), all);
    assert_eq!(info(r#"[""]"#), all);
    assert_eq!(info(r#"["txindex"]"#), r#"{"txindex":{"synced":true,"best_block_height":5}}"#);
    assert_eq!(
        info(r#"{"index_name":"coinstatsindex"}"#),
        r#"{"coinstatsindex":{"synced":false,"best_block_height":0}}"#
    );
    assert_eq!(info(r#"["nosuch"]"#), "{}");
}

fn filter_err(server: &RpcServer, params: &str) -> (i32, String) {
    method_err(server, "getblockfilter", params)
}

fn indexed(live: &Arc<Mutex<Live>>, scripts_invalid: bool) {
    let mut live = live.lock().unwrap();
    live.indexed = true;
    live.scripts_invalid = scripts_invalid;
}

fn on_index(ready: bool, row: Option<(BlockHash, Vec<u8>, String)>) -> Fake {
    Fake {
        filters: true,
        filter_ready: ready,
        filter_row: row,
        ..Fake::default()
    }
}

/// The default and an explicit `"basic"` both return the encoded filter and its header, in
/// that key order, including when the index has not caught up but already holds the row.
// Core: v24.0.2 src/rpc/blockchain.cpp:3067-3084 getblockfilter
#[test]
fn getblockfilter_returns_the_filter_and_header() {
    let block = hash(1);
    let bytes = vec![0x01, 0x6a, 0x05, 0xc0];
    let header = "ab".repeat(32);
    let row = Some((block, bytes.clone(), header.clone()));
    let (ready, live, fake) = server(on_index(true, row.clone()));
    indexed(&live, false);
    let id = block.to_string();
    let want = format!(r#"{{"filter":"{}","header":"{header}"}}"#, crate::descriptor::hex(&bytes));
    for (params, label) in
        [(format!(r#"["{id}"]"#), "default"), (format!(r#"["{id}","basic"]"#), "basic")]
    {
        fake.calls.lock().unwrap().clear();
        live.lock().unwrap().calls.clear();
        let json = call_method(&ready, "getblockfilter", &params).unwrap();
        assert_eq!(keys(&json), ["filter", "header"], "{params}");
        assert_eq!(json.write(), want, "{params}");
        assert_eq!(
            fake.calls.lock().unwrap().as_slice(),
            [format!("filter {label} none"), format!("filter {label} {id}")],
            "{params}"
        );
        assert_eq!(live.lock().unwrap().calls, [format!("lookup {id}")], "{params}");
    }

    let (indexing, live, _) = server(on_index(false, row));
    indexed(&live, false);
    let json = call_method(&indexing, "getblockfilter", &format!(r#"["{id}"]"#)).unwrap();
    assert_eq!(json.write(), want);
}

/// The header string is Core's `uint256::GetHex()`: the internal bytes reversed. A palindrome
/// would hide a second reversal.
// Core: v24.0.2 src/rpc/blockchain.cpp:3078-3080 getblockfilter; uint256.h GetHex
#[test]
fn getblockfilter_header_is_byte_reversed() {
    let mut bytes = [0u8; 32];
    bytes[0] = 0x01;
    bytes[31] = 0xef;
    let forward: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let reversed: String = bytes.iter().rev().map(|b| format!("{b:02x}")).collect();
    assert_ne!(forward, reversed);
    let block = hash(1);
    let (server, live, _) = server(on_index(true, Some((block, vec![0x01], reversed.clone()))));
    indexed(&live, false);
    let json = call_method(&server, "getblockfilter", &format!(r#"["{block}"]"#)).unwrap().write();
    assert!(json.contains(&format!(r#""header":"{reversed}""#)), "{json}");
    assert!(!json.contains(&format!(r#""header":"{forward}""#)), "{json}");
}

/// An unknown type is refused before the chain is read, and a known type with the index off
/// names that type. Both precede `LookupBlockIndex`, even with no chain.
// Core: v24.0.2 src/rpc/blockchain.cpp:3073-3076
#[test]
fn an_unknown_filter_type_or_a_disabled_index_is_refused() {
    let (off, live, fake) = server(Fake::default());
    let id = hash(1).to_string();
    assert_eq!(
        filter_err(&off, &format!(r#"["{id}","nope"]"#)),
        reply(RPC_INVALID_ADDRESS_OR_KEY, "Unknown filtertype")
    );
    assert_eq!(
        filter_err(&off, &format!(r#"["{id}","BASIC"]"#)),
        reply(RPC_INVALID_ADDRESS_OR_KEY, "Unknown filtertype")
    );
    assert!(live.lock().unwrap().calls.is_empty());
    for params in [format!(r#"["{id}"]"#), format!(r#"["{id}","basic"]"#)] {
        assert_eq!(
            filter_err(&off, &params),
            reply(RPC_MISC_ERROR, "Index is not enabled for filtertype basic"),
            "{params}"
        );
    }
    assert!(live.lock().unwrap().calls.is_empty(), "{:?}", fake.calls.lock().unwrap());

    let unloaded = crate::blockchain::tests::unloaded_index_server(Arc::new(Fake::default()));
    assert_eq!(
        method_err(&unloaded, "getblockfilter", &format!(r#"["{HASH_7}","nope"]"#)),
        reply(RPC_INVALID_ADDRESS_OR_KEY, "Unknown filtertype")
    );
    assert_eq!(
        method_err(&unloaded, "getblockfilter", &format!(r#"["{HASH_7}"]"#)),
        reply(RPC_MISC_ERROR, "Index is not enabled for filtertype basic")
    );
    let unloaded = crate::blockchain::tests::unloaded_index_server(Arc::new(on_index(true, None)));
    assert_eq!(
        method_err(&unloaded, "getblockfilter", &format!(r#"["{HASH_7}"]"#)),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
}

/// A hash the block index does not hold, with the index on.
// Core: v24.0.2 src/rpc/blockchain.cpp:3067-3074
#[test]
fn a_missing_block_is_not_found_before_the_index_is_waited_on() {
    let (on, live, fake) = server(on_index(true, None));
    let id = hash(1).to_string();
    assert_eq!(
        filter_err(&on, &format!(r#"["{id}"]"#)),
        reply(RPC_INVALID_ADDRESS_OR_KEY, "Block not found")
    );
    assert_eq!(fake.calls.lock().unwrap().as_slice(), ["filter default none".to_string()]);
    assert_eq!(live.lock().unwrap().calls, [format!("lookup {id}")]);
    assert_eq!(
        filter_err(&on, r#"["zz"]"#),
        reply(RPC_INVALID_PARAMETER, "blockhash must be of length 64 (not 2, for 'zz')")
    );
}

/// The three misses: never connected, still indexing, and a ready index with no row.
// Core: v24.0.2 src/rpc/blockchain.cpp:3076-3082
#[test]
fn a_missing_filter_names_why() {
    let id = hash(1).to_string();
    let params = format!(r#"["{id}"]"#);
    let (never, live, _) = server(on_index(true, None));
    indexed(&live, true);
    assert_eq!(
        filter_err(&never, &params),
        reply(
            RPC_INVALID_ADDRESS_OR_KEY,
            "Filter not found. Block was not connected to active chain."
        )
    );
    let (behind, live, _) = server(on_index(false, None));
    indexed(&live, false);
    assert_eq!(
        filter_err(&behind, &params),
        reply(
            RPC_MISC_ERROR,
            "Filter not found. Block filters are still in the process of being indexed."
        )
    );
    let (corrupt, live, fake) = server(on_index(true, None));
    indexed(&live, false);
    assert_eq!(
        filter_err(&corrupt, &params),
        reply(
            RPC_INTERNAL_ERROR,
            "Filter not found. This error is unexpected and indicates index corruption."
        )
    );
    assert_eq!(
        fake.calls.lock().unwrap().as_slice(),
        ["filter default none".to_string(), format!("filter default {id}")]
    );
}
