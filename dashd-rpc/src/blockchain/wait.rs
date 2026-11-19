//! `waitfornewblock`, `waitforblock` and `waitforblockheight` over Core's `latestblock`
//! (RP1a-2a): the RPC's own view of the best block, which `uiInterface.NotifyBlockTip` feeds
//! while the RPC server runs.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use dash_primitives::BlockHash;

use super::parse_hash_v;
use crate::UniValue;
use crate::protocol::RpcError;
use crate::request::{get_int_i32, param};
use crate::server::{Command, RpcServer};

/// Core's `CUpdatedBlock`: a block's hash and height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdatedBlock {
    pub hash: BlockHash,
    pub height: i32,
}

/// Core's `latestblock` and `cond_blockchange`, with the `NotifyBlockTip` connection
/// `OnRPCStarted` makes and `OnRPCStopped` drops. Before the first notification it is the zero
/// hash at height 0, as Core's static is.
// Core: rpc/blockchain.cpp:77-85, :300-308; init.cpp:558-568
#[derive(Debug, Default)]
pub struct BlockChange {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct State {
    latest: UpdatedBlock,
    /// `rpc_notify_block_change_connection`.
    connected: bool,
}

impl BlockChange {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The block the waits answer now.
    pub fn latest(&self) -> UpdatedBlock {
        self.state().latest
    }

    /// `uiInterface.NotifyBlockTip`'s handler: `RPCNotifyBlockChange(pindex)` while connected.
    pub fn notify_block_tip(&self, hash: BlockHash, height: u32) {
        let mut state = self.state();
        if !state.connected {
            return;
        }
        state.latest = updated(hash, height);
        drop(state);
        self.changed.notify_all();
    }

    /// `RPCNotifyBlockChange(ActiveTip())`, init's call before the warmup ends: the waits'
    /// view is the tip init loaded, whatever was notified before. With no tip (`nullptr`) it
    /// only wakes the waits.
    // Core: init.cpp:2761-2769
    pub fn set_tip(&self, tip: Option<(BlockHash, u32)>) {
        if let Some((hash, height)) = tip {
            self.state().latest = updated(hash, height);
        }
        self.changed.notify_all();
    }

    /// `OnRPCStarted`: connects the handler.
    fn started(&self) {
        self.state().connected = true;
    }

    /// `OnRPCStopped`: disconnects the handler, then `RPCNotifyBlockChange(nullptr)` wakes every
    /// wait, which sees the server is no longer running.
    fn stopped(&self) {
        self.state().connected = false;
        self.changed.notify_all();
    }

    /// `WAIT_LOCK(cs_blockchange)`, then `wait_for(timeout ms)` (`wait` for 0) until `done`
    /// holds of the block at the wait's start and the latest, or the server stops running; then
    /// the latest block. A negative timeout checks once.
    fn wait(
        &self,
        timeout_ms: i32,
        running: &dyn Fn() -> bool,
        done: impl Fn(&UpdatedBlock, &UpdatedBlock) -> bool,
    ) -> UpdatedBlock {
        let deadline = (timeout_ms != 0).then(|| {
            Instant::now() + Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0))
        });
        let mut state = self.state();
        let start = state.latest;
        while !done(&start, &state.latest) && running() {
            state = match deadline {
                None => self.changed.wait(state).unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    self.changed.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0
                }
            };
        }
        state.latest
    }
}

fn updated(hash: BlockHash, height: u32) -> UpdatedBlock {
    UpdatedBlock {
        hash,
        height: i32::try_from(height).expect("a block height fits Core's int"),
    }
}

fn reply(block: UpdatedBlock) -> UniValue {
    let mut ret = UniValue::new_object();
    ret.push_kv_end("hash", block.hash.to_string().into());
    ret.push_kv_end("height", i64::from(block.height).into());
    ret
}

/// The optional `timeout` argument: `getInt<int>`, 0 when null.
fn timeout(v: &UniValue) -> Result<i32, RpcError> {
    if v.is_null() {
        Ok(0)
    } else {
        get_int_i32(v)
    }
}

/// Registers the three waits on `server`, and `block_change`'s connection on its start and stop
/// (Core's `RPCServer::OnStarted` / `OnStopped`).
// Core: init.cpp:986-987
pub fn register_wait_commands(server: &Arc<RpcServer>, block_change: Arc<BlockChange>) {
    let b = Arc::clone(&block_change);
    server.on_started(move || b.started());
    let b = Arc::clone(&block_change);
    server.on_stopped(move || b.stopped());
    let running = |s: &Weak<RpcServer>| {
        let s = s.clone();
        move || s.upgrade().is_some_and(|s| s.is_running())
    };
    let (b, s) = (Arc::clone(&block_change), Arc::downgrade(server));
    server.append_command(Command::new("waitfornewblock", move |r| {
        waitfornewblock(&b, &running(&s), &r.params)
    }));
    let (b, s) = (Arc::clone(&block_change), Arc::downgrade(server));
    server.append_command(Command::new("waitforblock", move |r| {
        waitforblock(&b, &running(&s), &r.params)
    }));
    let s = Arc::downgrade(server);
    server.append_command(Command::new("waitforblockheight", move |r| {
        waitforblockheight(&block_change, &running(&s), &r.params)
    }));
}

// Core: rpc/blockchain.cpp:310-349
fn waitfornewblock(
    b: &BlockChange,
    running: &dyn Fn() -> bool,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let timeout = timeout(param(params, 0))?;
    Ok(reply(b.wait(timeout, running, |start, latest| latest != start)))
}

// Core: rpc/blockchain.cpp:352-395
fn waitforblock(
    b: &BlockChange,
    running: &dyn Fn() -> bool,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let hash = parse_hash_v(param(params, 0), "blockhash")?;
    let timeout = timeout(param(params, 1))?;
    Ok(reply(b.wait(timeout, running, |_, latest| latest.hash == hash)))
}

// Core: rpc/blockchain.cpp:398-441
fn waitforblockheight(
    b: &BlockChange,
    running: &dyn Fn() -> bool,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let height = get_int_i32(param(params, 0))?;
    let timeout = timeout(param(params, 1))?;
    Ok(reply(b.wait(timeout, running, |_, latest| latest.height >= height)))
}

#[cfg(test)]
mod tests;
