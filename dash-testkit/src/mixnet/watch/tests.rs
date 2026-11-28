use serde_json::json;

use super::*;

fn hash(n: u32) -> String {
    format!("{n:064x}")
}

fn config_json() -> Value {
    json!({
        "v": 1, "run_id":"unit", "run": {
            "run_kind":"rehearsal", "context":"regtest", "preset":"modeb5", "caps":[],
            "host":{"nproc":1,"mem":1,"disk":1}, "clock":{"source":"monotonic","offset_ms":0},
            "binaries":{"dashd":{"sha256":hash(1),"version":"test"},"rdashd":null}
        }, "llmq_params":[], "coinbase":{"known":"node0"},
        "nodes":[{"slot":"node0","runs":"dashd","rpc":"127.0.0.1:12345","cookie":"cookie",
                  "chainlocks":true,"chaintips":false}]
    })
}

#[test]
fn validates_nodes_and_known_slots() {
    fn valid(v: Value) -> bool {
        serde_json::from_value::<Config>(v).is_ok_and(|c| c.validate().is_ok())
    }
    let v = config_json();
    assert!(valid(v.clone()));
    for (pointer, bad) in [
        ("/v", json!(2)),
        ("/nodes", json!([])),
        ("/nodes/0/slot", json!("node1")),
        ("/nodes/0/runs", json!("rust")),
        ("/nodes/0/rpc", json!("bad")),
        ("/coinbase/known", json!("reg1")),
        ("/run/run_kind", json!("soak")),
    ] {
        let mut v = v.clone();
        *v.pointer_mut(pointer).unwrap() = bad;
        assert!(!valid(v), "{pointer}");
    }
    for pointer in ["", "/nodes/0", "/run", "/run/binaries/dashd"] {
        let mut v = v.clone();
        v.pointer_mut(pointer).unwrap()["extra"] = json!(true);
        assert!(!valid(v), "{pointer}");
    }
    let mut v = v;
    let mut peer = v["nodes"][0].clone();
    v["nodes"].as_array_mut().unwrap().push(peer.clone());
    assert!(!valid(v.clone()));
    peer["slot"] = json!("reg0");
    peer["runs"] = json!("rust");
    v["nodes"][1] = peer;
    assert!(!valid(v.clone()));
    v["nodes"][1]["chainlocks"] = json!(false);
    assert!(valid(v));
}

#[test]
fn extracts_null_and_non_null_mined_commitments() {
    let config: Config = serde_json::from_value(config_json()).unwrap();
    let tx = |index, signers, valid| {
        json!({"type":6,"qcTx":{"height":17,"commitment":{
        "llmqType":100,"quorumHash":hash(1),"quorumIndex":index,"signersCount":signers,
        "validMembersCount":valid,"quorumVvecHash":hash(2),"version":1}}})
    };
    let data = json!({"height":17,"hash":hash(17),"previousblockhash":hash(16),
        "tx":[{"type":5,"vout":[{"scriptPubKey":{"address":"elsewhere"}},
            {"scriptPubKey":{"address":"known"}}]},tx(0,0,0),tx(1,0,2),tx(2,2,0)]});
    let (b, qcs) = block_data(&config, &data).unwrap();
    assert_eq!(
        b.miner,
        Some(Miner {
            slot: SlotId::CONTROLLER,
            runs: Impl::Dashd
        })
    );
    assert_eq!(qcs.len(), 3);
    for (index, qc) in qcs.into_iter().enumerate() {
        let null = index == 0;
        assert_eq!(qc.null(), null);
        let info = if null {
            json!({"height":12})
        } else {
            json!({"height":12,"members":[{"proTxHash":hash(4),"valid":false},{"proTxHash":hash(3),"valid":true}]})
        };
        let record = qc.commitment(17, &info).unwrap();
        assert_eq!(
            record,
            Record::Commitment(Commitment {
                null,
                index: index as u16,
                base_height: 12,
                mined_height: 17,
                llmq_type: 100,
                quorum_hash: hash(1).parse().unwrap(),
                vvec_hash: hash(2).parse().unwrap(),
                members: if null {
                    vec![]
                } else {
                    vec![
                        Member {
                            pro_tx: hash(4).parse().unwrap(),
                            valid: false,
                        },
                        Member {
                            pro_tx: hash(3).parse().unwrap(),
                            valid: true,
                        },
                    ]
                },
            })
        );
    }
}

#[test]
fn extracts_added_mn_baselines_and_only_updated_pose_fields() {
    let diff = json!({"addedMNs":[{"proTxHash":hash(1),"state":{
        "PoSePenalty":0,"PoSeBanHeight":-1,"PoSeRevivedHeight":10}}],
        "updatedMNs":[{(hash(2)):{"PoSePenalty":10,"PoSeBanHeight":15}},
                      {(hash(3)):{"PoSeRevivedHeight":15}}, {(hash(6)):{"PoSeBanHeight":20}},
                      {(hash(4)):{"lastPaidHeight":15}}],
        "removedMNs":[hash(5)]});
    let row = |id, penalty, ban, revived| {
        Record::MnDiff(MnDiff {
            height: 15,
            pro_tx: hash(id).parse().unwrap(),
            pose_penalty: penalty,
            pose_ban_height: ban,
            pose_revived_height: revived,
        })
    };
    let baseline = row(1, Some(0), Some(-1), Some(10));
    assert_eq!(pose_rows(&diff, 15, true).unwrap(), std::slice::from_ref(&baseline));
    assert_eq!(
        pose_rows(&diff, 15, false).unwrap(),
        [
            baseline,
            row(2, Some(10), Some(15), None),
            row(3, None, None, Some(15)),
            row(6, None, Some(20), None)
        ]
    );
    let mut invalid = diff;
    invalid["addedMNs"][0]["state"].as_object_mut().unwrap().remove("PoSeBanHeight");
    assert!(pose_rows(&invalid, 15, false).is_err());
}

#[test]
fn restores_complete_rows_and_truncates_only_the_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log");
    let mut config = serde_json::from_value(config_json()).unwrap();
    let mut log = Log::open(&path, &config).unwrap();
    log.watch(Record::Watermark(Watermark {
        height: 10,
    }))
    .unwrap();
    let block = Record::Block(Block {
        height: 11,
        hash: hash(11).parse().unwrap(),
        prev: hash(10).parse().unwrap(),
        miner: None,
    });
    log.watch(block.clone()).unwrap();
    let last = log.state.last;
    drop(log);
    let prefix = fs::read(&path).unwrap();
    for torn in [br#"{"v":1,"ru"#.as_slice(), br#"{"v":1,"run":"unit","seq":"#] {
        OpenOptions::new().append(true).open(&path).unwrap().write_all(torn).unwrap();
        let log = Log::open(&path, &config).unwrap();
        assert_eq!(fs::read(&path).unwrap(), prefix);
        assert_eq!(log.state.last, last);
        assert_eq!(log.state.h0, Some(10));
        assert_eq!(log.state.next_height, Some(11));
        assert_eq!(log.state.pending, std::slice::from_ref(&block));
    }
    config.run_id.push('!');
    assert!(matches!(Log::open(&path, &config), Err(Error::Input(_))));
    assert_eq!(fs::read(&path).unwrap(), prefix);
    config.run_id.pop();
    OpenOptions::new().append(true).open(&path).unwrap().write_all(b"broken\n").unwrap();
    assert!(matches!(Log::open(&path, &config), Err(Error::Input(_))));
}

#[test]
fn parses_event_provenance_and_rejects_bad_lines() {
    let e = json!({"kind":"Chaos","id":"x","action":"stop","targets":[],"start_seq":1,"end_seq":1});
    let received = event(&e.to_string()).unwrap();
    assert_eq!(received.src, "driver");
    assert!(received.mono_ns <= monotonic_ns().unwrap());
    let mut e = e;
    e["src"] = json!("digest:reg0");
    e["mono_ns"] = json!(12);
    let received = event(&e.to_string()).unwrap();
    assert_eq!(received.src, "digest:reg0");
    assert_eq!(received.mono_ns, 12);
    for bad in ["", "null", "[]", "{}", "{"] {
        assert!(event(bad).is_err(), "{bad}");
    }
    let mut run = config_json()["run"].take();
    run["kind"] = json!("Run");
    for owned in [
        run,
        json!({"kind":"LlmqParams","type":100,"name":"llmq_test","size":3,"min_size":2,
            "threshold":2,"dkg_interval":24,"mining_window_start":10,"mining_window_end":18,
            "rotation":false,"signing_active_quorum_count":2,"evo_only":false}),
        json!({"kind":"Block","height":1,"hash":hash(1),"prev":hash(0),"miner":null}),
        json!({"kind":"Reorg","fork_height":1,"old_tip":hash(2),"new_tip":hash(3)}),
        json!({"kind":"Watermark","height":1}),
        json!({"kind":"Tip","node":"node0","height":1,"hash":hash(1)}),
        json!({"kind":"ChainTips","node":"node0","tips":[]}),
        json!({"kind":"Commitment","type":100,"quorum_hash":hash(1),"index":0,
            "base_height":1,"mined_height":2,"null":true,"members":[],"vvec_hash":hash(0)}),
        json!({"kind":"MnDiff","height":1,"pro_tx":hash(1),"pose_penalty":0,
            "pose_ban_height":null,"pose_revived_height":null}),
        json!({"kind":"ClSeen","node":"node0","height":1,"hash":hash(1)}),
    ] {
        assert_eq!(
            event(&owned.to_string()).err().unwrap().to_string(),
            "driver event has a watcher-owned kind",
            "{owned}"
        );
    }
    e["seq"] = json!(1);
    assert!(event(&e.to_string()).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn propagates_log_write_failure() {
    let config = serde_json::from_value(config_json()).unwrap();
    assert!(matches!(Log::open(Path::new("/dev/full"), &config), Err(Error::Output(_))));
}

#[test]
fn unrelated_trailing_fragments_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log");
    let config = serde_json::from_value(config_json()).unwrap();
    drop(Log::open(&path, &config).unwrap());
    let prefix = fs::read(&path).unwrap();
    for bytes in [
        br#"{"unrelated":true}"#.to_vec(),
        [prefix.as_slice(), br#"{"v":1,"run":"someone else""#].concat(),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(Log::open(&path, &config), Err(Error::Input(_))));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}

struct ScriptedRpc {
    tip: u32,
    fork: u32,
    fail: Option<&'static str>,
    diffs: std::cell::RefCell<Vec<(u32, u32)>>,
}

impl ScriptedRpc {
    fn new(tip: u32) -> Self {
        Self {
            tip,
            fork: u32::MAX,
            fail: None,
            diffs: Default::default(),
        }
    }

    fn hash(&self, h: u32) -> String {
        hash(if h > self.fork {
            100 + h
        } else {
            h
        })
    }
}

impl NodeRpc for ScriptedRpc {
    fn call(&self, method: &str, args: &[Value]) -> Result<Value, RpcError> {
        if self.fail == Some(method) {
            return Err(format!("scripted {method} error").into());
        }
        Ok(match method {
            "getblockcount" => json!(self.tip),
            "getbestblockhash" => json!(self.hash(self.tip)),
            "getblockhash" => {
                let h = u32::deserialize(&args[0])?;
                assert!(h <= self.tip);
                json!(self.hash(h))
            }
            "getblockheader" => {
                if let Some(h) = (0..=self.tip).find(|h| args[0] == self.hash(*h)) {
                    let prev = h.checked_sub(1).map(|p| self.hash(p));
                    json!({"height":h,"previousblockhash":prev})
                } else {
                    let h = (1..100).find(|h| args[0] == hash(*h)).unwrap();
                    json!({"height":h,"previousblockhash":hash(h-1)})
                }
            }
            "getblock" => {
                assert_eq!(args[1], 2);
                let h = (1..=self.tip).find(|h| args[0] == self.hash(*h)).unwrap();
                json!({"height":h,"hash":self.hash(h),"previousblockhash":self.hash(h-1),"tx":[
                    {"type":5,"vout":[{"scriptPubKey":{"address":"known"}}]},
                    {"type":6,"qcTx":{"height":h,"commitment":{"llmqType":100,
                        "quorumHash":hash(0),"quorumIndex":0,"signersCount":1,
                        "validMembersCount":1,"quorumVvecHash":hash(42)}}}]})
            }
            "quorum" => {
                assert_eq!(args, ["info".into(), json!(100), json!(hash(0))]);
                json!({"height":0,"members":[{"proTxHash":hash(42),"valid":true}]})
            }
            "protx" => {
                assert_eq!(args[0], "listdiff");
                let (base, h) = (u32::deserialize(&args[1])?, u32::deserialize(&args[2])?);
                self.diffs.borrow_mut().push((base, h));
                if base == 1 {
                    let added = |id| {
                        json!({"proTxHash":hash(id),"state":{
                        "PoSePenalty":8,"PoSeBanHeight":-1,"PoSeRevivedHeight":-1}})
                    };
                    json!({"addedMNs":[added(42),added(43)],"updatedMNs":[],"removedMNs":[]})
                } else {
                    assert_eq!(base, h - 1);
                    json!({"addedMNs":[],"updatedMNs":[{(hash(42)):{"PoSePenalty":7}},
                        {(hash(43)):{"PoSePenalty":2}}],"removedMNs":[]})
                }
            }
            "getchaintips" => json!([{"height":self.tip,"hash":self.hash(self.tip),
                "branchlen":0,"status":"active"}]),
            "getbestchainlock" => json!({"height":self.tip,"blockhash":self.hash(self.tip)}),
            _ => panic!("unexpected {method} {args:?}"),
        })
    }
}

#[test]
fn tip_height_matches_its_hash_when_the_node_advances_between_calls() {
    struct AdvancingRpc(std::cell::RefCell<ScriptedRpc>);

    impl NodeRpc for AdvancingRpc {
        fn call(&self, method: &str, args: &[Value]) -> Result<Value, RpcError> {
            let mut rpc = self.0.borrow_mut();
            let response = rpc.call(method, args)?;
            rpc.tip += 1;
            Ok(response)
        }
    }

    let config: Config = serde_json::from_value(config_json()).unwrap();
    let rpc = AdvancingRpc(ScriptedRpc::new(3).into());
    let rows = polls(&config.nodes[0], &rpc).unwrap();
    assert_eq!(
        rows[0],
        Record::Tip(Tip {
            node: SlotId::CONTROLLER,
            height: 3,
            hash: hash(3).parse().unwrap(),
        })
    );
}

fn records(path: &Path) -> Vec<Record> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Envelope>(line).unwrap().record)
        .collect()
}

#[test]
fn failed_height_is_written_once_on_retry() {
    let config = serde_json::from_value(config_json()).unwrap();
    for method in ["protx", "quorum"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let mut log = Log::open(&path, &config).unwrap();
        let mut rpc = ScriptedRpc::new(3);
        log.pass(&config, &rpc, false).unwrap();
        let before = fs::read(&path).unwrap();
        let start = records(&path).len();
        rpc.tip = 4;
        rpc.fail = Some(method);
        assert!(matches!(log.pass(&config, &rpc, false), Err(Error::Retry(_))));
        assert_eq!(fs::read(&path).unwrap(), before);
        rpc.fail = None;
        log.pass(&config, &rpc, false).unwrap();
        assert!(matches!(&records(&path)[start..],
            [Record::Block(b), Record::Commitment(c), Record::MnDiff(a), Record::MnDiff(d),
             Record::Watermark(w), Record::Tip(_), Record::ClSeen(_)]
             if b.height == 4 && c.mined_height == 4 && a.height == 4 && d.height == 4
             && a.pro_tx != d.pro_tx && w.height == 4));
    }
}

#[test]
fn resume_completes_pending_block_commitment_and_pose_rows() {
    let config = serde_json::from_value(config_json()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log");
    let mut log = Log::open(&path, &config).unwrap();
    let mut rpc = ScriptedRpc::new(3);
    log.pass(&config, &rpc, false).unwrap();
    let start = records(&path).len();
    rpc.tip += 1;
    log.pass(&config, &rpc, false).unwrap();
    drop(log);
    let complete = records(&path);
    assert!(matches!(
        &complete[start..],
        [
            Record::Block(_),
            Record::Commitment(_),
            Record::MnDiff(_),
            Record::MnDiff(_),
            Record::Watermark(_),
            ..
        ]
    ));
    let cut: String =
        fs::read_to_string(&path).unwrap().split_inclusive('\n').take(start + 3).collect();
    fs::write(&path, &cut).unwrap();
    Log::open(&path, &config).unwrap().pass(&config, &rpc, false).unwrap();
    assert!(fs::read(&path).unwrap().starts_with(cut.as_bytes()));
    assert_eq!(records(&path), complete);
}

#[test]
fn reorgs_live_and_on_resume_restore_fork_and_full_baselines_through_h0() {
    let config = serde_json::from_value(config_json()).unwrap();
    let h0 = 5;
    for resume in [false, true] {
        for (fork, tip) in [(6, 7), (6, 10), (2, 9)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("log");
            let mut log = Log::open(&path, &config).unwrap();
            let mut rpc = ScriptedRpc::new(h0);
            log.pass(&config, &rpc, false).unwrap();
            rpc.tip = 8;
            log.pass(&config, &rpc, false).unwrap();
            if resume {
                drop(log);
                log = Log::open(&path, &config).unwrap();
            }
            let start = records(&path).len();
            rpc.fork = fork;
            rpc.tip = tip;
            rpc.diffs.borrow_mut().clear();
            log.pass(&config, &rpc, false).unwrap();
            let rows = records(&path);
            let rows = &rows[start..];
            assert!(matches!(&rows[0], Record::Reorg(r) if r.fork_height == fork
                && r.old_tip.as_str() == hash(8) && r.new_tip.as_str() == rpc.hash(tip)));
            assert_eq!(rows.iter().filter(|r| matches!(r, Record::Reorg(_))).count(), 1);
            let diffs: Vec<_> = (fork + 1..=tip)
                .map(|h| {
                    (
                        if h <= h0 {
                            1
                        } else {
                            h - 1
                        },
                        h,
                    )
                })
                .collect();
            assert_eq!(*rpc.diffs.borrow(), diffs);
            for h in fork + 1..=h0 {
                let pose: Vec<_> = rows
                    .iter()
                    .filter_map(|r| match r {
                        Record::MnDiff(d) if d.height == h => Some(d),
                        _ => None,
                    })
                    .collect();
                assert_eq!(pose.len(), 2);
                assert!(pose.iter().all(|d| d.pose_penalty == Some(8)
                    && d.pose_ban_height == Some(-1)
                    && d.pose_revived_height == Some(-1)));
            }
        }
    }
}

#[test]
fn height_one_uses_listdiff_one_one() {
    let config = serde_json::from_value(config_json()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let rpc = ScriptedRpc::new(1);
    Log::open(&dir.path().join("log"), &config).unwrap().pass(&config, &rpc, false).unwrap();
    assert_eq!(*rpc.diffs.borrow(), [(1, 1)]);
}

#[test]
fn suppresses_unchanged_polls_live_and_on_resume_and_closes_after_changed_polls() {
    let mut config: Config = serde_json::from_value(config_json()).unwrap();
    config.nodes[0].chaintips = true;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log");
    let mut log = Log::open(&path, &config).unwrap();
    let mut rpc = ScriptedRpc::new(5);
    log.pass(&config, &rpc, false).unwrap();
    let first = fs::read(&path).unwrap();
    assert!(matches!(
        records(&path).as_slice(),
        [.., Record::Tip(_), Record::ChainTips(_), Record::ClSeen(_)]
    ));
    log.pass(&config, &rpc, false).unwrap();
    assert_eq!(fs::read(&path).unwrap(), first);
    drop(log);
    let mut log = Log::open(&path, &config).unwrap();
    log.pass(&config, &rpc, false).unwrap();
    assert_eq!(fs::read(&path).unwrap(), first);
    rpc.tip += 1;
    log.pass(&config, &rpc, true).unwrap();
    assert!(matches!(records(&path).as_slice(), [.., Record::Watermark(collected),
        Record::Tip(_), Record::ChainTips(_), Record::ClSeen(_), Record::Watermark(last)]
        if collected.height == rpc.tip && last.height == rpc.tip));
}

#[test]
fn queued_events_yield_to_the_next_pass() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log");
    let config = serde_json::from_value(config_json()).unwrap();
    let mut log = Log::open(&path, &config).unwrap();
    let mut rpc = ScriptedRpc::new(3);
    log.pass(&config, &rpc, false).unwrap();
    let (tx, rx) = mpsc::channel();
    let chaos = event(
        r#"{"kind":"Chaos","id":"load","action":"stop","targets":[],"start_seq":1,"end_seq":1}"#,
    )
    .unwrap();
    for _ in 0..8192 {
        tx.send(Ok(Event {
            record: chaos.record.clone(),
            src: chaos.src.clone(),
            mono_ns: chaos.mono_ns,
        }))
        .unwrap();
    }
    drop(tx);
    assert!(!log.receive_events(&rx, Duration::from_millis(1)).unwrap());
    rpc.tip += 1;
    log.pass(&config, &rpc, false).unwrap();
    assert!(rx.try_recv().is_ok());
    assert!(matches!(log.state.next_height, Some(5)));
}

struct ReorgsMidPass {
    rpc: std::cell::RefCell<ScriptedRpc>,
    on: (&'static str, Value),
    fork: u32,
}

impl NodeRpc for ReorgsMidPass {
    fn call(&self, method: &str, args: &[Value]) -> Result<Value, RpcError> {
        if method == self.on.0 && args.last() == Some(&self.on.1) {
            self.rpc.borrow_mut().fork = self.fork;
        }
        self.rpc.borrow().call(method, args)
    }
}

#[test]
fn a_reorg_inside_a_pass_certifies_nothing_from_the_old_branch() {
    let config = serde_json::from_value(config_json()).unwrap();
    for on in [("getblockhash", json!(5)), ("protx", json!(4))] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let mut log = Log::open(&path, &config).unwrap();
        log.pass(&config, &ScriptedRpc::new(3), false).unwrap();
        let rpc = ReorgsMidPass {
            rpc: ScriptedRpc::new(5).into(),
            on: on.clone(),
            fork: 3,
        };
        assert!(matches!(log.pass(&config, &rpc, false), Err(Error::Retry(_))), "{on:?}");
        assert!(records(&path).iter().all(|r| match r {
            Record::Block(b) => b.height <= 4,
            Record::Watermark(w) => w.height <= 4,
            _ => true,
        }));
        if on.0 == "protx" {
            assert!(!records(&path).iter().any(|r| matches!(r, Record::Block(b) if b.height == 4)));
        }
        log.pass(&config, &rpc, false).unwrap();
        let rows = records(&path);
        let reorg = rows.iter().position(|r| matches!(r, Record::Reorg(_)));
        if on.0 == "protx" {
            assert_eq!(reorg, None);
        } else {
            assert!(matches!(&rows[reorg.unwrap()], Record::Reorg(r) if r.fork_height == 3));
        }
        let after = reorg.map_or(0, |i| i + 1);
        let blocks: Vec<_> = rows[after..]
            .iter()
            .filter_map(|r| match r {
                Record::Block(b) if b.height > 3 => Some((b.height, b.hash.as_str().to_owned())),
                _ => None,
            })
            .collect();
        assert_eq!(blocks, [(4, rpc.rpc.borrow().hash(4)), (5, rpc.rpc.borrow().hash(5))]);
    }
}

fn page_config() -> Config {
    let mut v = config_json();
    v["llmq_params"] = json!([{
        "type": 100, "name": "llmq_test", "size": 3, "min_size": 2, "threshold": 2,
        "dkg_interval": 24, "mining_window_start": 10, "mining_window_end": 18,
        "rotation": false, "signing_active_quorum_count": 2, "evo_only": false
    }]);
    serde_json::from_value(v).unwrap()
}

fn write_page_script(dir: &Path, name: &str, out: &Path, exit_code: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join(name);
    fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\nexit {exit_code}\n", out.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn page_files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("PAGE-") || n.starts_with(".PAGE-"))
        .collect();
    names.sort();
    names
}

fn cmd_lines(path: &Path) -> Vec<String> {
    match fs::read_to_string(path) {
        Ok(s) => s.lines().map(str::to_owned).collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => panic!("{e}"),
    }
}

fn write_digest_mismatch(log: &mut Log, node: &str, height: u32) {
    let line = json!({
        "kind": "DigestCompare",
        "node": node,
        "from_height": height,
        "to_height": height,
        "result": {"mismatch": {"height": height}}
    })
    .to_string();
    let e = event(&line).unwrap();
    log.write(e.record, e.src, e.mono_ns).unwrap();
}

#[test]
fn digest_mismatch_pages_once_across_repeats_and_resume() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("obs.jsonl");
    let calls = dir.path().join("calls.txt");
    let script = write_page_script(dir.path(), "page.sh", &calls, 0);
    let config = page_config();
    let mut log = Log::open(&path, &config).unwrap();
    let mut rpc = ScriptedRpc::new(3);
    log.pass(&config, &rpc, false).unwrap();
    rpc.tip = 4;
    log.pass(&config, &rpc, false).unwrap();
    log.check_pages(Some(script.as_os_str())).unwrap();
    assert!(page_files(dir.path()).is_empty());

    write_digest_mismatch(&mut log, "reg0", 4);
    let seq_at_page = log.state.last.unwrap();
    log.check_pages(Some(script.as_os_str())).unwrap();

    let page_path = dir.path().join("PAGE-4-digest-mismatch.json");
    assert_eq!(page_files(dir.path()), ["PAGE-4-digest-mismatch.json"]);
    assert_eq!(cmd_lines(&calls), [page_path.display().to_string()]);

    let parsed: Value = serde_json::from_slice(&fs::read(&page_path).unwrap()).unwrap();
    let obj = parsed.as_object().unwrap();
    assert_eq!(obj.len(), 5);
    assert_eq!(obj["run"], json!("unit"));
    assert_eq!(obj["kind"], json!("digest-mismatch"));
    assert_eq!(obj["height"], json!(4));
    assert_eq!(obj["detail"], json!("reg0"));
    assert_eq!(obj["seq"], json!(seq_at_page));

    write_digest_mismatch(&mut log, "reg0", 4);
    log.check_pages(Some(script.as_os_str())).unwrap();
    assert_eq!(page_files(dir.path()), ["PAGE-4-digest-mismatch.json"]);
    assert_eq!(cmd_lines(&calls), [page_path.display().to_string()]);

    drop(log);
    let mut log = Log::open(&path, &config).unwrap();
    rpc.tip = 5;
    log.pass(&config, &rpc, false).unwrap();
    log.check_pages(Some(script.as_os_str())).unwrap();
    assert_eq!(page_files(dir.path()), ["PAGE-4-digest-mismatch.json"]);
    assert_eq!(cmd_lines(&calls), [page_path.display().to_string()]);
}

#[test]
fn resume_pages_a_replayed_divergence_once_without_a_resend() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("obs.jsonl");
    let calls = dir.path().join("calls.txt");
    let script = write_page_script(dir.path(), "page.sh", &calls, 0);
    let config = page_config();
    let mut log = Log::open(&path, &config).unwrap();
    let mut rpc = ScriptedRpc::new(3);
    log.pass(&config, &rpc, false).unwrap();
    rpc.tip = 5;
    log.pass(&config, &rpc, false).unwrap();
    write_digest_mismatch(&mut log, "reg0", 4);
    log.check_pages(Some(script.as_os_str())).unwrap();
    let first = dir.path().join("PAGE-4-digest-mismatch.json");
    let first_bytes = fs::read(&first).unwrap();
    write_digest_mismatch(&mut log, "reg0", 5);
    drop(log);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(br#"{"v":1,"run":"unit","seq":"#)
        .unwrap();

    let mut log = Log::open(&path, &config).unwrap();
    let rows: Vec<Envelope> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(log.entries, rows);
    log.check_pages(Some(script.as_os_str())).unwrap();
    log.check_pages(Some(script.as_os_str())).unwrap();
    let second = dir.path().join("PAGE-5-digest-mismatch.json");
    assert_eq!(
        page_files(dir.path()),
        ["PAGE-4-digest-mismatch.json", "PAGE-5-digest-mismatch.json"]
    );
    assert_eq!(cmd_lines(&calls), [first.display().to_string(), second.display().to_string()]);
    assert_eq!(fs::read(&first).unwrap(), first_bytes);
}

#[test]
fn best_hash_pages_a_tip_logged_once_settled_and_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("obs.jsonl");
    let calls = dir.path().join("calls.txt");
    let script = write_page_script(dir.path(), "page.sh", &calls, 0);
    let config = page_config();
    let catchup = criteria::Config::default().catchup;
    let mut log = Log::open(&path, &config).unwrap();
    let mut rpc = ScriptedRpc::new(3);
    log.pass(&config, &rpc, false).unwrap();
    rpc.tip = 5 + catchup - 1;
    log.pass(&config, &rpc, false).unwrap();
    let tip = |height, n| {
        Record::Tip(Tip {
            node: SlotId::Regular(0),
            height,
            hash: hash(n).parse().unwrap(),
        })
    };
    log.watch(tip(5, 998)).unwrap();
    log.check_pages(Some(script.as_os_str())).unwrap();
    assert!(page_files(dir.path()).is_empty());

    rpc.tip = 5 + catchup;
    log.pass(&config, &rpc, false).unwrap();
    log.check_pages(Some(script.as_os_str())).unwrap();
    assert!(page_files(dir.path()).is_empty(), "an unchanged unsettled tip stays unpaged");

    log.watch(tip(4, 999)).unwrap();
    log.check_pages(Some(script.as_os_str())).unwrap();
    let page_path = dir.path().join("PAGE-4-best-hash.json");
    assert_eq!(page_files(dir.path()), ["PAGE-4-best-hash.json"]);
    assert_eq!(cmd_lines(&calls), [page_path.display().to_string()]);
    let first_bytes = fs::read(&page_path).unwrap();

    rpc.tip = 5 + catchup + 1;
    log.pass(&config, &rpc, false).unwrap();
    log.watch(Record::Tip(Tip {
        node: SlotId::CONTROLLER,
        height: 4,
        hash: hash(997).parse().unwrap(),
    }))
    .unwrap();
    log.check_pages(Some(script.as_os_str())).unwrap();
    assert_eq!(page_files(dir.path()), ["PAGE-4-best-hash.json"]);
    assert_eq!(cmd_lines(&calls), [page_path.display().to_string()]);
    assert_eq!(fs::read(&page_path).unwrap(), first_bytes);
}

#[test]
fn no_watermark_yields_no_pages_and_no_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("obs.jsonl");
    let calls = dir.path().join("calls.txt");
    let script = write_page_script(dir.path(), "page.sh", &calls, 0);
    let config = page_config();
    let mut log = Log::open(&path, &config).unwrap();
    assert!(!log.pass(&config, &ScriptedRpc::new(0), false).unwrap());
    write_digest_mismatch(&mut log, "reg0", 1);
    log.check_pages(Some(script.as_os_str())).unwrap();
    assert!(page_files(dir.path()).is_empty());
    assert!(cmd_lines(&calls).is_empty());
}

#[test]
fn nonzero_page_cmd_keeps_file_and_does_not_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("obs.jsonl");
    let calls = dir.path().join("calls.txt");
    let script = write_page_script(dir.path(), "fail.sh", &calls, 3);
    let config = page_config();
    let mut log = Log::open(&path, &config).unwrap();
    let mut rpc = ScriptedRpc::new(3);
    log.pass(&config, &rpc, false).unwrap();
    rpc.tip = 4;
    log.pass(&config, &rpc, false).unwrap();
    write_digest_mismatch(&mut log, "reg0", 4);
    log.check_pages(Some(script.as_os_str())).unwrap();

    let page_path = dir.path().join("PAGE-4-digest-mismatch.json");
    assert_eq!(page_files(dir.path()), ["PAGE-4-digest-mismatch.json"]);
    assert_eq!(cmd_lines(&calls), [page_path.display().to_string()]);

    rpc.tip = 5;
    log.pass(&config, &rpc, false).unwrap();
    log.check_pages(Some(script.as_os_str())).unwrap();
    assert_eq!(cmd_lines(&calls), [page_path.display().to_string()]);
}

#[test]
fn existing_page_file_skips_command() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("obs.jsonl");
    let calls = dir.path().join("calls.txt");
    let script = write_page_script(dir.path(), "page.sh", &calls, 0);
    let page_path = dir.path().join("PAGE-4-digest-mismatch.json");
    fs::write(&page_path, b"preexisting").unwrap();

    let config = page_config();
    let mut log = Log::open(&path, &config).unwrap();
    let mut rpc = ScriptedRpc::new(3);
    log.pass(&config, &rpc, false).unwrap();
    rpc.tip = 4;
    log.pass(&config, &rpc, false).unwrap();
    write_digest_mismatch(&mut log, "reg0", 4);
    log.check_pages(Some(script.as_os_str())).unwrap();

    assert!(cmd_lines(&calls).is_empty());
    assert_eq!(fs::read(&page_path).unwrap(), b"preexisting");
}

#[test]
fn page_kind_names_match_serde() {
    for kind in [PageKind::DigestMismatch, PageKind::BestHash, PageKind::ClConflict, PageKind::Pose]
    {
        assert_eq!(serde_json::to_value(kind).unwrap(), json!(page_kind_name(kind)));
    }
}
