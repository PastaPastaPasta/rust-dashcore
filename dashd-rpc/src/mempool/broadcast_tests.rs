//! Unit checks of `sendrawtransaction` over a fake node; the DIFF against dashd is in
//! rdashd/tests/rpc_diff.rs.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_consensus::CoreSerialize as _;
use dash_consensus::coins::Coin;
use dash_consensus::primitives::{Transaction, TxIn, TxOut, TxType};
use dash_primitives::BlockHash;

use super::*;
use crate::blockchain::{
    Aborted, ActiveWindow, BestChainLock, ChainState, ChainTxError, ChainTxPoint, DeploymentError,
    DeploymentReport, HeaderFacts, TipInfo, UnspentCoin,
};
use crate::descriptor::hex;
use crate::protocol::{
    RPC_CLIENT_P2P_DISABLED, RPC_DATABASE_ERROR, RPC_INTERNAL_ERROR, RPC_MISC_ERROR, RPC_TYPE_ERROR,
};
use crate::request::JsonRpcRequest;
use crate::testutil::FakeControl;

type Answer = Result<Processed, NoVerdict>;

/// What lands between a refused call and the re-check: a relayed copy, or a block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Race {
    #[default]
    None,
    Pooled,
    Mined,
}

/// The node: what is attached, the tip's unspent outpoints, whether the pool holds the
/// transaction, the pool's answers in call order and what a refusal races with; every call it
/// takes is logged.
#[derive(Default)]
struct Fake {
    no_chain: bool,
    no_mempool: bool,
    no_peerman: bool,
    tip: Vec<OutPoint>,
    in_mempool: bool,
    answers: Mutex<VecDeque<Answer>>,
    race: Race,
    raced: Arc<AtomicBool>,
    log: Mutex<Vec<String>>,
}

impl Fake {
    fn answering(answers: &[Answer]) -> Fake {
        Fake {
            answers: Mutex::new(answers.iter().cloned().collect()),
            ..Fake::default()
        }
    }

    fn log(&self, line: String) {
        self.log.lock().unwrap().push(line);
    }

    fn calls(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

/// The tip's outpoints, and the transaction's outputs once a block has raced a refusal.
struct Tip(Vec<OutPoint>, Option<Arc<AtomicBool>>);

impl ChainState for Tip {
    fn tip_info(&self) -> Result<TipInfo, Aborted> {
        unreachable!()
    }
    fn size_on_disk(&self) -> u64 {
        unreachable!()
    }
    fn deployment_info(
        &self,
        _: Option<&BlockHash>,
    ) -> Result<Option<DeploymentReport>, DeploymentError> {
        unreachable!()
    }
    fn chain_tx_final(&self, _: Option<&BlockHash>) -> Result<ChainTxPoint, ChainTxError> {
        unreachable!()
    }
    fn chain_tx_ancestor(&self, _: &BlockHash, _: u32) -> Result<Option<ChainTxPoint>, Aborted> {
        unreachable!()
    }
    fn txout(&self, _: &OutPoint) -> Result<Option<UnspentCoin>, Aborted> {
        unreachable!()
    }
    fn txouts(&self, outs: &[OutPoint]) -> Result<Vec<Option<Coin>>, Aborted> {
        let mined = self.1.as_ref().is_some_and(|raced| raced.load(Ordering::SeqCst));
        Ok(outs
            .iter()
            .map(|o| (mined || self.0.contains(o)).then(|| Coin::new(1, &[0x51], 1, false)))
            .collect())
    }
    fn best_chainlock(&self) -> Option<BestChainLock> {
        unreachable!()
    }
    fn has_block(&self, _: &BlockHash) -> bool {
        unreachable!()
    }
    fn headers_from(&self, _: &BlockHash, _: usize) -> Result<Vec<HeaderFacts>, Aborted> {
        unreachable!()
    }
    fn active_window(&self, _: Option<u32>, _: u32) -> Result<ActiveWindow, Aborted> {
        unreachable!()
    }
}

impl NodeChainState for Fake {
    fn chain_state(&self) -> Option<Box<dyn ChainState>> {
        self.log(format!("txouts {}", self.tip.len()));
        let mined = (self.race == Race::Mined).then(|| Arc::clone(&self.raced));
        (!self.no_chain).then(|| Box::new(Tip(self.tip.clone(), mined)) as _)
    }
}

impl TxBroadcast for Fake {
    fn has_mempool(&self) -> bool {
        !self.no_mempool
    }
    fn has_peerman(&self) -> bool {
        !self.no_peerman
    }
    fn in_mempool(&self, _: &Tx) -> bool {
        self.in_mempool || (self.race == Race::Pooled && self.raced.load(Ordering::SeqCst))
    }
    fn process_transaction(&self, tx: &TxRef, test_accept: bool, bypass_limits: bool) -> Answer {
        let kind = if test_accept {
            "test"
        } else {
            "accept"
        };
        self.log(format!("{kind} {} bypass={bypass_limits}", tx.txid()));
        let answer = self.answers.lock().unwrap().pop_front().expect("an answer");
        if !matches!(answer, Ok(Processed::Valid(_))) {
            self.raced.store(true, Ordering::SeqCst);
        }
        answer
    }
    fn add_unbroadcast(&self, txid: &TxHash) {
        self.log(format!("unbroadcast {txid}"));
    }
    fn sync_with_validation_queue(&self) {
        self.log("sync".into());
    }
    fn relay_transaction(&self, txid: &TxHash) {
        self.log(format!("relay {txid}"));
    }
}

/// A version-2 spend of one made-up outpoint to two `OP_TRUE` outputs.
fn tx() -> Tx {
    let prevout = OutPoint::new(TxHash::from([7; 32]), 3);
    Tx::new(Transaction::new(
        2,
        TxType::Spend,
        vec![TxIn::new(prevout, vec![0x51].into(), u32::MAX)],
        vec![
            TxOut::from_duffs(5_000, vec![0x51].into()),
            TxOut::from_duffs(6_000, vec![0x51].into()),
        ],
        0,
        Vec::new(),
    ))
}

fn tx_hex() -> String {
    hex(&tx().raw().core_serialize())
}

fn txid() -> String {
    tx().txid().to_string()
}

/// `sendrawtransaction` with `rest` after the transaction's hex, through the server.
fn send(fake: Fake, rest: &str) -> (Result<String, (i32, String)>, Vec<String>) {
    send_params(fake, &format!(r#"["{}"{rest}]"#, tx_hex()))
}

fn send_params(fake: Fake, params: &str) -> (Result<String, (i32, String)>, Vec<String>) {
    let fake = Arc::new(fake);
    let server = RpcServer::new(Arc::new(FakeControl::default()));
    register_broadcast_commands(&server, Arc::clone(&fake) as _, Arc::clone(&fake) as _);
    server.start();
    server.set_warmup_finished();
    let reply = server
        .execute(&JsonRpcRequest {
            method: "sendrawtransaction".into(),
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
        });
    (reply, fake.calls())
}

fn error(code: i32, message: &str) -> Result<String, (i32, String)> {
    Err((code, message.into()))
}

fn reject(result: TxValidationResult, reason: &'static str, debug: &str) -> Answer {
    Ok(Processed::Invalid(TxValidationError::new(result, reason).with_debug(debug)))
}

fn fee(base_fee: i64) -> Answer {
    Ok(Processed::Valid(Some(base_fee)))
}

/// The default `maxfeerate`'s fee: 0.10 DASH/kB of the transaction's size.
fn default_max_fee() -> i64 {
    10_000 * i64::from(tx().size())
}

/// A fresh admission: the test (with a fee rate), the admission, `AddUnbroadcastTx`, the
/// validation queue's drain and the relay, in Core's order.
// Core: v24.0.2 src/node/transaction.cpp:64-122
#[test]
fn an_admission_is_tested_admitted_marked_unbroadcast_drained_and_relayed() {
    let paid = fee(10_000);
    let (reply, calls) = send(Fake::answering(&[paid.clone(), paid]), "");
    assert_eq!(reply, Ok(format!(r#""{}""#, txid())));
    let t = txid();
    assert_eq!(
        calls,
        [
            "txouts 0".to_owned(),
            format!("test {t} bypass=false"),
            format!("accept {t} bypass=false"),
            format!("unbroadcast {t}"),
            "sync".into(),
            format!("relay {t}"),
        ]
    );
}

/// `maxfeerate` 0 skips the test; `bypasslimits` reaches both calls; `instantsend` is ignored.
// Core: v24.0.2 src/node/transaction.cpp:72; src/rpc/mempool.cpp:57-58, :82-83
#[test]
fn a_zero_rate_skips_the_test_and_bypasslimits_reaches_the_pool() {
    let t = txid();
    let (reply, calls) = send(Fake::answering(&[fee(1 << 40)]), ", 0, false, true");
    assert_eq!(reply, Ok(format!(r#""{t}""#)));
    assert_eq!(calls[1], format!("accept {t} bypass=true"));
    assert_eq!(calls.len(), 5);

    let paid = fee(10_000);
    let (_, calls) = send(Fake::answering(&[paid.clone(), paid]), r#", "0.5", true, true"#);
    assert_eq!(calls[1], format!("test {t} bypass=true"));
    assert_eq!(calls[2], format!("accept {t} bypass=true"));
}

/// A fee above the rate's fee of the transaction's size is refused after the test, with
/// `TransactionErrorString`; the fee equal to it passes.
// Core: v24.0.2 src/node/transaction.cpp:72-80; src/util/error.cpp:34-35; src/policy/feerate.cpp:23-37
#[test]
fn a_fee_above_the_maximum_is_refused_after_the_test() {
    let over = fee(default_max_fee() + 1);
    let (reply, calls) = send(Fake::answering(&[over]), "");
    assert_eq!(
        reply,
        error(
            RPC_TRANSACTION_ERROR,
            "Fee exceeds maximum configured by user (e.g. -maxtxfee, maxfeerate)"
        )
    );
    assert_eq!(calls, ["txouts 0".to_owned(), format!("test {} bypass=false", txid())]);

    let at = fee(default_max_fee());
    assert!(send(Fake::answering(&[at.clone(), at]), "").0.is_ok());
    // One duff per kB still allows one duff: `GetFee`'s rounding up.
    let (reply, _) = send(Fake::answering(&[fee(2)]), ", 0.00000001");
    assert_eq!(reply.unwrap_err().0, RPC_TRANSACTION_ERROR);
    assert_eq!(get_fee(1, tx().size()), 1);
    assert_eq!(get_fee(-1, 0), 0);
    assert_eq!(get_fee(1_500, 3), 5);
}

/// Any unspent output of the txid in the coins tip ends the call before the pool is asked.
// Core: v24.0.2 src/node/transaction.cpp:47-55; src/rpc/util.cpp:1161-1162
#[test]
fn an_unspent_output_in_the_tip_is_already_in_chain() {
    let fake = Fake {
        tip: vec![OutPoint::new(tx().txid(), 1)],
        ..Fake::default()
    };
    let (reply, calls) = send(fake, "");
    assert_eq!(
        reply,
        error(RPC_TRANSACTION_ALREADY_IN_CHAIN, "Transaction already in block chain")
    );
    assert_eq!(calls, ["txouts 1"]);

    let other = Fake {
        tip: vec![OutPoint::new(TxHash::from([7; 32]), 3), OutPoint::new(tx().txid(), 2)],
        ..Fake::answering(&[fee(0), fee(0)])
    };
    assert!(send(other, "").0.is_ok(), "the spent input and a third output are not the tx's");
}

/// A pooled txid is relayed again with no test, admission, mark or drain.
// Core: v24.0.2 src/node/transaction.cpp:63-69, :119-121
#[test]
fn a_pooled_transaction_is_only_relayed_again() {
    let fake = Fake {
        in_mempool: true,
        ..Fake::default()
    };
    let (reply, calls) = send(fake, ", 1e-8");
    assert_eq!(reply, Ok(format!(r#""{}""#, txid())));
    assert_eq!(calls, ["txouts 0".to_owned(), format!("relay {}", txid())]);
}

/// `HandleATMPError`: missing inputs are -25, any other refusal -26, each with the state's
/// `ToString()` (the reason, then `, ` and the debug message when there is one), at the test or
/// at the admission.
// Core: v24.0.2 src/node/transaction.cpp:21-31, :76-87; src/consensus/validation.h:105-116
#[test]
fn refusals_carry_the_state_string() {
    use TxValidationResult::{Conflict, Consensus, MempoolPolicy, MissingInputs};
    let missing = reject(MissingInputs, "bad-txns-inputs-missingorspent", "");
    let (reply, calls) = send(Fake::answering(&[missing]), "");
    assert_eq!(reply, error(RPC_TRANSACTION_ERROR, "bad-txns-inputs-missingorspent"));
    assert_eq!(calls.len(), 2);

    let policy = reject(MempoolPolicy, "min relay fee not met", "0 < 192");
    let (reply, _) = send(Fake::answering(&[policy]), "");
    assert_eq!(reply, error(RPC_TRANSACTION_REJECTED, "min relay fee not met, 0 < 192"));

    let conflict = reject(Conflict, "txn-mempool-conflict", "");
    let (reply, calls) = send(Fake::answering(&[fee(0), conflict]), "");
    assert_eq!(reply, error(RPC_TRANSACTION_REJECTED, "txn-mempool-conflict"));
    assert_eq!(calls.len(), 3, "no mark, drain or relay after a refused admission");

    let missing = reject(MissingInputs, "bad-txns-inputs-missingorspent", "");
    let (reply, _) = send(Fake::answering(&[missing]), ", 0");
    assert_eq!(reply, error(RPC_TRANSACTION_ERROR, "bad-txns-inputs-missingorspent"));

    let consensus = reject(Consensus, "bad-txns-in-belowout", "value in (0.00) < value out (1.00)");
    let (reply, _) = send(Fake::answering(&[consensus]), ", 0");
    assert_eq!(
        reply,
        error(RPC_TRANSACTION_REJECTED, "bad-txns-in-belowout, value in (0.00) < value out (1.00)")
    );
}

/// `DecodeHexTx`'s refusal, before the fee rate is read: not hex, empty, odd, trailing bytes.
// Core: v24.0.2 src/rpc/mempool.cpp:77-80; src/core_read.cpp:120-128
#[test]
fn an_undecodable_transaction_is_refused_first() {
    let message = "TX decode failed. Make sure the tx has at least one input.";
    for hexstring in ["zz", "", "0", &format!("{}00", tx_hex())] {
        let (reply, calls) = send_params(Fake::default(), &format!(r#"["{hexstring}", -1]"#));
        assert_eq!(reply, error(RPC_DESERIALIZATION_ERROR, message), "{hexstring:?}");
        assert!(calls.is_empty());
    }
}

/// `ParseFeeRate`'s errors (`AmountFromValue`'s, then the one-DASH bound), before the node is
/// asked; a null rate is the default.
// Core: v24.0.2 src/rpc/util.cpp:68-85; src/rpc/mempool.cpp:82
#[test]
fn maxfeerate_follows_parse_fee_rate() {
    for (rate, code, message) in [
        ("-1", RPC_TYPE_ERROR, "Amount out of range"),
        ("-0.00000001", RPC_TYPE_ERROR, "Amount out of range"),
        ("0.000000001", RPC_TYPE_ERROR, "Invalid amount"),
        (r#""x""#, RPC_TYPE_ERROR, "Invalid amount"),
        ("true", RPC_TYPE_ERROR, "Amount is not a number or string"),
        ("[]", RPC_TYPE_ERROR, "Amount is not a number or string"),
        ("21000001", RPC_TYPE_ERROR, "Amount out of range"),
        ("1", RPC_INVALID_PARAMETER, "Fee rates larger than or equal to 1DASH/kB are not accepted"),
        (
            r#""1.0""#,
            RPC_INVALID_PARAMETER,
            "Fee rates larger than or equal to 1DASH/kB are not accepted",
        ),
    ] {
        let (reply, calls) = send(Fake::default(), &format!(", {rate}"));
        assert_eq!(reply, error(code, message), "{rate}");
        assert!(calls.is_empty(), "{rate}");
    }
    let paid = fee(default_max_fee());
    assert!(send(Fake::answering(&[paid.clone(), paid]), ", null").0.is_ok());
    let paid = fee(99_999_999 * i64::from(tx().size()) / 1000);
    assert!(send(Fake::answering(&[paid.clone(), paid]), ", 0.99999999").0.is_ok());
}

/// The server's checks: arity is the help, and `instantsend` and `bypasslimits` must be booleans.
// Core: v24.0.2 src/rpc/util.cpp:525-551, :714-758; src/rpc/mempool.cpp:58-59
#[test]
fn the_arguments_are_type_checked() {
    let (reply, _) = send_params(Fake::default(), "[]");
    let (code, help) = reply.unwrap_err();
    assert_eq!(code, RPC_MISC_ERROR);
    assert!(help.starts_with("sendrawtransaction \"hexstring\""), "{help}");
    for rest in [r#", 0, "x""#, ", 0, false, 1", ", 0, null, false, 0"] {
        let (reply, calls) = send(Fake::default(), rest);
        assert_eq!(
            reply.unwrap_err().0,
            if rest.ends_with(" 0") {
                RPC_MISC_ERROR
            } else {
                RPC_TYPE_ERROR
            }
        );
        assert!(calls.is_empty());
    }
    let (reply, _) = send_params(Fake::default(), "[1]");
    assert_eq!(reply.unwrap_err().0, RPC_TYPE_ERROR);
}

/// Core asserts the chainman, the pool and the peer manager; here they are the `Ensure*`
/// errors, checked before any read, and a pool whose admission worker is gone is the pool's.
// Core: v24.0.2 src/node/transaction.cpp:38-40; src/rpc/server_util.cpp:47-53, :142-148
#[test]
fn a_missing_part_of_the_node_is_its_ensure_error() {
    let fake = Fake {
        no_chain: true,
        ..Fake::default()
    };
    assert_eq!(send(fake, "").0, error(RPC_INTERNAL_ERROR, "Node chainman not found"));
    let fake = Fake {
        no_mempool: true,
        no_peerman: true,
        ..Fake::default()
    };
    let no_pool = error(RPC_CLIENT_MEMPOOL_DISABLED, "Mempool disabled or instance not found");
    assert_eq!(send(fake, "").0, no_pool);
    let fake = Fake {
        no_peerman: true,
        ..Fake::default()
    };
    let (reply, calls) = send(fake, "");
    assert_eq!(
        reply,
        error(RPC_CLIENT_P2P_DISABLED, "Error: Peer-to-peer functionality missing or disabled")
    );
    assert_eq!(calls, ["txouts 0"]);

    let (reply, _) = send(Fake::answering(&[Err(NoVerdict::NoMempool)]), "");
    assert_eq!(reply, no_pool);
    let (reply, calls) = send(Fake::answering(&[fee(0), Err(NoVerdict::NoMempool)]), "");
    assert_eq!(reply, no_pool);
    assert_eq!(calls.len(), 3);
}

/// Core holds `cs_main` from the chain check to the admission. A relayed copy admitted between
/// our turns makes a refusal re-read as the pooled path (relay only), at the test or at the
/// admission, as Core answers once that copy is in.
// Core: v24.0.2 src/node/transaction.cpp:46-69, :119-121
#[test]
fn a_copy_pooled_during_a_refusal_is_only_relayed() {
    let t = txid();
    let already = reject(TxValidationResult::Conflict, "txn-already-in-mempool", "");
    let fake = Fake {
        race: Race::Pooled,
        ..Fake::answering(std::slice::from_ref(&already))
    };
    let (reply, calls) = send(fake, "");
    assert_eq!(reply, Ok(format!(r#""{t}""#)));
    assert_eq!(
        calls,
        ["txouts 0".to_owned(), format!("test {t} bypass=false"), format!("relay {t}")]
    );

    let fake = Fake {
        race: Race::Pooled,
        ..Fake::answering(&[fee(0), already])
    };
    let (reply, calls) = send(fake, "");
    assert_eq!(reply, Ok(format!(r#""{t}""#)));
    assert_eq!(calls[2..], [format!("accept {t} bypass=false"), format!("relay {t}")]);
}

/// A block confirming the transaction between our turns makes a refusal re-read as
/// `ALREADY_IN_CHAIN`, never `txn-already-known`.
// Core: v24.0.2 src/node/transaction.cpp:46-55
#[test]
fn a_block_during_a_refusal_is_already_in_chain() {
    let in_chain = error(RPC_TRANSACTION_ALREADY_IN_CHAIN, "Transaction already in block chain");
    let known = reject(TxValidationResult::Conflict, "txn-already-known", "");
    for answers in [vec![known.clone()], vec![fee(0), known]] {
        let fake = Fake {
            race: Race::Mined,
            ..Fake::answering(&answers)
        };
        let (reply, calls) = send(fake, "");
        assert_eq!(reply, in_chain, "{answers:?}");
        assert_eq!(calls.len(), 1 + answers.len(), "no mark, drain or relay");
    }
}

/// A dropped reply on an attached pool is V's abort (D9); a `MEMPOOL_ENTRY` result is
/// `MEMPOOL_ERROR` with its valid state's `ToString()`; a valid test without a base fee is
/// `.value()`'s throw, and nothing is admitted.
// Core: v24.0.2 src/node/transaction.cpp:21-31, :75-80; src/util/error.cpp:28-29
#[test]
fn a_result_without_a_verdict_or_fee_follows_core() {
    let (reply, _) = send(Fake::answering(&[Err(NoVerdict::Aborted)]), "");
    assert_eq!(reply, error(RPC_DATABASE_ERROR, crate::blockchain::ABORTED));
    let (reply, _) = send(Fake::answering(&[fee(0), Err(NoVerdict::Aborted)]), "");
    assert_eq!(reply, error(RPC_DATABASE_ERROR, crate::blockchain::ABORTED));

    let (reply, _) = send(Fake::answering(&[Ok(Processed::MempoolEntry)]), "");
    assert_eq!(reply, error(RPC_TRANSACTION_ERROR, "Valid"));
    assert_eq!(
        transaction_error(TransactionError::MempoolError, ""),
        RpcError::new(RPC_TRANSACTION_ERROR, "Mempool internal error")
    );

    let (reply, calls) = send(Fake::answering(&[Ok(Processed::Valid(None))]), "");
    assert_eq!(reply, error(RPC_MISC_ERROR, "bad optional access"));
    assert_eq!(calls.len(), 2, "no admission");
    let (reply, _) = send(Fake::answering(&[Ok(Processed::Valid(None))]), ", 0");
    assert!(reply.is_ok(), "the admission's fee is not read");
}
