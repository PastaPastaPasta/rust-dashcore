//! Exercise the chain collector on a connected pair of regtest nodes.
#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use dash_testkit::mixnet::contract::{
    Block, Envelope, Hex32, Impl, Miner, Record, SlotId, check_log,
};
use dash_testkit::mixnet::criteria::{Config, Gate, judge, metrics};
use dash_testkit::{MixedNetwork, NodeId, NodeRpc, PATIENCE};
use dashcore::{Address, Network};
use serde_json::{Value, json};

const H0: u32 = 1;

struct Watch(Child);

impl Watch {
    fn finish(mut self, code: i32) {
        drop(self.0.stdin.take());
        until(|| self.0.try_wait().unwrap().is_some());
        assert_eq!(self.0.wait().unwrap().code(), Some(code));
    }

    fn send(&mut self, value: Value) {
        writeln!(self.0.stdin.as_mut().unwrap(), "{value}").unwrap();
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "condition still false after {PATIENCE:?}");
        thread::sleep(Duration::from_millis(50));
    }
}

struct Fixture {
    net: MixedNetwork,
    nodes: PathBuf,
    out: PathBuf,
    address: Address,
}

fn log(path: &Path) -> Vec<Envelope> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("{e}"),
    };
    bytes
        .split_inclusive(|b| *b == b'\n')
        .filter(|s| s.ends_with(b"\n"))
        .map(|s| serde_json::from_slice(s).unwrap())
        .collect()
}

impl Fixture {
    async fn start() -> Option<Self> {
        if std::env::var_os("SKIP_DASHD_TESTS").is_some() {
            return None;
        }
        let net = MixedNetwork::builder()
            .dashd(2)
            .connect(NodeId::Dashd(0), NodeId::Dashd(1))
            .start()
            .await
            .unwrap();
        let nodes = net.dir().join("nodes.json");
        let out = net.dir().join("obs.jsonl");
        let address = Address::dummy(Network::Regtest, 0);
        net.dashd(0).generate_blocks(H0.into(), &address);
        let node = |i, slot, runs| {
            let n = net.dashd(i);
            json!({"rpc": format!("127.0.0.1:{}", n.rpc_port()), "cookie": n.datadir().join("regtest/.cookie"),
                "slot": slot, "runs": runs, "chainlocks": i == 0, "chaintips": true})
        };
        let cfg = json!({"v":1, "run_id":"integration", "run":{"run_kind":"rehearsal", "context":"regtest",
            "preset":"alt5", "caps":[], "host":{"nproc":2,"mem":100,"disk":100},
            "clock":{"source":"monotonic","offset_ms":0},
            "binaries":{"dashd":{"sha256":"01".repeat(32),"version":"test"},
                        "rdashd":{"sha256":"02".repeat(32),"version":"test"}}},
            "llmq_params":[{"type":100,"name":"llmq_test","size":3,"min_size":2,"threshold":2,
                "dkg_interval":24,"mining_window_start":10,"mining_window_end":18,"rotation":false,
                "signing_active_quorum_count":2,"evo_only":false}],
            "nodes":[node(0,"node0","dashd"),node(1,"reg0","rust")],
            "coinbase":{(address.to_string()):"node0"}});
        fs::write(&nodes, serde_json::to_vec(&cfg).unwrap()).unwrap();
        Some(Self {
            net,
            nodes,
            out,
            address,
        })
    }

    fn watch(&self) -> Watch {
        self.watch_with(&[])
    }

    fn watch_with(&self, extra: &[&Path]) -> Watch {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mixnet-watch"));
        cmd.arg("--nodes").arg(&self.nodes).arg("--out").arg(&self.out).args([
            "--events",
            "-",
            "--poll-ms",
            "20",
        ]);
        for arg in extra {
            cmd.arg(arg);
        }
        Watch(cmd.stdin(Stdio::piped()).spawn().unwrap())
    }

    fn watermark(&self, h: u32, w: &mut Watch) {
        until(|| {
            assert_eq!(w.0.try_wait().unwrap(), None, "watcher exited before {h}");
            log(&self.out)
                .iter()
                .any(|e| matches!(&e.record, Record::Watermark(w) if w.height == h))
        });
    }

    fn rpc(&self, i: usize, method: &str, args: &[Value]) -> Value {
        self.net.rpc(NodeId::Dashd(i)).unwrap().call(method, args).unwrap()
    }

    fn mine(&self, count: u64) {
        self.net.dashd(0).generate_blocks(count, &self.address);
    }

    fn check(&self, end: u32) -> Vec<Envelope> {
        let log = log(&self.out);
        check_log(&log).unwrap();
        assert!(log.iter().all(|e| e.run == "integration"));
        assert!(judge(&log, Gate::Rehearsal, &Config::default()).passed);
        assert_eq!(metrics(&log, &Config::default()).unwrap().range, (H0, end));
        let mut chain = BTreeMap::new();
        for e in &log {
            match &e.record {
                Record::Reorg(r) => chain.retain(|h, _| *h <= r.fork_height),
                Record::Block(b) => {
                    assert!(chain.insert(b.height, b).is_none(), "duplicate block at {}", b.height)
                }
                _ => {}
            }
        }
        assert_eq!(chain.keys().copied().collect::<Vec<_>>(), (H0..=end).collect::<Vec<_>>());
        let rpc = self.net.rpc(NodeId::Dashd(0)).unwrap();
        for (h, b) in chain {
            assert_eq!(rpc.call("getblockhash", &[h.into()]).unwrap(), b.hash.as_str());
        }
        log
    }
}

#[tokio::test]
async fn collects_200_blocks() {
    let Some(f) = Fixture::start().await else {
        return;
    };
    let mut w = f.watch();
    f.watermark(H0, &mut w);
    for _ in 0..4 {
        f.mine(50);
    }
    w.finish(0);
    let log = f.check(H0 + 200);
    let mut tips = BTreeSet::new();
    let mut branches = BTreeSet::new();
    for e in log {
        match e.record {
            Record::Tip(t) => {
                tips.insert(t.node);
            }
            Record::ChainTips(t) => {
                branches.insert(t.node);
            }
            Record::Block(b) => assert_eq!(
                b.miner,
                Some(Miner {
                    slot: SlotId::CONTROLLER,
                    runs: Impl::Dashd
                })
            ),
            _ => {}
        }
    }
    assert_eq!(tips, [SlotId::CONTROLLER, SlotId::Regular(0)].into_iter().collect());
    assert_eq!(branches, tips);
}

#[tokio::test]
async fn retracts_a_fork() {
    let Some(f) = Fixture::start().await else {
        return;
    };
    let mut w = f.watch();
    f.watermark(H0, &mut w);
    f.mine(20);
    let tip = H0 + 20;
    f.watermark(tip, &mut w);
    let old = f.rpc(0, "getbestblockhash", &[]);
    until(|| f.rpc(1, "getbestblockhash", &[]) == old);
    let invalid = f.rpc(0, "getblockhash", &[(tip - 3).into()]);
    for i in [1, 0] {
        f.rpc(i, "invalidateblock", std::slice::from_ref(&invalid));
    }
    f.net.dashd(0).generate_blocks(6, &Address::dummy(Network::Regtest, 1));
    w.finish(0);
    let fork = tip - 4;
    let log = f.check(fork + 6);
    let reorgs: Vec<_> = log
        .iter()
        .filter_map(|e| match &e.record {
            Record::Reorg(r) => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(reorgs.len(), 1);
    assert_eq!(reorgs[0].old_tip.as_str(), old.as_str().unwrap());
    assert_eq!(reorgs[0].fork_height, fork);
    assert!(matches!(
        log.iter().rev().find_map(|e| match &e.record {
            Record::Block(b) => Some(b),
            _ => None,
        }),
        Some(Block {
            miner: None,
            ..
        })
    ));
}

async fn restart(torn: bool) {
    let Some(f) = Fixture::start().await else {
        return;
    };
    let mut w = f.watch();
    f.watermark(H0, &mut w);
    f.mine(20);
    f.watermark(H0 + 20, &mut w);
    w.0.kill().unwrap();
    w.0.wait().unwrap();
    f.mine(20);
    if torn {
        let h = H0 + 21;
        let hash = f.rpc(0, "getblockhash", &[h.into()]);
        let mut e = log(&f.out).pop().unwrap();
        e.seq += 1;
        e.record = Record::Block(Block {
            height: h,
            hash: hash.as_str().unwrap().parse().unwrap(),
            prev: f.rpc(0, "getblockhash", &[(h - 1).into()]).as_str().unwrap().parse().unwrap(),
            miner: Some(Miner {
                slot: SlotId::CONTROLLER,
                runs: Impl::Dashd,
            }),
        });
        let mut out = OpenOptions::new().append(true).open(&f.out).unwrap();
        writeln!(out, "{}", serde_json::to_string(&e).unwrap()).unwrap();
    }
    let prefix = fs::read(&f.out).unwrap();
    if torn {
        OpenOptions::new()
            .append(true)
            .open(&f.out)
            .unwrap()
            .write_all(br#"{"v":1,"run":"integ"#)
            .unwrap();
    }
    f.watch().finish(0);
    assert!(fs::read(&f.out).unwrap().starts_with(&prefix));
    let log = f.check(H0 + 40);
    assert_eq!(log.iter().filter(|e| matches!(e.record, Record::LlmqParams(_))).count(), 1);
}

#[tokio::test]
async fn resumes_after_sigkill() {
    restart(false).await;
}

#[tokio::test]
async fn resumes_after_a_torn_line() {
    restart(true).await;
}

#[tokio::test]
async fn orders_events_and_rejects_a_driver_block() {
    let Some(f) = Fixture::start().await else {
        return;
    };
    let mut w = f.watch();
    f.watermark(H0, &mut w);
    for (id, action) in [("first", "stop"), ("second", "start")] {
        w.send(json!({"kind":"Chaos","id":id,"action":action,"targets":["reg0"],"start_seq":1,"end_seq":1}));
    }
    until(|| log(&f.out).iter().any(|e| matches!(&e.record, Record::Chaos(c) if c.id == "second")));
    f.mine(1);
    w.finish(0);
    let events = log(&f.out);
    check_log(&events).unwrap();
    assert!(events.windows(2).all(|pair| pair[1].seq == pair[0].seq + 1));
    let initial = events
        .iter()
        .position(|e| matches!(&e.record, Record::Watermark(w) if w.height == H0))
        .unwrap();
    let next = events
        .iter()
        .position(|e| matches!(&e.record, Record::Block(b) if b.height == H0 + 1))
        .unwrap();
    let between = &events[initial + 1..next];
    assert!(!between.iter().any(|e| matches!(e.record, Record::Block(_))));
    let ids: Vec<_> = between
        .iter()
        .filter_map(|e| match &e.record {
            Record::Chaos(c) => {
                assert_eq!(e.src, "driver");
                Some(c.id.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["first", "second"]);
    assert!(matches!(&events.last().unwrap().record, Record::Watermark(w) if w.height == H0 + 1));
    let hash: Hex32 = "ff".repeat(32).parse().unwrap();
    let mut w = f.watch();
    w.send(json!({"kind":"Block", "height":100, "hash":hash, "prev":hash, "miner":null}));
    w.finish(2);
    assert!(!log(&f.out).iter().any(|e| matches!(&e.record, Record::Block(b) if b.hash == hash)));
}

#[tokio::test]
async fn finds_a_fork_below_the_first_watermark() {
    let Some(f) = Fixture::start().await else {
        return;
    };
    f.mine(10);
    let mut w = f.watch();
    let initial = H0 + 10;
    f.watermark(initial, &mut w);
    let old_tip = f.rpc(0, "getbestblockhash", &[]);
    until(|| f.rpc(1, "getbestblockhash", &[]) == old_tip);
    let fork = initial - 3;
    let invalid = f.rpc(0, "getblockhash", &[(fork + 1).into()]);
    for i in [1, 0] {
        f.rpc(i, "invalidateblock", std::slice::from_ref(&invalid));
    }
    f.net.dashd(0).generate_blocks(5, &Address::dummy(Network::Regtest, 1));
    w.finish(0);
    let rows = log(&f.out);
    check_log(&rows).unwrap();
    assert!(judge(&rows, Gate::Rehearsal, &Config::default()).passed);
    let reorgs: Vec<_> = rows
        .iter()
        .filter_map(|e| match &e.record {
            Record::Reorg(r) => Some(r.fork_height),
            _ => None,
        })
        .collect();
    assert_eq!(reorgs, [fork]);
    assert_eq!(metrics(&rows, &Config::default()).unwrap().range, (initial, fork + 5));
}

#[tokio::test]
async fn digest_mismatch_pages_once_across_restart() {
    use std::os::unix::fs::PermissionsExt;

    let Some(f) = Fixture::start().await else {
        return;
    };
    let calls = f.net.dir().join("page-calls.txt");
    let script = f.net.dir().join("page.sh");
    fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\n", calls.display()))
        .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let extra = [Path::new("--page-cmd"), script.as_path()];
    let mut w = f.watch_with(&extra);
    f.watermark(H0, &mut w);
    f.mine(20);
    f.watermark(H0 + 20, &mut w);
    let mismatch = json!({
        "kind": "DigestCompare",
        "node": "reg0",
        "from_height": H0 + 1,
        "to_height": H0 + 14,
        "result": {"mismatch": {"height": H0 + 10}}
    });
    w.send(mismatch.clone());
    w.finish(0);

    let page_name = format!("PAGE-{}-digest-mismatch.json", H0 + 10);
    let page_path = f.net.dir().join(&page_name);
    let pages = || -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(f.net.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("PAGE-") || n.starts_with(".PAGE-"))
            .collect();
        names.sort();
        names
    };
    assert_eq!(pages(), [page_name.as_str()]);
    assert_eq!(
        fs::read_to_string(&calls).unwrap().lines().collect::<Vec<_>>(),
        [page_path.display().to_string()]
    );
    let parsed: Value = serde_json::from_slice(&fs::read(&page_path).unwrap()).unwrap();
    assert_eq!(parsed["run"], json!("integration"));
    assert_eq!(parsed["kind"], json!("digest-mismatch"));
    assert_eq!(parsed["height"], json!(H0 + 10));
    assert_eq!(parsed["detail"], json!("reg0"));
    assert!(parsed["seq"].as_u64().is_some_and(|s| s > 0));

    let mut w = f.watch_with(&extra);
    w.send(mismatch);
    w.finish(0);

    assert_eq!(pages(), [page_name.as_str()]);
    assert_eq!(
        fs::read_to_string(&calls).unwrap().lines().collect::<Vec<_>>(),
        [page_path.display().to_string()]
    );
    f.check(H0 + 20);
}
