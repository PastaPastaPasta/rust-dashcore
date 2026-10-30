use std::sync::{Arc, Mutex};

use dash_consensus::validation::BlockValidationResult;
use dash_consensus::{Block, BlockValidationError, Header};
use dash_primitives::BlockHash;

use super::{StateCatcher, ValidationState, register_mining_commands};
use crate::UniValue;
use crate::blockchain::{
    BlockCommandError, BlockReadError, BloomFilterData, ChainTipInfo, Chainman, HeaderFacts,
    IndexStatus, NodeChain, ProcessedBlock, PublishedChain, TxOutSetError, TxOutSetInfo,
    UndoReadError,
};
use crate::protocol::{RPC_DESERIALIZATION_ERROR, RPC_INTERNAL_ERROR, RPC_VERIFY_ERROR, RpcError};
use crate::request::JsonRpcRequest;
use crate::server::RpcServer;
use crate::testutil::FakeControl;
use dash_consensus::coins::BlockUndo;
use dash_consensus::coins::stats::CoinStatsHashType;

/// What `ProcessNewBlock` does in a test: its outcome, and the `BlockChecked` it signals (for
/// the block it was given, or for another).
#[derive(Clone)]
struct Processing {
    processed: ProcessedBlock,
    checked: Option<ValidationState>,
    other_block: bool,
}

/// The chainman's state, shared by the test and the commands; `loaded` false before it loads.
#[derive(Default)]
struct Fake {
    loaded: bool,
    index: Option<IndexStatus>,
    processing: Option<Processing>,
    /// `force_processing` of each `ProcessNewBlock`.
    calls: Vec<bool>,
    /// What `ProcessNewBlockHeaders` leaves, and the headers it was given.
    header_state: Option<ValidationState>,
    headers: Vec<BlockHash>,
}

#[derive(Default)]
struct FakeChain(Arc<Mutex<Fake>>);

struct FakeChainman(Arc<Mutex<Fake>>);

impl NodeChain for FakeChain {
    fn published(&self) -> Option<Box<dyn PublishedChain>> {
        unreachable!("submitblock reads the chainman")
    }

    fn chainman(&self) -> Option<Box<dyn Chainman>> {
        self.0.lock().unwrap().loaded.then(|| Box::new(FakeChainman(Arc::clone(&self.0))) as _)
    }
}

impl Chainman for FakeChainman {
    fn lookup_block_index(&self, _: &BlockHash) -> Option<IndexStatus> {
        self.0.lock().unwrap().index
    }

    fn process_new_block(
        &self,
        block: Arc<Block>,
        force_processing: bool,
        catcher: &Arc<StateCatcher>,
    ) -> ProcessedBlock {
        let mut fake = self.0.lock().unwrap();
        fake.calls.push(force_processing);
        let p = fake.processing.clone().expect("a processing set by the test");
        if let Some(state) = p.checked {
            let checked = if p.other_block {
                decode(&block_hex(OTHER_TIME))
            } else {
                block
            };
            catcher.block_checked(&checked, || state);
        }
        p.processed
    }

    fn chain_tips(&self) -> Vec<ChainTipInfo> {
        unreachable!("submitblock reads no tips")
    }

    fn header_facts(&self, _: &BlockHash) -> Option<HeaderFacts> {
        unreachable!("submitblock reads no header")
    }

    fn read_block(&self, _: &BlockHash) -> Result<(HeaderFacts, Arc<Block>), BlockReadError> {
        unreachable!("submitblock reads no block")
    }

    fn read_undo(&self, _: &BlockHash) -> Result<BlockUndo, UndoReadError> {
        unreachable!("submitblock reads no undo")
    }

    fn block_subsidy(&self, _: &BlockHash) -> Option<i64> {
        unreachable!("submitblock reads no subsidy")
    }

    fn merkle_blocks(
        &self,
        _: BloomFilterData,
        _: &BlockHash,
        _: u32,
    ) -> Result<Vec<Vec<u8>>, BlockReadError> {
        unreachable!("submitblock walks no merkle blocks")
    }

    fn invalidate_block(&self, _: BlockHash) -> Result<(), BlockCommandError> {
        unreachable!("submitblock invalidates nothing")
    }

    fn reconsider_block(&self, _: BlockHash, _: bool) -> Result<(), BlockCommandError> {
        unreachable!("submitblock reconsiders nothing")
    }

    fn precious_block(&self, _: BlockHash) -> Result<(), BlockCommandError> {
        unreachable!("submitblock marks nothing precious")
    }

    fn sync_validation_queue(&self) {
        unreachable!("submitblock syncs nothing")
    }

    fn txout_set_info(&self, _: CoinStatsHashType) -> Result<TxOutSetInfo, TxOutSetError> {
        unreachable!("no mining command reads the UTXO set")
    }

    fn process_new_block_header(&self, header: Header) -> ValidationState {
        let mut fake = self.0.lock().unwrap();
        fake.headers.push(header.hash());
        fake.header_state.clone().expect("a header state set by the test")
    }

    fn txout_proof(
        &self,
        _: &[dash_primitives::TxHash],
        _: Option<BlockHash>,
    ) -> Result<Vec<u8>, crate::blockchain::TxOutProofError> {
        unreachable!("no mining command proves a transaction")
    }
}

const TIME: u32 = 1;
const OTHER_TIME: u32 = 2;

/// A block whose first transaction spends `prevout` (all zeros and `ffffffff` is a coinbase's).
fn raw_block(time: u32, prevout: &str) -> String {
    let header = format!(
        "00000020{}{}{}ffff7f2000000000",
        "00".repeat(32),
        "00".repeat(32),
        hex(&time.to_le_bytes())
    );
    let tx = format!("0100000001{prevout}0251ffffffff01{}0151{}", "00".repeat(8), "00".repeat(4));
    format!("{header}01{tx}")
}

fn block_hex(time: u32) -> String {
    raw_block(time, &format!("{}ffffffff", "00".repeat(32)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode(hex: &str) -> Arc<Block> {
    Arc::new(super::decode_hex_blk(hex).expect("a block"))
}

fn server(chain: &Arc<FakeChain>) -> Arc<RpcServer> {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    crate::commands::register_control_commands(&server);
    register_mining_commands(
        &server,
        Arc::clone(chain) as _,
        Arc::new(generate::StubMiner::default()) as _,
        crate::descriptor::tests::regtest(),
    );
    server.start();
    server.set_warmup_finished();
    server
}

fn call(server: &RpcServer, params: UniValue) -> Result<UniValue, RpcError> {
    server.execute(&JsonRpcRequest {
        method: "submitblock".into(),
        params,
        ..Default::default()
    })
}

fn submit(server: &RpcServer, hex: &str) -> Result<UniValue, RpcError> {
    call(server, UniValue::Arr(vec![hex.into()]))
}

/// The reply, or the error's code and message as the reply writes them.
fn answer(r: Result<UniValue, RpcError>) -> Result<UniValue, (i32, String)> {
    r.map_err(|e| (e.code(), e.to_object().write()))
}

fn err(r: Result<UniValue, RpcError>) -> (i32, String) {
    answer(r).unwrap_err()
}

fn reply(code: i32, message: &str) -> (i32, String) {
    (code, format!(r#"{{"code":{code},"message":"{message}"}}"#))
}

fn loaded(fake: Fake) -> Arc<FakeChain> {
    Arc::new(FakeChain(Arc::new(Mutex::new(Fake {
        loaded: true,
        ..fake
    }))))
}

fn processing(accepted: bool, new_block: bool, checked: Option<ValidationState>) -> Fake {
    Fake {
        processing: Some(Processing {
            processed: ProcessedBlock {
                accepted,
                new_block,
            },
            checked,
            other_block: false,
        }),
        ..Fake::default()
    }
}

fn invalid(reason: &'static str) -> ValidationState {
    ValidationState::Invalid(BlockValidationError::new(BlockValidationResult::Consensus, reason))
}

/// `DecodeHexBlk` and the coinbase check come before `EnsureAnyChainman`: a node with no
/// chainman answers them.
#[test]
fn a_block_that_does_not_decode_or_lacks_a_coinbase_is_refused_before_the_chainman() {
    let server = server(&Arc::new(FakeChain::default()));
    let block = block_hex(TIME);
    let decode_failed = reply(RPC_DESERIALIZATION_ERROR, "Block decode failed");
    // `IsHex`: empty, odd, a non-digit (a space or a NUL included); then the block itself.
    for hex in ["", "0", "zz", " 00", "00\u{0}0", &block[..block.len() - 1], &block[..160]] {
        assert_eq!(err(submit(&server, hex)), decode_failed, "{hex:?}");
    }
    let no_coinbase = reply(RPC_DESERIALIZATION_ERROR, "Block does not start with a coinbase");
    // No transaction; a first transaction with a prevout.
    let empty = format!("{}00", &block[..160]);
    let spends = raw_block(TIME, &format!("{}00000000", "11".repeat(32)));
    for hex in [empty, spends] {
        assert_eq!(err(submit(&server, &hex)), no_coinbase, "{hex}");
    }
    let no_chainman = reply(RPC_INTERNAL_ERROR, "Node chainman not found");
    assert_eq!(err(submit(&server, &block)), no_chainman);
}

/// `DecodeHexBlk` reads one block and ignores what follows; `IsHex` takes either case.
#[test]
fn a_block_decodes_with_trailing_bytes_and_in_upper_case() {
    let chain = loaded(processing(true, true, Some(ValidationState::Valid)));
    let server = server(&chain);
    let block = block_hex(TIME);
    for hex in [block.clone(), format!("{block}00ff"), block.to_uppercase()] {
        assert_eq!(submit(&server, &hex).unwrap(), UniValue::Null, "{hex}");
    }
    // The dummy argument is ignored.
    let two = UniValue::Arr(vec![block.into(), "x".into()]);
    assert_eq!(call(&server, two).unwrap(), UniValue::Null);
    assert_eq!(chain.0.lock().unwrap().calls, [true; 4], "force_processing");
}

/// An indexed block answers from its status, without `ProcessNewBlock`.
#[test]
fn an_indexed_block_answers_duplicate_or_duplicate_invalid() {
    for (valid_scripts, failed, answer) in [
        (true, false, Some("duplicate")),
        (false, true, Some("duplicate-invalid")),
        (false, false, None),
    ] {
        let chain = loaded(Fake {
            index: Some(IndexStatus {
                valid_scripts,
                failed,
            }),
            ..processing(true, false, None)
        });
        let reply = submit(&server(&chain), &block_hex(TIME)).unwrap();
        let calls = chain.0.lock().unwrap().calls.len();
        match answer {
            Some(answer) => {
                assert_eq!(reply, UniValue::from(answer));
                assert_eq!(calls, 0, "{answer}: answered from the index");
            }
            // Stored, not connected (a side chain's): processed, then found stored already.
            None => assert_eq!((reply, calls), (UniValue::from("duplicate"), 1)),
        }
    }
}

/// `ProcessNewBlock`'s outcome and the caught state, in Core's order: an accepted block not
/// written now is a duplicate whatever was caught; nothing caught is inconclusive; the rest is
/// `BIP22ValidationResult`.
#[test]
fn a_processed_block_answers_by_bip22() {
    let error = ValidationState::Error("System error: x".into());
    let cases = [
        (processing(true, false, Some(invalid("x"))), Ok("duplicate".into())),
        (processing(true, true, None), Ok("inconclusive".into())),
        (processing(false, false, None), Ok("inconclusive".into())),
        (processing(true, true, Some(ValidationState::Valid)), Ok(UniValue::Null)),
        (processing(false, false, Some(invalid("bad-txnmrklroot"))), Ok("bad-txnmrklroot".into())),
        (processing(true, true, Some(invalid(""))), Ok("rejected".into())),
        (processing(true, true, Some(error)), Err(reply(RPC_VERIFY_ERROR, "System error: x"))),
    ];
    for (fake, expected) in cases {
        assert_eq!(answer(submit(&server(&loaded(fake)), &block_hex(TIME))), expected);
    }
}

/// The catcher keeps only its own block's `BlockChecked`.
#[test]
fn another_blocks_check_is_not_caught() {
    let mut fake = processing(true, true, Some(invalid("high-hash")));
    fake.processing.as_mut().unwrap().other_block = true;
    let reply = submit(&server(&loaded(fake)), &block_hex(TIME)).unwrap();
    assert_eq!(reply, UniValue::from("inconclusive"));
}

fn submitheader(server: &RpcServer, hex: &str) -> Result<UniValue, (i32, String)> {
    answer(server.execute(&JsonRpcRequest {
        method: "submitheader".into(),
        params: UniValue::Arr(vec![hex.into()]),
        ..Default::default()
    }))
}

/// Core's `submitheader`: decode (trailing bytes ignored), then the chainman, the parent's
/// lookup, and the header's state as `RPC_VERIFY_ERROR`: the reject reason, or an error's
/// `ToString()`.
// Core: rpc/mining.cpp:1083-1103
#[test]
fn submitheader_needs_the_parent_then_answers_the_headers_state() {
    let header = &block_hex(TIME)[..160];
    let decode_failed = reply(RPC_DESERIALIZATION_ERROR, "Block header decode failed");
    let unloaded = server(&Arc::new(FakeChain::default()));
    for bad in ["", "00", &header[..158], "zz"] {
        assert_eq!(submitheader(&unloaded, bad), Err(decode_failed.clone()), "{bad:?}");
    }
    assert_eq!(
        submitheader(&unloaded, header),
        Err(reply(RPC_INTERNAL_ERROR, "Node chainman not found"))
    );

    let chain = loaded(Fake::default());
    let s = server(&chain);
    let no_parent = format!("Must submit previous header ({}) first", BlockHash::from([0; 32]));
    assert_eq!(submitheader(&s, header), Err(reply(RPC_VERIFY_ERROR, &no_parent)));
    assert!(
        chain.0.lock().unwrap().headers.is_empty(),
        "no header is processed without its parent"
    );

    let index = IndexStatus {
        valid_scripts: true,
        failed: false,
    };
    for (state, expected) in [
        (ValidationState::Valid, Ok(UniValue::Null)),
        (invalid("bad-diffbits"), Err(reply(RPC_VERIFY_ERROR, "bad-diffbits"))),
        (ValidationState::Error("disk".into()), Err(reply(RPC_VERIFY_ERROR, "disk"))),
    ] {
        let mut fake = chain.0.lock().unwrap();
        fake.index = Some(index);
        fake.header_state = Some(state);
        drop(fake);
        assert_eq!(submitheader(&s, &format!("{header}ff")), expected);
    }
    assert_eq!(chain.0.lock().unwrap().headers.len(), 3);
}

mod generate;
