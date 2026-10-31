//! The checks a recovered image must pass once C0 ([`super::inspect`]) has (PLAN §3.4): C1 a start
//! without reindex, C2 the coin statistics at the recovered tip, C3 freshness and C4 the stored
//! blocks. C5, the continuation, is [`super::baseline::run`] from the recovered tip.

use std::path::Path;
use std::time::Duration;

use serde_json::json;

use super::baseline::node_args;
use super::fixture::{Fixture, MUHASH_FIELDS, SERIALIZED_FIELDS};
use super::logwrites::Rng;
use crate::{NodeRpc, RustNode, RustNodeConfig, find_available_port};

/// How long the harness waits for one RPC reply. Regtest checks the whole block index after each
/// header and each connected block (`-checkblockindex`, as Core's `ProcessNewBlockHeaders`,
/// `validation.cpp:4737`), so a call queued behind a 2,000-header batch waits minutes at W-40k's
/// heights, where jsonrpc gives up after 15 s.
pub(crate) const RPC_PATIENCE: Duration = Duration::from_secs(600);

/// How long C1 gives rdashd on a crash image to load (saturated fsync on a loaded box).
pub const C1_PATIENCE: Duration = Duration::from_secs(120);

/// Text in debug.log or stderr that fails C1: the node did not recover from its own data.
pub const C1_FORBIDDEN: [&str; 7] = [
    "Error opening block database",
    "Unable to replay blocks",
    "Found EvoDB inconsistency",
    "-reindex",
    "Corruption",
    "panicked at",
    // AbortNode's log line.
    "*** ",
];

/// How many ancestors of the recovered tip C4 reads, and how many other stored blocks.
pub const C4_ANCESTORS: usize = 10;
pub const C4_SAMPLE: usize = 32;

/// The lines of `text` that fail C1.
pub fn c1_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| C1_FORBIDDEN.iter().any(|f| l.contains(f)))
        .map(|l| format!("C1: {l}"))
        .collect()
}

/// C1: rdashd starts on `datadir` with no peers and loads within [`C1_PATIENCE`]. The node is
/// returned even when its log fails C1, so the other checks still run and it stops cleanly.
pub async fn c1(
    rdashd: &Path,
    fixture: &Fixture,
    datadir: &Path,
    extra: &[String],
    failures: &mut Vec<String>,
) -> Option<RustNode> {
    let config = RustNodeConfig::with_binary(rdashd, datadir)
        .with_mocktime(Some(crate::MockTime::at(fixture.mocktime)))
        .without_peers()
        .with_rpc(find_available_port())
        .with_args(node_args(fixture, extra));
    match tokio::time::timeout(C1_PATIENCE, RustNode::start(config)).await {
        Ok(Ok(node)) => {
            failures.extend(c1_lines(&node.log()));
            failures.extend(c1_lines(&node.stderr()));
            Some(node)
        }
        Ok(Err(e)) => {
            failures.push(format!("C1: rdashd did not start: {e}"));
            None
        }
        Err(_) => {
            failures.push(format!("C1: rdashd did not load within {} s", C1_PATIENCE.as_secs()));
            None
        }
    }
}

/// C2: both hash types' statistics at the node's tip equal the oracle row of that tip. Returns the
/// tip `gettxoutsetinfo` reported.
pub fn c2(
    rpc: &dyn NodeRpc,
    fixture: &Fixture,
    failures: &mut Vec<String>,
) -> Result<String, String> {
    let mut best = String::new();
    for (hash_type, fields) in
        [("muhash", &MUHASH_FIELDS[..]), ("hash_serialized_2", &SERIALIZED_FIELDS[..])]
    {
        let stats = rpc.call("gettxoutsetinfo", &[json!(hash_type)]).map_err(|e| e.to_string())?;
        match fixture.check_stats(&stats, fields) {
            Ok(tip) => best = tip,
            Err(why) => {
                failures.push(format!("C2 {hash_type}: {why}"));
                best = stats["bestblock"].as_str().unwrap_or_default().to_owned();
            }
        }
    }
    Ok(best)
}

/// What a point's state must be at least as fresh as (C3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Floor {
    /// No acknowledgement precedes the point: any tip on the trajectory, genesis included.
    Start,
    /// The last acknowledgement before the point saw the trajectory's tip at this position.
    Ack(usize),
    /// The node stopped cleanly at the trajectory's tip at this position, and nothing is replayed.
    Stopped(usize),
}

/// C3: the recovered `tip` is on the trajectory at or after the floor; its position there.
pub fn c3(fixture: &Fixture, tip: &str, floor: Floor, log: &str) -> Result<usize, String> {
    match floor {
        Floor::Start | Floor::Ack(_) => {
            let from = if let Floor::Ack(p) = floor {
                p
            } else {
                0
            };
            fixture
                .on_trajectory(tip, from)
                .ok_or_else(|| format!("C3: tip {tip} is not on the trajectory at or after {from}"))
        }
        Floor::Stopped(p) => {
            if fixture.trajectory[p] != tip {
                return Err(format!(
                    "C3: tip {tip}, but the node stopped at {}",
                    fixture.trajectory[p]
                ));
            }
            if log.contains("Replaying blocks") {
                return Err("C3: a cleanly stopped node replayed blocks".to_owned());
            }
            Ok(p)
        }
    }
}

/// The blocks C4 reads: the tip, its [`C4_ANCESTORS`] ancestors, and [`C4_SAMPLE`] others from
/// `stored` (the blocks C0 found the index has the data of) drawn with `seed`.
pub fn c4_blocks(fixture: &Fixture, tip: &str, stored: &[String], seed: u64) -> Vec<String> {
    let mut picked = vec![tip.to_owned()];
    let mut at = tip;
    while picked.len() <= C4_ANCESTORS {
        match fixture.rows.get(at).and_then(|r| r.prev.as_deref()) {
            Some(prev) => {
                picked.push(prev.to_owned());
                at = prev;
            }
            None => break,
        }
    }
    let mut rest: Vec<&String> = stored.iter().filter(|h| !picked.contains(h)).collect();
    let mut rng = Rng::new(seed);
    for _ in 0..C4_SAMPLE.min(rest.len()) {
        let i = rng.below(rest.len() as u64) as usize;
        picked.push(rest.swap_remove(i).clone());
    }
    picked
}

/// C4: `getblock <h> 0` is the fixture's bytes for every block of [`c4_blocks`], F01's side blocks
/// included.
pub fn c4(
    rpc: &dyn NodeRpc,
    fixture: &Fixture,
    tip: &str,
    stored: &[String],
    seed: u64,
    failures: &mut Vec<String>,
) {
    for hash in c4_blocks(fixture, tip, stored, seed) {
        let Some(want) = fixture.block_hex(&hash) else {
            failures.push(format!("C4: {hash} is no fixture block"));
            continue;
        };
        match rpc.call("getblock", &[json!(hash), json!(0)]) {
            Ok(got) if got.as_str() == Some(want) => {}
            Ok(_) => failures.push(format!("C4: getblock {hash} differs from the fixture")),
            Err(e) => failures.push(format!("C4: getblock {hash}: {e}")),
        }
    }
}

/// What C1–C4 found on one recovered image.
#[derive(Debug, Default)]
pub struct PointReport {
    /// The recovered tip, and its position on the trajectory (C3) when it is there.
    pub tip: String,
    pub pos: Option<usize>,
    pub failures: Vec<String>,
}

/// C1–C4 on the image's `datadir` (rdashd writes to it), then a clean stop. `stored` is C0's
/// list of the blocks the index has the data of.
pub async fn check_point(
    rdashd: &Path,
    fixture: &Fixture,
    datadir: &Path,
    extra: &[String],
    floor: Floor,
    stored: &[String],
    seed: u64,
) -> PointReport {
    let mut report = PointReport::default();
    let Some(node) = c1(rdashd, fixture, datadir, extra, &mut report.failures).await else {
        return report;
    };
    match node.rpc_with_timeout(RPC_PATIENCE) {
        Ok(rpc) => match c2(&rpc, fixture, &mut report.failures) {
            Ok(tip) => {
                match c3(fixture, &tip, floor, &node.log()) {
                    Ok(pos) => report.pos = Some(pos),
                    Err(e) => report.failures.push(e),
                }
                c4(&rpc, fixture, &tip, stored, seed, &mut report.failures);
                report.tip = tip;
            }
            Err(e) => report.failures.push(format!("C2: gettxoutsetinfo: {e}")),
        },
        Err(e) => report.failures.push(format!("C1: no RPC client: {e}")),
    }
    match node.stop().await {
        Ok(stopped) if stopped.is_clean() => {}
        Ok(stopped) => report.failures.push(format!("rdashd did not stop cleanly: {stopped:?}")),
        Err(e) => report.failures.push(format!("stopping rdashd: {e}")),
    }
    report
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use serde_json::Value;

    use super::*;
    use crate::crash::fixture::{F01, Row};

    /// A chain `b0 … b19` with every block stored, and a trajectory that reorgs `b5` out for
    /// `s5` and back: `b0 … b5 b4 s5 b4 b5 … b19`.
    fn chain() -> Fixture {
        let name = |i: usize| format!("b{i}");
        let mut rows = HashMap::new();
        for i in 0..20 {
            let row = Row {
                hash: name(i),
                height: i as u64,
                prev: i.checked_sub(1).map(name),
                fields: serde_json::Map::new(),
            };
            rows.insert(name(i), row);
        }
        let mut trajectory: Vec<String> = (0..=5).map(name).collect();
        trajectory.extend(["b4", "s5", "b4"].map(String::from));
        trajectory.extend((5..20).map(name));
        Fixture {
            dir: PathBuf::new(),
            blocks: rows.keys().map(|h| (h.clone(), format!("raw-{h}"))).collect(),
            rows,
            p2p_datadir: PathBuf::new(),
            p2p_tip: name(10),
            submit: (11..20).map(name).collect(),
            final_tip: name(19),
            mocktime: 0,
            trajectory,
            f01: None,
            chain_args: Vec::new(),
        }
    }

    #[test]
    fn c3_holds_the_tip_to_the_floor() {
        let f = chain();
        assert_eq!(c3(&f, "b0", Floor::Start, ""), Ok(0));
        assert_eq!(c3(&f, "s5", Floor::Start, ""), Ok(7));
        // After an ack at b5 (the second time, past the reorg), b4 and s5 are stale.
        assert_eq!(c3(&f, "b5", Floor::Ack(9), ""), Ok(9));
        assert!(c3(&f, "s5", Floor::Ack(9), "").is_err());
        assert!(c3(&f, "b4", Floor::Ack(9), "").is_err());
        assert_eq!(c3(&f, "b19", Floor::Ack(9), ""), Ok(23));
        assert!(c3(&f, "x", Floor::Start, "").is_err());
        // A stop pins the tip, and nothing may be replayed.
        assert_eq!(c3(&f, "b19", Floor::Stopped(23), ""), Ok(23));
        assert!(c3(&f, "b18", Floor::Stopped(23), "").is_err());
        assert!(c3(&f, "b19", Floor::Stopped(23), "t Replaying blocks\n").is_err());
    }

    #[test]
    fn c4_reads_the_tip_its_ancestors_and_a_seeded_sample() {
        let f = chain();
        let stored: Vec<String> = (0..20).map(|i| format!("b{i}")).collect();
        let picked = c4_blocks(&f, "b15", &stored, 7);
        assert_eq!(
            picked[..=C4_ANCESTORS],
            (5..=15).rev().map(|i| format!("b{i}")).collect::<Vec<_>>()
        );
        // The rest of the stored blocks, each once: fewer than C4_SAMPLE remain.
        let mut rest = picked[C4_ANCESTORS + 1..].to_vec();
        rest.sort();
        let mut want: Vec<String> = (0..5).chain(16..20).map(|i| format!("b{i}")).collect();
        want.sort();
        assert_eq!(rest, want);
        assert_eq!(picked, c4_blocks(&f, "b15", &stored, 7));
        // Near genesis the ancestors run out.
        assert_eq!(c4_blocks(&f, "b2", &[], 7), ["b2", "b1", "b0"]);
    }

    /// Replies to `getblock <h> 0` from `blocks`.
    struct Blocks(HashMap<String, String>);

    impl NodeRpc for Blocks {
        fn call(&self, method: &str, params: &[Value]) -> Result<Value, crate::RpcError> {
            assert_eq!(method, "getblock");
            let hash = params[0].as_str().expect("a hash");
            self.0.get(hash).map(|b| json!(b)).ok_or_else(|| format!("no block {hash}").into())
        }
    }

    #[test]
    fn c4_knows_the_f01_side_blocks() {
        let mut f = chain();
        f.f01 = Some(F01 {
            after: "b12".into(),
            f_hash: "f".into(),
            f_hex: "raw-f".into(),
            c_hash: "c".into(),
            c_hex: "raw-c".into(),
            dashd_submit_f: Value::Null,
            dashd_submit_c: Value::Null,
            dashd_c_status: Value::Null,
        });
        assert_eq!(f.block_hex("f"), Some("raw-f"));
        assert_eq!(f.block_hex("c"), Some("raw-c"));
        assert_eq!(f.block_hex("b3"), Some("raw-b3"));
        assert_eq!(f.block_hex("x"), None);
        // The node stores F and C on a side branch, so C0 lists them and C4 may pick them.
        let stored: Vec<String> = ["b0", "b1", "f", "c"].map(String::from).to_vec();
        let mut node = f.blocks.clone();
        node.extend([("f".into(), "raw-f".into()), ("c".into(), "raw-c".into())]);
        let mut failures = Vec::new();
        c4(&Blocks(node.clone()), &f, "b1", &stored, 7, &mut failures);
        assert_eq!(failures, Vec::<String>::new());
        // A side block whose bytes differ still fails.
        node.insert("c".into(), "torn".into());
        c4(&Blocks(node), &f, "b1", &stored, 7, &mut failures);
        assert_eq!(failures, ["C4: getblock c differs from the fixture"]);
    }

    #[test]
    fn c1_names_each_forbidden_line() {
        let log = "2026-10-29T00:00:00Z init message: Loading block index…\n\
                   2026-10-29T00:00:01Z Error opening block database\n\
                   2026-10-29T00:00:01Z Please restart with -reindex or -reindex-chainstate to recover.\n\
                   2026-10-29T00:00:02Z *** Failed to read block\n\
                   2026-10-29T00:00:03Z Done loading\n";
        assert_eq!(c1_lines(log).len(), 3);
        assert!(c1_lines("thread 'main' panicked at src/x.rs:1:1:\n")[0].starts_with("C1: "));
        assert!(c1_lines("2026-10-29T00:00:00Z UpdateTip: new best=00ab height=7\n").is_empty());
    }
}
