//! S2's image (O2b §2.5, O2b-4 4d): the S0/S1 image, then [`LINE`] blocks above its tip, built
//! but held as headers only, with `-assumevalid` naming the [`AV_OFFSET`]th of them.
//!
//! Core skips a block's script checks when it is an ancestor of the assumevalid block and of the
//! best header, and the best header's proof-equivalent time over it is more than two weeks.
//! With regtest's constant bits that time is 150 s per block of lead, so with the best header at
//! the line's top, heights up to `tip + LINE - 8,065` (tip + 35) are past two weeks, and the
//! assumevalid block at `tip + 30` is the binding bound: exactly the line's first 30 blocks skip.
//! Heights above it, and any block off the line, are checked.
//!
//! Each header's time is its median time past plus one, so no header is ahead of the image's
//! build clock (`T_RECENT`), and every block an input can connect is older than the image tip,
//! whose age still decides IBD. The bits never vary (the tie model's and harness site `h1`'s
//! premise). The first [`BODIES`] line blocks spend two image coins each (`tx=spends`), so a
//! connected line does real work; three of them, below the assumevalid block, also carry a spend
//! that fails its script ([`AV_OPERATORS`]'s `av_script`), which only `-assumevalid` lets
//! through. Above [`BODIES`] a
//! line block is its coinbase alone: no input connects that high ([`MAX_BLOCKS`]), and above the
//! raised DIP3 such a body would be refused, as a header there is not.
// Core: src/validation.cpp:2553-2579 at v24.0.0

use std::sync::OnceLock;

use super::build::{self, BlockPlan, REGTEST_BITS, SPACING, TESTDUMMY_BIT, VERSION_BITS_TOP};
use super::core::{T_RECENT, image_world};
use super::generate::{FAN_OUT, MAX_BLOCKS, MIN_COIN, effects, fee_total, mtp, shaped_txs};
use super::intent::TxShape;
use super::mutate::{self, AV_OPERATORS, Operator};
use super::world::{Block, Coin, Hash, Held, Seq, World};

/// The line's length: more than two weeks of best-header lead at 150 s a block (8,064).
pub const LINE: u32 = 8_100;
/// The assumevalid block's place on the line (1: the image tip's child).
pub const AV_OFFSET: u32 = 30;
/// Two weeks in seconds: Core skips scripts only past this proof-equivalent time.
pub const TWO_WEEKS: i64 = 60 * 60 * 24 * 7 * 2;
/// The line places of the blocks whose one script fails: all below [`AV_OFFSET`].
pub const DEFECT_OFFSETS: [u32; 3] = [5, 15, 25];
/// The line places with spends: as many as an input can connect.
pub const BODIES: u32 = MAX_BLOCKS;

const _: () = assert!(DEFECT_OFFSETS[2] < AV_OFFSET && AV_OFFSET <= BODIES && BODIES < LINE);
const _: () = assert!((LINE - AV_OFFSET) as i64 * SPACING as i64 > TWO_WEEKS);

/// Where S2's line sits in a world: block ids `first ..= last()`, each the one before's child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    /// The image tip's line child.
    pub first: usize,
    /// The assumevalid block.
    pub av: usize,
    /// The blocks whose one script fails.
    pub defects: [usize; 3],
}

impl Line {
    /// The line's top: the best header.
    pub fn last(&self) -> usize {
        self.first + LINE as usize - 1
    }

    pub fn contains(&self, id: usize) -> bool {
        (self.first..=self.last()).contains(&id)
    }

    /// The line block on `parent`, if `parent` is the image tip or a line block below the top.
    pub fn child(&self, parent: usize) -> Option<usize> {
        (self.first - 1..self.last()).contains(&parent).then_some(parent + 1)
    }

    /// Whether Core skips block `id`'s scripts if it connects now: an ancestor of the
    /// assumevalid block and of Core's best header ([`World::best_header`]), more than two weeks
    /// of proof-equivalent time below it (constant bits: the height difference times the
    /// spacing; regtest's minimum chain work is 0). `None` when the model does not know the
    /// best header. The best header starts at the line's top and leaves it when a failure
    /// below it resets it to the active tip, or a load picks the most-work entry not failed.
    // Core: src/validation.cpp:2558-2578 at v24.0.0
    // Core: src/validation.cpp:2054-2055 at v24.0.0
    // Core: src/validation.cpp:5379-5380 at v24.0.0
    pub fn skips(&self, w: &World, id: usize) -> Option<bool> {
        let best = w.best_header?;
        let height = w.blocks[id].height;
        let lead = i64::from(w.blocks[best].height) - i64::from(height);
        Some(
            w.ancestor(self.av, height) == Some(id)
                && w.ancestor(best, height) == Some(id)
                && lead * i64::from(SPACING) > TWO_WEEKS,
        )
    }

    /// The fault a defect block `id` carries if it connects now, unless Core skips its
    /// scripts: `script_false`, the chain operator of the defect's shape (`av_script`'s), whose
    /// rejects name both `-par` settings' reasons; `None` for any other block.
    pub fn fault(&self, w: &World, id: usize) -> Option<&'static Operator> {
        (self.defects.contains(&id) && self.skips(w, id) == Some(false))
            .then(|| mutate::by_name("script_false").expect("script_false"))
    }
}

/// S2's image world: [`image_world`] and the line, each line block held as a header.
pub fn world() -> World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(build_world).clone()
}

/// The assumevalid block's hash (internal order).
pub fn av_hash() -> Hash {
    let w = world();
    let line = w.line.expect("S2's world has its line");
    w.blocks[line.av].hash
}

/// `-assumevalid=<the assumevalid block's hash, in RPC order>`.
pub fn av_arg() -> &'static str {
    static ARG: OnceLock<String> = OnceLock::new();
    ARG.get_or_init(|| {
        let hex: String = av_hash().iter().rev().map(|b| format!("{b:02x}")).collect();
        format!("-assumevalid={hex}")
    })
}

fn build_world() -> World {
    let mut w = image_world();
    let first = w.blocks.len();
    let mut parent = w.tip;
    for place in 1..=LINE {
        parent = w.add(line_block(&w, parent, place));
    }
    let at = |place: u32| first + place as usize - 1;
    w.line = Some(Line {
        first,
        av: at(AV_OFFSET),
        defects: DEFECT_OFFSETS.map(at),
    });
    w.best_header = Some(at(LINE));
    w
}

/// The line's `place`th block, on `parent`.
fn line_block(w: &World, parent: usize, place: u32) -> Block {
    let height = w.blocks[parent].height + 1;
    let mtp = mtp(w, parent);
    let mut txs = Vec::new();
    let mut rest = Vec::new();
    if place <= BODIES {
        let utxos = w.utxos(parent);
        let coins: Vec<Coin> = utxos
            .values()
            .filter(|c| c.mature_at(height) && c.value >= MIN_COIN)
            .copied()
            .collect();
        let (spends, used) = shaped_txs(TxShape::Spends, &coins, FAN_OUT);
        txs = spends;
        rest = coins[used..].to_vec();
        let fees = fee_total(&txs, &utxos);
        txs.insert(0, build::coinbase(height, build::subsidy(height) + fees, u64::from(height)));
    } else {
        txs.push(build::coinbase(height, build::subsidy(height), u64::from(height)));
    }
    let mut plan = BlockPlan {
        prev: w.blocks[parent].hash,
        version: VERSION_BITS_TOP | 1 << TESTDUMMY_BIT,
        time: mtp + 1,
        bits: REGTEST_BITS,
        txs,
        merkle_override: None,
    };
    if DEFECT_OFFSETS.contains(&place) {
        assert!(rest.len() >= mutate::COINS, "a defect's coins");
        let cx = mutate::Cx {
            height,
            mtp,
            mocktime: T_RECENT,
            coins: &rest,
            immature: None,
            l3: None,
        };
        (AV_OPERATORS[0].apply)(&mut plan, &cx);
    }
    let built = plan.build();
    let (creates, spends) = effects(&plan, &built, height);
    Block {
        hash: built.hash,
        parent: Some(parent),
        height,
        time: plan.time,
        built: Some(built),
        creates,
        spends,
        held: Held::Header,
        invalid: false,
        mutation: None,
        seq: Seq::Loaded,
        durable: Some((Held::Header, false)),
    }
}

#[cfg(test)]
mod tests {
    use dash_oracle_sys::ChainOp;

    use super::*;
    use crate::chain::build::{Lock, MAX_FUTURE};
    use crate::chain::census::Census;
    use crate::chain::core::{IMAGE_HEIGHT, Schedule, T_OLD};
    use crate::chain::generate::{Expect, Gen, Planned, What, expect, may_fail_at_connect};
    use crate::chain::intent::Program;

    fn line() -> (World, Line) {
        let w = world();
        let line = w.line.expect("the line");
        (w, line)
    }

    /// Each line header is its parent's median time past plus one, at regtest's bits, one height
    /// above its parent, from the image tip up.
    #[test]
    fn av_line_is_compressed_to_mtp_plus_one() {
        let (w, line) = line();
        assert_eq!(w.blocks.len(), IMAGE_HEIGHT as usize + 1 + LINE as usize);
        assert_eq!((w.tip, w.image_tip, line.first), (line.first - 1, line.first - 1, w.tip + 1));
        for id in line.first..=line.last() {
            let b = &w.blocks[id];
            let parent = b.parent.expect("a parent");
            assert_eq!(parent, id - 1);
            assert_eq!(b.height, w.blocks[parent].height + 1);
            assert_eq!(b.time, mtp(&w, parent) + 1, "height {}", b.height);
            let header = b.built.as_ref().expect("built").header;
            assert_eq!(u32::from_le_bytes(header[72..76].try_into().expect("4")), REGTEST_BITS);
            assert_eq!(u32::from_le_bytes(header[68..72].try_into().expect("4")), b.time);
            let version = i32::from_le_bytes(header[..4].try_into().expect("4"));
            assert_eq!(version, VERSION_BITS_TOP | 1 << TESTDUMMY_BIT, "it signals testdummy");
            assert_eq!(
                (b.held, b.seq, b.durable),
                (Held::Header, Seq::Loaded, Some((Held::Header, false)))
            );
        }
        assert_eq!(w.blocks[line.last()].height, IMAGE_HEIGHT + LINE);
    }

    /// The line stays inside the clock's windows: no header past the image's build clock
    /// (`T_RECENT`), so none is ever `MAX_FUTURE` ahead of it; every block an input can connect is
    /// older than the image tip, so the tip's age still decides IBD: out of it at `T_RECENT`
    /// (under 6 h), in it at `T_OLD`.
    #[test]
    fn av_line_stays_within_the_clock_windows() {
        let (w, line) = line();
        let tip_time = i64::from(w.blocks[w.image_tip].time);
        for id in line.first..=line.last() {
            assert!(i64::from(w.blocks[id].time) <= T_RECENT, "height {}", w.blocks[id].height);
        }
        for id in line.first..line.first + BODIES as usize {
            let time = i64::from(w.blocks[id].time);
            assert!(time < tip_time, "height {}", w.blocks[id].height);
            assert!(T_RECENT - time < 6 * 3600, "out of IBD at T_RECENT");
            assert!(T_OLD - time > 6 * 3600, "in IBD at T_OLD");
        }
        assert!(T_RECENT - tip_time < 6 * 3600 && T_OLD - tip_time > 6 * 3600);
        assert!(T_RECENT + MAX_FUTURE > i64::from(w.blocks[line.last()].time));
    }

    /// The assumevalid block is the line's 30th; Core's skip holds for the line's first 30
    /// blocks and no other: heights 31..35 are past two weeks but above the assumevalid block,
    /// and 36 up are within two weeks of the best header.
    #[test]
    fn av_block_is_the_thirtieth_and_skips_exactly_below_it() {
        let (w, line) = line();
        assert_eq!(line.av, line.first + 29);
        assert_eq!(w.blocks[line.av].height, IMAGE_HEIGHT + AV_OFFSET);
        assert_eq!(av_hash(), w.blocks[line.av].hash);
        let past_two_weeks: Vec<u32> = (line.first..=line.last())
            .map(|id| w.blocks[id].height)
            .filter(|&h| i64::from(IMAGE_HEIGHT + LINE - h) * i64::from(SPACING) > TWO_WEEKS)
            .collect();
        assert_eq!(past_two_weeks, (IMAGE_HEIGHT + 1..=IMAGE_HEIGHT + 35).collect::<Vec<_>>());
        for id in line.first..=line.last() {
            let place = id - line.first + 1;
            assert_eq!(line.skips(&w, id), Some(place <= AV_OFFSET as usize), "place {place}");
        }
    }

    /// The skip follows Core's best header: a defect is skipped while the best header is on the
    /// line more than two weeks (8,064 blocks) above it, and checked, `script_false`'s fault, once
    /// the best header is lower or off the line (a failure's reset to the active tip). A best
    /// header the model does not know decides nothing.
    #[test]
    fn a_defect_is_skipped_only_under_a_best_header_two_weeks_up_the_line() {
        let (mut w, line) = line();
        let defect = line.defects[0];
        let blocks = TWO_WEEKS / i64::from(SPACING);
        assert_eq!(blocks * i64::from(SPACING), TWO_WEEKS);
        let edge = defect + usize::try_from(blocks).expect("8,064 blocks");
        for (best, skips) in [(edge, false), (edge + 1, true), (line.last(), true), (w.tip, false)]
        {
            w.best_header = Some(best);
            assert_eq!(line.skips(&w, defect), Some(skips), "best header {best}");
            let fault = line.fault(&w, defect).map(|op| op.name);
            assert_eq!(fault, (!skips).then_some("script_false"), "best header {best}");
            assert!(line.fault(&w, defect + 1).is_none(), "not a defect");
        }
        w.best_header = None;
        assert_eq!(line.skips(&w, defect), None);
        assert!(line.fault(&w, defect).is_none());
    }

    /// The three script defects are the line's 5th, 15th and 25th blocks, each one funded spend of
    /// an always-false script; no other line block holds a false script.
    #[test]
    fn av_defects_sit_below_the_av_block() {
        let (w, line) = line();
        let false_script = Lock::False.script_pubkey();
        for id in line.first..line.first + BODIES as usize {
            let plan = BlockPlan::from_block(&w.blocks[id].built_bytes()).expect("a block");
            let locked = plan.txs.iter().flat_map(|t| &t.outputs).filter(|o| o.1 == false_script);
            let defect = line.defects.contains(&id);
            assert_eq!(locked.count(), usize::from(defect), "place {}", id - line.first + 1);
            if defect {
                assert!(w.blocks[id].height < w.blocks[line.av].height);
                assert_eq!(line.skips(&w, id), Some(true));
                assert!(line.fault(&w, id).is_none());
            }
            assert_eq!(plan.txs.len(), 3 + 2 * usize::from(defect), "spends, and the defect's two");
        }
        for id in line.first + BODIES as usize..=line.last() {
            let plan = BlockPlan::from_block(&w.blocks[id].built_bytes()).expect("a block");
            assert_eq!(plan.txs.len(), 1, "a coinbase alone above the bodies");
        }
    }

    /// S2's arguments are S1's and the assumevalid block, by its RPC-order hash; the chain
    /// parameters (D8's check) are S1's.
    #[test]
    fn s2_args_are_s1s_plus_assumevalid() {
        let mut want = Schedule::S1.args();
        want.push(av_arg());
        assert_eq!(Schedule::S2.args(), want);
        assert_eq!(Schedule::S2.chain_args(), Schedule::S1.chain_args());
        let hex = av_arg().strip_prefix("-assumevalid=").expect("the option");
        assert_eq!(crate::chain::world::display_hash(hex), Some(av_hash()));
        assert_eq!(Schedule::S2.name(), "S2");
    }

    /// The ops `text`'s intents resolve to on a fresh S2 world (no step observed: each intent
    /// starts from the image tip), and the world after.
    fn resolved(text: &str) -> (Vec<crate::chain::generate::Planned>, World, Gen) {
        let mut w = world();
        let mut generator = Gen::default();
        let mut census = Census::default();
        let mut ops = Vec::new();
        for intent in &Program::parse(text).expect("a program").intents {
            ops.extend(generator.resolve(&mut w, intent, &mut census));
        }
        (ops, w, generator)
    }

    /// S2's world with the line's first `n` blocks connected, after `stored`'s blocks are held
    /// with data (`failed`: marked failed too, under the input's own INVALIDATE of the first) as
    /// Core keeps a lighter branch, resolved while every skip holds; then `probe` resolved under
    /// the best header `best`. Its op count and `av_reconnect` drops.
    fn probed(
        n: usize,
        stored: &str,
        failed: bool,
        best: Option<usize>,
        probe: &str,
    ) -> (usize, u64) {
        let mut w = world();
        let line = w.line.expect("the line");
        for id in line.first..line.first + n {
            w.blocks[id].held = Held::Data;
        }
        w.tip = line.first + n - 1;
        let mut generator = Gen::default();
        let mut census = Census::default();
        for intent in &Program::parse(stored).expect("a program").intents {
            for p in generator.resolve(&mut w, intent, &mut census) {
                if let What::Block(id) = p.what {
                    w.blocks[id].held = Held::Data;
                    w.blocks[id].invalid = failed;
                    if failed && w.own_invalidates.is_empty() {
                        w.own_invalidates.insert(id);
                    }
                }
            }
        }
        assert_eq!(census.avoided.get("av_reconnect"), None, "{stored}: stored while skips hold");
        w.best_header = best;
        let mut census = Census::default();
        let mut ops = 0;
        for intent in &Program::parse(probe).expect("a program").intents {
            ops += generator.resolve(&mut w, intent, &mut census).len();
        }
        (ops, census.avoided.get("av_reconnect").copied().unwrap_or(0))
    }

    fn resolved_connected(best: Option<usize>, text: &str) -> (usize, u64) {
        probed(6, "", false, best, text)
    }

    /// DEC-1368: once a connected defect's skip has lapsed (the best header reset to the tip, or
    /// unknown), a branch forking below it with more work and a block that may fail at connect
    /// is not sent: Core's fallback would have to reconnect the defect. A clean branch, one
    /// forking above the defect, or the same branch while the skip holds, is sent.
    #[test]
    fn a_branch_that_would_reconnect_a_checked_defect_is_dropped() {
        let line = world().line.expect("the line");
        let tip = Some(line.first + 5);
        let risky = "mutate script_false\nfork 4 6\n";
        assert_eq!(resolved_connected(tip, risky), (0, 1), "the skip lapsed: dropped");
        assert_eq!(resolved_connected(None, risky), (0, 1), "an unknown best header: dropped");
        assert_eq!(resolved_connected(Some(line.last()), risky), (6, 0), "the skip holds");
        assert_eq!(resolved_connected(tip, "fork 4 6\n"), (6, 0), "a clean branch");
        let above = "mutate script_false\nfork 0 3\n";
        assert_eq!(resolved_connected(tip, above), (3, 0), "forking above the defect");
        let short = "mutate script_false\nfork 4 2\n";
        assert_eq!(resolved_connected(tip, short), (2, 0), "less work than the tip");
        // Only a Connect-stage operator Core rejects may fail the branch at connect: a Check- or
        // Pow-stage one is refused on arrival, nothing stored.
        for op in ["merkle_root", "bits_plus"] {
            let text = format!("mutate {op}\nfork 4 6\n");
            assert_eq!(resolved_connected(tip, &text), (6, 0), "{op}: sent");
        }
    }

    /// The operators that may fail a stored branch at connect. `bip68_off` is a valid-edge
    /// operator (no rejects): no `chain-v2` slot reaches one yet, so it is judged directly.
    #[test]
    fn only_a_rejected_connect_stage_operator_may_fail_at_connect() {
        for (name, may) in [
            ("script_false", true),
            ("bip68_height", true),
            ("bip68_off", false),
            ("merkle_root", false),
            ("bits_plus", false),
            ("time_too_new", false),
            ("cb_height", false),
        ] {
            let op = mutate::by_name(name).expect(name);
            assert_eq!(may_fail_at_connect(op), may, "{name}");
        }
    }

    /// The other arms: with the skip lapsed (best header at the tip, 206), an INVALIDATE of the
    /// tip with a stored risky branch of equal work, a PRECIOUS of that branch's tip, and a
    /// RECONSIDER of a stored risky branch with more work, marked failed, each make Core try the
    /// branch, and are dropped. The INVALIDATE with a clean branch, or while the skip holds, is
    /// sent.
    #[test]
    fn each_arm_drops_a_switch_to_a_risky_branch() {
        let line = world().line.expect("the line");
        let tip = Some(line.first + 5);
        let level = "mutate script_false\nfork 4 4\n";
        assert_eq!(probed(6, level, false, tip, "invalidate main@-0\n"), (0, 1), "INVALIDATE");
        assert_eq!(probed(6, "fork 4 4\n", false, tip, "invalidate main@-0\n"), (1, 0));
        let held = Some(line.last());
        assert_eq!(probed(6, level, false, held, "invalidate tip#1\n"), (1, 0), "not active");
        assert_eq!(probed(6, level, false, tip, "precious tip#1\n"), (0, 1), "PRECIOUS");
        assert_eq!(probed(6, "fork 4 4\n", false, tip, "precious tip#1\n"), (1, 0));
        let heavier = "mutate script_false\nfork 4 5\n";
        assert_eq!(probed(6, heavier, true, tip, "reconsider tip#1\n"), (0, 1), "RECONSIDER");
        assert_eq!(probed(6, "fork 4 5\n", true, tip, "reconsider tip#1\n"), (1, 0));
    }

    /// B1 (4d r1): an INVALIDATE of an ancestor of the best header resets it to the tip's parent
    /// within its own op, and the same op's `ActivateBestChain` then tries a stored risky branch
    /// forking below a defect whose skip held a moment before: dropped. With the best header
    /// off the target's chain, the skip still holds through the op, and it is sent.
    #[test]
    fn an_invalidate_that_resets_the_best_header_is_judged_after_the_reset() {
        let line = world().line.expect("the line");
        let stored = "mutate script_false\nfork 6 5\n";
        let invalidate = "invalidate main@-1\n";
        assert_eq!(probed(8, stored, false, Some(line.last()), invalidate), (0, 1), "B1");
        assert_eq!(probed(8, "fork 6 5\n", false, Some(line.last()), invalidate), (1, 0));
    }

    /// B2 (4d r1): with defects 205 and 215 both connected and lapsed, an INVALIDATE at 210
    /// disconnects 215 but leaves 205 connected, and a stored risky branch from 204 with more
    /// work than 209 is tried: dropped.
    #[test]
    fn an_invalidate_above_a_lower_lapsed_defect_is_dropped() {
        let line = world().line.expect("the line");
        let tip = Some(line.first + 14);
        let stored = "mutate script_false\nfork 11 8\n";
        assert_eq!(probed(15, stored, false, tip, "invalidate main@-5\n"), (0, 1), "B2");
        assert_eq!(probed(15, "fork 11 8\n", false, tip, "invalidate main@-5\n"), (1, 0));
    }

    /// A line defect sent under a best header the model does not know is expected `Any`, and
    /// its row is `av_best_unknown`; under the line's top it is expected `ok` (skipped).
    #[test]
    fn a_defect_under_an_unknown_best_header_is_expected_any() {
        let mut w = world();
        let line = w.line.expect("the line");
        for id in line.first..line.defects[0] {
            w.blocks[id].held = Held::Data;
        }
        w.tip = line.defects[0] - 1;
        let p = Planned::new(ChainOp::Block(Vec::new()), What::Block(line.defects[0]));
        assert_eq!(expect(&w, &p), Expect::Ok);
        w.best_header = None;
        assert_eq!(expect(&w, &p), Expect::Any);
    }

    /// On S2 a main-branch extension takes the line's next blocks, whatever its flags shape:
    /// `headers` sends their headers first, `hold` holds their data for a `deliver`. An armed
    /// operator builds a sibling of the line's block instead, and so does a fork: neither is on the
    /// line, and neither is its ancestor.
    #[test]
    fn s2_main_extension_delivers_the_line() {
        let line = world().line.expect("the line");
        let block = |id: usize| What::Block(id);
        let (ops, _, generator) = resolved("extend main 3 tx=fan signal\n");
        let whats: Vec<What> = ops.iter().map(|p| p.what.clone()).collect();
        assert_eq!(whats, (line.first..line.first + 3).map(block).collect::<Vec<_>>());
        assert!(ops.iter().all(|p| matches!(p.op, ChainOp::Block(_))));
        assert_eq!(generator.built, (line.first..line.first + 3).collect::<Vec<_>>());
        assert_eq!(generator.blocks_left, MAX_BLOCKS - 3);

        let (ops, ..) = resolved("extend main 2 headers\n");
        let whats: Vec<What> = ops.iter().map(|p| p.what.clone()).collect();
        let headers = [What::Header(line.first), What::Header(line.first + 1)];
        assert_eq!(whats[..2], headers);
        assert_eq!(whats[2..], [block(line.first), block(line.first + 1)]);

        let (ops, w, _) = resolved("extend main 2 hold\ndeliver b1\n");
        assert_eq!(w.branches, [line.first + 1]);
        let whats: Vec<What> = ops.iter().map(|p| p.what.clone()).collect();
        assert_eq!(whats, [block(line.first), block(line.first + 1)]);

        for text in ["mutate script_false\nextend main 1\n", "fork 0 1\n"] {
            let (ops, w, _) = resolved(text);
            let [p] = &ops[..] else {
                panic!("{text}: {ops:?}");
            };
            let What::Block(id) = p.what else {
                panic!("{text}: {p:?}");
            };
            assert!(id > line.last(), "{text}: a new block");
            assert_eq!(w.blocks[id].parent, Some(w.image_tip), "{text}: a sibling of the line's");
            assert_eq!(w.blocks[id].mutation.is_some(), text.starts_with("mutate"), "{text}");
            assert!(!line.contains(id) && line.skips(&w, id) == Some(false));
        }
    }

    /// `l3::leg::av`'s descendant count and two weeks are the line's (one rule, two builders).
    #[cfg(feature = "l3")]
    #[test]
    fn chain_av_constants_match_l3s() {
        assert_eq!(LINE, crate::l3::leg::av::DESCENDANTS);
        assert_eq!(TWO_WEEKS, crate::l3::leg::av::TWO_WEEKS);
    }
}
