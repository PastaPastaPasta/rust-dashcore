//! `evo_deep_window` (CH7a-5a, BL-646/CH7b-3): CH7 §7.1's growth guard (D22, DEC-527 6),
//! the process `contrib/node/evo-deep-window.sh` counts. Not a PF0 row.
//!
//! `evo_deep_window CHAIN MODE [FORK]`: connects the text recording with its header's chain
//! parameters. With FORK, connects M through S's parent, then S, then the rest of M, reorging off
//! S before measuring the last 1,000 blocks. The segment must carry non-null commitments of all
//! three recorded types and CbTx version >= 2. Without FORK the null-commitment precursor works.
//!
//! - `shallow`: flush at the segment parent, leaving an empty evo window;
//! - `deep`: flush at 1,000, leaving about 19,000 blocks and S's tombstones in the window.
//!
//! The flush must not run again: `-dbcache` (`EVO_DEEP_WINDOW_DBCACHE`, MiB, or KiB with a `k`
//! suffix; default 4,096 MiB) holds the coins and the window, and the clock is fixed, so nothing
//! is periodic. The last line, `segment mode=M from=H HASH to=H HASH flushed_at=H HASH
//! begins_flushed=N durable_before=HASH frozen_before=0|1 begins_before=N durable_after=HASH
//! frozen_after=0|1 begins_after=N mined_before=N tombstones_before=N
//! mined_expected=N tombstones_expected=N wall_ms=MS`, reports the
//! durable block, any flush in flight, W's full-flush counter, and the unflushed mined entries
//! (including tombstones) and tombstone subset at the segment start, beside their expected
//! counts derived from the recording.
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

#[path = "support/deep_window.rs"]
mod deep_window;
#[path = "support/evo_node.rs"]
mod evo_node;
#[path = "support/recorded.rs"]
mod recorded;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dash_chain::BlockRef;
use dashd_chainstate::coins::Ratio;
use dashd_chainstate::flush::FlushStateMode;

use evo_node::{Flush, FlushState, Node, decode};

/// The segment: the chain's last blocks.
const SEGMENT: usize = 1_000;
/// The deep configuration's flush height.
const DEEP_FLUSH: usize = 1_000;

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
        eprintln!("usage: evo_deep_window CHAIN shallow|deep [FORK]");
        return;
    };
    let text = std::fs::read_to_string(&chain).expect("the chain");
    let p = recorded::params(recorded::header_args(&text).split_whitespace());
    let blocks = decode(&text);
    let fork = args.next().map(|path| {
        let fork_text = std::fs::read_to_string(path).expect("the side branch");
        assert_eq!(
            recorded::header_args(&text),
            recorded::header_args(&fork_text),
            "M/S parameters"
        );
        decode(&fork_text)
    });
    assert!(args.next().is_none(), "CHAIN MODE [FORK]");
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
        p,
        i64::from(last) + 86_400,
        Flush::Node {
            dbcache,
            f,
        },
    );

    let (before, segment) = blocks.split_at(blocks.len() - SEGMENT);
    let fork_at = fork.as_ref().map(|side| {
        let commitments = deep_window::validate_segment(segment, before.len() + 1);
        println!("workload blocks={SEGMENT} cbtx_min_version=2 nonnull_commitments={commitments}");
        deep_window::fork_parent(&blocks, side, before.len(), DEEP_FLUSH)
    });
    let (mined_expected, tombstones_expected) = match (&fork, fork_at) {
        (Some(side), Some(_)) if mode == "deep" => {
            let main_count = deep_window::non_null_commitments(&before[flush_at..]);
            let side_count = deep_window::non_null_commitments(side);
            (2 * main_count + 2 * side_count, 2 * side_count)
        }
        _ => (0, 0),
    };
    let connect_before = |start: usize, end: usize| {
        if let (Some(side), Some(parent)) = (&fork, fork_at)
            && start <= parent
            && parent < end
        {
            node.connect(&before[start..parent]);
            node.connect_branch(side);
            assert_eq!(node.v.view().snap.tip.tip_hash, side.last().expect("S").hash(), "S's tip");
            let seq = node.v.view().snap.tip_seq;
            let reorg_end = parent + side.len() + 1;
            node.connect_branch(&before[parent..reorg_end]);
            let view = node.v.view();
            assert_eq!(view.snap.tip.tip_hash, before[reorg_end - 1].hash(), "M wins");
            assert_eq!(
                view.snap.tip_seq,
                seq + 2 * side.len() as u64 + 1,
                "S's disconnects and M's connects"
            );
            node.connect(&before[reorg_end..end]);
        } else {
            node.connect(&before[start..end]);
        }
    };
    connect_before(0, flush_at);
    node.v.flush(FlushStateMode::Always).recv().expect("the forced flush");
    let flushed = node.flush_state();
    assert_eq!(
        flushed.durable.map(|r| r.height as usize),
        Some(flush_at),
        "the forced flush's block"
    );
    assert!(!flushed.frozen, "ALWAYS waits for W's release");
    node.hold_w(hold_w);
    connect_before(flush_at, before.len());
    let (mined, tombstones) = node.mined_counts();
    let pre = node.flush_state();
    if fork.is_some() && !hold_w && pre == flushed {
        assert_eq!(
            (mined, tombstones),
            (mined_expected, tombstones_expected),
            "the recording's unflushed mined entries and tombstones"
        );
    }

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
         {} {} mined_before={mined} tombstones_before={tombstones} \
         mined_expected={mined_expected} tombstones_expected={tombstones_expected} wall_ms={}",
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
