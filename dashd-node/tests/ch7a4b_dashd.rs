//! CH7a-4b's dashd differential (L1, CH7 §7.2, D15, D64): dashd v24.0.0-rc.4's recorded
//! DIP0003-active regtest chain (`dashd-chainstate/tests/data/ch7a4b`, DIP0003 at 48, DIP0024
//! moved out) goes as the same bytes to a fresh dashd's `submitblock` and to V. After each block
//! both nodes are compared: the tip, `getdeploymentinfo`'s report field by field, and the
//! masternode list (dashd's `protx list` and V's lists at its tip and DMN tip, all empty). V's
//! H4 checks each CbTx's `merkleRootMNList` and `merkleRootQuorums` against the roots it
//! computes, so a block V connects is one whose roots V computed as dashd did.
//!
//! Then a reorg back below DIP0003 restarts on both (BL-539, D64): `invalidateblock 41`, fork b's
//! blocks 41 to 47, a restart of each node on its own datadir, and `reconsiderblock 41`, which
//! returns both to main's tip across DIP0003's block 48.
//!
//! Needs dashd v24.0.0-rc.4 (see `v24`).

mod v24;

use dash_primitives::BlockHash;
use serde_json::json;

use v24::{Harness, blocks};

/// dashd's main chain, blocks 1 to 200.
const MAIN: &str = include_str!("../../dashd-chainstate/tests/data/ch7a4b/blocks.txt");
/// Fork b: blocks 41 to 47 off main's 40, below DIP0003.
const FORK_B: &str = include_str!("../../dashd-chainstate/tests/data/ch7a4b/fork-b.txt");
/// The height fork b branches off.
const FORK_B_OFF: usize = 40;

/// Both nodes agree at `height`, whose tip must be `tip`: their tips, their deployment reports
/// and their (empty) masternode lists.
fn assert_agree(h: &Harness, tip: &BlockHash, height: usize) {
    let (list, dmn) = h.assert_agree_on_tip(tip, height);
    let protx = h.dashd().call("protx", json!(["list"]));
    assert_eq!(protx, json!([]), "dashd's list at {height}");
    // `evo_view`'s list at the tip is the tip's own from DIP0003; the DMN tip's list
    // (`GetListAtChainTip`, which `protx list` reads) can lag it after a disconnect, as V has no
    // mempool to move it (D16), and is empty either way.
    assert_eq!((list.len(), dmn.len()), (0, 0), "V's lists at {height}");
}

/// Feeds main's blocks 1 to 200 to both nodes, comparing after each (every mining window's null
/// commitments through 186), then runs the reorg below DIP0003 and the restart (BL-539).
fn differential() {
    let Some(mut h) = Harness::start() else {
        return;
    };
    let main = blocks(MAIN);
    let fork_b = blocks(FORK_B);
    assert_eq!((main.len(), fork_b.len()), (200, 7));
    for (i, b) in main.iter().enumerate() {
        h.submit(b, i + 1);
        assert_agree(&h, &b.hash(), i + 1);
    }

    // `invalidateblock 41`: both walk back to 40, below DIP0003, then take fork b.
    let cut = main[FORK_B_OFF].hash();
    h.dashd().call("invalidateblock", json!([cut.to_string()]));
    let chain = h.sut.chain().expect("V runs");
    assert_eq!(chain.invalidate_block(cut).recv(), Ok(Ok(())));
    assert_agree(&h, &main[FORK_B_OFF - 1].hash(), FORK_B_OFF);
    for (i, b) in fork_b.iter().enumerate() {
        h.submit(b, FORK_B_OFF + 1 + i);
        assert_agree(&h, &b.hash(), FORK_B_OFF + 1 + i);
    }
    let fork_tip = fork_b.last().unwrap().hash();

    // Both restart at a tip below DIP0003 whose datadir holds rows main's evo blocks wrote.
    h.restart();
    assert_eq!(h.sut_tip(), fork_tip, "V restarts at fork b's tip");
    assert_agree(&h, &fork_tip, FORK_B_OFF + fork_b.len());

    // `reconsiderblock 41`: main has more work, so both return to its tip across 48.
    h.dashd().call("reconsiderblock", json!([cut.to_string()]));
    let chain = h.sut.chain().expect("V runs");
    assert_eq!(chain.reconsider_block(cut, false).recv(), Ok(Ok(())));
    assert_agree(&h, &main[199].hash(), 200);
}

/// Under `sut::`, the gate's feature-step filter (`[package.metadata.gate]`).
mod sut {
    use super::*;

    #[test]
    fn the_dip3_chain_past_the_first_window_equals_dashds() {
        differential();
    }
}
