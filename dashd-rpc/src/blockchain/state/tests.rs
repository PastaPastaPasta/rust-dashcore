use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dash_consensus::coins::Coin;
use dash_consensus::ctx::DeploymentPos;
use dash_consensus::deployments::ThresholdState;
use dash_consensus::deployments::info::{Bip9Info, Bip9StatsInfo, BuriedInfo, DeploymentInfo};
use dash_consensus::primitives::OutPoint;
use dash_params::BuriedDeployment;
use dash_primitives::{BlockHash, BlockHeader};

use super::*;
use crate::request::JsonRpcRequest;
use crate::testutil::FakeControl;

/// Block `h`'s hash: `h` in its first byte.
fn hash(h: u8) -> BlockHash {
    let mut b = [0; 32];
    b[0] = h;
    BlockHash::from(b)
}

/// A chain of blocks 0..=`tip`, block `h` with `h` transactions before it, a median time past
/// of `1000 + 10h` and a time of `1005 + 10h`; block 200 is off the chain.
struct FakeState {
    tip: u32,
    coin: Option<Coin>,
    chainlock: Option<BestChainLock>,
    /// V has stopped: every read V answers fails.
    stopped: bool,
    /// The median time past's step per block in `getchaintxstats`' points.
    mtp_step: i64,
    /// Between two V turns: after the first read answers, the chain reorganizes onto [`FORK`]'s
    /// branch.
    reorg_after_first_read: bool,
    reorged: AtomicBool,
    /// The reads made, in order.
    reads: Mutex<Vec<String>>,
}

impl Default for FakeState {
    fn default() -> Self {
        FakeState {
            tip: 9,
            coin: None,
            chainlock: None,
            stopped: false,
            mtp_step: 10,
            reorg_after_first_read: false,
            reorged: AtomicBool::new(false),
            reads: Mutex::default(),
        }
    }
}

const OFF_CHAIN: u8 = 200;

/// The reorg's branch: it forks after block 5 and its tip is block 10; block `h` on it has the
/// hash `fork(h)`, `h + 100` transactions before it and a median time past of `2000 + 10h`.
const FORK: u32 = 5;

fn fork(h: u8) -> BlockHash {
    let mut b = [0; 32];
    b[0] = h;
    b[1] = 1;
    BlockHash::from(b)
}

impl FakeState {
    fn read(&self, what: String) -> Result<(), Aborted> {
        let mut reads = self.reads.lock().unwrap();
        reads.push(what);
        if self.reorg_after_first_read && reads.len() == 2 {
            self.reorged.store(true, Ordering::SeqCst);
        }
        if self.stopped {
            Err(Aborted)
        } else {
            Ok(())
        }
    }

    fn point(&self, h: u32) -> ChainTxPoint {
        ChainTxPoint {
            hash: hash(h as u8),
            height: h,
            time: 1005 + 10 * h,
            median_time_past: 1000 + self.mtp_step * i64::from(h),
            chain_tx: h + 1,
        }
    }

    fn reorged(&self) -> bool {
        self.reorged.load(Ordering::SeqCst)
    }

    /// Block `h` of the branch `of` is on: the fork's above [`FORK`], else the first chain's.
    fn point_on(&self, of: &BlockHash, h: u32) -> ChainTxPoint {
        if of.as_bytes()[1] == 1 && h > FORK {
            ChainTxPoint {
                hash: fork(h as u8),
                height: h,
                time: 2005 + 10 * h,
                median_time_past: 2000 + 10 * i64::from(h),
                chain_tx: h + 101,
            }
        } else {
            self.point(h)
        }
    }

    fn height_of(&self, hash: &BlockHash) -> Option<u32> {
        let h = u32::from(hash.as_bytes()[0]);
        (hash.as_bytes()[1..] == [0; 31] && (h <= self.tip || h == u32::from(OFF_CHAIN)))
            .then_some(h)
    }

    fn facts(&self, h: u32) -> HeaderFacts {
        let on_chain = h <= self.tip;
        HeaderFacts {
            hash: hash(h as u8),
            header: BlockHeader {
                version: 0x2000_0000,
                prev_hash: hash(h.saturating_sub(1) as u8),
                merkle_root: Default::default(),
                time: 1005 + 10 * h,
                bits: 0x207f_ffff,
                nonce: h,
            },
            height: h,
            confirmations: if on_chain {
                (self.tip - h + 1) as i32
            } else {
                -1
            },
            median_time_past: 1000 + 10 * i64::from(h),
            chain_work: format!("{:064x}", 2 * (h + 1)),
            n_tx: 1,
            next: (on_chain && h < self.tip).then(|| hash(h as u8 + 1)),
            chainlock: false,
        }
    }
}

impl ChainState for Arc<FakeState> {
    fn tip_info(&self) -> Result<TipInfo, Aborted> {
        self.read("tip_info".into())?;
        Ok(TipInfo {
            height: self.tip,
            hash: hash(self.tip as u8),
            bits: 0x207f_ffff,
            time: 1005 + 10 * self.tip,
            median_time_past: 1000 + 10 * i64::from(self.tip),
            headers: self.tip + 2,
            verification_progress: 1.0,
            initial_block_download: false,
            chain_work: format!("{:064x}", 2 * (self.tip + 1)),
        })
    }

    fn size_on_disk(&self) -> u64 {
        4321
    }

    fn deployment_info(&self, at: Option<&BlockHash>) -> Result<Option<DeploymentReport>, Aborted> {
        self.read(format!("deployment_info {at:?}"))?;
        let h = match at {
            Some(at) => match self.height_of(at) {
                Some(h) => h,
                None => return Ok(None),
            },
            None if self.reorged() => {
                return Ok(Some(DeploymentReport {
                    hash: fork(10),
                    height: 10,
                    deployments: fork_deployments(),
                }));
            }
            None => self.tip,
        };
        Ok(Some(DeploymentReport {
            hash: hash(h as u8),
            height: h,
            deployments: deployments(),
        }))
    }

    fn chain_tx_final(&self, hash: Option<&BlockHash>) -> Result<ChainTxPoint, ChainTxError> {
        self.read(format!("chain_tx_final {hash:?}")).map_err(|_| ChainTxError::Aborted)?;
        let h = match hash {
            None if self.reorged() => return Ok(self.point_on(&fork(10), 10)),
            None => self.tip,
            Some(hash) => match self.height_of(hash) {
                None => return Err(ChainTxError::NotFound),
                Some(h) if h > self.tip => return Err(ChainTxError::NotInMainChain),
                Some(h) => h,
            },
        };
        Ok(self.point(h))
    }

    fn chain_tx_ancestor(
        &self,
        of: &BlockHash,
        height: u32,
    ) -> Result<Option<ChainTxPoint>, Aborted> {
        self.read(format!("ancestor {} {height}", of.as_bytes()[0]))?;
        Ok(Some(self.point_on(of, height)))
    }

    fn txout(&self, out: &OutPoint) -> Result<Option<UnspentCoin>, Aborted> {
        self.read(format!("txout {}", out.index))?;
        Ok(self.coin.clone().filter(|_| out.index == 1).map(|coin| UnspentCoin {
            coin,
            best_block: hash(self.tip as u8),
            best_height: self.tip,
        }))
    }

    fn best_chainlock(&self) -> Option<BestChainLock> {
        self.chainlock.clone()
    }

    fn has_block(&self, hash: &BlockHash) -> bool {
        let _ = self.read("has_block".into());
        self.height_of(hash).is_some()
    }

    fn headers_from(&self, hash: &BlockHash, count: usize) -> Result<Vec<HeaderFacts>, Aborted> {
        self.read(format!("headers_from {count}"))?;
        let Some(h) = self.height_of(hash) else {
            return Ok(Vec::new());
        };
        if h > self.tip {
            return Ok(vec![self.facts(h)]);
        }
        Ok((h..=self.tip).take(count).map(|h| self.facts(h)).collect())
    }
}

/// A buried deployment and a versionbits one locked in, with every optional field.
fn deployments() -> Vec<DeploymentInfo> {
    vec![
        DeploymentInfo::Buried(BuriedInfo {
            deployment: BuriedDeployment::HeightInCoinbase,
            active: true,
            height: 1,
        }),
        DeploymentInfo::Bip9(Bip9Info {
            pos: DeploymentPos::TestDummy,
            bit: Some(28),
            start_time: 0,
            timeout: 9_223_372_036_854_775_807,
            ehf: false,
            ehf_height: Some(7),
            activation_height: Some(288),
            min_activation_height: 0,
            status: ThresholdState::LockedIn,
            since: 144,
            status_next: ThresholdState::Active,
            statistics: Some(Bip9StatsInfo {
                period: 144,
                elapsed: 3,
                count: 2,
                threshold: None,
                possible: None,
            }),
            signalling: Some("#-#".into()),
            height: Some(288),
            active: true,
        }),
    ]
}

/// [`deployments`] at the fork's tip: bip34 buried at a height of its own.
fn fork_deployments() -> Vec<DeploymentInfo> {
    vec![DeploymentInfo::Buried(BuriedInfo {
        deployment: BuriedDeployment::HeightInCoinbase,
        active: true,
        height: 7,
    })]
}

/// The node: a chain state, or none before the load.
#[derive(Default)]
struct FakeNode(Option<Arc<FakeState>>);

impl NodeChainState for FakeNode {
    fn chain_state(&self) -> Option<Box<dyn ChainState>> {
        Some(Box::new(Arc::clone(self.0.as_ref()?)))
    }
}

struct Warnings;

impl NodeWarnings for Warnings {
    fn get_warnings(&self, verbose: bool) -> String {
        assert!(!verbose, "getblockchaininfo reads the concise text");
        "a warning".into()
    }
}

fn server_with(node: FakeNode, softforks: bool) -> Arc<RpcServer> {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    register_chain_state_commands(
        &server,
        Arc::new(node),
        ChainStateParams {
            chain: "regtest".into(),
            pow_target_spacing: 150,
            softforks,
            key_io: crate::descriptor::tests::regtest(),
            warnings: Arc::new(Warnings),
        },
    );
    server.start();
    server.set_warmup_finished();
    server
}

fn server(state: &Arc<FakeState>) -> Arc<RpcServer> {
    server_with(FakeNode(Some(Arc::clone(state))), false)
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

fn reply(code: i32, message: &str) -> (i32, String) {
    (code, format!(r#"{{"code":{code},"message":"{message}"}}"#))
}

fn hex_of(h: u8) -> String {
    hash(h).to_string()
}

const DEPLOYMENTS: &str = r##"{"bip34":{"type":"buried","active":true,"height":1},"testdummy":{"type":"bip9","height":288,"active":true,"bip9":{"bit":28,"start_time":0,"timeout":9223372036854775807,"ehf":false,"ehf_height":7,"activation_height":288,"min_activation_height":0,"status":"locked_in","since":144,"status_next":"active","statistics":{"period":144,"elapsed":3,"count":2},"signalling":"#-#"}}}"##;

/// Every command answers Core's missing chainman before the load, after its own parsing where
/// Core parses first.
#[test]
fn every_command_needs_the_chainstate() {
    let server = server_with(FakeNode::default(), false);
    let missing = reply(RPC_INTERNAL_ERROR, "Node chainman not found");
    let txid = "11".repeat(32);
    for (method, params) in [
        ("getblockchaininfo", "[]".to_owned()),
        ("getdifficulty", "[]".to_owned()),
        ("getdeploymentinfo", r#"["00"]"#.to_owned()),
        ("getchaintxstats", r#"[1, "00"]"#.to_owned()),
        ("gettxout", r#"["00", -1]"#.to_owned()),
        ("gettxout", format!(r#"["{txid}", 1]"#)),
        // Before the ChainLock, where Core reads it first (RP1a D35).
        ("getbestchainlock", "[]".to_owned()),
    ] {
        assert_eq!(call(&server, method, &params), Err(missing.clone()), "{method}");
    }
    // `getblockheaders` parses its hash before `EnsureChainman`.
    assert_eq!(
        call(&server, "getblockheaders", r#"["00"]"#),
        Err(reply(RPC_INVALID_PARAMETER, "blockhash must be of length 64 (not 2, for '00')"))
    );
    assert_eq!(call(&server, "getblockheaders", &format!(r#"["{}"]"#, hex_of(1))), Err(missing));
}

/// A V an `AbortNode` stopped answers Core's fatal error for each read it would make (RP1a D9).
#[test]
fn a_stopped_chainstate_answers_aborted() {
    let state = Arc::new(FakeState {
        stopped: true,
        ..FakeState::default()
    });
    let server = server(&state);
    let aborted = reply(RPC_DATABASE_ERROR, ABORTED);
    let txid = "11".repeat(32);
    for (method, params) in [
        ("getblockchaininfo", "[]".to_owned()),
        ("getdifficulty", "[]".to_owned()),
        ("getdeploymentinfo", "[]".to_owned()),
        ("getchaintxstats", "[]".to_owned()),
        ("gettxout", format!(r#"["{txid}", 1]"#)),
        ("getblockheaders", format!(r#"["{}"]"#, hex_of(1))),
    ] {
        assert_eq!(call(&server, method, &params), Err(aborted.clone()), "{method}");
    }
}

/// Core's field order, with `softforks` only under `-deprecatedrpc=softforks`, at the tip it
/// reported.
// Core: rpc/blockchain.cpp:1594-1631
#[test]
fn getblockchaininfo_is_cores_fields_in_order() {
    let state = Arc::new(FakeState::default());
    let tip = hex_of(9);
    let fields = format!(
        r#"{{"chain":"regtest","blocks":9,"headers":11,"bestblockhash":"{tip}","difficulty":4.656542373906925e-10,"time":1095,"mediantime":1090,"verificationprogress":1,"initialblockdownload":false,"chainwork":"{:064x}","size_on_disk":4321,"pruned":false"#,
        20
    );
    assert_eq!(
        call(&server(&state), "getblockchaininfo", "[]"),
        Ok(format!(r#"{fields},"warnings":"a warning"}}"#))
    );
    state.reads.lock().unwrap().clear();
    let deprecated = server_with(FakeNode(Some(Arc::clone(&state))), true);
    assert_eq!(
        call(&deprecated, "getblockchaininfo", "[]"),
        Ok(format!(r#"{fields},"softforks":{DEPLOYMENTS},"warnings":"a warning"}}"#))
    );
    assert_eq!(
        *state.reads.lock().unwrap(),
        ["tip_info".to_owned(), format!("deployment_info {:?}", Some(hash(9)))]
    );
}

/// A reorg between a command's V turns (Sol r1 n-1): getblockchaininfo's `softforks` and
/// getchaintxstats' window stay on the block the first turn reported, which the reorg leaves
/// off the active chain but in the index, as Core's reads under one `cs_main` would see it.
// Core: rpc/blockchain.cpp:1594-1631, :2103-2153
#[test]
fn a_reorg_between_turns_keeps_the_reported_block() {
    let reorging = || {
        Arc::new(FakeState {
            reorg_after_first_read: true,
            ..FakeState::default()
        })
    };
    let state = reorging();
    let info =
        call(&server_with(FakeNode(Some(Arc::clone(&state))), true), "getblockchaininfo", "[]")
            .unwrap();
    assert!(info.contains(&format!(r#""bestblockhash":"{}""#, hex_of(9))), "{info}");
    assert!(info.contains(&format!(r#""softforks":{DEPLOYMENTS}"#)), "{info}");
    assert!(state.reorged(), "the reorg ran between the turns");

    let state = reorging();
    assert_eq!(
        call(&server(&state), "getchaintxstats", "[1]"),
        Ok(format!(
            r#"{{"time":1095,"txcount":10,"window_final_block_hash":"{}","window_final_block_height":9,"window_block_count":1,"window_tx_count":1,"window_interval":10,"txrate":0.1}}"#,
            hex_of(9)
        ))
    );
    assert!(state.reorged(), "the reorg ran between the turns");
}

#[test]
fn getdifficulty_is_the_tips() {
    let state = Arc::new(FakeState::default());
    assert_eq!(call(&server(&state), "getdifficulty", "[]"), Ok("4.656542373906925e-10".into()));
}

/// The tip's deployments, or a block's by hash; a hash the index doesn't have is -5.
// Core: rpc/blockchain.cpp:1716-1736
#[test]
fn getdeploymentinfo_reads_the_tip_or_a_block() {
    let state = Arc::new(FakeState::default());
    let server = server(&state);
    assert_eq!(
        call(&server, "getdeploymentinfo", "[]"),
        Ok(format!(r#"{{"hash":"{}","height":9,"deployments":{DEPLOYMENTS}}}"#, hex_of(9)))
    );
    assert_eq!(
        call(&server, "getdeploymentinfo", &format!(r#"["{}"]"#, hex_of(3))),
        Ok(format!(r#"{{"hash":"{}","height":3,"deployments":{DEPLOYMENTS}}}"#, hex_of(3)))
    );
    assert_eq!(
        call(&server, "getdeploymentinfo", &format!(r#"["{}"]"#, hex_of(99))),
        Err(reply(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"))
    );
    assert_eq!(
        call(&server, "getdeploymentinfo", r#"["zz"]"#),
        Err(reply(RPC_INVALID_PARAMETER, "blockhash must be of length 64 (not 2, for 'zz')"))
    );
}

/// `statistics` drops `threshold` and `possible` while locked in; started prints them.
#[test]
fn started_statistics_print_threshold_and_possible() {
    let Some(DeploymentInfo::Bip9(mut b)) = deployments().pop() else {
        unreachable!()
    };
    b.status = ThresholdState::Started;
    b.activation_height = None;
    b.ehf_height = None;
    b.height = None;
    b.active = false;
    b.status_next = ThresholdState::Started;
    b.statistics = Some(Bip9StatsInfo {
        period: 144,
        elapsed: 0,
        count: 0,
        threshold: Some(108),
        possible: Some(true),
    });
    b.signalling = Some(String::new());
    assert_eq!(
        deployments_to_json(&[DeploymentInfo::Bip9(b)]).write(),
        r#"{"testdummy":{"type":"bip9","active":false,"bip9":{"bit":28,"start_time":0,"timeout":9223372036854775807,"ehf":false,"min_activation_height":0,"status":"started","since":144,"status_next":"started","statistics":{"period":144,"elapsed":0,"count":0,"threshold":108,"possible":true},"signalling":""}}}"#
    );
}

/// The default window is a month of blocks clamped to `height − 1`; a given one must be below
/// the height; the window fields print only for a non-empty window, `txrate` only for a
/// positive interval.
// Core: rpc/blockchain.cpp:2097-2150
#[test]
fn getchaintxstats_windows_are_cores() {
    let state = Arc::new(FakeState::default());
    let server = server(&state);
    let tip = hex_of(9);
    assert_eq!(
        call(&server, "getchaintxstats", "[]"),
        Ok(format!(
            r#"{{"time":1095,"txcount":10,"window_final_block_hash":"{tip}","window_final_block_height":9,"window_block_count":8,"window_tx_count":8,"window_interval":80,"txrate":0.1}}"#
        ))
    );
    assert_eq!(
        call(&server, "getchaintxstats", "[0]"),
        Ok(format!(
            r#"{{"time":1095,"txcount":10,"window_final_block_hash":"{tip}","window_final_block_height":9,"window_block_count":0}}"#
        ))
    );
    let at = |h: u8| format!(r#"[null, "{}"]"#, hex_of(h));
    // Height 1: a default window of 0; height 0: `max(0, −1)`.
    for h in [0, 1] {
        assert_eq!(
            call(&server, "getchaintxstats", &at(h)),
            Ok(format!(
                r#"{{"time":{},"txcount":{},"window_final_block_hash":"{}","window_final_block_height":{h},"window_block_count":0}}"#,
                1005 + 10 * u32::from(h),
                h + 1,
                hex_of(h)
            ))
        );
    }
    let bad_count = reply(
        RPC_INVALID_PARAMETER,
        "Invalid block count: should be between 0 and the block's height - 1",
    );
    for params in ["[-1]", "[9]", "[10]"] {
        assert_eq!(call(&server, "getchaintxstats", params), Err(bad_count.clone()), "{params}");
    }
    assert!(call(&server, "getchaintxstats", "[8]").is_ok());
    // The lookups come before the count is checked, or read: the server's own checks pass a
    // number that is not an `int`.
    for count in ["10", "1.5"] {
        assert_eq!(
            call(&server, "getchaintxstats", &format!(r#"[{count}, "{}"]"#, hex_of(99))),
            Err(reply(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"))
        );
        assert_eq!(
            call(&server, "getchaintxstats", &format!(r#"[{count}, "{}"]"#, hex_of(OFF_CHAIN))),
            Err(reply(RPC_INVALID_PARAMETER, "Block is not in main chain"))
        );
    }
    assert_eq!(
        call(&server, "getchaintxstats", "[1.5]"),
        Err(reply(RPC_MISC_ERROR, "JSON integer out of range"))
    );
}

/// A window whose median time past does not advance has no `txrate`.
#[test]
fn a_window_without_time_has_no_rate() {
    for step in [0, -1] {
        let state = Arc::new(FakeState {
            mtp_step: step,
            ..FakeState::default()
        });
        let out = call(&server(&state), "getchaintxstats", "[2]").unwrap();
        let interval = format!(r#""window_tx_count":2,"window_interval":{}}}"#, 2 * step);
        assert!(out.ends_with(&interval), "{out}");
    }
}

/// A month at Dash's 150 s spacing is 17,280 blocks; at 600 s, 4,320.
#[test]
fn the_default_window_is_a_month_of_blocks() {
    let state = Arc::new(FakeState {
        tip: 20_000,
        ..FakeState::default()
    });
    let server = server(&state);
    // The fake's hashes hold one byte; the window reads the tip only.
    let out = call(&server, "getchaintxstats", "[]").unwrap();
    assert!(out.contains(r#""window_block_count":17280,"#), "{out}");
    assert_eq!(state.reads.lock().unwrap().last().unwrap(), "ancestor 32 2720");
}

/// An unspent coin with its script as `ScriptToUniv` prints it with hex and address, its
/// confirmations from the coins' best block; a missing coin is null.
// Core: rpc/blockchain.cpp:1365-1408
#[test]
fn gettxout_prints_cores_coin() {
    let p2pkh =
        crate::request::parse_hex("76a91400112233445566778899aabbccddeeff0011223388ac").unwrap();
    let state = Arc::new(FakeState {
        coin: Some(Coin::new(150_000_000, &p2pkh, 7, true)),
        ..FakeState::default()
    });
    let server = server(&state);
    let txid = "11".repeat(32);
    let script =
        crate::core_write::script_to_univ(&p2pkh, true, true, &crate::descriptor::tests::regtest())
            .write();
    let found = format!(
        r#"{{"bestblock":"{}","confirmations":3,"value":1.50000000,"scriptPubKey":{script},"coinbase":true}}"#,
        hex_of(9)
    );
    for params in [
        format!(r#"["{txid}", 1]"#),
        format!(r#"["{txid}", 1, true]"#),
        format!(r#"["{txid}", 1, false]"#),
    ] {
        assert_eq!(call(&server, "gettxout", &params), Ok(found.clone()), "{params}");
    }
    assert_eq!(call(&server, "gettxout", &format!(r#"["{txid}", 0]"#)), Ok("null".into()));
    let out_of_range = reply(RPC_MISC_ERROR, "JSON integer out of range");
    for n in ["-1", "4294967296", "1.5"] {
        assert_eq!(
            call(&server, "gettxout", &format!(r#"["{txid}", {n}]"#)),
            Err(out_of_range.clone()),
            "{n}"
        );
    }
    assert!(call(&server, "gettxout", &format!(r#"["{txid}", 4294967295]"#)).is_ok());
    assert_eq!(
        call(&server, "gettxout", r#"["11", 1]"#),
        Err(reply(RPC_INVALID_PARAMETER, "txid must be of length 64 (not 2, for '11')"))
    );
    // The parameters are read before the coin.
    assert_eq!(
        *state.reads.lock().unwrap(),
        ["txout 1", "txout 1", "txout 1", "txout 0", "txout 4294967295"]
    );
}

/// None is -32603; a lock prints its stored signature and `ChainLockSig`'s serialization.
// Core: rpc/blockchain.cpp:271-293
#[test]
fn getbestchainlock_is_cores() {
    let state = Arc::new(FakeState::default());
    assert_eq!(
        call(&server(&state), "getbestchainlock", "[]"),
        Err(reply(RPC_INTERNAL_ERROR, "Unable to find any ChainLock"))
    );
    let sig: [u8; 96] = std::array::from_fn(|i| i as u8);
    let state = Arc::new(FakeState {
        chainlock: Some(BestChainLock {
            height: 0x0102,
            block_hash: BlockHash::from(std::array::from_fn(|i| i as u8 + 1)),
            sig,
            known_block: true,
        }),
        ..FakeState::default()
    });
    let sig_hex = hex(&sig);
    // `ChainLockSig`'s serialization spelled out: `nHeight` LE, then `blockHash` in its stored
    // order (the reverse of the hex it displays as), then the signature.
    let shown = "201f1e1d1c1b1a191817161514131211100f0e0d0c0b0a090807060504030201";
    let stored = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
    assert_eq!(
        call(&server(&state), "getbestchainlock", "[]"),
        Ok(format!(
            r#"{{"blockhash":"{shown}","height":258,"signature":"{sig_hex}","known_block":true,"hex":"02010000{stored}{sig_hex}"}}"#
        ))
    );
}

/// `count` headers along the active chain from the block, one off it; -5 before the count is
/// read, then its range; the verbose objects are `blockheaderToJSON`'s.
// Core: rpc/blockchain.cpp:726-781
#[test]
fn getblockheaders_walks_the_active_chain() {
    let state = Arc::new(FakeState::default());
    let server = server(&state);
    let from = |h: u8, rest: &str| format!(r#"["{}"{rest}]"#, hex_of(h));
    let verbose = call(&server, "getblockheaders", &from(7, "")).unwrap();
    let objects: Vec<String> =
        (7..=9).map(|h| blockheader_to_json(&state.facts(h)).write()).collect();
    assert_eq!(verbose, format!("[{}]", objects.join(",")));
    assert_eq!(state.reads.lock().unwrap().last().unwrap(), "headers_from 2000");

    let hex_headers = call(&server, "getblockheaders", &from(3, ", 2, false")).unwrap();
    let header =
        |h: u32| hex(&dash_consensus::CoreSerialize::core_serialize(&state.facts(h).header));
    assert_eq!(hex_headers, format!(r#"["{}","{}"]"#, header(3), header(4)));
    assert_eq!(
        call(&server, "getblockheaders", &from(OFF_CHAIN, ", 5")).unwrap(),
        format!("[{}]", blockheader_to_json(&state.facts(200)).write())
    );
    assert!(call(&server, "getblockheaders", &from(0, ", 2000")).is_ok());

    let out_of_range = reply(RPC_INVALID_PARAMETER, "Count is out of range");
    for count in ["0", "-1", "2001"] {
        assert_eq!(
            call(&server, "getblockheaders", &from(1, &format!(", {count}"))),
            Err(out_of_range.clone()),
            "{count}"
        );
    }
    // The lookup comes before the count is checked, or read.
    for count in ["0", "1.5"] {
        assert_eq!(
            call(&server, "getblockheaders", &from(99, &format!(", {count}"))),
            Err(reply(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"))
        );
    }
    assert_eq!(
        call(&server, "getblockheaders", &from(1, ", 1.5")),
        Err(reply(RPC_MISC_ERROR, "JSON integer out of range"))
    );
}

/// Without a timestamp index: Core's -1 before either timestamp is read (the server's own checks
/// pass a number that is not an `int`).
#[test]
fn getblockhashes_has_no_index() {
    let state = Arc::new(FakeState::default());
    let no_index = reply(
        RPC_MISC_ERROR,
        "Timestamp index is not enabled. Start with -timestampindex to enable.",
    );
    for params in ["[1, 2]", "[1.5, -2147483649]"] {
        assert_eq!(call(&server(&state), "getblockhashes", params), Err(no_index.clone()));
    }
    let unloaded = server_with(FakeNode::default(), false);
    assert_eq!(call(&unloaded, "getblockhashes", "[1, 2]"), Err(no_index));
}
