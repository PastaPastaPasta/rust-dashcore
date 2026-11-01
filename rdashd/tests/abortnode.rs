//! `feature_abortnode`'s reorg half as a regtest process test, on rdashd and on dashd v24 (CH3
//! §7.5, D46, D55; CH3b D212, D213): three blocks, the undo file `rev00000.dat` unlinked under
//! the running node, then a fork with more work. The fork's reorg cannot read the undo data of
//! the tip it disconnects, so `ActivateBestChainStep` aborts the node ("Failed to disconnect
//! block"), which prints Core's fatal error and exits 1. The blocks come by `submitblock`, where
//! the FT's blocks come from a peer: either way `ProcessNewBlock` runs the reorg. The FT's
//! restart half needs CH5's startup VerifyDB and closes with it (D55). It cannot be skipped: with
//! `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use common::mine::*;
use common::*;

/// `len` blocks on `genesis`, each a second later than its height's on the other chain when
/// `side`.
fn chain(genesis: &[u8; 32], len: u8, side: bool) -> Vec<Mined> {
    let mut blocks: Vec<Mined> = Vec::new();
    for height in 1..=len {
        let prev = blocks.last().map_or(genesis, |b| &b.hash);
        let time = GENESIS_TIME + 60 * u32::from(height) + u32::from(side);
        blocks.push(mine(prev, time, &[coinbase(height, 0)], None, true));
    }
    blocks
}

/// What a node did, as the test compares it.
#[derive(Debug, PartialEq)]
struct Abort {
    status: Option<i32>,
    stderr: String,
    /// Each `submitblock`'s result.
    replies: Vec<String>,
    /// The debug log's lines from the failed undo read through the abort's `InitError`, their
    /// times cut, the datadir and the old tip's hash masked.
    lines: Vec<String>,
    /// `ProcessNewBlock`'s line, after the `InitError`: Core's caller logs once `AbortNode` has
    /// reported (D213). Only the shutdown, on another thread, may log between them.
    process_new_block: Option<String>,
}

/// The reorg half on `binary` with `extra` arguments, in a private datadir, through the node's
/// shutdown.
fn abort_on_reorg(binary: &Path, extra: &[&str]) -> Abort {
    let (home, datadir) = home_and_datadir();
    std::fs::set_permissions(&datadir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let node = RpcNode::start_on(binary, home.path(), &datadir, extra);
    let submit = |block: &Mined| {
        node.call("submitblock", &format!(r#"["{}"]"#, block.hex)).result().to_owned()
    };

    let genesis_hex = node.call("getblockhash", "[0]").result().trim_matches('"').to_owned();
    let mut genesis = [0; 32];
    for (i, b) in genesis.iter_mut().rev().enumerate() {
        *b = u8::from_str_radix(&genesis_hex[2 * i..2 * i + 2], 16).unwrap();
    }
    let (main, fork) = (chain(&genesis, 3, false), chain(&genesis, 4, true));

    let mut replies: Vec<String> = main.iter().map(submit).collect();
    // Deleting the undo file will result in reorg failure (the FT's words).
    let rev = net(&datadir).join("blocks").join("rev00000.dat");
    std::fs::remove_file(&rev).unwrap_or_else(|e| panic!("{}: {e}", rev.display()));
    // The fork's first three blocks tie the tip, which stays; the fourth has more work.
    replies.extend(fork.iter().map(submit));
    let out = node.node.wait();

    let log = read(&debug_log(&datadir));
    // The abort's shutdown is Core's, as a `stop`'s is.
    assert_shut_down_in_order(&log);
    let dir = datadir.display().to_string();
    let mut tip = main[2].hash;
    tip.reverse();
    let tip = hex(&tip);
    let mut lines: Vec<_> = log
        .lines()
        .map(|l| {
            let l = l.split_once(' ').map_or(l, |(_, text)| text);
            l.replace(&dir, "<datadir>").replace(&tip, "<tip>")
        })
        .skip_while(|l| !l.starts_with("Unable to open file"))
        .collect();
    let error = lines.iter().position(|l| l.starts_with("Error: "));
    let after = lines.split_off(error.map_or(lines.len(), |i| i + 1));
    let process_new_block =
        after.into_iter().find(|l| l.contains("ProcessNewBlock: ActivateBestChain failed"));
    Abort {
        status: out.status.code(),
        stderr: stderr(&out),
        replies,
        lines,
        process_new_block,
    }
}

#[test]
fn a_reorg_without_its_undo_data_aborts_the_node_as_dashd_v24_does() {
    let core = abort_on_reorg(&require_dashd(), &["-disablewallet"]);
    let ours = abort_on_reorg(Path::new(RDASHD), &[]);
    assert_eq!(ours, core);
    // What both did, so the comparison cannot pass on a change both share.
    let fatal = "A fatal internal error occurred, see debug.log for details";
    let failed = "Failed to disconnect block; see debug.log for details";
    assert_eq!(
        ours,
        Abort {
            status: Some(1),
            stderr: format!("Error: {fatal}\n"),
            replies: [vec!["null"; 3], vec![r#""inconclusive""#; 4]]
                .concat()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            lines: vec![
                "Unable to open file <datadir>/regtest/blocks/rev00000.dat".to_owned(),
                "[error] UndoReadFromDisk: OpenUndoFile failed".to_owned(),
                "[error] DisconnectBlock(): failure reading undo data".to_owned(),
                "[error] DisconnectTip(): DisconnectBlock <tip> failed".to_owned(),
                format!("*** {failed}"),
                format!("Error: {fatal}"),
            ],
            process_new_block: Some(format!(
                "[error] ProcessNewBlock: ActivateBestChain failed ({failed})"
            )),
        }
    );
}
