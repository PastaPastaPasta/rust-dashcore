use std::sync::{Arc, Mutex};

use dash_consensus::Block;
use dash_consensus::primitives::OutPoint;
use dash_num::Arith256;
use dash_primitives::BlockHash;

use super::*;
use crate::blockchain::{
    Aborted, ActiveWindow, BestChainLock, ChainTxError, ChainTxPoint, DeploymentError,
    DeploymentReport, HeaderFacts, TipInfo, UnspentCoin,
};
use crate::mining::{BlockCheck, Grind, LastBlockStats, TemplateRefusal};
use crate::protocol::RPC_MISC_ERROR;
use crate::request::JsonRpcRequest;
use crate::testutil::FakeControl;

/// `DifficultyAdjustmentInterval()` of the tests' chains: 4 blocks.
const INTERVAL: i64 = 4;

/// An active chain, block `h` at `times[h]` with a chain work of `2(h + 1)`.
struct FakeState {
    times: Vec<u32>,
    stopped: bool,
    /// Each [`ChainState::active_window`] call's arguments.
    reads: Mutex<Vec<(Option<u32>, u32)>>,
}

impl FakeState {
    /// Blocks 0..=`tip`, one every 150 seconds.
    fn new(tip: u32) -> Arc<FakeState> {
        FakeState::with_times((0..=tip).map(|h| 1000 + 150 * h).collect())
    }

    fn with_times(times: Vec<u32>) -> Arc<FakeState> {
        Arc::new(FakeState {
            times,
            stopped: false,
            reads: Mutex::default(),
        })
    }

    fn block(&self, h: u32) -> WindowBlock {
        WindowBlock {
            height: h,
            bits: 0x207f_ffff,
            time: self.times[h as usize],
            chain_work: work(h),
        }
    }
}

fn work(h: u32) -> Arith256 {
    Arith256::from_u64(2 * (u64::from(h) + 1))
}

impl ChainState for Arc<FakeState> {
    fn active_window(&self, height: Option<u32>, back: u32) -> Result<ActiveWindow, Aborted> {
        self.reads.lock().unwrap().push((height, back));
        if self.stopped {
            return Err(Aborted);
        }
        let tip = self.times.len() as u32 - 1;
        let at = height.unwrap_or(tip);
        let blocks = if at > tip {
            Vec::new()
        } else {
            (0..=at).rev().take(back as usize + 1).map(|h| self.block(h)).collect()
        };
        Ok(ActiveWindow {
            height: tip,
            blocks,
        })
    }

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
    fn best_chainlock(&self) -> Option<BestChainLock> {
        unreachable!()
    }
    fn has_block(&self, _: &BlockHash) -> bool {
        unreachable!()
    }
    fn headers_from(&self, _: &BlockHash, _: usize) -> Result<Vec<HeaderFacts>, Aborted> {
        unreachable!()
    }
}

/// The chainstate, or none before it loads.
struct FakeNode(Option<Arc<FakeState>>);

impl NodeChainState for FakeNode {
    fn chain_state(&self) -> Option<Box<dyn ChainState>> {
        Some(Box::new(Arc::clone(self.0.as_ref()?)))
    }
}

/// An assembler that has assembled the block `last`, or none.
struct FakeMiner(Option<LastBlockStats>);

impl Miner for FakeMiner {
    fn create_new_block(&self, _: &[u8], _: bool) -> Result<Block, TemplateRefusal> {
        unreachable!()
    }
    fn grind(&self, _: &mut Block, _: &mut u64, _: &dyn Fn() -> bool) -> Grind {
        unreachable!()
    }
    fn test_block_validity(&self, _: Arc<Block>) -> BlockCheck {
        unreachable!()
    }
    fn last_block_stats(&self) -> Option<LastBlockStats> {
        self.0
    }
}

struct Warnings;

impl NodeWarnings for Warnings {
    fn get_warnings(&self, verbose: bool) -> String {
        assert!(!verbose, "getmininginfo reads the concise text");
        "a warning".into()
    }
}

fn server_with(state: Option<&Arc<FakeState>>, last: Option<LastBlockStats>) -> Arc<RpcServer> {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    register_mining_info_commands(
        &server,
        Arc::new(FakeNode(state.cloned())),
        Arc::new(FakeMiner(last)),
        MiningInfoParams {
            network_id: "devnet".into(),
            difficulty_adjustment_interval: INTERVAL,
            warnings: Arc::new(Warnings),
        },
    );
    server.start();
    server.set_warmup_finished();
    server
}

fn call(server: &RpcServer, method: &str, params: &str) -> Result<String, (i32, String)> {
    server
        .execute(&JsonRpcRequest {
            method: method.into(),
            params: UniValue::read(params.as_bytes()).unwrap(),
            ..Default::default()
        })
        .map(|v| v.write())
        .map_err(|e| (e.code(), e.to_object().write()))
}

fn hash_ps(state: &Arc<FakeState>, params: &str) -> Result<String, (i32, String)> {
    call(&server_with(Some(state), None), "getnetworkhashps", params)
}

/// The rate of a work delta over a time span, as the command prints it.
fn rate(work: u64, seconds: u32) -> String {
    UniValue::from_f64(work as f64 / f64::from(seconds)).unwrap().write()
}

fn invalid(message: &str) -> (i32, String) {
    (RPC_INVALID_PARAMETER, format!(r#"{{"code":-8,"message":"{message}"}}"#))
}

#[test]
fn nblocks_must_be_positive_or_minus_one() {
    let state = FakeState::new(9);
    let refused = invalid("Invalid nblocks. Must be a positive number or -1.");
    for nblocks in ["0", "-2", "-2147483648"] {
        assert_eq!(hash_ps(&state, &format!("[{nblocks}]")), Err(refused.clone()), "{nblocks}");
    }
    // Refused before the height is checked.
    assert_eq!(hash_ps(&state, "[0, -2]"), Err(refused));
}

#[test]
fn the_height_must_be_on_the_active_chain() {
    let state = FakeState::new(9);
    let refused = invalid("Block does not exist at specified height");
    for height in ["-2", "10", "2147483647"] {
        assert_eq!(hash_ps(&state, &format!("[1, {height}]")), Err(refused.clone()), "{height}");
    }
    assert_eq!(hash_ps(&state, "[1, 9]"), hash_ps(&state, "[1, -1]"));
    assert_eq!(hash_ps(&state, "[1, 9]"), Ok(rate(2, 150)));
}

#[test]
fn the_genesis_block_and_a_window_in_one_second_have_no_rate() {
    assert_eq!(hash_ps(&FakeState::new(0), "[]"), Ok("0".into()));
    assert_eq!(hash_ps(&FakeState::new(9), "[5, 0]"), Ok("0".into()));
    let flat = FakeState::with_times(vec![1000, 1600, 1600, 1600]);
    assert_eq!(hash_ps(&flat, "[2]"), Ok("0".into()));
    assert_eq!(hash_ps(&flat, "[3]"), Ok(rate(6, 600)));
}

#[test]
fn the_window_is_capped_at_the_blocks_height() {
    let state = FakeState::new(9);
    // Blocks 9 back to genesis: a work of 20 − 2 over 1,350 seconds.
    for nblocks in ["[]", "[9]", "[10]", "[1000]", "[2147483647]"] {
        assert_eq!(hash_ps(&state, nblocks), Ok(rate(18, 1350)), "{nblocks}");
    }
    assert_eq!(hash_ps(&state, "[3]"), Ok(rate(6, 450)));
    // At block 5: blocks 5 back to genesis.
    assert_eq!(hash_ps(&state, "[1000, 5]"), Ok(rate(10, 750)));
}

#[test]
fn minus_one_reads_the_blocks_since_the_last_difficulty_change() {
    let state = FakeState::new(13);
    // With an interval of 4, block h's window is h % 4 + 1 blocks back: 13 back to 11, 11 back
    // to 7, 12 back to 11, and 3 back to genesis, capped at its height.
    for (height, blocks) in [(13, 2), (11, 4), (12, 1), (3, 3)] {
        let got = hash_ps(&state, &format!("[-1, {height}]"));
        assert_eq!(got, Ok(rate(2 * blocks, 150 * blocks as u32)), "height {height}");
    }
    // The tip's, and the node is asked for the whole interval.
    state.reads.lock().unwrap().clear();
    assert_eq!(hash_ps(&state, "[-1]"), Ok(rate(4, 300)));
    assert_eq!(*state.reads.lock().unwrap(), [(None, 4)]);
}

#[test]
fn the_rate_spans_the_earliest_and_latest_times_in_the_window() {
    // Times out of order: the window's extremes are inside it, not at its ends.
    let state = FakeState::with_times(vec![1000, 1100, 900, 1500, 1200]);
    assert_eq!(hash_ps(&state, "[3]"), Ok(rate(6, 600)));
    assert_eq!(hash_ps(&state, "[2]"), Ok(rate(4, 600)));
    assert_eq!(hash_ps(&state, "[1]"), Ok(rate(2, 300)));
}

#[test]
fn the_window_is_one_read() {
    let state = FakeState::new(9);
    hash_ps(&state, "[3, 7]").unwrap();
    hash_ps(&state, "[]").unwrap();
    assert_eq!(*state.reads.lock().unwrap(), [(Some(7), 3), (None, 120)]);
}

#[test]
fn getmininginfo_reports_the_tip_and_the_default_rate() {
    // Blocks further apart up the chain, so each window has its own rate: the default's is the
    // whole chain's, where -1's (blocks 9 back to 7) would be 4 over 1,700 seconds.
    let state =
        FakeState::with_times(vec![1000, 1100, 1300, 1600, 2000, 2500, 3100, 3800, 4600, 5500]);
    let info = call(&server_with(Some(&state), None), "getmininginfo", "[]").unwrap();
    let difficulty = crate::blockchain::difficulty(0x207f_ffff).write();
    assert_eq!(
        info,
        format!(
            r#"{{"blocks":9,"difficulty":{difficulty},"networkhashps":{},"pooledtx":0,"chain":"devnet","warnings":"a warning"}}"#,
            rate(18, 4500)
        )
    );
    // One read, as `getnetworkhashps` makes with its defaults.
    assert_eq!(*state.reads.lock().unwrap(), [(None, 120)]);
}

#[test]
fn getmininginfo_reports_the_last_assembly_once_there_is_one() {
    let state = FakeState::new(0);
    let last = LastBlockStats {
        size: 1000,
        num_txs: 0,
    };
    let info = call(&server_with(Some(&state), Some(last)), "getmininginfo", "[]").unwrap();
    let difficulty = crate::blockchain::difficulty(0x207f_ffff).write();
    assert_eq!(
        info,
        format!(
            r#"{{"blocks":0,"currentblocksize":1000,"currentblocktx":0,"difficulty":{difficulty},"networkhashps":0,"pooledtx":0,"chain":"devnet","warnings":"a warning"}}"#
        )
    );
}

#[test]
fn before_the_chainstate_loads_and_after_it_stops() {
    let none = server_with(None, None);
    for method in ["getnetworkhashps", "getmininginfo"] {
        let (code, _) = call(&none, method, "[]").unwrap_err();
        assert_eq!(code, no_chainman().code(), "{method}");
    }
    let stopped = Arc::new(FakeState {
        times: vec![1000],
        stopped: true,
        reads: Mutex::default(),
    });
    let server = server_with(Some(&stopped), None);
    for method in ["getnetworkhashps", "getmininginfo"] {
        let (code, _) = call(&server, method, "[]").unwrap_err();
        assert_eq!(code, RpcError::from(Aborted).code(), "{method}");
    }
}

#[test]
fn integer_arguments_read_as_core_s_int() {
    let state = FakeState::new(9);
    let (code, _) = hash_ps(&state, "[1.5]").unwrap_err();
    assert_eq!(code, RPC_MISC_ERROR);
    let (code, _) = hash_ps(&state, "[1, 4294967296]").unwrap_err();
    assert_eq!(code, RPC_MISC_ERROR);
    assert_eq!(hash_ps(&state, "[null, null]"), hash_ps(&state, "[120, -1]"));
}
