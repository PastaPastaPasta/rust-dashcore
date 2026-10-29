use std::sync::{Arc, Mutex};

use dash_consensus::coins::BlockUndo;
use dash_consensus::coins::stats::{CoinStatsHashType, UtxoSetStats};
use dash_consensus::{Block, Header};
use dash_primitives::{BlockHash, BlockHeader};

use super::{
    BlockCommandError, BlockReadError, ChainTipInfo, Chainman, HeaderFacts, IndexStatus, NodeChain,
    ProcessedBlock, PublishedChain, TxOutSetError, TxOutSetInfo, UndoReadError,
    register_blockchain_commands,
};
use crate::UniValue;
use crate::core_write::TxJsonParams;
use crate::mining::{StateCatcher, ValidationState};
use crate::protocol::{
    RPC_CLIENT_NOT_CONNECTED, RPC_DATABASE_ERROR, RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY,
    RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RPC_TYPE_ERROR, RpcError,
};
use crate::request::JsonRpcRequest;
use crate::server::RpcServer;
use crate::testutil::FakeControl;

/// A chain of `len` blocks, block `h`'s hash `h` in its first byte; `None` before it loads.
#[derive(Default)]
struct FakeChain {
    len: Mutex<Option<i32>>,
}

/// Block `h`'s hash: `h` in its first byte.
pub(super) fn hash(h: u8) -> BlockHash {
    let mut b = [0; 32];
    b[0] = h;
    BlockHash::from(b)
}

struct Active(i32);

impl PublishedChain for Active {
    fn height(&self) -> i32 {
        self.0 - 1
    }
    fn block_hash(&self, height: i32) -> BlockHash {
        assert!((0..self.0).contains(&height), "read inside the chain");
        hash(height as u8)
    }
}

impl NodeChain for FakeChain {
    fn published(&self) -> Option<Box<dyn PublishedChain>> {
        self.len.lock().unwrap().map(|n| Box::new(Active(n)) as _)
    }

    fn chainman(&self) -> Option<Box<dyn super::Chainman>> {
        unreachable!("getblockhash reads the published chain")
    }
}

fn server(chain: &Arc<FakeChain>) -> Arc<RpcServer> {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    crate::commands::register_control_commands(&server);
    register_blockchain_commands(&server, Arc::clone(chain) as _, tx_json());
    server.start();
    server.set_warmup_finished();
    server
}

fn call(server: &RpcServer, params: &str) -> Result<UniValue, RpcError> {
    call_method(server, "getblockhash", params)
}

pub(super) fn call_method(
    server: &RpcServer,
    method: &str,
    params: &str,
) -> Result<UniValue, RpcError> {
    server.execute(&JsonRpcRequest {
        method: method.into(),
        params: UniValue::read(params.as_bytes()).unwrap(),
        ..Default::default()
    })
}

/// The error's code and message, as the reply writes them.
fn err(server: &RpcServer, params: &str) -> (i32, String) {
    let e = call(server, params).unwrap_err();
    (e.code(), e.to_object().write())
}

pub(super) fn reply(code: i32, message: &str) -> (i32, String) {
    (code, format!(r#"{{"code":{code},"message":"{message}"}}"#))
}

#[test]
fn getblockhash_reads_the_active_chain_at_a_height() {
    let chain = Arc::new(FakeChain::default());
    *chain.len.lock().unwrap() = Some(3);
    let server = server(&chain);
    let hex = |h: u8| hash(h).to_string();
    for (params, h) in [("[0]", 0), ("[2]", 2), (r#"{"height":1}"#, 1)] {
        assert_eq!(call(&server, params).unwrap(), UniValue::from(hex(h)), "{params}");
    }
    // Core's `GetHex`: the hash's bytes reversed.
    assert!(hex(1).ends_with("01"), "{}", hex(1));
    let out_of_range = reply(RPC_INVALID_PARAMETER, "Block height out of range");
    for params in ["[3]", "[-1]", "[2147483647]", "[-2147483648]"] {
        assert_eq!(err(&server, params), out_of_range, "{params}");
    }
    // `getInt<int>`: a number that is not an int.
    let not_int = reply(RPC_MISC_ERROR, "JSON integer out of range");
    for params in ["[2147483648]", "[1.5]", "[1e0]"] {
        assert_eq!(err(&server, params), not_int, "{params}");
    }
}

#[test]
fn getblockhash_without_a_chain_is_cores_missing_chainman() {
    let chain = Arc::new(FakeChain::default());
    let server = server(&chain);
    // The chainman check comes before the height is read.
    for params in ["[0]", "[1.5]", "[-1]"] {
        assert_eq!(
            err(&server, params),
            reply(RPC_INTERNAL_ERROR, "Node chainman not found"),
            "{params}"
        );
    }
    // `Height()` of an empty chain is −1: no height is in range.
    *chain.len.lock().unwrap() = Some(0);
    assert_eq!(err(&server, "[0]"), reply(RPC_INVALID_PARAMETER, "Block height out of range"));
    *chain.len.lock().unwrap() = Some(1);
    assert!(call(&server, "[0]").is_ok());
}

#[test]
fn getblockcount_is_the_active_chains_height() {
    let chain = Arc::new(FakeChain::default());
    let server = server(&chain);
    let count = |params| call_method(&server, "getblockcount", params);
    let e = count("[]").unwrap_err();
    assert_eq!(
        (e.code(), e.to_object().write()),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
    // `Height()` of an empty chain is −1.
    for (len, height) in [(0, -1), (1, 0), (3, 2)] {
        *chain.len.lock().unwrap() = Some(len);
        assert_eq!(count("[]").unwrap(), UniValue::from(height), "{len} blocks");
    }
    // `IsValidNumArgs`: no argument; one is the help text, a runtime error.
    let e = count("[1]").unwrap_err();
    assert!(e.to_object().write().contains("getblockcount"), "{}", e.to_object().write());
}

/// What the chainman answers, and what the commands asked of it.
#[derive(Default)]
pub(super) struct Live {
    pub(super) tips: Vec<ChainTipInfo>,
    pub(super) facts: Option<HeaderFacts>,
    /// The block commands' answer.
    pub(super) answer: Option<Result<(), BlockCommandError>>,
    /// Each call: the command and its arguments.
    pub(super) calls: Vec<String>,
    /// `gettxoutsetinfo`'s scan; `None` is an unreadable set.
    pub(super) utxo: Option<Result<TxOutSetInfo, TxOutSetError>>,
    /// `getblock`'s read.
    pub(super) block: Option<Result<(HeaderFacts, Arc<Block>), BlockReadError>>,
    /// The undo read; `None` is an unreadable record.
    pub(super) undo: Option<BlockUndo>,
    /// `block_subsidy`'s answer.
    pub(super) subsidy: Option<i64>,
    /// Whether `LookupBlockIndex` finds a block.
    pub(super) indexed: bool,
    /// The undo read finds the block pruned.
    pub(super) undo_pruned: bool,
}

/// A loaded chain of `len` blocks, as [`FakeChain`], whose chainman is [`Live`]; `None` for
/// the chain before it loads.
struct LiveChain {
    len: i32,
    live: Option<Arc<Mutex<Live>>>,
}

struct LiveChainman(Arc<Mutex<Live>>);

impl NodeChain for LiveChain {
    fn published(&self) -> Option<Box<dyn PublishedChain>> {
        self.live.as_ref().map(|_| Box::new(Active(self.len)) as _)
    }

    fn chainman(&self) -> Option<Box<dyn Chainman>> {
        self.live.as_ref().map(|l| Box::new(LiveChainman(Arc::clone(l))) as _)
    }
}

impl LiveChainman {
    fn answer(&self, call: String) -> Result<(), BlockCommandError> {
        let mut live = self.0.lock().unwrap();
        live.calls.push(call);
        live.answer.expect("an answer set by the test")
    }
}

impl Chainman for LiveChainman {
    fn lookup_block_index(&self, hash: &BlockHash) -> Option<IndexStatus> {
        let mut live = self.0.lock().unwrap();
        live.calls.push(format!("lookup {hash}"));
        live.indexed.then_some(IndexStatus {
            valid_scripts: true,
            failed: false,
        })
    }

    fn process_new_block(&self, _: Arc<Block>, _: bool, _: &Arc<StateCatcher>) -> ProcessedBlock {
        unreachable!("no blockchain command processes a block")
    }

    fn chain_tips(&self) -> Vec<ChainTipInfo> {
        self.0.lock().unwrap().tips.clone()
    }

    fn header_facts(&self, hash: &BlockHash) -> Option<HeaderFacts> {
        let mut live = self.0.lock().unwrap();
        live.calls.push(format!("header {hash}"));
        live.facts.clone()
    }

    fn read_block(&self, hash: &BlockHash) -> Result<(HeaderFacts, Arc<Block>), BlockReadError> {
        let mut live = self.0.lock().unwrap();
        live.calls.push(format!("block {hash}"));
        live.block.clone().expect("a block read set by the test")
    }

    fn read_undo(&self, hash: &BlockHash) -> Result<BlockUndo, UndoReadError> {
        let mut live = self.0.lock().unwrap();
        live.calls.push(format!("undo {hash}"));
        if live.undo_pruned {
            return Err(UndoReadError::Pruned);
        }
        live.undo.clone().ok_or(UndoReadError::Unreadable)
    }

    fn block_subsidy(&self, hash: &BlockHash) -> Option<i64> {
        let mut live = self.0.lock().unwrap();
        live.calls.push(format!("subsidy {hash}"));
        live.subsidy
    }

    fn invalidate_block(&self, hash: BlockHash) -> Result<(), BlockCommandError> {
        self.answer(format!("invalidate {hash}"))
    }

    fn reconsider_block(&self, hash: BlockHash, ignore: bool) -> Result<(), BlockCommandError> {
        self.answer(format!("reconsider {hash} {ignore}"))
    }

    fn precious_block(&self, hash: BlockHash) -> Result<(), BlockCommandError> {
        self.answer(format!("precious {hash}"))
    }

    fn sync_validation_queue(&self) {
        self.0.lock().unwrap().calls.push("sync".into());
    }

    fn process_new_block_header(&self, _: Header) -> ValidationState {
        unreachable!("no blockchain command processes a header")
    }

    fn txout_set_info(&self, hash_type: CoinStatsHashType) -> Result<TxOutSetInfo, TxOutSetError> {
        let mut live = self.0.lock().unwrap();
        live.calls.push(format!("utxo {hash_type:?}"));
        live.utxo.clone().unwrap_or(Err(TxOutSetError::Unreadable))
    }
}

pub(super) fn live_server(len: i32) -> (Arc<RpcServer>, Arc<Mutex<Live>>) {
    let live = Arc::new(Mutex::new(Live::default()));
    let chain = Arc::new(LiveChain {
        len,
        live: Some(Arc::clone(&live)),
    });
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    register_blockchain_commands(&server, chain as _, tx_json());
    server.start();
    server.set_warmup_finished();
    (server, live)
}

/// Regtest's addresses, without `-deprecatedrpc=service`.
fn tx_json() -> TxJsonParams {
    TxJsonParams {
        key_io: crate::descriptor::tests::regtest(),
        service: false,
    }
}

pub(super) fn unloaded_server() -> Arc<RpcServer> {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    register_blockchain_commands(
        &server,
        Arc::new(LiveChain {
            len: 0,
            live: None,
        }) as _,
        tx_json(),
    );
    server.start();
    server.set_warmup_finished();
    server
}

/// The error's code and reply object.
pub(super) fn method_err(server: &RpcServer, method: &str, params: &str) -> (i32, String) {
    let e = call_method(server, method, params).unwrap_err();
    (e.code(), e.to_object().write())
}

pub(super) const HASH_7: &str = "0000000000000000000000000000000000000000000000000000000000000007";

#[test]
fn getbestblockhash_is_the_active_tip() {
    let (server, _) = live_server(3);
    assert_eq!(call_method(&server, "getbestblockhash", "[]").unwrap(), hash(2).to_string().into());
    assert_eq!(
        method_err(&unloaded_server(), "getbestblockhash", "[]"),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
}

#[test]
fn a_block_hash_argument_is_cores_parse_hash_v() {
    let (server, live) = live_server(1);
    live.lock().unwrap().answer = Some(Ok(()));
    let short = r#"["00ff"]"#;
    assert_eq!(
        method_err(&server, "invalidateblock", short),
        reply(RPC_INVALID_PARAMETER, "blockhash must be of length 64 (not 4, for '00ff')")
    );
    let not_hex = format!(r#"["{}"]"#, "g".repeat(64));
    assert_eq!(
        method_err(&server, "preciousblock", &not_hex),
        reply(
            RPC_INVALID_PARAMETER,
            &format!("blockhash must be hexadecimal string (not '{}')", "g".repeat(64))
        )
    );
    // `getblockheader` names its argument `hash`.
    assert_eq!(
        method_err(&server, "getblockheader", short),
        reply(RPC_INVALID_PARAMETER, "hash must be of length 64 (not 4, for '00ff')")
    );
    // A hash is read as Core's `uint256S`: the hex is the reversed bytes.
    call_method(&server, "invalidateblock", &format!(r#"["{HASH_7}"]"#)).unwrap();
    assert_eq!(live.lock().unwrap().calls, [format!("invalidate {}", hash_with_last(7))]);
}

/// The hash whose `GetHex` ends in `b`.
fn hash_with_last(b: u8) -> BlockHash {
    hash(b)
}

#[test]
fn the_block_commands_map_the_lookup_and_the_abort_to_cores_errors() {
    let (server, live) = live_server(1);
    let arg = format!(r#"["{HASH_7}"]"#);
    for method in ["invalidateblock", "reconsiderblock", "preciousblock"] {
        live.lock().unwrap().answer = Some(Ok(()));
        assert_eq!(call_method(&server, method, &arg).unwrap(), UniValue::Null, "{method}");
        live.lock().unwrap().answer = Some(Err(BlockCommandError::NotFound));
        assert_eq!(
            method_err(&server, method, &arg),
            reply(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"),
            "{method}"
        );
        live.lock().unwrap().answer = Some(Err(BlockCommandError::Aborted));
        assert_eq!(method_err(&server, method, &arg).0, RPC_DATABASE_ERROR, "{method}");
    }
    // `ignore_chainlocks` defaults to false.
    live.lock().unwrap().calls.clear();
    live.lock().unwrap().answer = Some(Ok(()));
    call_method(&server, "reconsiderblock", &format!(r#"["{HASH_7}", true]"#)).unwrap();
    call_method(&server, "reconsiderblock", &arg).unwrap();
    let h = hash_with_last(7);
    assert_eq!(
        live.lock().unwrap().calls,
        [format!("reconsider {h} true"), format!("reconsider {h} false")]
    );
}

#[test]
fn reconsiderblock_reaches_the_chainman_before_it_parses() {
    let server = unloaded_server();
    let chainman = reply(RPC_INTERNAL_ERROR, "Node chainman not found");
    assert_eq!(method_err(&server, "reconsiderblock", r#"["00"]"#), chainman);
    // The others parse first.
    for method in ["invalidateblock", "preciousblock"] {
        assert_eq!(method_err(&server, method, r#"["00"]"#).0, RPC_INVALID_PARAMETER, "{method}");
        assert_eq!(method_err(&server, method, &format!(r#"["{HASH_7}"]"#)), chainman);
    }
}

#[test]
fn syncwithvalidationinterfacequeue_syncs_when_the_chain_is_loaded() {
    let (server, live) = live_server(1);
    let sync = |s: &RpcServer| call_method(s, "syncwithvalidationinterfacequeue", "[]").unwrap();
    assert_eq!(sync(&server), UniValue::Null);
    assert_eq!(live.lock().unwrap().calls, ["sync"]);
    assert_eq!(sync(&unloaded_server()), UniValue::Null);
}

fn tip(height: u32, branchlen: u32, status: &'static str) -> ChainTipInfo {
    ChainTipInfo {
        height,
        hash: hash(height as u8),
        bits: 0x207f_ffff,
        chain_work: format!("{:064x}", 2 * (height + 1)),
        branchlen,
        fork_point: hash((height - branchlen) as u8),
        status,
    }
}

#[test]
fn getchaintips_filters_by_branch_length_before_it_counts() {
    let (server, live) = live_server(1);
    live.lock().unwrap().tips = vec![
        tip(5, 0, "active"),
        tip(4, 2, "valid-fork"),
        tip(3, 1, "invalid"),
        tip(2, 2, "headers-only"),
    ];
    let heights = |params: &str| -> Vec<String> {
        let res = call_method(&server, "getchaintips", params).unwrap();
        res.get_values()
            .unwrap()
            .iter()
            .map(|t| t.write())
            .map(|t| t.split(r#""height":"#).nth(1).unwrap().split(',').next().unwrap().to_owned())
            .collect()
    };
    assert_eq!(heights("[]"), ["5", "4", "3", "2"]);
    assert_eq!(heights("[2]"), ["5", "4"]);
    // Core's `branchLen < nBranchMin` skips before `nCountMax-- < 1` counts.
    assert_eq!(heights("[1, 2]"), ["4"]);
    assert_eq!(heights("[0]"), Vec::<String>::new());
    assert_eq!(heights("[-5, 1]"), Vec::<String>::new());
    let one = call_method(&server, "getchaintips", "[1]").unwrap().write();
    assert_eq!(
        one,
        format!(
            r#"[{{"height":5,"hash":"{}","difficulty":4.656542373906925e-10,"chainwork":"{:064x}","branchlen":0,"forkpoint":"{}","status":"active"}}]"#,
            hash(5),
            12,
            hash(5)
        )
    );
}

pub(super) fn facts(height: u32) -> HeaderFacts {
    HeaderFacts {
        hash: hash(height as u8),
        header: BlockHeader {
            version: 0x2000_0000,
            prev_hash: hash(height.wrapping_sub(1) as u8),
            merkle_root: Default::default(),
            time: 1_296_688_602,
            bits: 0x207f_ffff,
            nonce: 2,
        },
        height,
        confirmations: 1,
        median_time_past: 1_296_688_601,
        chain_work: format!("{:064x}", 2),
        n_tx: 1,
        next: None,
        chainlock: false,
    }
}

#[test]
fn getblockheader_is_cores_blockheader_to_json() {
    let (server, live) = live_server(1);
    let arg = format!(r#"["{HASH_7}"]"#);
    assert_eq!(
        method_err(&server, "getblockheader", &arg),
        reply(RPC_INVALID_ADDRESS_OR_KEY, "Block not found")
    );
    let mut f = facts(1);
    f.next = Some(hash(2));
    f.confirmations = 2;
    f.chainlock = true;
    live.lock().unwrap().facts = Some(f.clone());
    let json = call_method(&server, "getblockheader", &arg).unwrap().write();
    assert_eq!(
        json,
        format!(
            concat!(
                r#"{{"hash":"{}","confirmations":2,"height":1,"version":536870912,"versionHex":"20000000","#,
                r#""merkleroot":"{}","time":1296688602,"mediantime":1296688601,"nonce":2,"bits":"207fffff","#,
                r#""difficulty":4.656542373906925e-10,"chainwork":"{:064x}","nTx":1,"previousblockhash":"{}","#,
                r#""nextblockhash":"{}","chainlock":true}}"#
            ),
            hash(1),
            f.header.merkle_root,
            2,
            hash(0),
            hash(2)
        )
    );
    // Genesis has no previous block; the tip no next one.
    live.lock().unwrap().facts = Some(facts(0));
    let json = call_method(&server, "getblockheader", &arg).unwrap().write();
    assert!(!json.contains("previousblockhash") && !json.contains("nextblockhash"), "{json}");
    // `verbose = false`: the 80 serialized bytes.
    let hex = call_method(&server, "getblockheader", &format!(r#"["{HASH_7}", false]"#)).unwrap();
    let hex = hex.get_str().unwrap();
    assert_eq!(hex.len(), 160);
    assert!(hex.starts_with("00000020"), "{hex}");
    // A non-bool `verbose` is the argument check's type error.
    assert_eq!(
        method_err(&server, "getblockheader", &format!(r#"["{HASH_7}", 1]"#)).0,
        RPC_TYPE_ERROR
    );
}

fn utxo(hashed: bool) -> TxOutSetInfo {
    let mut h = [0; 32];
    h[0] = 0xab;
    TxOutSetInfo {
        best_block: hash(9),
        height: 9,
        stats: UtxoSetStats {
            transactions: 9,
            txouts: 10,
            bogosize: 750,
            total_amount: Some(450_000_000_001),
            hash: hashed.then_some(h),
        },
        disk_size: 1234,
    }
}

/// Core's `gettxoutsetinfo` without coinstatsindex: the hash type's field (printed as
/// `GetHex`), `transactions` and `disk_size`, and the index-only arguments refused.
// Core: rpc/blockchain.cpp:1229-1326
#[test]
fn gettxoutsetinfo_reports_the_coins_at_their_best_block() {
    let (server, live) = live_server(10);
    live.lock().unwrap().utxo = Some(Ok(utxo(true)));
    let json = call_method(&server, "gettxoutsetinfo", "[]").unwrap().write();
    assert_eq!(
        json,
        format!(
            concat!(
                r#"{{"height":9,"bestblock":"{}","txouts":10,"bogosize":750,"#,
                r#""hash_serialized_2":"{}ab","total_amount":4500.00000001,"transactions":9,"#,
                r#""disk_size":1234}}"#
            ),
            hash(9),
            "00".repeat(31)
        )
    );
    let muhash = call_method(&server, "gettxoutsetinfo", r#"["muhash"]"#).unwrap().write();
    assert!(muhash.contains(r#""muhash":""#) && !muhash.contains("hash_serialized_2"), "{muhash}");
    live.lock().unwrap().utxo = Some(Ok(utxo(false)));
    let none = call_method(&server, "gettxoutsetinfo", r#"["none", null, false]"#).unwrap().write();
    assert!(!none.contains("muhash") && !none.contains("hash_serialized"), "{none}");
    assert_eq!(live.lock().unwrap().calls, ["utxo HashSerialized", "utxo MuHash", "utxo None"]);

    assert_eq!(
        method_err(&server, "gettxoutsetinfo", r#"["sha256"]"#),
        reply(RPC_INVALID_PARAMETER, "'sha256' is not a valid hash_type")
    );
    for params in [r#"["muhash", 5]"#, r#"["none", "00"]"#, r#"[null, 0]"#] {
        assert_eq!(
            method_err(&server, "gettxoutsetinfo", params),
            reply(RPC_INVALID_PARAMETER, "Querying specific block heights requires coinstatsindex"),
            "{params}"
        );
    }
    live.lock().unwrap().utxo = None;
    assert_eq!(
        method_err(&server, "gettxoutsetinfo", "[]"),
        reply(RPC_INTERNAL_ERROR, "Unable to read UTXO set")
    );
    // An interrupted scan is Core's `RpcInterruptionPoint` throw.
    live.lock().unwrap().utxo = Some(Err(TxOutSetError::Interrupted));
    assert_eq!(
        method_err(&server, "gettxoutsetinfo", "[]"),
        reply(RPC_CLIENT_NOT_CONNECTED, "Shutting down")
    );
    assert_eq!(live.lock().unwrap().calls.len(), 5, "no scan for a refused argument");
    // The hash type is parsed before the chainman is reached.
    let unloaded = unloaded_server();
    assert_eq!(method_err(&unloaded, "gettxoutsetinfo", r#"["x"]"#).0, RPC_INVALID_PARAMETER);
    assert_eq!(
        method_err(&unloaded, "gettxoutsetinfo", "[]"),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
}

/// A block of one coinbase on `facts(1)`'s header; with `payload`, the coinbase is a version 3
/// `TRANSACTION_COINBASE` carrying it.
pub(super) fn block(payload: Option<&str>) -> Arc<Block> {
    let mut header = dash_consensus::CoreSerialize::core_serialize(&facts(1).header);
    let mut hex: String = header.drain(..).map(|b| format!("{b:02x}")).collect();
    hex.push_str("01");
    hex.push_str(if payload.is_some() {
        "03000500"
    } else {
        "01000000"
    });
    hex.push_str(&format!("01{}ffffffff0151ffffffff01{}0151", "00".repeat(32), "00".repeat(8)));
    hex.push_str("00000000");
    if let Some(p) = payload {
        hex.push_str(&format!("{:02x}{p}", p.len() / 2));
    }
    Arc::new(crate::mining::decode_hex_blk(&hex).expect("a block"))
}

#[test]
fn getblock_is_cores_block_to_json_over_get_block_checked() {
    let (server, live) = live_server(1);
    let arg = |rest: &str| format!(r#"["{HASH_7}"{rest}]"#);
    for (read, code, message) in [
        (BlockReadError::NotFound, RPC_INVALID_ADDRESS_OR_KEY, "Block not found"),
        (BlockReadError::Pruned, RPC_MISC_ERROR, "Block not available (pruned data)"),
        (BlockReadError::NotOnDisk, RPC_MISC_ERROR, "Block not found on disk"),
    ] {
        live.lock().unwrap().block = Some(Err(read));
        assert_eq!(method_err(&server, "getblock", &arg("")), reply(code, message));
    }
    let b = block(None);
    live.lock().unwrap().block = Some(Ok((facts(1), Arc::clone(&b))));
    // Verbosity 1 (the default, or `true`): the header's JSON, then `size` and the txids.
    let json = call_method(&server, "getblock", &arg("")).unwrap().write();
    assert_eq!(call_method(&server, "getblock", &arg(", true")).unwrap().write(), json);
    assert_eq!(call_method(&server, "getblock", &arg(", 1")).unwrap().write(), json);
    let tail = format!(r#","size":{},"tx":["{}"]}}"#, b.size(), b.txs()[0].txid());
    assert!(json.ends_with(&tail), "{json}");
    let mut want = blockheader_json(&facts(1));
    want.truncate(want.len() - 1);
    assert_eq!(json, format!("{want}{tail}"));
    // Verbosity 0 (or below, or `false`): the block's bytes.
    let bytes: String = dash_consensus::CoreSerialize::core_serialize(&*b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect();
    for v in [", 0", ", -5", ", false"] {
        assert_eq!(call_method(&server, "getblock", &arg(v)).unwrap(), bytes.clone().into(), "{v}");
    }
    // A string verbosity is getInt's type error, as the argument check skips it.
    assert_eq!(
        method_err(&server, "getblock", &arg(r#", "1""#)),
        reply(RPC_TYPE_ERROR, "JSON value of type string is not of expected type number")
    );
    // Verbosity 1 reads no undo data.
    assert!(!live.lock().unwrap().calls.iter().any(|c| c.starts_with("undo")));
    // The hash is parsed first, then the chainman is ensured.
    assert_eq!(
        method_err(&unloaded_server(), "getblock", r#"["00"]"#),
        reply(RPC_INVALID_PARAMETER, "blockhash must be of length 64 (not 2, for '00')")
    );
    assert_eq!(
        method_err(&unloaded_server(), "getblock", &arg("")),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
}

/// The value at `path` (object keys, or array indices as numbers); `Null` where there is none.
pub(super) fn at(v: &UniValue, path: &str) -> UniValue {
    let mut cur = v.clone();
    for part in path.split('.') {
        cur = match (&cur, part.parse::<usize>()) {
            (UniValue::Arr(items), Ok(i)) => items.get(i).cloned(),
            (UniValue::Obj(entries), _) => {
                entries.iter().find(|(k, _)| k == part).map(|(_, v)| v.clone())
            }
            _ => None,
        }
        .unwrap_or(UniValue::Null);
    }
    cur
}

/// An object's keys, in order.
pub(super) fn keys(v: &UniValue) -> Vec<String> {
    match v {
        UniValue::Obj(entries) => entries.iter().map(|(k, _)| k.clone()).collect(),
        _ => panic!("not an object: {}", v.write()),
    }
}

/// A spend of a coin with value `in_value`: one input of the vectors' signature and key, one
/// output of `out_value` to a P2PKH script.
pub(super) const SPEND_IN: i64 = 5_000_000_000;
pub(super) const SPEND_OUT: i64 = 4_999_990_000;

pub(super) fn spend_hex() -> String {
    let sig = "304502207fa7a6d1e0ee81132a269ad84e68d695483745cde8b541e3bf630749894e342a022100c1f7ab20e13e22fb95281a870f3dcf38d782e53023ee313d741ad0cfbc0c509001";
    let key = "03b0da749730dc9b4b1f4a14d6902877a92541f5368778853d9c4a0cb7802dcfb2";
    let script_sig = format!("{:02x}{sig}{:02x}{key}", sig.len() / 2, key.len() / 2);
    format!(
        "0100000001{}00000000{:02x}{script_sig}feffffff01{}1976a914{}88ac00000000",
        "11".repeat(32),
        script_sig.len() / 2,
        hex_le(SPEND_OUT),
        "22".repeat(20),
    )
}

fn hex_le(v: i64) -> String {
    v.to_le_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

/// [`block`]'s coinbase, then `txs` (hex).
pub(super) fn block_with(txs: &[String]) -> Arc<Block> {
    let base = dash_consensus::CoreSerialize::core_serialize(&*block(None));
    let mut hex: String = base[..80].iter().map(|b| format!("{b:02x}")).collect();
    hex.push_str(&format!("{:02x}", txs.len() + 1));
    hex.extend(base[81..].iter().map(|b| format!("{b:02x}")));
    for tx in txs {
        hex.push_str(tx);
    }
    Arc::new(crate::mining::decode_hex_blk(&hex).expect("a block"))
}

/// One coin per input of the spend.
pub(super) fn spend_undo(height: u32) -> BlockUndo {
    let script = crate::request::parse_hex(&format!("76a914{}88ac", "33".repeat(20))).unwrap();
    BlockUndo {
        txs: vec![dash_consensus::coins::TxUndo {
            coins: vec![dash_consensus::coins::Coin::new(SPEND_IN, &script, height, true)],
        }],
    }
}

#[test]
fn getblock_verbosity_2_and_3_print_each_tx_with_its_undo() {
    let (server, live) = live_server(1);
    let arg = |rest: &str| format!(r#"["{HASH_7}"{rest}]"#);
    let b = block_with(&[spend_hex()]);
    let mut f = facts(1);
    f.chainlock = true;
    live.lock().unwrap().block = Some(Ok((f.clone(), Arc::clone(&b))));
    live.lock().unwrap().undo = Some(spend_undo(7));
    let p = tx_json();
    let tx_obj = |i: usize, verbosity, undo: Option<&dash_consensus::coins::TxUndo>| {
        let mut o = crate::core_write::tx_to_univ(&b.txs()[i], None, true, undo, verbosity, &p);
        // No InstantSend manager: `instantlock` is the block's ChainLock (D40).
        o.push_kv_end("instantlock", true.into());
        o.push_kv_end("instantlock_internal", false.into());
        o
    };
    let undo = spend_undo(7);
    for (v, verbosity) in [
        (", 2", crate::core_write::TxVerbosity::Details),
        (", 3", crate::core_write::TxVerbosity::DetailsAndPrevout),
        (", 9", crate::core_write::TxVerbosity::DetailsAndPrevout),
    ] {
        live.lock().unwrap().calls.clear();
        let json = call_method(&server, "getblock", &arg(v)).unwrap();
        let mut txs = UniValue::new_array();
        // The coinbase has no undo data; the spend reads the block's first `TxUndo`.
        txs.push_back(tx_obj(0, verbosity, None));
        txs.push_back(tx_obj(1, verbosity, Some(&undo.txs[0])));
        let mut want = super::blockheader_to_json(&f);
        want.push_kv_end("size", i64::from(b.size()).into());
        want.push_kv_end("tx", txs);
        assert_eq!(json.write(), want.write(), "{v}");
        assert_eq!(
            live.lock().unwrap().calls,
            [format!("block {HASH_7}"), format!("undo {}", f.hash)]
        );
        assert_eq!(at(&json, "tx.1.fee").write(), "0.00010000", "{v}");
        assert_eq!(!at(&json, "tx.1.vin.0.prevout").is_null(), v != ", 2", "{v}");
    }
    let prevout =
        at(&call_method(&server, "getblock", &arg(", 3")).unwrap(), "tx.1.vin.0.prevout").write();
    assert_eq!(
        prevout,
        format!(
            r#"{{"generated":true,"height":7,"value":50.00000000,"scriptPubKey":{}}}"#,
            crate::core_write::script_to_univ(
                &crate::request::parse_hex(&format!("76a914{}88ac", "33".repeat(20))).unwrap(),
                true,
                true,
                &p.key_io
            )
            .write()
        )
    );
    // TxToUniv's key order, then blockToJSON's two InstantSend keys.
    let json = call_method(&server, "getblock", &arg(", 3")).unwrap();
    assert_eq!(
        keys(&at(&json, "tx.1")),
        [
            "txid",
            "version",
            "type",
            "size",
            "locktime",
            "vin",
            "vout",
            "fee",
            "hex",
            "instantlock",
            "instantlock_internal"
        ]
    );
    assert_eq!(
        keys(&at(&json, "tx.1.vin.0")),
        ["txid", "vout", "scriptSig", "prevout", "sequence"]
    );
    // Undo data short of a spending transaction is `vtxundo.at(i - 1)`'s out_of_range; an entry
    // short of an input fails the same way (D41).
    live.lock().unwrap().undo = Some(BlockUndo {
        txs: vec![],
    });
    assert_eq!(
        method_err(&server, "getblock", &arg(", 2")),
        reply(
            RPC_MISC_ERROR,
            "vector::_M_range_check: __n (which is 0) >= this->size() (which is 0)"
        )
    );
    live.lock().unwrap().undo = Some(BlockUndo {
        txs: vec![dash_consensus::coins::TxUndo {
            coins: vec![],
        }],
    });
    assert_eq!(
        method_err(&server, "getblock", &arg(", 3")),
        reply(
            RPC_MISC_ERROR,
            "vector::_M_range_check: __n (which is 0) >= this->size() (which is 0)"
        )
    );
    // Verbosity 1 reads no undo, so short undo data cannot fail it.
    assert!(call_method(&server, "getblock", &arg("")).is_ok());
    // Without undo data (pruned, or an unreadable record) the block prints without `prevout` and
    // `fee`.
    live.lock().unwrap().undo = None;
    let json = call_method(&server, "getblock", &arg(", 3")).unwrap();
    assert_eq!(
        at(&json, "tx.1").write(),
        tx_obj(1, crate::core_write::TxVerbosity::DetailsAndPrevout, None).write()
    );
    assert!(at(&json, "tx.1.fee").is_null());
    // A coinbase payload `GetTxPayload<CCbTx>` reads whole adds `cbTx`, at every verbosity
    // above 0, after `tx`; the coinbase's own object carries it too.
    let cbtx = format!("0100{}{}", hex_le(7)[..8].to_owned(), "44".repeat(32));
    live.lock().unwrap().block = Some(Ok((facts(1), block(Some(&cbtx)))));
    let want_cbtx =
        format!(r#""cbTx":{{"version":1,"height":7,"merkleRootMNList":"{}"}}}}"#, "44".repeat(32));
    for v in ["", ", 2"] {
        let json = call_method(&server, "getblock", &arg(v)).unwrap().write();
        assert!(json.ends_with(&want_cbtx), "{v}: {json}");
    }
    let json = call_method(&server, "getblock", &arg(", 2")).unwrap();
    assert_eq!(at(&json, "tx.0.cbTx").write(), at(&json, "cbTx").write());
    // A payload with a byte left over is not read: no `cbTx`.
    live.lock().unwrap().block = Some(Ok((facts(1), block(Some(&format!("{cbtx}00"))))));
    let json = call_method(&server, "getblock", &arg("")).unwrap();
    assert!(at(&json, "cbTx").is_null());
}

/// [`blockheader_to_json`]'s text for `f`.
fn blockheader_json(f: &HeaderFacts) -> String {
    super::blockheader_to_json(f).write()
}
