//! A `-dbcrashratio` crash in the `-reindex-chainstate` replay on the node (CH5-2a) against dashd
//! v24.0.0, on one rdashd and one dashd crash node beside an uncrashed dashd reference, on the
//! same flags and test-built blocks (CH3b b4b's harness, `common::node`). rdashd's crash height
//! follows its coins salt, which the test-hooks coins-salt seam fixes (BL-723), so this target
//! requires the feature. It cannot be skipped: with `SKIP_DASHD_TESTS` set it fails
//! (`common::require_dashd`).
#![cfg(unix)]

mod common;

use common::mine::*;
use common::node::*;
use common::*;

const FLAG: &str = "-reindex-chainstate";

/// The coins-salt seam's seed for rdashd's launches.
const SALT_SEED: &str = "18";

/// The height rdashd's first cache-pressure flush in the replay crashes at under [`SALT_SEED`]:
/// this fixture's calibration. CH2's layer layout, the coins budget or the hasher (BL-729) move
/// it; it is then measured again (each round's crash height, over several runs) and changed
/// deliberately. Few random salts crash there, so the pin also shows that the seed reached the
/// cache.
const RDASHD_CRASH_HEIGHT: u32 = 104;

/// The seam's line in a launch whose salt it fixed.
const SALT_FIXED: &str = "Coins cache salt fixed by";

/// The tip, the tips and `gettxoutsetinfo muhash`.
type State = (String, Vec<Tip>, Vec<(String, String)>);

fn state(node: &Node) -> State {
    (node.best(), node.tips(), node.utxo_set())
}

/// F1..F101 coinbase only, then blocks 102..=`last` of `outputs` zero-value outputs each and a
/// spend of F(h − 100)'s coinbase and of its parent's spend: enough coins that `-dbcache=4`
/// flushes inside the replay.
fn heavy_chain(genesis: &[u8; 32], last: u16, outputs: usize) -> Vec<Mined> {
    let fork = Fork::new(genesis);
    let mut chain = fork.f;
    let mut parent: Option<[u8; 32]> = None;
    for height in 102..=last {
        let coinbase = txid(&coinbase_paying(height - 100, b"f", &[0]));
        let mut inputs = vec![(coinbase, 0)];
        inputs.extend(parent.map(|txid| (txid, 0)));
        let spend = spend(&inputs, &[0, 0, 0]);
        parent = Some(txid(&spend));
        let prev = chain.last().unwrap().hash;
        chain.push(block(&prev, height, b"h", outputs, Some(spend)));
    }
    chain
}

/// The height of the last `UpdateTip` before the crash line in `log`.
fn crash_height(log: &str) -> u32 {
    let at = log.rfind(CRASH_LINE).expect("the crash line");
    let tip = log[..at].lines().rev().find(|l| l.contains("UpdateTip:")).expect("a tip");
    let height = tip.split_whitespace().find_map(|w| w.strip_prefix("height=")).unwrap();
    height.parse().unwrap()
}

/// `-dbcrashratio=1` during the `-reindex-chainstate` replay: the first cache-pressure flush of
/// startup's ABC crashes at its first threshold batch (Core's line, exit 0, no shutdown). A
/// restart without the flag replays the crashed flush forward from the null tip (no roll back)
/// and finishes the ABC; one with the flag wipes again, with no replay. Both end at the uncrashed
/// reference's state, on rdashd and on a dashd crash node. The flush rdashd selects comes earlier
/// than dashd's (CH3b-notes D225: under `-dbcache=4` rdashd selects about twice as many
/// cache-pressure flushes). Each node is held to its own crash height, the same in both rounds:
/// rdashd's salt, which picks its flush, is fixed by the seam (every crash launch logs that it
/// was), and its height is pinned at [`RDASHD_CRASH_HEIGHT`]; dashd's is held to its rounds.
// Core: txdb.cpp:152-161; validation.cpp:5238-5301; node/chainstate.cpp:233-236
#[test]
fn a_dbcrash_in_the_reindex_chainstate_replay_recovers_as_on_dashd_v24() {
    let home = RetainedHome::new("ch5-2a-dbcrash-");
    let dashd = require_dashd();
    let flags = ["-dbcache=4", "-dbbatchsize=200000"];
    let mut reference = Node::new("reference", &dashd, home.path(), &flags);
    let mut crash = [
        Node::rdashd(home.path(), &flags, Storage::Live),
        Node::new("dashd-crash", &dashd, home.path(), &flags),
    ];
    crash[0].env.push(("RDASHD_TEST_COINS_SALT", SALT_SEED.to_owned()));
    reference.start(0);
    for node in &mut crash {
        node.start(0);
    }
    for block in &heavy_chain(&genesis(&reference), 130, 6000) {
        let want = reference.submit(block);
        for node in &crash {
            assert_eq!(node.submit(block), want, "{}", node.name);
        }
    }
    let want = state(&reference);
    let mut heights = [Vec::new(), Vec::new()];
    for flagged in [false, true] {
        for (node, heights) in crash.iter_mut().zip(&mut heights) {
            node.stop();
            node.flags.push(FLAG.to_owned());
            let log = match node.launch(1) {
                // The replay is startup's ABC, after init's wait for genesis.
                Ok(()) => {
                    wait_until("the crash", || node.run.as_mut().unwrap().0.exited());
                    node.crashed()
                }
                Err(log) => panic!("{} crashed during init:\n{log}", node.name),
            };
            let loaded = log.find("init message: Done loading").expect("init ends");
            assert!(loaded < log.find(CRASH_LINE).unwrap(), "{}:\n{log}", node.name);
            if node.name == "rdashd" {
                assert!(log.contains(SALT_FIXED), "the seam fixed rdashd's salt:\n{log}");
            }
            let height = crash_height(&log);
            heights.push(height);
            if !flagged {
                node.flags.pop();
            }
            node.start(0);
            wait_until("the reference's tip", || node.best() == want.0);
            let log = node.new_log();
            let replay = replay_lines(&log);
            if flagged {
                assert!(replay.is_empty(), "{} wipes again: {replay:?}", node.name);
                node.flags.pop();
            } else {
                assert_eq!(replay[0], "Replaying blocks", "{}", node.name);
                assert!(replay[1..].iter().all(|l| l.starts_with("Rolling forward")), "{replay:?}");
                let last = format!("({})", height + 1);
                assert!(replay.last().unwrap().ends_with(&last), "{}: {replay:?}", node.name);
            }
            assert_eq!(state(node), want, "{} (flag {flagged}): the reference's", node.name);
        }
    }
    for (node, heights) in crash.iter().zip(&heights) {
        assert_eq!(heights[0], heights[1], "{}: the same flush in both rounds", node.name);
        assert!(heights[0] > 101 && heights[0] < 130, "{}: inside the replay", node.name);
    }
    assert_eq!(heights[0][0], RDASHD_CRASH_HEIGHT, "rdashd's crash height under seed {SALT_SEED}");
    for node in crash.iter_mut().chain([&mut reference]) {
        node.stop();
    }
}
