//! Unit checks of `getrawmempool` and `getmempoolinfo` over a fake pool; the node's live pool is
//! in dashd-node's `sut::tests::mp1a11`.

use std::sync::Mutex;

use super::*;
use crate::protocol::{RPC_MISC_ERROR, RPC_TYPE_ERROR};
use crate::request::JsonRpcRequest;
use crate::testutil::FakeControl;

struct FakePool {
    entries: Vec<MempoolEntryView>,
    sequence: u64,
}

impl MempoolView for FakePool {
    fn txids_and_sequence(&self) -> (Vec<TxHash>, u64) {
        (self.entries.iter().map(|e| e.txid).collect(), self.sequence)
    }
    fn entries(&self) -> Vec<MempoolEntryView> {
        self.entries.clone()
    }
    fn info(&self) -> MempoolInfo {
        MempoolInfo {
            loaded: self.sequence > 1,
            size: self.entries.len() as u64,
            bytes: 382,
            usage: 1_664,
            total_fee: 20_000,
            max_mempool: 300_000_000,
            min_fee_per_k: self.sequence as i64 * 100,
            min_relay_fee_per_k: 1_000,
            unbroadcast_count: 3,
            pending_asset_unlocks: 2_100_000_000_000_000,
        }
    }
}

#[derive(Default)]
struct Locked(Mutex<Vec<TxHash>>);

impl IsLockView for Locked {
    fn is_locked(&self, txid: &TxHash) -> bool {
        self.0.lock().unwrap().contains(txid)
    }
    fn lock_count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

/// Internal bytes `first ‥ last`: `lo` sorts first as `uint256`, `hi` first as display hex.
fn hash(first: u8, last: u8) -> TxHash {
    let mut b = [0x33; 32];
    b[0] = first;
    b[31] = last;
    TxHash::from(b)
}

fn lo() -> TxHash {
    hash(0x01, 0x02)
}

fn hi() -> TxHash {
    hash(0x02, 0x01)
}

fn entry(txid: TxHash) -> MempoolEntryView {
    MempoolEntryView {
        txid,
        vsize: 191,
        time: 1_700_000_123,
        height: 101,
        descendant_count: 2,
        descendant_size: 382,
        ancestor_count: 1,
        ancestor_size: 191,
        fee: 10_000,
        modified_fee: -5,
        ancestor_fees: 123_456_789_012,
        descendant_fees: 20_000,
        depends: Vec::new(),
        spent_by: Vec::new(),
        unbroadcast: false,
    }
}

fn server(entries: Vec<MempoolEntryView>, sequence: u64, locked: &[TxHash]) -> RpcServer {
    let server = RpcServer::new(Arc::new(FakeControl::default()));
    let pool = FakePool {
        entries,
        sequence,
    };
    register_mempool_commands(&server, Arc::new(pool), Arc::new(Locked(Mutex::new(locked.into()))));
    server.start();
    server.set_warmup_finished();
    server
}

fn call(server: &RpcServer, params: &str) -> Result<String, (i32, String)> {
    call_method(server, "getrawmempool", params)
}

fn call_method(server: &RpcServer, method: &str, params: &str) -> Result<String, (i32, String)> {
    server
        .execute(&JsonRpcRequest {
            method: method.into(),
            params: UniValue::read(params.as_bytes()).expect("JSON"),
            ..Default::default()
        })
        .map(|v| v.write())
        .map_err(|e| {
            let code = e.code();
            match e {
                RpcError::Json {
                    message,
                    ..
                }
                | RpcError::Type(message)
                | RpcError::Runtime(message) => (code, message),
            }
        })
}

fn keys(json: &str) -> BTreeSet<String> {
    match UniValue::read(json.as_bytes()).unwrap() {
        UniValue::Obj(kv) => kv.into_iter().map(|(k, _)| k).collect(),
        UniValue::Arr(a) => a.iter().map(|v| v.get_str().unwrap().to_owned()).collect(),
        v => panic!("{}", v.write()),
    }
}

/// The two orders differ for [`lo`] and [`hi`], and `TxHash`'s `Ord` is `uint256 <`.
#[test]
fn the_fixture_hashes_order_differently_as_bytes_and_as_hex() {
    assert!(lo() < hi());
    assert!(hi().to_string() < lo().to_string());
    assert_eq!(lo().to_string(), format!("02{}01", "33".repeat(30)));
}

/// The server's checks: at most two arguments (-1 with the help), each a bool or null (-3);
/// named arguments by Core's names.
// Core: rpc/mempool.cpp:354-366, rpc/util.cpp:525-551, 622-632, 714-758
#[test]
fn arguments_are_two_optional_bools() {
    let s = server(vec![entry(lo())], 5, &[]);
    let help = dash_rpc_spec::command("getrawmempool").unwrap().help.to_owned();
    assert_eq!(call(&s, "[false, false, 1]"), Err((RPC_MISC_ERROR, help)));
    let wrong = |pos: &str, got: &str| {
        let mut o = UniValue::new_object();
        o.push_kv_end(pos, format!("JSON value of type {got} is not of expected type bool").into());
        Err((RPC_TYPE_ERROR, format!("Wrong type passed:\n{}", o.write_pretty(4, 0))))
    };
    assert_eq!(call(&s, "[1]"), wrong("Position 1 (verbose)", "number"));
    assert_eq!(call(&s, "[false, \"true\"]"), wrong("Position 2 (mempool_sequence)", "string"));
    let plain = format!("[\"{}\"]", lo());
    for params in ["[]", "[null]", "[false]", "[null, null]", "[false, false]", "{}"] {
        assert_eq!(call(&s, params).as_deref(), Ok(plain.as_str()), "{params}");
    }
    let with_sequence = format!("{{\"txids\":[\"{}\"],\"mempool_sequence\":5}}", lo());
    for params in ["[false, true]", "[null, true]", "{\"mempool_sequence\": true}"] {
        assert_eq!(call(&s, params).as_deref(), Ok(with_sequence.as_str()), "{params}");
    }
    assert_eq!(keys(&call(&s, "{\"verbose\": true}").unwrap()), keys(&plain));
}

/// Verbose with the sequence is refused, before any read of the pool.
// Core: rpc/mempool.cpp:317-320
#[test]
fn verbose_with_a_sequence_is_an_invalid_parameter() {
    let s = server(vec![entry(lo())], 5, &[]);
    let refused = Err((
        RPC_INVALID_PARAMETER,
        "Verbose results cannot contain mempool sequence values.".to_owned(),
    ));
    assert_eq!(call(&s, "[true, true]"), refused);
    assert_eq!(call(&s, "{\"verbose\": true, \"mempool_sequence\": true}"), refused);
}

/// A fresh, empty pool: `[]`, `{}`, and sequence 1.
// Core: txmempool.h:337
#[test]
fn an_empty_pool() {
    let s = server(Vec::new(), 1, &[]);
    assert_eq!(call(&s, "[]").as_deref(), Ok("[]"));
    assert_eq!(call(&s, "[true]").as_deref(), Ok("{}"));
    assert_eq!(call(&s, "[false, true]").as_deref(), Ok("{\"txids\":[],\"mempool_sequence\":1}"));
}

/// Both shapes list the same set, every entry keyed by its txid's display hex.
// Core: rpc/mempool.cpp:321-347
#[test]
fn both_shapes_list_the_same_txids() {
    let s = server(vec![entry(hi()), entry(lo())], 9, &[]);
    let plain = keys(&call(&s, "[]").unwrap());
    let want: BTreeSet<String> = [lo(), hi()].iter().map(ToString::to_string).collect();
    assert_eq!(plain, want);
    assert_eq!(keys(&call(&s, "[true]").unwrap()), want);
}

/// One entry, every field in `entryToJSON`'s order: amounts as `ValueFromAmount` (a negative
/// modified fee too), `depends` as a set of hex strings (deduplicated, display-hex order),
/// `spentby` in `uint256` order, `instantlock` as the string `IsLocked` prints and
/// `unbroadcast` as the pool's flag.
// Core: rpc/mempool.cpp:268-313, kernel/mempool_entry.h:39-52, core_write.cpp:35-46
#[test]
fn a_verbose_entry_follows_entry_to_json() {
    let child = hash(0x00, 0xff);
    let mut e = entry(child);
    e.depends = vec![lo(), hi(), lo()];
    e.spent_by = vec![hi(), lo()];
    e.unbroadcast = true;
    let s = server(vec![e.clone()], 1, &[child]);
    let body = |deps: [TxHash; 2], kids: [TxHash; 2], locked: &str, unbroadcast: bool| {
        format!(
            "{{\"{child}\":{{\"vsize\":191,\"time\":1700000123,\"height\":101,\
             \"descendantcount\":2,\"descendantsize\":382,\"ancestorcount\":1,\
             \"ancestorsize\":191,\"fees\":{{\"base\":0.00010000,\"modified\":-0.00000005,\
             \"ancestor\":1234.56789012,\"descendant\":0.00020000}},\
             \"depends\":[\"{}\",\"{}\"],\"spentby\":[\"{}\",\"{}\"],\
             \"instantlock\":\"{locked}\",\"unbroadcast\":{unbroadcast}}}}}",
            deps[0], deps[1], kids[0], kids[1]
        )
    };
    assert_eq!(call(&s, "[true]"), Ok(body([hi(), lo()], [lo(), hi()], "true", true)));
    e.unbroadcast = false;
    let s = server(vec![e], 1, &[]);
    assert_eq!(call(&s, "[true]"), Ok(body([hi(), lo()], [lo(), hi()], "false", false)));
}

/// `getmempoolinfo` takes no argument (-1 with the help) and lists the pool's figures in
/// `MempoolInfoToJSON`'s order: amounts as `ValueFromAmount`, `mempoolminfee` the larger of
/// `GetMinFee()` and the minimum relay feerate, `instantsendlocks` from InstantSend's count.
// Core: rpc/mempool.cpp:657-704, core_write.cpp:35-46
#[test]
fn mempool_info_follows_mempool_info_to_json() {
    let s = server(vec![entry(lo()), entry(hi())], 1, &[lo()]);
    let help = dash_rpc_spec::command("getmempoolinfo").unwrap().help.to_owned();
    assert_eq!(call_method(&s, "getmempoolinfo", "[true]"), Err((RPC_MISC_ERROR, help)));
    let body = |loaded: bool, min_fee: &str, locks: u32| {
        format!(
            "{{\"loaded\":{loaded},\"size\":2,\"bytes\":382,\"usage\":1664,\
             \"total_fee\":0.00020000,\"maxmempool\":300000000,\"mempoolminfee\":{min_fee},\
             \"minrelaytxfee\":0.00001000,\"instantsendlocks\":{locks},\"unbroadcastcount\":3,\
             \"pendingassetunlocks\":21000000.00000000}}"
        )
    };
    assert_eq!(call_method(&s, "getmempoolinfo", "[]"), Ok(body(false, "0.00001000", 1)));
    let s = server(vec![entry(lo()), entry(hi())], 11, &[]);
    assert_eq!(call_method(&s, "getmempoolinfo", "{}"), Ok(body(true, "0.00001100", 0)));
}
