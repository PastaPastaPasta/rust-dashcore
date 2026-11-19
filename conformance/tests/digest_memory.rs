//! The streamed replay's memory bound (CH3a D149; O8's ruling of 2026-10-13, the manager's of
//! 2026-10-21): a replay's peak RSS stays flat as the block count grows, rather than tracking the
//! size of `blocks.dat`.
//!
//! Each replay runs in a child process, this test binary run again on [`memory_child`] alone,
//! which reports its peak RSS (`VmHWM`, `/proc/self/status`).
//!
//! - **Synthetic:** two regtest `blocks.dat` files of N and 4N blocks, one the other's prefix.
//!   Every block but genesis is a coinbase of about 990 kB, most of the 1 MB limit
//!   (`-testactivationheight=dip0001@...` keeps DIP0001's 2 MB away), paying 0 to one
//!   `OP_RETURN` output, which is unspendable: the coins set does not grow with the blocks. Both
//!   files replay through the `CoreConnector` leg's [`replay_window`] over [`Frames`] reading the
//!   file. The peak RSS of the larger replay exceeds the smaller one's by less than an eighth of
//!   the growth in file size ([`GROWTH_BOUND`]).
//! - **feature_block/node0**, the corpus's largest node (`blocks.dat` is about 1.09 GB, zstd
//!   compressed to 0.65 MB), replays on O9a-2's streamed [`replay`] with a peak RSS under a quarter
//!   of its `blocks.dat`; the whole-file read it replaces held all of it. The `CoreConnector`
//!   leg's [`replay_node`] streams it too (`digest_replay_core`), but a debug build spends minutes
//!   on its MuHash, so its figure is the D-row's, measured once.

mod digest_common;

use std::io::{BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use dash_conformance::digest::frames::Frames;
use dash_conformance::digest::replay::replay;
use dash_conformance::digest::replay_core::replay_window;
use dash_consensus::Block;
use dash_consensus::primitives::core_serialize;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_statedigest::Chain;
use dashd_chainstate::chain::genesis_block;
use digest_common::{coinbase_sig, decode, mine, stored, tempdir};

/// The child's task, from the parent: `blocks <path>` or `null <label>`.
const CHILD: &str = "CH3A_DIGEST_MEMORY_CHILD";
/// The child's scratch directory.
const CHILD_DIR: &str = "CH3A_DIGEST_MEMORY_DIR";

/// Blocks in the smaller synthetic file, genesis included; the larger holds four times as many.
const N: usize = 16;
/// The `OP_RETURN` push in each synthetic coinbase: the block is about 990 kB.
const PUSH: usize = 990_000;
/// The peak RSS growth allowed, as a share of the file-size growth: an eighth.
const GROWTH_BOUND: u64 = 8;
/// feature_block's peak RSS allowed, as a share of its `blocks.dat`: a quarter.
const NODE_BOUND: u64 = 4;
/// Far past any synthetic height: DIP0001's 2 MB limit never applies.
const NO_DIP0001: u32 = 1_000_000;

/// Regtest at v24 with DIP0001 out of reach: the block limit is the legacy 1 MB.
fn params() -> Arc<ChainSetup> {
    let builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::TestActivationHeight, format!("dip0001@{NO_DIP0001}"));
    Arc::new(builder.build().unwrap())
}

/// The coinbase at `height`: its one output pays 0 to `OP_RETURN OP_PUSHDATA4 <PUSH bytes>`.
fn coinbase(height: u32) -> Vec<u8> {
    let sig = coinbase_sig(height);
    let mut script = vec![0x6a, 0x4e];
    script.extend((PUSH as u32).to_le_bytes());
    script.resize(script.len() + PUSH, 0xa5);

    let mut raw = 1i32.to_le_bytes().to_vec();
    raw.push(1);
    raw.extend([0u8; 32]);
    raw.extend(u32::MAX.to_le_bytes());
    raw.push(sig.len() as u8);
    raw.extend(sig);
    raw.extend(u32::MAX.to_le_bytes());
    raw.push(1);
    raw.extend(0i64.to_le_bytes());
    raw.push(0xfe);
    raw.extend((script.len() as u32).to_le_bytes());
    raw.extend(script);
    raw.extend(0u32.to_le_bytes());
    raw
}

/// Block `height` on `prev`: the coinbase alone.
fn block(prev: &Block, height: u32) -> Arc<Block> {
    mine(prev, vec![decode(&coinbase(height))])
}

/// `blocks` in `-loadblock` framing.
fn write_blocks(path: &Path, magic: [u8; 4], blocks: &[Arc<Block>]) -> u64 {
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    for b in blocks {
        let bytes = core_serialize(&**b);
        file.write_all(&magic).unwrap();
        file.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
        file.write_all(&bytes).unwrap();
    }
    file.into_inner().unwrap().sync_all().unwrap();
    std::fs::metadata(path).unwrap().len()
}

/// This process's peak RSS in bytes.
fn vm_hwm() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status.lines().find_map(|l| l.strip_prefix("VmHWM:")).expect("VmHWM");
    let kb = line.trim().strip_suffix("kB").expect("VmHWM in kB").trim();
    kb.parse::<u64>().unwrap() * 1024
}

/// The child: one replay, then its peak RSS on stdout. Without the parent's variables, nothing.
#[test]
fn memory_child() {
    let Ok(task) = std::env::var(CHILD) else {
        return;
    };
    let dir = PathBuf::from(std::env::var(CHILD_DIR).unwrap());
    let out = dir.join("digest.jsonl");
    let work = dir.join("work");
    let (kind, arg) = task.split_once(' ').unwrap();
    let blocks = match kind {
        "blocks" => {
            let p = params();
            let file = std::fs::File::open(arg).unwrap();
            let frames = Frames::new(BufReader::new(file), p.message_start);
            // The mock clock far enough on for every synthetic block's time.
            let time = i64::from(genesis_block(&p).header().time()) + 150 * 4 * N as i64;
            replay_window(p, Chain::Regtest, time, frames, &out, &work).unwrap().blocks
        }
        "null" => replay(&stored(arg), &out).unwrap().blocks,
        _ => panic!("task {task}"),
    };
    println!("CH3A-MEMORY blocks={blocks} vm_hwm={}", vm_hwm());
}

/// Runs `task` in a child; its blocks handed to V and its peak RSS.
fn child(task: &str) -> (usize, u64) {
    let dir = tempdir();
    let start = std::time::Instant::now();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "memory_child", "--nocapture", "--test-threads=1"])
        .env(CHILD, task)
        .env(CHILD_DIR, dir.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{task}: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let line = stdout
        .lines()
        // The harness prints the test's name on the same line.
        .find_map(|l| l.split_once("CH3A-MEMORY ").map(|(_, r)| r))
        .unwrap_or_else(|| panic!("{task}: no report\n{stdout}"));
    let field = |name: &str| {
        line.split(' ')
            .find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
            .unwrap_or_else(|| panic!("{task}: no {name} in {line}"))
    };
    let blocks = field("blocks").parse().unwrap();
    let rss = field("vm_hwm").parse().unwrap();
    eprintln!("{task}: {blocks} blocks, peak RSS {rss} bytes, {:.1?}", start.elapsed());
    (blocks, rss)
}

/// CH3a D149: four times the blocks, nearly four times the file, and the peak RSS about where it
/// was.
#[test]
fn peak_rss_stays_flat_as_the_file_grows() {
    let p = params();
    let mut chain = vec![genesis_block(&p)];
    for h in 1..4 * N as u32 {
        chain.push(block(chain.last().unwrap(), h));
    }
    let dir = tempdir();
    let (small, large) = (dir.path().join("small.dat"), dir.path().join("large.dat"));
    let small_len = write_blocks(&small, p.message_start, &chain[..N]);
    let large_len = write_blocks(&large, p.message_start, &chain);
    assert!(large_len >= 4 * small_len, "{large_len} is not four times {small_len}");
    // Most of the 1 MB limit: Core's -loadblock frame cap is the 2 MB DIP0001 one.
    assert!(chain[1..].iter().all(|b| (990_000..1_000_000).contains(&core_serialize(&**b).len())));

    let (small_blocks, small_rss) = child(&format!("blocks {}", small.display()));
    let (large_blocks, large_rss) = child(&format!("blocks {}", large.display()));
    assert_eq!((small_blocks, large_blocks), (N - 1, 4 * N - 1));
    let file_growth = large_len - small_len;
    let rss_growth = large_rss.saturating_sub(small_rss);
    eprintln!(
        "files {small_len} -> {large_len} bytes (+{file_growth}); peak RSS {small_rss} -> \
         {large_rss} bytes (+{rss_growth}, {:.2}% of the file growth)",
        100.0 * rss_growth as f64 / file_growth as f64
    );
    assert!(
        rss_growth * GROWTH_BOUND < file_growth,
        "peak RSS grew {rss_growth} bytes for {file_growth} bytes of file"
    );
}

/// CH3a D149: the largest corpus node replays on the streamed path holding well under its
/// `blocks.dat` at its peak.
#[test]
fn feature_block_replays_on_the_streamed_path() {
    const LABEL: &str = "feature_block/node0";
    let node = stored(LABEL);
    let mut input = dash_conformance::digest::store::open(&node.blocks()).unwrap();
    let size = std::io::copy(&mut input, &mut std::io::sink()).unwrap();
    let (blocks, rss) = child(&format!("null {LABEL}"));
    assert_eq!(blocks, 1224);
    eprintln!(
        "{LABEL}: blocks.dat {size} bytes, peak RSS {rss} bytes ({:.2}%)",
        100.0 * rss as f64 / size as f64
    );
    assert!(rss * NODE_BOUND < size, "peak RSS {rss} bytes for {size} bytes of blocks");
}
