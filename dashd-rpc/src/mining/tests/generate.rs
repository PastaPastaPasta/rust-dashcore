//! The `generate*` commands over a stub [`Miner`] and the fake chainman.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use dash_consensus::CoreSerialize as _;

use super::*;
use crate::descriptor::tests::regtest;
use crate::key_io::decode_destination;
use crate::mining::{BlockCheck, Grind, LastBlockStats, Miner, TemplateRefusal};
use crate::protocol::{
    RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_METHOD_NOT_FOUND, RPC_MISC_ERROR,
};

/// `rpc_generate.py`'s regtest address of the generator point's key.
const ADDRESS: &str = "yWziQMcwmKjRdzi7eWjwiQX8EjWcd6dSg6";

/// What the stub was asked, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Template {
        script: Vec<u8>,
        use_mempool: bool,
    },
    /// `max_tries` as the grind found it.
    Grind(u64),
    /// The checked block's transaction count.
    Check(usize),
}

/// The answers the test sets, taken in order; once a queue is empty, a template is made, a
/// grind finds a block for one try, and a check passes.
#[derive(Default)]
struct Stub {
    refusals: VecDeque<TemplateRefusal>,
    grinds: VecDeque<Grind>,
    checks: VecDeque<BlockCheck>,
    calls: Vec<Call>,
    /// The blocks `test_block_validity` was given.
    checked: Vec<Arc<Block>>,
    templates: u32,
    /// Templates come without their coinbase (a CHECK_NONFATAL's failure).
    empty: bool,
}

#[derive(Default)]
pub(super) struct StubMiner(Mutex<Stub>);

/// A coinbase paying `script`: one input spending nothing, one output.
fn coinbase(script: &[u8], tag: u32) -> String {
    format!(
        "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff06{}51ffffffff0100f2052a01000000{:02x}{}00000000",
        hex(&{
            let mut sig = vec![0x04];
            sig.extend_from_slice(&tag.to_le_bytes());
            sig
        }),
        script.len(),
        hex(script),
    )
}

impl Miner for StubMiner {
    fn create_new_block(
        &self,
        coinbase_script: &[u8],
        use_mempool: bool,
    ) -> Result<Block, TemplateRefusal> {
        let mut stub = self.0.lock().unwrap();
        stub.calls.push(Call::Template {
            script: coinbase_script.to_vec(),
            use_mempool,
        });
        if let Some(refusal) = stub.refusals.pop_front() {
            return Err(refusal);
        }
        stub.templates += 1;
        let header = format!(
            "00000020{}{}{}ffff7f2000000000",
            "00".repeat(32),
            "00".repeat(32),
            hex(&stub.templates.to_le_bytes())
        );
        let block = format!("{header}01{}", coinbase(coinbase_script, stub.templates));
        let block = super::super::decode_hex_blk(&block).expect("a block");
        Ok(if stub.empty {
            Block::new(*block.header(), Vec::new())
        } else {
            block
        })
    }

    fn grind(&self, _: &mut Block, max_tries: &mut u64, shutdown: &dyn Fn() -> bool) -> Grind {
        let mut stub = self.0.lock().unwrap();
        stub.calls.push(Call::Grind(*max_tries));
        if shutdown() {
            return Grind::Shutdown;
        }
        *max_tries -= 1;
        stub.grinds.pop_front().unwrap_or(Grind::Found)
    }

    fn test_block_validity(&self, block: Arc<Block>) -> BlockCheck {
        let mut stub = self.0.lock().unwrap();
        stub.calls.push(Call::Check(block.txs().len()));
        stub.checked.push(block);
        stub.checks.pop_front().unwrap_or(BlockCheck::Valid)
    }

    fn last_block_stats(&self) -> Option<LastBlockStats> {
        unreachable!("the generate commands never read it")
    }
}

struct Node {
    server: Arc<RpcServer>,
    chain: Arc<FakeChain>,
    miner: Arc<StubMiner>,
    control: Arc<FakeControl>,
}

impl Node {
    fn new(chain: Arc<FakeChain>) -> Node {
        let control = Arc::new(FakeControl::default());
        let server = Arc::new(RpcServer::new(Arc::clone(&control) as _));
        let miner = Arc::new(StubMiner::default());
        register_mining_commands(
            &server,
            Arc::clone(&chain) as _,
            Arc::clone(&miner) as _,
            regtest(),
        );
        server.start();
        server.set_warmup_finished();
        Node {
            server,
            chain,
            miner,
            control,
        }
    }

    /// A loaded chainman whose `ProcessNewBlock` accepts (or not) every block.
    fn accepting(accepted: bool) -> Node {
        Node::new(loaded(processing(accepted, accepted, None)))
    }

    fn call(&self, method: &str, params: UniValue) -> Result<UniValue, RpcError> {
        self.server.execute(&JsonRpcRequest {
            method: method.into(),
            params,
            ..Default::default()
        })
    }

    fn stub(&self) -> std::sync::MutexGuard<'_, Stub> {
        self.miner.0.lock().unwrap()
    }

    fn processed(&self) -> Vec<bool> {
        self.chain.0.lock().unwrap().calls.clone()
    }
}

fn params(json: &str) -> UniValue {
    UniValue::read(json.as_bytes()).expect("JSON")
}

fn field<'a>(v: &'a UniValue, key: &str) -> Option<&'a UniValue> {
    match v {
        UniValue::Obj(o) => o.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

fn fails(r: Result<UniValue, RpcError>, code: i32, message: &str) {
    assert_eq!(answer(r), Err((code, RpcError::new(code, message).to_object().write())));
}

fn address_script() -> Vec<u8> {
    decode_destination(ADDRESS, &regtest()).unwrap().script()
}

fn hashes(v: &UniValue) -> Vec<String> {
    crate::request::values(v).iter().map(|h| h.get_str().unwrap().to_owned()).collect()
}

#[test]
fn generatetoaddress_mines_each_block_to_the_address_and_processes_it() {
    let node = Node::accepting(true);
    let reply = node.call("generatetoaddress", params(&format!(r#"[3, "{ADDRESS}"]"#))).unwrap();
    assert_eq!(hashes(&reply).len(), 3);
    let template = Call::Template {
        script: address_script(),
        use_mempool: true,
    };
    // One budget for every block: 1,000,000 by default, one try spent per block here.
    assert_eq!(
        node.stub().calls,
        [
            template.clone(),
            Call::Grind(1_000_000),
            template.clone(),
            Call::Grind(999_999),
            template,
            Call::Grind(999_998),
        ]
    );
    assert_eq!(node.processed(), [true, true, true]);
}

/// `maxtries` is an `int` stored in a `uint64_t` by `generatetoaddress`, so -1 is 2^64-1;
/// `generatetodescriptor` reads a `uint64_t`, which -1 is not.
// Core: rpc/mining.cpp:244-246, 284-285
#[test]
fn maxtries_is_read_as_core_reads_it() {
    let node = Node::accepting(true);
    node.call("generatetoaddress", params(&format!(r#"[1, "{ADDRESS}", -1]"#))).unwrap();
    assert_eq!(node.stub().calls[1], Call::Grind(u64::MAX));
    fails(
        node.call("generatetodescriptor", params(&format!(r#"[1, "addr({ADDRESS})", -1]"#))),
        RPC_MISC_ERROR,
        "JSON integer out of range",
    );
    node.call(
        "generatetodescriptor",
        params(&format!(r#"[1, "addr({ADDRESS})", 18446744073709551615]"#)),
    )
    .unwrap();
    assert_eq!(node.stub().calls.last(), Some(&Call::Grind(u64::MAX)));
}

/// `GenerateBlock` false (tries out, shutdown) ends the run with the blocks so far; an
/// exhausted nonce makes a new template and keeps the budget.
// Core: rpc/mining.cpp:139-164, 176-185
#[test]
fn a_run_stops_on_tries_or_shutdown_and_retemplates_on_exhausted_nonces() {
    let node = Node::accepting(true);
    node.stub().grinds.extend([Grind::NonceExhausted, Grind::Found, Grind::OutOfTries]);
    let reply =
        node.call("generatetoaddress", params(&format!(r#"[5, "{ADDRESS}", 10]"#))).unwrap();
    assert_eq!(hashes(&reply).len(), 1);
    let grinds: Vec<_> =
        node.stub().calls.iter().filter(|c| matches!(c, Call::Grind(_))).cloned().collect();
    assert_eq!(grinds, [Call::Grind(10), Call::Grind(9), Call::Grind(8)]);
    assert_eq!(node.processed(), [true]);

    let node = Node::accepting(true);
    node.control.shutdown.store(true, Ordering::SeqCst);
    let reply = node.call("generatetoaddress", params(&format!(r#"[5, "{ADDRESS}"]"#))).unwrap();
    assert!(hashes(&reply).is_empty());
    assert!(node.stub().calls.is_empty(), "the loop checks shutdown before a template");
}

#[test]
fn a_block_processnewblock_refuses_is_an_internal_error() {
    let node = Node::accepting(false);
    fails(
        node.call("generatetoaddress", params(&format!(r#"[2, "{ADDRESS}"]"#))),
        RPC_INTERNAL_ERROR,
        "ProcessNewBlock, block not accepted",
    );
    assert_eq!(node.processed(), [true]);
}

/// The template's refusals: `NotReady` is the recorded divergence (DEC-607 D4, DEC-630), `Failed`
/// Core's `runtime_error`, `NoChain` `EnsureChainman`'s.
#[test]
fn template_refusals_answer_as_planned() {
    for (refusal, code, message) in [
        (
            TemplateRefusal::NotReady(
                "block 1000 needs DIP0003, which is not served until MN0-2".into(),
            ),
            RPC_INTERNAL_ERROR,
            "Couldn't create new block: block 1000 needs DIP0003, which is not served until MN0-2",
        ),
        (TemplateRefusal::Failed("bad-cb-amount".into()), RPC_MISC_ERROR, "bad-cb-amount"),
        (TemplateRefusal::NoChain, RPC_INTERNAL_ERROR, "Node chainman not found"),
    ] {
        let node = Node::accepting(true);
        node.stub().refusals.push_back(refusal.clone());
        fails(
            node.call("generatetoaddress", params(&format!(r#"[1, "{ADDRESS}"]"#))),
            code,
            message,
        );
        let node = Node::accepting(true);
        node.stub().refusals.push_back(refusal);
        fails(node.call("generateblock", params(&format!(r#"["{ADDRESS}", []]"#))), code, message);
    }
}

/// The output is checked before the chainman.
// Core: rpc/mining.cpp:249-255, 286-292
#[test]
fn a_bad_output_fails_before_the_chainman() {
    let unloaded = Node::new(Arc::new(FakeChain::default()));
    fails(
        unloaded.call("generatetoaddress", params(r#"[1, "1234"]"#)),
        RPC_INVALID_ADDRESS_OR_KEY,
        "Error: Invalid address",
    );
    fails(
        unloaded.call("generatetodescriptor", params(r#"[1, "1234"]"#)),
        RPC_INVALID_ADDRESS_OR_KEY,
        "'1234' is not a valid descriptor function",
    );
    fails(
        unloaded.call("generatetoaddress", params(&format!(r#"[1, "{ADDRESS}"]"#))),
        RPC_INTERNAL_ERROR,
        "Node chainman not found",
    );
    assert!(unloaded.stub().calls.is_empty());
}

// Core: test/functional/rpc_generate.py:96-103
#[test]
fn ranged_and_underivable_descriptors_are_refused() {
    let node = Node::accepting(true);
    let tpub = "tpubD6NzVbkrYhZ4XgiXtGrdW5XDAPFCL9h7we1vwNCpn8tGbBcgfVYjXyhWo4E1xkh56hjod1RhGjxbaTLV3X4FyWuejifB9jusQ46QzG87VKp";
    for method in ["generatetodescriptor", "generateblock"] {
        let p = |d: String| {
            if method == "generateblock" {
                params(&format!(r#"["{d}", []]"#))
            } else {
                params(&format!(r#"[1, "{d}"]"#))
            }
        };
        fails(
            node.call(method, p(format!("pkh({tpub}/0/*)"))),
            RPC_INVALID_PARAMETER,
            "Ranged descriptor not accepted. Maybe pass through deriveaddresses first?",
        );
        fails(
            node.call(method, p(format!("pkh({tpub}/0'/0)"))),
            RPC_INVALID_ADDRESS_OR_KEY,
            "Cannot derive script without private keys",
        );
    }
}

#[test]
fn generatetodescriptor_mines_to_the_descriptors_script() {
    let node = Node::accepting(true);
    node.call("generatetodescriptor", params(r#"[1, "raw(51)"]"#)).unwrap();
    assert_eq!(
        node.stub().calls[0],
        Call::Template {
            script: vec![0x51],
            use_mempool: true
        }
    );
}

/// `generateblock` to an address or a descriptor; without `submit` it answers the block's hex
/// and processes nothing.
// Core: test/functional/rpc_generate.py:28-56, rpc/mining.cpp:323-420
#[test]
fn generateblock_mines_one_block_without_the_mempool() {
    let node = Node::accepting(true);
    let reply = node.call("generateblock", params(&format!(r#"["{ADDRESS}", []]"#))).unwrap();
    assert!(field(&reply, "hex").is_none());
    let combo = "combo(0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798)";
    node.call("generateblock", params(&format!(r#"["{combo}", []]"#))).unwrap();
    let template = Call::Template {
        script: address_script(),
        use_mempool: false,
    };
    assert_eq!(
        node.stub().calls,
        [
            template.clone(),
            Call::Check(1),
            Call::Grind(1_000_000),
            template,
            Call::Check(1),
            Call::Grind(1_000_000),
        ]
    );
    assert_eq!(node.processed(), [true, true]);

    let reply =
        node.call("generateblock", params(&format!(r#"["{ADDRESS}", [], false]"#))).unwrap();
    let block =
        super::super::decode_hex_blk(field(&reply, "hex").unwrap().get_str().unwrap()).unwrap();
    assert_eq!(field(&reply, "hash").unwrap().get_str().unwrap(), block.hash().to_string());
    assert_eq!(node.processed().len(), 2);
}

#[test]
fn generateblock_appends_raw_transactions_and_checks_the_block() {
    let node = Node::accepting(true);
    let (tx, tx2) = (coinbase(&[0x51], 99), coinbase(&[0x52], 100));
    let reply = node
        .call("generateblock", params(&format!(r#"["{ADDRESS}", ["{tx}", "{tx2}"], false]"#)))
        .unwrap();
    assert_eq!(node.stub().calls[1], Call::Check(3));
    // The template's coinbase, then the raw transactions in their order, byte for byte: in the
    // checked block and in the unsubmitted block's hex.
    let made =
        super::super::decode_hex_blk(field(&reply, "hex").unwrap().get_str().unwrap()).unwrap();
    for block in [&*node.stub().checked[0], &made] {
        let txs: Vec<String> = block.txs().iter().map(|t| hex(&t.core_serialize())).collect();
        assert_eq!(txs, [coinbase(&address_script(), 1), tx.clone(), tx2.clone()]);
    }
    assert!(node.processed().is_empty());

    node.stub().checks.push_back(BlockCheck::Invalid("bad-txns-inputs-missingorspent".into()));
    fails(
        node.call("generateblock", params(&format!(r#"["{ADDRESS}", ["{tx}"]]"#))),
        RPC_VERIFY_ERROR,
        "TestBlockValidity failed: bad-txns-inputs-missingorspent",
    );

    // The tip moved between the template and the check: a new template (D17).
    let node = Node::accepting(true);
    node.stub().checks.extend([BlockCheck::TipMoved, BlockCheck::Valid]);
    node.call("generateblock", params(&format!(r#"["{ADDRESS}", []]"#))).unwrap();
    assert_eq!(node.stub().templates, 2);

    // A shutdown request ends the rebuilds as `GenerateBlock`'s shutdown does.
    let node = Node::accepting(true);
    node.stub().checks.extend([BlockCheck::TipMoved, BlockCheck::TipMoved]);
    node.control.shutdown.store(true, Ordering::SeqCst);
    fails(
        node.call("generateblock", params(&format!(r#"["{ADDRESS}", []]"#))),
        RPC_MISC_ERROR,
        "Failed to make block.",
    );
    assert_eq!(node.stub().templates, 1, "no rebuild after shutdown");

    // Any other "not tested" answer is an internal bug, not a rebuild.
    let node = Node::accepting(true);
    node.stub().checks.push_back(BlockCheck::NotTested("duplicate".into()));
    let e = node.call("generateblock", params(&format!(r#"["{ADDRESS}", []]"#))).unwrap_err();
    let RpcError::Runtime(text) = &e else {
        panic!("{e:?}")
    };
    assert!(text.starts_with("Internal bug detected: TestBlockValidity answered duplicate\n"));
    assert_eq!(node.stub().templates, 1);

    // CHECK_NONFATAL(block.vtx.size() >= 1) answers -1 "Internal bug detected", not a panic.
    let node = Node::accepting(true);
    node.stub().empty = true;
    let e = node.call("generateblock", params(&format!(r#"["{ADDRESS}", []]"#))).unwrap_err();
    let RpcError::Runtime(text) = &e else {
        panic!("{e:?}")
    };
    assert!(text.starts_with("Internal bug detected: block.vtx.size() >= 1\n"), "{text}");
    assert!(node.stub().checked.is_empty());

    let node = Node::accepting(true);
    node.stub().checks.push_back(BlockCheck::NoChain);
    fails(
        node.call("generateblock", params(&format!(r#"["{ADDRESS}", []]"#))),
        RPC_INTERNAL_ERROR,
        "Node chainman not found",
    );
}

// Core: test/functional/rpc_generate.py:84-94
#[test]
fn generateblock_refuses_bad_outputs_and_transactions() {
    let node = Node::accepting(true);
    fails(
        node.call("generateblock", params(r#"["1234", []]"#)),
        RPC_INVALID_ADDRESS_OR_KEY,
        "Error: Invalid address or descriptor",
    );
    let txid = "0".repeat(64);
    fails(
        node.call("generateblock", params(&format!(r#"["{ADDRESS}", ["{txid}"]]"#))),
        RPC_INVALID_ADDRESS_OR_KEY,
        &format!("Transaction {txid} not in mempool."),
    );
    for bad in ["0000", "zz", &format!("{}00", coinbase(&[0x51], 1))] {
        fails(
            node.call("generateblock", params(&format!(r#"["{ADDRESS}", ["{bad}"]]"#))),
            RPC_DESERIALIZATION_ERROR,
            &format!(
                "Transaction decode failed for {bad}. Make sure the tx has at least one input."
            ),
        );
    }
    assert!(node.stub().calls.is_empty(), "every refusal comes before the template");
}

/// A grind that ends without a block fails `generateblock`, whatever the reason.
// Core: rpc/mining.cpp:401-405
#[test]
fn generateblock_without_a_block_fails_to_make_one() {
    for grind in [Grind::OutOfTries, Grind::NonceExhausted, Grind::Shutdown] {
        let node = Node::accepting(true);
        node.stub().grinds.push_back(grind);
        fails(
            node.call("generateblock", params(&format!(r#"["{ADDRESS}", []]"#))),
            RPC_MISC_ERROR,
            "Failed to make block.",
        );
        assert!(node.processed().is_empty());
    }
}

// Core: test/functional/rpc_generate.py:107-121
#[test]
fn generate_points_to_the_cli_option() {
    let node = Node::accepting(true);
    let help = "generate\n\nhas been replaced by the -generate cli option. Refer to -help for more information.\n";
    fails(node.call("generate", params("[]")), RPC_METHOD_NOT_FOUND, help);
}
