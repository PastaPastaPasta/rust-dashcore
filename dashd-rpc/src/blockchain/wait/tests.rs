use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dash_primitives::BlockHash;

use super::{BlockChange, register_wait_commands};
use crate::UniValue;
use crate::protocol::{RPC_INVALID_PARAMETER, RPC_TYPE_ERROR, RpcError};
use crate::request::JsonRpcRequest;
use crate::server::RpcServer;
use crate::testutil::FakeControl;

fn hash(n: u8) -> BlockHash {
    BlockHash::from([n; 32])
}

/// A started server with the waits, out of warmup, and their `latestblock`.
fn server() -> (Arc<RpcServer>, Arc<BlockChange>) {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    let block_change = Arc::new(BlockChange::default());
    register_wait_commands(&server, Arc::clone(&block_change));
    server.start();
    server.set_warmup_finished();
    (server, block_change)
}

fn call(server: &RpcServer, method: &str, params: &str) -> Result<UniValue, RpcError> {
    server.execute(&JsonRpcRequest {
        method: method.into(),
        params: UniValue::read(params.as_bytes()).unwrap(),
        ..Default::default()
    })
}

fn block(n: u8, height: i64) -> String {
    format!(r#"{{"hash":"{}","height":{height}}}"#, hash(n))
}

/// How many rounds of 5 ms or more [`waits`] pokes before it fails: at least 2 s.
const ROUNDS: u32 = 400;

/// `call` on its own thread, while `poke` runs until the call returns: the call's reply. A call
/// that outlives [`ROUNDS`] fails the test rather than hanging it.
fn waits(
    server: &Arc<RpcServer>,
    method: &'static str,
    params: &'static str,
    mut poke: impl FnMut(u32),
) -> String {
    let s = Arc::clone(server);
    let call = thread::spawn(move || call(&s, method, params).unwrap().write());
    let mut round = 0;
    while !call.is_finished() {
        assert!(round < ROUNDS, "{method} {params} outlived {ROUNDS} rounds");
        poke(round);
        round += 1;
        thread::sleep(Duration::from_millis(5));
    }
    call.join().unwrap()
}

/// Before the RPC server starts nothing is connected, and `latestblock` is Core's zero static;
/// init's `set_tip` sets it regardless. A negative or short timeout answers the current block.
// Core: rpc/blockchain.cpp:84-85, init.cpp:2769
#[test]
fn the_waits_read_latestblock_from_rpc_start() {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    let b = Arc::new(BlockChange::default());
    register_wait_commands(&server, Arc::clone(&b));
    b.notify_block_tip(hash(1), 1);
    assert_eq!(b.latest(), super::UpdatedBlock::default(), "not connected before StartRPC");
    b.set_tip(Some((hash(2), 2)));
    server.start();
    server.set_warmup_finished();
    assert_eq!(call(&server, "waitfornewblock", "[-1]").unwrap().write(), block(2, 2));
    // A negative timeout checks once, though nothing will satisfy the wait.
    assert_eq!(waits(&server, "waitforblockheight", "[100, -1]", |_| {}), block(2, 2));
    let t0 = Instant::now();
    assert_eq!(call(&server, "waitfornewblock", "[20]").unwrap().write(), block(2, 2));
    assert!(t0.elapsed() < Duration::from_secs(2), "20 ms, not 20 s: {:?}", t0.elapsed());
    b.notify_block_tip(hash(3), 3);
    assert_eq!(call(&server, "waitforblockheight", "[3]").unwrap().write(), block(3, 3));
    assert_eq!(
        call(&server, "waitforblock", &format!(r#"["{}"]"#, hash(3))).unwrap().write(),
        block(3, 3)
    );
    // A null timeout is 0: no timeout.
    assert_eq!(call(&server, "waitforblockheight", "[2, null]").unwrap().write(), block(3, 3));
    b.set_tip(None);
    assert_eq!(b.latest().height, 3, "no tip only wakes the waits");
}

/// `waitfornewblock` with no timeout returns at the first block after its call's; a
/// notification of the same block is no change. From the tenth round each round's block differs
/// from the last, so a call that starts late still sees a change.
// Core: rpc/blockchain.cpp:334-343
#[test]
fn waitfornewblock_returns_the_next_block() {
    let (server, b) = server();
    b.notify_block_tip(hash(1), 1);
    let got = waits(&server, "waitfornewblock", "[]", |round| {
        // The same block again, then new ones once the call has had time to start.
        let n = u8::try_from(round.saturating_sub(9) % 200 + 1).unwrap();
        b.notify_block_tip(hash(n), u32::from(n));
    });
    assert!((1..=200).any(|n| got == block(n, i64::from(n))), "{got}");
    if got == block(1, 1) {
        panic!("the same block is no change: the call outlived 200 rounds");
    }
}

/// A block at the same height with another hash is a new block: a reorg to a sibling, as
/// `preciousblock` makes. From the tenth round two siblings alternate, so a call that starts
/// late still sees a change; a height-only comparison never does.
// Core: rpc/blockchain.cpp:339-341
#[test]
fn waitfornewblock_sees_a_same_height_sibling() {
    let (server, b) = server();
    b.notify_block_tip(hash(1), 1);
    let got = waits(&server, "waitfornewblock", "[]", |round| {
        if round >= 10 {
            b.notify_block_tip(hash(8 + u8::from(round % 2 == 0)), 1);
        }
    });
    assert!(got == block(8, 1) || got == block(9, 1), "{got}");
}

/// A timed wait keeps one deadline across the blocks that wake it and do not satisfy it, and
/// counts it in milliseconds: `[100, 100]` under a block every 5 ms ends in about 100 ms.
// Core: rpc/blockchain.cpp:428-431 (`wait_for` is `wait_until(now + ms)`)
#[test]
fn a_timed_wait_under_traffic_times_out() {
    let (server, b) = server();
    b.notify_block_tip(hash(1), 1);
    let got = waits(&server, "waitforblockheight", "[100, 100]", |round| {
        let n = u8::try_from(round % 50 + 1).unwrap();
        b.notify_block_tip(hash(n), u32::from(n));
    });
    assert!((1..=50).any(|n| got == block(n, i64::from(n))), "{got}");
}

/// A timed wait rechecks at each notification and returns at its block, long before its
/// deadline.
// Core: rpc/blockchain.cpp:428-434
#[test]
fn a_timed_wait_returns_at_its_block() {
    let (server, b) = server();
    let got = waits(&server, "waitforblockheight", "[3, 60000]", |round| {
        let n = u8::try_from(round.min(3)).unwrap();
        b.notify_block_tip(hash(n), u32::from(n));
    });
    assert_eq!(got, block(3, 3));
}

/// `waitforblock` returns once the latest block is the hash, `waitforblockheight` once its
/// height is at least the argument; both with no timeout.
// Core: rpc/blockchain.cpp:381-388, :428-435
#[test]
fn waitforblock_and_waitforblockheight_wait_for_their_block() {
    let (server, b) = server();
    let got = waits(&server, "waitforblockheight", "[5]", |round| {
        let n = u8::try_from(round % 7).unwrap();
        b.notify_block_tip(hash(n), u32::from(n));
    });
    assert_eq!(got, block(5, 5));
    const SEVEN: &str = r#"["0707070707070707070707070707070707070707070707070707070707070707"]"#;
    let got = waits(&server, "waitforblock", SEVEN, |round| {
        let n = u8::try_from(round % 8).unwrap();
        b.notify_block_tip(hash(n), u32::from(n));
    });
    assert_eq!(got, block(7, 7));
}

/// At shutdown `OnRPCStopped` wakes each of the three waits with no timeout once the server is
/// interrupted, and it answers the latest block; the handler is disconnected from then on.
// Core: init.cpp:563-568, rpc/blockchain.cpp:341, :387, :434
#[test]
fn a_stopped_server_ends_every_wait() {
    const NINE: &str = r#"["0909090909090909090909090909090909090909090909090909090909090909"]"#;
    for (method, params) in
        [("waitfornewblock", "[]"), ("waitforblock", NINE), ("waitforblockheight", "[100]")]
    {
        let (server, b) = server();
        b.notify_block_tip(hash(4), 4);
        let s = Arc::clone(&server);
        let got = waits(&server, method, params, |round| {
            if round == 10 {
                s.interrupt();
                s.stop();
            }
        });
        assert_eq!(got, block(4, 4), "{method}");
        b.notify_block_tip(hash(5), 5);
        assert_eq!(b.latest().height, 4, "{method}: disconnected at stop");
    }
}

/// The arguments are Core's: `ParseHashV`'s errors, and `getInt<int>` heights and timeouts.
// Core: rpc/util.cpp:87-95
#[test]
fn the_waits_refuse_cores_bad_arguments() {
    let (server, _) = server();
    let err = |method, params| call(&server, method, params).unwrap_err();
    let e = err("waitforblock", r#"["00"]"#);
    assert_eq!(
        (e.code(), e.to_object().write()),
        (
            RPC_INVALID_PARAMETER,
            r#"{"code":-8,"message":"blockhash must be of length 64 (not 2, for '00')"}"#.into()
        )
    );
    assert_eq!(err("waitforblockheight", r#"["1"]"#).code(), RPC_TYPE_ERROR);
    assert_eq!(err("waitfornewblock", r#"["1"]"#).code(), RPC_TYPE_ERROR);
}
