//! `evo_deep_window` (CH7a-5a): CH7 §7.1's `evo-deep-window` growth guard (D22, DEC-527 6), the
//! process `contrib/node/evo-deep-window.sh` counts. Not a PF0 row.
//!
//! `evo_deep_window CHAIN MODE`: `support/evo_node.rs`'s node with the production flush connects
//! CHAIN (`record.sh deep`'s regtest chain: DIP0003 at 48, DIP0024, V20 and MN_RR past its end, so
//! every block runs the evo hooks over an empty list), forces an ALWAYS flush, then connects the
//! segment, the chain's last 1,000 blocks:
//!
//! - `shallow`: the flush at the segment's parent, so the segment starts on an empty evo window;
//! - `deep`: the flush at 1,000, so the segment starts about 19,000 blocks deep in the window.
//!
//! The flush must not run again: `-dbcache` (`EVO_DEEP_WINDOW_DBCACHE`, MiB, or KiB with a `k`
//! suffix; default 4,096 MiB) holds the coins and the window, and the clock is fixed, so nothing
//! is periodic. The last line, `segment mode=M from=H HASH to=H HASH flushed_at=H HASH
//! begins_flushed=N durable_before=HASH frozen_before=0|1 begins_before=N durable_after=HASH
//! frozen_after=0|1 begins_after=N wall_ms=MS`, says where the coins were durable after the
//! forced flush and around the segment, whether a flush was in flight there (a frozen layer W has
//! not released), and how many full flushes W had begun.
//!
//! With `EVO_DEEP_WINDOW_SYNC=DIR` the segment waits for the script's counter: the bench writes
//! V's thread id to `DIR/tid`, waits for `DIR/go`, writes `DIR/done` after the segment and waits
//! for `DIR/end` before it stops V. It panics if its parent exits while it waits.
//!
//! `EVO_DEEP_WINDOW_HOLD_W=1` is the script's self-test: W stops at the first full flush after
//! the forced one until the last line is read, so a flush in flight is left in the segment, and
//! THRESHOLD is at a sixteenth of the budget rather than half.

// PROCESS.md: benches measure production only, so a test feature turned on here (by a self
// dev-dependency or `--all-features`) fails the build. Clippy's `--all-features` run is exempt.
#[cfg(all(not(clippy), any(feature = "crash", feature = "planted-bugs")))]
compile_error!("benches measure production only: build them without `crash` and `planted-bugs`");

#[path = "support/evo_node.rs"]
mod evo_node;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dash_chain::BlockRef;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dashd_chainstate::coins::Ratio;
use dashd_chainstate::flush::FlushStateMode;

use evo_node::{Flush, FlushState, Node, decode};

/// The segment: the chain's last blocks.
const SEGMENT: usize = 1_000;
/// The deep configuration's flush height.
const DEEP_FLUSH: usize = 1_000;

/// `record.sh deep`'s parameters, and `-checkblockindex` off.
fn params() -> Arc<ChainSetup> {
    let mut p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "48:48")
        .arg(ChainOption::TestActivationHeight, "dip0024@1000000")
        .arg(ChainOption::TestActivationHeight, "v20@1000000")
        .arg(ChainOption::TestActivationHeight, "mn_rr@1000000")
        .build()
        .expect("regtest");
    p.default_consistency_checks = false;
    Arc::new(p)
}

/// Waits for the script's `path`, while the script runs.
fn wait_for(path: &Path, parent: u32) {
    while !path.exists() {
        assert_eq!(std::os::unix::process::parent_id(), parent, "the script exited");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn hash(r: Option<BlockRef>) -> String {
    r.map_or_else(|| "none".into(), |r| r.hash.to_string())
}

fn state(at: &str, s: FlushState) -> String {
    format!(
        "durable_{at}={} frozen_{at}={} begins_{at}={}",
        hash(s.durable),
        u8::from(s.frozen),
        s.begins
    )
}

fn main() {
    let mut args = std::env::args().skip(1).filter(|a| a != "--bench");
    let (Some(chain), Some(mode)) = (args.next(), args.next()) else {
        // `cargo bench` with no arguments runs every bench target: this one has nothing to do.
        eprintln!("usage: evo_deep_window CHAIN shallow|deep");
        return;
    };
    let blocks = decode(&std::fs::read_to_string(&chain).expect("the chain"));
    assert!(blocks.len() > DEEP_FLUSH + SEGMENT, "the chain is too short for the segment");
    let flush_at = match mode.as_str() {
        "shallow" => blocks.len() - SEGMENT,
        "deep" => DEEP_FLUSH,
        other => panic!("mode {other}: shallow or deep"),
    };
    let dbcache = std::env::var("EVO_DEEP_WINDOW_DBCACHE").map_or(4096 << 20, |v| {
        let (n, shift) = v.strip_suffix('k').map_or((v.as_str(), 20), |k| (k, 10));
        n.parse::<u64>().expect("EVO_DEEP_WINDOW_DBCACHE in MiB, or KiB with a `k`") << shift
    });
    let hold_w = std::env::var("EVO_DEEP_WINDOW_HOLD_W").is_ok_and(|v| v == "1");
    // A held W's frozen layer counts in native Σ until the end, and V would wait for its release
    // at CRITICAL: the hold's run freezes at a sixteenth of the budget, which leaves the active
    // layer room to the segment's end.
    let f = if hold_w {
        Ratio::new(1, 16)
    } else {
        Ratio::HALF
    };
    let last = blocks.last().expect("blocks").header().raw().time();
    let node = Node::start(
        params(),
        i64::from(last) + 86_400,
        Flush::Node {
            dbcache,
            f,
        },
    );

    let (before, segment) = blocks.split_at(blocks.len() - SEGMENT);
    node.connect(&before[..flush_at]);
    node.v.flush(FlushStateMode::Always).recv().expect("the forced flush");
    let flushed = node.flush_state();
    assert_eq!(
        flushed.durable.map(|r| r.height as usize),
        Some(flush_at),
        "the forced flush's block"
    );
    assert!(!flushed.frozen, "ALWAYS waits for W's release");
    node.hold_w(hold_w);
    node.connect(&before[flush_at..]);
    let pre = node.flush_state();

    let sync = std::env::var_os("EVO_DEEP_WINDOW_SYNC").map(PathBuf::from);
    let parent = std::os::unix::process::parent_id();
    if let Some(dir) = &sync {
        // V's thread, which `perf stat -t` counts: `/proc/thread-self` is `PID/task/TID`.
        let tid = node
            .v
            .query(|_| std::fs::read_link("/proc/thread-self").expect("/proc/thread-self"))
            .recv()
            .expect("V");
        let tid = tid.file_name().expect("a thread id").to_owned();
        std::fs::write(dir.join("tid.part"), tid.as_encoded_bytes()).expect("tid");
        std::fs::rename(dir.join("tid.part"), dir.join("tid")).expect("tid");
        wait_for(&dir.join("go"), parent);
    }
    let start = Instant::now();
    node.connect(segment);
    let wall = start.elapsed();
    if let Some(dir) = &sync {
        std::fs::write(dir.join("done"), b"").expect("done");
        wait_for(&dir.join("end"), parent);
    }
    let post = node.flush_state();
    node.hold_w(false);

    let first = blocks.len() - SEGMENT + 1;
    println!(
        "segment mode={mode} from={first} {} to={} {} flushed_at={flush_at} {} begins_flushed={} \
         {} {} wall_ms={}",
        segment[0].hash(),
        blocks.len(),
        segment[SEGMENT - 1].hash(),
        hash(flushed.durable),
        flushed.begins,
        state("before", pre),
        state("after", post),
        wall.as_millis(),
    );
}
