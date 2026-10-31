//! Coverage and the recent-lock horizon over synthetic chains (SB0 §4.3.1, §5.2.1; D24, D25):
//! headers that carry only their height, sets of commitments held through a test seam. Coverage
//! reads each commitment's type and quorum hash and the chain's ancestry, nothing else, so
//! no signature or list is needed. The cases with Core's replies are in `testnet` and `regtest`.

use std::num::NonZeroUsize;
use std::sync::Arc;

use dash_chain::{AncestryError, AnchoredAncestry, BlockRef, StoreError};
use dash_consensus::BlockId;
use dash_consensus::ctx::HeaderCtx;
use dash_evo::llmq::WORK_DIFF_DEPTH;
use dash_evo::locks::LockReject;
use dash_num::{Arith256, CompactTarget};
use dash_params::LlmqType;
use dash_params::{
    BuriedDeployment, ChainOption, ChainParamsBuilder, LlmqParams, Network, Release,
};
use dash_primitives::{BlockHash, Commitment, MerkleRoot, QuorumHash, QuorumVvecHash};
use dash_types::Numeric;

use super::persist::Store;
use super::{
    CHAIN_LOCKS, FixtureChain, asked_heights, clsigs, core_quorums, genesis_diff, llmq, stepped,
};
use crate::{
    Coverage, FetchLookupError, LlmqConfig, LockOutcome, Lookup, MasternodeListEngine, MnChain,
    MnEvent, MnReject, MnRequest, Mode, QuorumState, RequestKey, SetAt, Unavailable,
};

/// The anchor of SB0's mainnet witnesses.
const ANCHOR: u32 = 2_500_000;

fn config(network: Network, args: &[(ChainOption, &str)]) -> LlmqConfig {
    let mut builder = ChainParamsBuilder::new(network, Release::V24_0);
    for (option, value) in args {
        builder = builder.arg(*option, *value);
    }
    LlmqConfig::new(Arc::new(builder.build().expect("params"))).expect("in the domain")
}

fn mainnet() -> LlmqConfig {
    config(Network::Main, &[])
}

/// The block hash at `height`: its height, then a fixed fill.
fn hash_of(height: u32) -> BlockHash {
    let mut bytes = [0xd5; 32];
    bytes[..4].copy_from_slice(&height.to_le_bytes());
    BlockHash::from(bytes)
}

/// A chain from `floor` to `tip`, height as id.
struct Synth {
    floor: u32,
    tip: u32,
}

impl Synth {
    fn check(&self, height: u32) -> Result<(), AncestryError> {
        if height < self.floor {
            return Err(AncestryError::BelowFloor {
                height,
            });
        }
        assert!(height <= self.tip, "an id the chain never issued");
        Ok(())
    }
}

impl AnchoredAncestry for Synth {
    fn floor(&self) -> u32 {
        self.floor
    }

    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError> {
        self.check(id.0)?;
        Ok(HeaderCtx {
            height: id.0,
            hash: hash_of(id.0),
            time: 0,
            bits: CompactTarget::new(0x207f_ffff),
            version: 4,
            mtp: 0,
            chain_work: Arith256::ZERO,
        })
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError> {
        if height > id.0 {
            return Ok(None);
        }
        self.check(height).map(|_| Some(BlockId(height)))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError> {
        match id.0.checked_sub(1) {
            None => Ok(None),
            Some(h) => self.check(h).map(|_| Some(BlockId(h))),
        }
    }
}

impl MnChain for Synth {
    fn ancestry(&self) -> &dyn AnchoredAncestry {
        self
    }

    fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError> {
        let bytes = hash.as_bytes();
        let height = u32::from_le_bytes(bytes[..4].try_into().expect("four bytes"));
        let known = hash_of(height) == *hash && (self.floor..=self.tip).contains(&height);
        Ok(known.then_some(BlockId(height)))
    }

    fn active_tip(&self) -> Result<BlockRef, AncestryError> {
        Ok(BlockRef {
            id: BlockId(self.tip),
            hash: hash_of(self.tip),
            height: self.tip,
        })
    }

    fn merkle_root(&self, _: BlockId) -> Result<Option<MerkleRoot>, AncestryError> {
        Ok(None)
    }
}

/// A [`Synth`] whose lowest 24 entries are an anchor pack's context, as a `HeaderChain`'s.
struct Packed(Synth);

impl MnChain for Packed {
    fn ancestry(&self) -> &dyn AnchoredAncestry {
        &self.0
    }

    fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError> {
        self.0.lookup(hash)
    }

    fn active_tip(&self) -> Result<BlockRef, AncestryError> {
        self.0.active_tip()
    }

    fn merkle_root(&self, id: BlockId) -> Result<Option<MerkleRoot>, AncestryError> {
        self.0.merkle_root(id)
    }

    fn anchor(&self) -> u32 {
        self.0.floor + 24
    }
}

/// A [`Synth`] whose active block at `at`, if any, is another branch's (an id `Synth` never
/// issues) as an ancestor of the blocks above it, or, with `fault`, whose store fails reading it.
struct Moved {
    chain: Synth,
    at: Option<u32>,
    fault: bool,
}

/// The other branch's block at `height`, as [`Moved`] issues it.
fn moved_id(height: u32) -> BlockId {
    BlockId(height | 1 << 31)
}

/// The other branch's block hash at `height`: its height, then another fill than [`hash_of`]'s.
fn moved_hash(height: u32) -> BlockHash {
    let mut bytes = [0x5d; 32];
    bytes[..4].copy_from_slice(&height.to_le_bytes());
    BlockHash::from(bytes)
}

impl AnchoredAncestry for Moved {
    fn floor(&self) -> u32 {
        self.chain.floor
    }

    fn header(&self, id: BlockId) -> Result<HeaderCtx, AncestryError> {
        let height = id.0 & !(1 << 31);
        if id != moved_id(height) {
            return self.chain.header(id);
        }
        Ok(HeaderCtx {
            hash: moved_hash(height),
            ..self.chain.header(BlockId(height))?
        })
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, AncestryError> {
        if self.at != Some(height) || height >= id.0 {
            return self.chain.ancestor(id, height);
        }
        if self.fault {
            return Err(AncestryError::Storage(StoreError::Io("injected".into())));
        }
        Ok(Some(moved_id(height)))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, AncestryError> {
        self.chain.prev(id)
    }
}

impl MnChain for Moved {
    fn ancestry(&self) -> &dyn AnchoredAncestry {
        self
    }

    /// Either branch's block, as Core's block index knows every accepted header.
    fn lookup(&self, hash: &BlockHash) -> Result<Option<BlockId>, AncestryError> {
        let height = u32::from_le_bytes(hash.as_bytes()[..4].try_into().expect("four bytes"));
        if moved_hash(height) == *hash && (self.chain.floor..=self.chain.tip).contains(&height) {
            return Ok(Some(moved_id(height)));
        }
        self.chain.lookup(hash)
    }

    fn active_tip(&self) -> Result<BlockRef, AncestryError> {
        self.chain.active_tip()
    }

    fn merkle_root(&self, id: BlockId) -> Result<Option<MerkleRoot>, AncestryError> {
        self.chain.merkle_root(id)
    }
}

/// A commitment of `llmq_type` based at `base`, at `index` for a rotated type: one of the
/// regtest capture's, renamed.
fn commitment(llmq_type: LlmqType, base: u32, index: Option<i16>) -> Commitment {
    let mut qc = genesis_diff(281).new_quorums[0].clone();
    qc.llmq_type = llmq_type;
    qc.quorum_hash = QuorumHash::from(*hash_of(base).as_bytes());
    qc.version = if index.is_some() {
        4
    } else {
        3
    };
    qc.quorum_index = index;
    qc
}

fn lp(cfg: &LlmqConfig, llmq_type: LlmqType) -> &LlmqParams {
    cfg.params().consensus.llmq(llmq_type).expect("configured")
}

fn interval(cfg: &LlmqConfig, llmq_type: LlmqType) -> u32 {
    u32::from(lp(cfg, llmq_type).dkg_interval)
}

/// The used types, each once.
fn used(cfg: &LlmqConfig) -> Vec<LlmqType> {
    let c = &cfg.params().consensus;
    let mut types = vec![c.llmq_type_chain_locks, c.llmq_type_dip0024_instant_send];
    types.push(c.llmq_type_platform);
    types.dedup();
    types
}

/// The newest cycle of `llmq_type` mined by `at` when every round mines at its window's end.
fn newest_cycle(cfg: &LlmqConfig, llmq_type: LlmqType, at: u32) -> u32 {
    let mined = at - u32::from(lp(cfg, llmq_type).dkg_mining_window_end);
    mined - mined % interval(cfg, llmq_type)
}

/// The rotated type's commitments of `cycle`, one per index.
fn cycle_of(cfg: &LlmqConfig, cycle: u32) -> Vec<Commitment> {
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let count = lp(cfg, rotated).signing_active_quorum_count;
    (0..count)
        .map(|i| {
            let i = i16::try_from(i).expect("small");
            commitment(rotated, cycle + u32::try_from(i).expect("small"), Some(i))
        })
        .collect()
}

/// Core's active set at `at` under regular DKG for every used type: each rotated index's last
/// mined commitment and each other type's last A.
fn regular(cfg: &LlmqConfig, at: u32) -> Vec<Commitment> {
    let mut set = Vec::new();
    for t in used(cfg) {
        let newest = newest_cycle(cfg, t, at);
        if lp(cfg, t).use_rotation {
            set.extend(cycle_of(cfg, newest));
        } else {
            let a = u32::from(lp(cfg, t).signing_active_quorum_count);
            set.extend((0..a).map(|k| commitment(t, newest - k * interval(cfg, t), None)));
        }
    }
    set
}

/// The domain's two ends at `tip`: max(tip − H_keep, A_q) and tip − 8.
fn ends(cfg: &LlmqConfig, tip: u32) -> (u32, u32) {
    (tip.saturating_sub(cfg.h_keep()).max(cfg.a_q()), tip - 8)
}

/// An engine holding `oldest` and `newest` as the authenticated sets at the domain's ends.
fn holding(
    cfg: &LlmqConfig,
    tip: u32,
    oldest: &[Commitment],
    newest: &[Commitment],
) -> MasternodeListEngine {
    let (o, n) = ends(cfg, tip);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(o), o, hash_of(o), oldest);
    engine.hold_set(BlockId(n), n, hash_of(n), newest);
    engine
}

fn coverage(engine: &mut MasternodeListEngine, cfg: &LlmqConfig, floor: u32, tip: u32) -> Coverage {
    engine
        .coverage(
            &Synth {
                floor,
                tip,
            },
            cfg,
        )
        .expect("no store fault")
}

/// The regular-DKG control: covered once the tip is L_est + H_keep above the anchor, not with the
/// tip only H_keep above it.
#[test]
fn regular_dkg_is_covered_l_est_plus_h_keep_above_the_anchor() {
    let cfg = mainnet();
    for tip in [ANCHOR + cfg.history_blocks(), ANCHOR + cfg.h_keep()] {
        let (o, n) = ends(&cfg, tip);
        let mut engine = holding(&cfg, tip, &regular(&cfg, o), &regular(&cfg, n));
        let covered = coverage(&mut engine, &cfg, ANCHOR, tip) == Coverage::Covered;
        assert_eq!(covered, tip == ANCHOR + cfg.history_blocks(), "{tip}");
        assert!(engine.requests().is_empty());
    }
}

/// Sol r2's witness: at the domain's oldest end, 2,501,506 (L_est above the anchor), one rotated
/// index (5 here) was last mined at cycle 2,499,840 after repeated null rounds; its k = 3 work
/// block is 2,498,968. The tip's set has that index's later round, so only the oldest set needs
/// it, and it still counts. With the base below the anchor its height is unknown; with only the
/// work block below, coverage names it; the anchor 2,450,000 covers it. Each floor has its
/// regular-DKG twin, covered (D52 item 5).
#[test]
fn repeated_null_rounds_of_one_rotated_index() {
    let cfg = mainnet();
    let tip = ANCHOR + cfg.history_blocks();
    let (o, n) = ends(&cfg, tip);
    assert_eq!(o, 2_501_506);
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let late = 2_499_840;
    assert_eq!(late % interval(&cfg, rotated), 0, "a cycle");
    let mut oldest = regular(&cfg, o);
    // Index 5, based five blocks into the cycle: its work blocks follow the cycle start.
    let i = oldest.iter().position(|q| q.quorum_index == Some(5)).expect("the rotated set");
    oldest[i] = commitment(rotated, late + 5, Some(5));
    let work = late - 3 * interval(&cfg, rotated) - WORK_DIFF_DEPTH;
    assert_eq!(work, 2_498_968);
    for (floor, expected) in [
        (
            ANCHOR,
            Coverage::Below {
                lowest: None,
            },
        ),
        (
            work + 1,
            Coverage::Below {
                lowest: Some(work),
            },
        ),
        (work, Coverage::Covered),
        (2_450_000, Coverage::Covered),
    ] {
        // Its regular-DKG twin: the same tip and floor, Core's regular sets, covered (D52 item 5).
        let mut control = holding(&cfg, tip, &regular(&cfg, o), &regular(&cfg, n));
        assert_eq!(coverage(&mut control, &cfg, floor, tip), Coverage::Covered, "control {floor}");
        let mut engine = holding(&cfg, tip, &oldest, &regular(&cfg, n));
        assert_eq!(coverage(&mut engine, &cfg, floor, tip), expected, "{floor}");
    }
}

/// A very old surviving index whose base predates the anchor: uncovered with no known height,
/// so the fallback steps back a checkpoint. Below its base, the work block is named. Each floor
/// has its regular-DKG twin, covered (D52 item 5).
#[test]
fn a_very_old_surviving_index_has_no_known_height() {
    let cfg = mainnet();
    let tip = ANCHOR + cfg.history_blocks();
    let (o, n) = ends(&cfg, tip);
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let old = 2_400_192;
    assert_eq!(old % interval(&cfg, rotated), 0, "a cycle");
    let mut newest = regular(&cfg, n);
    let i = newest.iter().position(|q| q.llmq_type == rotated).expect("the rotated set");
    newest[i] = commitment(rotated, old, Some(0));
    let work = old - 3 * interval(&cfg, rotated) - WORK_DIFF_DEPTH;
    for (floor, expected) in [
        (
            2_450_000,
            Coverage::Below {
                lowest: None,
            },
        ),
        (
            old,
            Coverage::Below {
                lowest: Some(work),
            },
        ),
        (work, Coverage::Covered),
    ] {
        // Its regular-DKG twin: the same tip and floor, Core's regular sets, covered (D52 item 5).
        let mut control = holding(&cfg, tip, &regular(&cfg, o), &regular(&cfg, n));
        assert_eq!(coverage(&mut control, &cfg, floor, tip), Coverage::Covered, "control {floor}");
        let mut engine = holding(&cfg, tip, &regular(&cfg, o), &newest);
        assert_eq!(coverage(&mut engine, &cfg, floor, tip), expected, "{floor}");
    }
}

/// Non-rotated, A successes with null gaps: the CL type's four last mined commitments are every
/// other cycle, so the fourth's work block lies deeper than A·C + W + 16 below the tip, and it
/// is required. Each floor has its regular-DKG twin, covered (D52 item 5).
#[test]
fn non_rotated_successes_with_null_gaps_are_required() {
    let cfg = mainnet();
    let tip = ANCHOR + cfg.history_blocks();
    let (o, n) = ends(&cfg, tip);
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let p = lp(&cfg, cl);
    let c = interval(&cfg, cl);
    let a = u32::from(p.signing_active_quorum_count);
    let newest_cl = newest_cycle(&cfg, cl, n);
    let mut newest: Vec<Commitment> =
        regular(&cfg, n).into_iter().filter(|q| q.llmq_type != cl).collect();
    newest.extend((0..a).map(|k| commitment(cl, newest_cl - 2 * k * c, None)));
    let work = newest_cl - 2 * (a - 1) * c - WORK_DIFF_DEPTH;
    let reach = a * c + u32::from(p.dkg_mining_window_end) + 16;
    assert!(tip - work > reach, "deeper than regular DKG's estimate");
    for (floor, expected) in [
        (
            work + 1,
            Coverage::Below {
                lowest: Some(work),
            },
        ),
        (work, Coverage::Covered),
    ] {
        // Its regular-DKG twin: the same tip and floor, Core's regular sets, covered (D52 item 5).
        let mut control = holding(&cfg, tip, &regular(&cfg, o), &regular(&cfg, n));
        assert_eq!(coverage(&mut control, &cfg, floor, tip), Coverage::Covered, "control {floor}");
        let mut engine = holding(&cfg, tip, &regular(&cfg, o), &newest);
        assert_eq!(coverage(&mut engine, &cfg, floor, tip), expected, "{floor}");
    }
}

/// A `Skipped` type's deep history does not count (mainnet's MNHF type); a used role sharing the
/// MNHF type's number does (testnet's MNHF type is its CL type).
#[test]
fn a_skipped_types_history_does_not_count_and_a_used_role_sharing_its_number_does() {
    for (network, counted) in [(Network::Main, false), (Network::Testnet3, true)] {
        let cfg = config(network, &[]);
        let mnhf = cfg.params().consensus.llmq_type_mnhf;
        let tip = ANCHOR + cfg.history_blocks();
        let (o, n) = ends(&cfg, tip);
        let mut newest = regular(&cfg, n);
        newest.push(commitment(mnhf, ANCHOR - 1_000, None));
        let mut engine = holding(&cfg, tip, &regular(&cfg, o), &newest);
        let expected = if counted {
            Coverage::Below {
                lowest: None,
            }
        } else {
            Coverage::Covered
        };
        assert_eq!(coverage(&mut engine, &cfg, ANCHOR, tip), expected, "{network:?}");
    }
}

/// Sol r1's witness: anchor 2,500,000 with the tip 100 above it, and work blocks 2,499,832 and
/// 2,498,104 required (cycle 2,499,840's k = 0, cycle 2,498,976's k = 3): uncovered, the oldest
/// end itself below the anchor. The anchor 2,450,000 holds both blocks; one block higher, the
/// deeper is named.
#[test]
fn sol_r1s_witness_is_uncovered_until_the_previous_anchor() {
    let cfg = mainnet();
    let tip = ANCHOR + 100;
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let c = interval(&cfg, rotated);
    let witness =
        [commitment(rotated, 2_499_840, Some(0)), commitment(rotated, 2_498_976, Some(0))];
    assert_eq!(2_499_840 - WORK_DIFF_DEPTH, 2_499_832);
    assert_eq!(2_498_976 - 3 * c - WORK_DIFF_DEPTH, 2_498_104);
    let (o, _) = ends(&cfg, tip);
    for (floor, expected) in [
        (
            ANCHOR,
            Coverage::Below {
                lowest: Some(o),
            },
        ),
        (
            2_498_105,
            Coverage::Below {
                lowest: Some(2_498_104),
            },
        ),
        (2_450_000, Coverage::Covered),
    ] {
        let mut engine = holding(&cfg, tip, &witness, &witness);
        assert_eq!(coverage(&mut engine, &cfg, floor, tip), expected, "{floor}");
    }
}

/// An end coverage does not hold is asked for through `set_at` (DEC-374): from the nearest
/// authenticated held list below it, a null base only when none is held. Asking twice lists it
/// once; one whose fetch failed is asked again.
#[test]
fn a_coverage_request_diffs_from_the_nearest_held_list() {
    let cfg = mainnet();
    let tip = ANCHOR + cfg.history_blocks();
    let chain = Synth {
        floor: ANCHOR,
        tip,
    };
    let (o, n) = ends(&cfg, tip);
    let ask = |base: Option<u32>, target: u32| MnRequest::MnListDiff {
        base: base.map(hash_of),
        target: hash_of(target),
        target_height: target,
    };

    let mut engine = MasternodeListEngine::new(Mode::Client);
    assert_eq!(engine.coverage(&chain, &cfg), Ok(Coverage::Pending));
    assert_eq!(engine.requests(), [ask(None, o), ask(None, n)]);

    let below = o - 80;
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(below), below, hash_of(below), &[]);
    assert_eq!(
        engine.set_at(BlockId(o), &chain, &cfg),
        Ok(SetAt::Fetch {
            base: Some(hash_of(below)),
            target: hash_of(o),
            target_height: o,
        })
    );
    for _ in 0..2 {
        assert_eq!(engine.coverage(&chain, &cfg), Ok(Coverage::Pending));
    }
    assert_eq!(engine.requests(), [ask(Some(below), o), ask(Some(below), n)]);

    engine.hold_set(BlockId(o), o, hash_of(o), &regular(&cfg, o));
    let newest = ask(Some(o), n);
    engine.request_failed(newest.key());
    engine.request_failed(ask(Some(below), n).key());
    assert_eq!(engine.coverage(&chain, &cfg), Ok(Coverage::Pending));
    assert!(engine.requests().contains(&newest), "asked again from the newer base");

    engine.hold_set(BlockId(n), n, hash_of(n), &regular(&cfg, n));
    assert_eq!(engine.coverage(&chain, &cfg), Ok(Coverage::Covered));
}

/// SB0 §5.2.1's classification before any set is read: below DIP3 no quorum, below A_q no root,
/// below tip − H_keep or off the active chain outside the horizon; none of them fetched.
#[test]
fn set_at_classifies_without_a_fetch_outside_the_domain() {
    let cfg = config(Network::Regtest, &[(ChainOption::TestActivationHeight, "dip0008@500")]);
    assert_eq!((cfg.dip3_height(), cfg.a_q(), cfg.h_keep()), (432, 500, 56));
    let tip = 700;
    let chain = Synth {
        floor: 0,
        tip,
    };
    let engine = MasternodeListEngine::new(Mode::Client);
    let at = |h: u32| engine.set_at(BlockId(h), &chain, &cfg).expect("no fault");
    assert_eq!(at(0), SetAt::NoQuorum);
    assert_eq!(at(431), SetAt::NoQuorum);
    assert_eq!(at(432), SetAt::NoRoot);
    assert_eq!(at(499), SetAt::NoRoot);
    assert_eq!(at(tip - cfg.h_keep() - 1), SetAt::OutsideHorizon);
    assert!(matches!(
        at(tip - cfg.h_keep()),
        SetAt::Fetch {
            base: None,
            ..
        }
    ));
    assert!(matches!(at(tip), SetAt::Fetch { .. }));
    assert!(engine.requests().is_empty());
}

/// A ChainLock whose selection start lies below DIP3 (or below 0) gets `NoQuorum`; one in
/// [DIP3, A_q) is unavailable; neither asks for anything.
#[test]
fn locks_outside_the_domain_ask_for_nothing() {
    let cfg = config(Network::Regtest, &[(ChainOption::TestActivationHeight, "dip0008@500")]);
    let (template, _) = clsigs().into_iter().next().expect("a clsig");
    let lock = |height: u32| {
        let mut cl = template.clone();
        cl.height = i32::try_from(height).expect("small");
        cl.block_hash = hash_of(height);
        cl
    };
    let mut engine = MasternodeListEngine::new(Mode::Client);
    for tip in [0, 7, 8, 20] {
        let chain = Synth {
            floor: 0,
            tip,
        };
        assert_eq!(
            engine.verify_chainlock(&lock(tip), &chain, &cfg),
            Err(LockOutcome::Rejected(LockReject::NoQuorum)),
            "{tip}"
        );
    }
    let chain = Synth {
        floor: 0,
        tip: 470,
    };
    assert_eq!(
        engine.verify_chainlock(&lock(460), &chain, &cfg),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
    assert!(engine.requests().is_empty());
}

/// A start inside the domain whose scan is stored below it (D13, D24): with default regtest
/// (DIP3 = A_q = 432) a ChainLock at 448 selects at 440 and stores at 426, below DIP3, so its set
/// is empty and nothing is asked for; with A_q at 500, one at 516 selects at 508 and stores at
/// 498, in [DIP3, A_q): the set at A_q gives the type's entries, so it is asked for there. Held
/// unauthenticated it is neither read nor asked for again; held authenticated (empty) the lock
/// gets `NoQuorum` with no further request. A store block exactly at DIP3 is read.
#[test]
fn a_scan_stored_below_the_domain_reads_at_most_down_to_a_q() {
    let (template, _) = clsigs().into_iter().next().expect("a clsig");
    let lock = |height: u32| {
        let mut cl = template.clone();
        cl.height = i32::try_from(height).expect("small");
        cl.block_hash = hash_of(height);
        cl
    };
    let regtest = config(Network::Regtest, &[]);
    assert_eq!((regtest.dip3_height(), regtest.a_q()), (432, 432));
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let chain = Synth {
        floor: 0,
        tip: 448,
    };
    assert_eq!(
        engine.verify_chainlock(&lock(448), &chain, &regtest),
        Err(LockOutcome::Rejected(LockReject::NoQuorum))
    );
    assert!(engine.requests().is_empty());

    let cfg = config(Network::Regtest, &[(ChainOption::TestActivationHeight, "dip0008@500")]);
    let chain = Synth {
        floor: 0,
        tip: 516,
    };
    assert_eq!(
        engine.verify_chainlock(&lock(516), &chain, &cfg),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
    assert_eq!(
        engine.requests(),
        [MnRequest::MnListDiff {
            base: None,
            target: hash_of(500),
            target_height: 500,
        }]
    );
    // Held but not authenticated: not read, and not asked for again.
    engine.hold_set(BlockId(500), 500, hash_of(500), &[]);
    engine.forget_authentication(BlockId(500));
    let requests = engine.requests();
    assert_eq!(
        engine.verify_chainlock(&lock(516), &chain, &cfg),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
    assert_eq!(engine.requests(), requests);
    engine.forget_list(BlockId(500));
    engine.hold_set(BlockId(500), 500, hash_of(500), &[]);
    assert_eq!(
        engine.verify_chainlock(&lock(516), &chain, &cfg),
        Err(LockOutcome::Rejected(LockReject::NoQuorum))
    );
    assert_eq!(engine.requests(), requests);

    // DIP3 at 450: a ChainLock at 458 selects at 450, the type 100 mining window's end, so it
    // stores there, at DIP3, and asks for that set.
    let cfg = config(
        Network::Regtest,
        &[
            (ChainOption::Dip3Params, "450:450"),
            (ChainOption::TestActivationHeight, "v20@450"),
            (ChainOption::TestActivationHeight, "mn_rr@450"),
        ],
    );
    assert_eq!((cfg.dip3_height(), cfg.a_q()), (450, 450));
    let chain = Synth {
        floor: 0,
        tip: 458,
    };
    let mut engine = MasternodeListEngine::new(Mode::Client);
    assert_eq!(
        engine.verify_chainlock(&lock(458), &chain, &cfg),
        Err(LockOutcome::Rejected(LockReject::QuorumUnavailable))
    );
    assert_eq!(
        engine.requests(),
        [MnRequest::MnListDiff {
            base: None,
            target: hash_of(450),
            target_height: 450,
        }]
    );
}

/// Bootstrap from genesis (Sol r4 R4-M1) with default regtest (A_q 432, H_keep 56) and devnet
/// (A_q 2, H_keep 80) parameters: an empty domain asks for nothing and is covered; from A_q + 8
/// the domain's ends are asked for, the oldest staying at A_q until it meets tip − H_keep, then
/// sliding. No target is negative or below A_q.
#[test]
fn bootstrap_from_genesis_asks_only_for_the_domains_ends() {
    let targets = |cfg: &LlmqConfig, tip: u32| {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        let c = coverage(&mut engine, cfg, 0, tip);
        let targets: Vec<u32> = engine
            .requests()
            .iter()
            .map(|r| match r {
                MnRequest::MnListDiff {
                    base: None,
                    target_height,
                    ..
                } => *target_height,
                other => panic!("{other:?}"),
            })
            .collect();
        (c, targets)
    };
    let regtest = config(Network::Regtest, &[]);
    let devnet = config(Network::Devnet, &[]);
    assert_eq!((regtest.a_q(), regtest.h_keep()), (432, 56));
    assert_eq!((devnet.a_q(), devnet.h_keep()), (2, 80));
    for tip in [0, 7, 8, 20] {
        assert_eq!(targets(&regtest, tip), (Coverage::Covered, vec![]), "regtest {tip}");
    }
    assert_eq!(targets(&regtest, 450), (Coverage::Pending, vec![432, 442]));
    for cfg in [&regtest, &devnet] {
        let (a_q, h_keep) = (cfg.a_q(), cfg.h_keep());
        for tip in [0, 7] {
            assert_eq!(targets(cfg, tip), (Coverage::Covered, vec![]), "{tip}");
        }
        assert_eq!(targets(cfg, a_q + 7), (Coverage::Covered, vec![]));
        assert_eq!(targets(cfg, a_q + 8), (Coverage::Pending, vec![a_q]));
        for (tip, oldest) in
            [(a_q + h_keep - 1, a_q), (a_q + h_keep, a_q), (a_q + h_keep + 1, a_q + 1)]
        {
            assert_eq!(targets(cfg, tip), (Coverage::Pending, vec![oldest, tip - 8]), "{tip}");
        }
    }
}

/// Devnet's first rotated cycle: the work blocks of its three previous cycles lie at negative
/// heights, so they are dropped, never counted and never asked for.
#[test]
fn the_first_rotated_cycles_work_blocks_below_genesis_are_dropped() {
    let cfg = config(Network::Devnet, &[]);
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let first = interval(&cfg, rotated);
    let tip = first + cfg.h_keep() + 20;
    let set = cycle_of(&cfg, first);
    let mut engine = holding(&cfg, tip, &set, &set);
    // A floor at 1 tells a dropped work block from one clipped to genesis.
    assert!(first > WORK_DIFF_DEPTH, "the k = 0 work block is above the floor");
    assert_eq!(coverage(&mut engine, &cfg, 1, tip), Coverage::Covered);
    assert!(engine.requests().is_empty());
}

/// A non-rotated commitment needs its work block, the base less 8, only once V20 is active after
/// its base (`GetAllQuorumMembers`, rc.3:715-722): based at V20 − 2 it needs nothing below the
/// base; at V20 − 1 its work block is named.
#[test]
fn a_non_rotated_work_block_is_required_from_v20() {
    let cfg = mainnet();
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let v20 = cfg.params().consensus.deployment_height(BuriedDeployment::V20).to_u32();
    for (base, expected) in [
        (v20 - 2, Coverage::Covered),
        (
            v20 - 1,
            Coverage::Below {
                lowest: Some(v20 - 1 - WORK_DIFF_DEPTH),
            },
        ),
    ] {
        let tip = base + cfg.h_keep() + 8;
        let set = [commitment(cl, base, None)];
        let mut engine = holding(&cfg, tip, &set, &set);
        assert_eq!(coverage(&mut engine, &cfg, base, tip), expected, "{base}");
    }
}

/// Context entries cover nothing (SB0 §4.3.1): a work block held only as an anchor pack's context
/// entry is uncovered at its height, and covered once the anchor is at or below it. So is a
/// domain end there.
#[test]
fn context_entries_below_the_anchor_cover_nothing() {
    let cfg = mainnet();
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let base = ANCHOR;
    let work = base - WORK_DIFF_DEPTH;
    let tip = base + cfg.h_keep() + 8;
    let set = [commitment(cl, base, None)];
    let packed = |anchor: u32| {
        Packed(Synth {
            floor: anchor - 24,
            tip,
        })
    };
    for (anchor, expected) in [
        (work, Coverage::Covered),
        (
            work + 1,
            Coverage::Below {
                lowest: Some(work),
            },
        ),
    ] {
        let mut engine = holding(&cfg, tip, &set, &set);
        assert_eq!(engine.coverage(&packed(anchor), &cfg), Ok(expected), "{anchor}");
    }
    // The oldest end not held, the newest's set empty: only the end itself can be named.
    let (oldest, newest) = ends(&cfg, tip);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(newest), newest, hash_of(newest), &[]);
    assert_eq!(
        engine.coverage(&packed(oldest + 1), &cfg),
        Ok(Coverage::Below {
            lowest: Some(oldest)
        })
    );
    assert!(engine.requests().is_empty(), "an end below the anchor is not asked for");
}

/// A later miss (D26) is raised for a set within the horizon holding the commitment: one held
/// only one block below tip − H_keep raises nothing, though a set within the horizon not holding
/// it is held, and is pruned with its row (D30, D31); once the set at tip − H_keep holds it, it
/// is raised, naming the anchor.
#[test]
fn a_miss_is_raised_only_for_a_set_holding_it_within_the_horizon() {
    let cfg = mainnet();
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let tip = ANCHOR + cfg.history_blocks();
    let chain = Synth {
        floor: ANCHOR,
        tip,
    };
    let lost = commitment(cl, ANCHOR - 1000, None);
    let horizon = tip - cfg.h_keep();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(
        BlockId(horizon - 1),
        horizon - 1,
        hash_of(horizon - 1),
        std::slice::from_ref(&lost),
    );
    engine.hold_set(BlockId(tip - 8), tip - 8, hash_of(tip - 8), &[]);
    let change = engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(change.events, []);
    assert!(!engine.has_authenticated_set(BlockId(horizon - 1)));
    assert!(engine.quorum(cl, &lost.quorum_hash).is_none());
    engine.hold_set(BlockId(horizon), horizon, hash_of(horizon), std::slice::from_ref(&lost));
    let change = engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(
        change.events,
        [MnEvent::CoverageMiss {
            llmq_type: cl,
            quorum_hash: lost.quorum_hash,
            anchor: ANCHOR,
            missing: None,
        }]
    );
}

/// Sol's slice 2 r1 case (DEC-377): a miss raised at tip 2,501,826 for its holder at 2,501,506,
/// once while it lasts, ends when the tip leaves the holder below the horizon (2,502,147), and is
/// raised again when the tip comes back. The same at the horizon's edge: still held at
/// holder + H_keep, not one block higher.
#[test]
fn a_miss_whose_holder_leaves_and_reenters_the_horizon_is_raised_again() {
    let cfg = mainnet();
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let lost = commitment(cl, ANCHOR - 1000, None);
    let holder = 2_501_506;
    let (low, high) = (2_501_826, 2_502_147);
    assert!(low <= holder + cfg.h_keep() && holder + cfg.h_keep() < high);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(holder), holder, hash_of(holder), std::slice::from_ref(&lost));
    let mut raised = |tip: u32| {
        let chain = Synth {
            floor: ANCHOR,
            tip,
        };
        engine.tip_changed(&chain, &cfg).expect("step 5").events.len()
    };
    assert_eq!(raised(low), 1);
    assert_eq!(raised(low), 0, "one event while it lasts");
    assert_eq!(raised(high), 0, "the holder below the horizon");
    assert_eq!(raised(low), 1, "raised again once the holder is back within the horizon");
    assert_eq!(raised(low), 0);
    let edge = holder + cfg.h_keep();
    assert_eq!(raised(edge), 0, "still held at the horizon");
    assert_eq!(raised(edge + 1), 0);
    assert_eq!(raised(edge), 1);
}

/// PLAN SB11 row item 7 (D26, D52, slice 8): a later miss from a reorg that restores an older
/// commitment. The main branch's set at 2,501,506 holds a commitment based above the anchor; the
/// other branch's block there holds one based 1000 below it. On that branch the older one is
/// `Unavailable` and raised once, naming the chain's anchor; nothing is asked (no fallback below
/// the anchor) and the newer one's verdict stands. Every quorum row is as before once the chain
/// returns, and the store holds the engine's rows at each step.
#[test]
fn a_reorg_restoring_an_older_commitment_is_a_later_miss() {
    let cfg = mainnet();
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let newer = commitment(cl, ANCHOR + 1000, None);
    let older = commitment(cl, ANCHOR - 1000, None);
    let (h, tip) = (2_501_506, 2_501_826);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(h), h, hash_of(h), std::slice::from_ref(&newer));
    engine.hold_set(moved_id(h), h, moved_hash(h), std::slice::from_ref(&older));
    let on = |restored: bool| Moved {
        chain: Synth {
            floor: ANCHOR,
            tip,
        },
        at: restored.then_some(h),
        fault: false,
    };
    let store = Store::new();
    let state =
        |e: &MasternodeListEngine, qc: &Commitment| e.quorum(cl, &qc.quorum_hash).expect("held").1;
    let quorum_rows = |e: &mut MasternodeListEngine| -> Vec<(Vec<u8>, Vec<u8>)> {
        e.persisted().into_iter().filter(|(k, _)| k[0] == b'Q').collect()
    };
    assert_eq!(engine.tip_changed(&on(false), &cfg).expect("step 5").events, []);
    store.commit(&engine.take_dirty());
    let (rows, verdict) = (quorum_rows(&mut engine), state(&engine, &newer));
    let restored = on(true);
    let change = engine.tip_changed(&restored, &cfg).expect("step 5");
    assert_eq!(
        change.events,
        [MnEvent::CoverageMiss {
            llmq_type: cl,
            quorum_hash: older.quorum_hash,
            anchor: MnChain::anchor(&restored),
            missing: None,
        }]
    );
    assert_eq!(state(&engine, &older), QuorumState::Unavailable(Unavailable::UnknownBase));
    assert_eq!(state(&engine, &newer), verdict);
    assert!(engine.requests().is_empty(), "nothing asked: no fallback");
    store.commit(&engine.take_dirty());
    store.assert_holds(&restored, &mut engine);
    assert_eq!(engine.tip_changed(&on(false), &cfg).expect("step 5").events, []);
    assert_eq!(quorum_rows(&mut engine), rows, "every quorum row as before");
    store.commit(&engine.take_dirty());
    store.assert_holds(&on(false), &mut engine);
}

/// A reorg moving the holder's block off the active chain ends a raised miss, and moving back
/// raises it again (D26), the tip unchanged.
#[test]
fn a_miss_whose_holder_leaves_the_active_chain_is_raised_again_on_return() {
    let cfg = mainnet();
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let lost = commitment(cl, ANCHOR - 1000, None);
    let (holder, tip) = (2_501_506, 2_501_826);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(holder), holder, hash_of(holder), std::slice::from_ref(&lost));
    let mut raised = |off: bool| {
        let chain = Moved {
            chain: Synth {
                floor: ANCHOR,
                tip,
            },
            at: off.then_some(holder),
            fault: false,
        };
        engine.tip_changed(&chain, &cfg).expect("step 5").events.len()
    };
    assert_eq!(raised(false), 1);
    assert_eq!(raised(true), 0, "the holder off the active chain");
    assert_eq!(raised(false), 1, "raised again once the holder is back on it");
    assert_eq!(raised(false), 0);
}

/// A store fault while misses are raised raises none of them, so the retry raises each once.
#[test]
fn a_store_fault_while_raising_misses_raises_each_once_on_retry() {
    let cfg = mainnet();
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let (first, second) =
        (commitment(cl, ANCHOR - 1000, None), commitment(cl, ANCHOR - 2000, None));
    let (h1, h2, tip) = (2_501_506, 2_501_510, 2_501_826);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(h1), h1, hash_of(h1), std::slice::from_ref(&first));
    engine.hold_set(BlockId(h2), h2, hash_of(h2), std::slice::from_ref(&second));
    let chain = |fault: bool| Moved {
        chain: Synth {
            floor: ANCHOR,
            tip,
        },
        at: fault.then_some(h2),
        fault,
    };
    // Step 5 reads no ancestry at h2 (the bases are unknown), so the fault is `raise_misses`'s
    // read of the second's holder, after the first's miss is found.
    assert!(engine.tip_changed(&chain(true), &cfg).is_err(), "the fault on the second's holder");
    let events = engine.tip_changed(&chain(false), &cfg).expect("step 5").events;
    let raised: Vec<_> = events
        .iter()
        .map(|e| match e {
            MnEvent::CoverageMiss {
                quorum_hash,
                ..
            } => *quorum_hash,
        })
        .collect();
    assert_eq!(raised, [first.quorum_hash, second.quorum_hash]);
}

/// Sol r3's case (SB0 §5.2.1): the ChainLock for 2,500,155 selects from the set at 2,500,147,
/// before the CL-type mine at 2,500,148. Delivered at tip 2,500,156 that set is fetched; it stays
/// within the horizon until tip 2,500,147 + 320 = 2,500,467, and from 2,500,468 it is outside the
/// horizon, never fetched (DEC-384 keeps H_keep at 320: no lock Core's signer sends needs more).
#[test]
fn sol_r3s_set_stays_within_the_horizon() {
    let cfg = mainnet();
    assert_eq!(cfg.h_keep(), 320);
    let s = 2_500_147;
    let engine = MasternodeListEngine::new(Mode::Client);
    let at = |tip: u32| {
        let chain = Synth {
            floor: ANCHOR - 1_000,
            tip,
        };
        engine.set_at(BlockId(s), &chain, &cfg).expect("no fault")
    };
    for tip in [2_500_156, 2_500_300, 2_500_467] {
        assert!(
            matches!(
                at(tip),
                SetAt::Fetch {
                    target_height: 2_500_147,
                    ..
                }
            ),
            "{tip}"
        );
    }
    assert_eq!(at(2_500_468), SetAt::OutsideHorizon);
    assert!(engine.requests().is_empty());
}

/// D31: a row no held set holds stays while it was replaced at r ≥ tip − R (regtest: R = 76,
/// H_keep = 56). A commitment held at 1100 and replaced at 1101 outlives its set, dropped below
/// tip − H_keep, through tip 1177 = r + R; at 1178 its row goes.
#[test]
fn a_replaced_row_outlives_its_sets_until_tip_minus_r() {
    let cfg = config(Network::Regtest, &[]);
    assert_eq!((cfg.h_keep(), cfg.r()), (56, 76));
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    // Based below the floor: no request keeps a list for it.
    let qc = commitment(cl, 10, None);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1101), 1101, hash_of(1101), &[]);
    let tip_at = |engine: &mut MasternodeListEngine, tip| {
        engine
            .tip_changed(
                &Synth {
                    floor: 1000,
                    tip,
                },
                &cfg,
            )
            .expect("step 5");
    };
    tip_at(&mut engine, 1101);
    tip_at(&mut engine, 1100 + 56 + 1);
    assert!(!engine.has_authenticated_set(BlockId(1100)));
    assert!(engine.holds_row(&qc));
    tip_at(&mut engine, 1101 + 76);
    assert!(engine.holds_row(&qc));
    tip_at(&mut engine, 1101 + 76 + 1);
    assert!(!engine.holds_row(&qc));
}

/// D38 on D12: a verdict whose mining scheme a set decided is reloaded with it while the chain
/// knows that set's block, and reset to be decided again when it does not (a branch it forgot):
/// `Found` with it, `Fetch` without, and the reset row is rewritten.
#[test]
fn a_verdict_decided_on_a_forgotten_block_is_reset_at_load() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, verified) = cores_144();
    let at = |tip| Synth {
        floor: 1000,
        tip,
    };
    let lookup = |e: &MasternodeListEngine| {
        e.lookup_quorum(qc.llmq_type, &qc.quorum_hash, 1110, &at(1130), &cfg).expect("held")
    };
    // The block at 1125 on a branch no header of `at` names.
    let mut forgotten = *hash_of(1125).as_bytes();
    forgotten[31] ^= 1;
    for (decider, known) in [(hash_of(1125), true), (BlockHash::from(forgotten), false)] {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
        engine.hold_set(BlockId(1101), 1101, hash_of(1101), &[]);
        engine.tip_changed(&at(1130), &cfg).expect("step 5");
        engine.set_state(&qc, verified.clone());
        engine.set_decider(&qc, BlockId(1125), 1125, decider);
        assert!(matches!(lookup(&engine), Lookup::Found(_)));
        let store = Store::new();
        store.commit(&engine.take_dirty());
        let mut loaded = store.load(&at(1130)).expect("its rows");
        let w = loaded.take_dirty();
        if known {
            assert!(matches!(lookup(&loaded), Lookup::Found(_)));
            assert!(w.is_empty());
        } else {
            assert_eq!(lookup(&loaded), Lookup::Fetch);
            assert!(w.ops().any(|(k, v)| k[0] == b'Q' && v.is_some()), "the row is rewritten");
        }
    }
}

/// D38, validator m3: `Skipped` follows the configuration, so it is stored as not yet run: a
/// type skipped before a restart and served after it is decided again.
#[test]
fn a_skipped_verdict_is_decided_again_after_a_restart() {
    let cfg = config(Network::Regtest, &[]);
    let unused = (0..=u8::MAX)
        .map(LlmqType::from_base)
        .find(|t| cfg.params().consensus.llmq(*t).is_some() && !cfg.is_used(*t))
        .expect("a type regtest configures and no role uses");
    let qc = commitment(unused, 1090, None);
    let at = Synth {
        floor: 1000,
        tip: 1110,
    };
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    engine.tip_changed(&at, &cfg).expect("step 5");
    let state = |e: &MasternodeListEngine| e.quorum(unused, &qc.quorum_hash).expect("held").1;
    assert_eq!(state(&engine), QuorumState::Skipped);
    let mut restarted = super::persist::reloaded(&mut engine, &at);
    let served = serving(unused);
    restarted.tip_changed(&at, &served).expect("step 5");
    let mut never_skipped = MasternodeListEngine::new(Mode::Client);
    never_skipped.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    never_skipped.tip_changed(&at, &served).expect("step 5");
    assert_ne!(state(&restarted), QuorumState::Skipped);
    assert_eq!(state(&restarted), state(&never_skipped), "decided as if never skipped");
}

/// D38 on D31: restarted after its sets went, the row still goes at 1178, as a delete of its
/// stored row: `replaced` and `held_to` come back from the store, not from the sets.
#[test]
fn a_replaced_rows_expiry_survives_a_restart() {
    let cfg = config(Network::Regtest, &[]);
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let qc = commitment(cl, 10, None);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1101), 1101, hash_of(1101), &[]);
    let at = |tip| Synth {
        floor: 1000,
        tip,
    };
    for tip in [1101, 1100 + 56 + 1] {
        engine.tip_changed(&at(tip), &cfg).expect("step 5");
    }
    assert!(!engine.has_authenticated_set(BlockId(1100)));
    let store = Store::new();
    store.commit(&engine.take_dirty());
    let mut engine = store.load(&at(1157)).expect("its rows");
    engine.tip_changed(&at(1101 + 76), &cfg).expect("step 5");
    assert!(engine.holds_row(&qc));
    store.commit(&engine.take_dirty());
    engine.tip_changed(&at(1101 + 76 + 1), &cfg).expect("step 5");
    assert!(!engine.holds_row(&qc));
    let w = engine.take_dirty();
    assert!(w.ops().any(|(k, v)| k[0] == b'Q' && v.is_none()), "the row is deleted");
    store.commit(&w);
    store.assert_holds(&at(1101 + 76 + 1), &mut engine);
}

/// D31, validator r2 finding 2: across a gap that straddles the horizon (a list kept at 1100, the
/// next set at 1200, tip − H_keep = 1144), a commitment the list holds and the set does not was
/// replaced at the gap's top, 1200, set once: it does not follow the horizon up when the first
/// set above it moves to 1201 (tip 1257). A set kept below on a branch the chain left (1120, not
/// holding it) is not the list the gap starts from.
#[test]
fn a_row_replaced_across_a_gap_over_the_horizon_was_replaced_at_its_top() {
    let cfg = config(Network::Regtest, &[]);
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let qc = commitment(cl, 10, None);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1120), 1120, hash_of(1120), &[]);
    for kept in [1100, 1120] {
        engine.pin_list(BlockId(kept));
    }
    for tip in [1200, 1201, 1257] {
        engine.hold_set(BlockId(tip), tip, hash_of(tip), &[]);
        let chain = Moved {
            chain: Synth {
                floor: 1000,
                tip,
            },
            at: Some(1120),
            fault: false,
        };
        engine.tip_changed(&chain, &cfg).expect("step 5");
        assert_eq!(engine.replaced_at(&qc), Some(1200), "{tip}");
    }
    assert!(!engine.has_authenticated_set(BlockId(1200)));
}

/// D31: a commitment the set below the horizon holds and the first set above does not, which a
/// later set holds again, is not replaced: the walk's mark stands over the gap's (1100 holds it,
/// 1200 does not, 1201 does; tip 1201).
#[test]
fn a_row_held_again_above_a_gap_over_the_horizon_is_not_replaced() {
    let cfg = config(Network::Regtest, &[]);
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let qc = commitment(cl, 10, None);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    for (h, holds) in [(1100, true), (1200, false), (1201, true)] {
        let held: &[Commitment] = if holds {
            std::slice::from_ref(&qc)
        } else {
            &[]
        };
        engine.hold_set(BlockId(h), h, hash_of(h), held);
    }
    let chain = Synth {
        floor: 1000,
        tip: 1201,
    };
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(engine.replaced_at(&qc), None);
}

/// D31, validator r3: the gap starts at a set this call's retention drops (the old tip after more
/// than H_keep blocks offline, no list kept below): its row is replaced at the gap's top and
/// stays through tip − R.
#[test]
fn a_row_replaced_by_a_tip_diff_after_a_long_gap_outlives_its_set() {
    let cfg = config(Network::Regtest, &[]);
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let qc = commitment(cl, 10, None);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    let tip_at = |engine: &mut MasternodeListEngine, tip| {
        engine.hold_set(BlockId(tip), tip, hash_of(tip), &[]);
        let chain = Synth {
            floor: 1000,
            tip,
        };
        engine.tip_changed(&chain, &cfg).expect("step 5");
    };
    tip_at(&mut engine, 1200);
    assert!(!engine.has_authenticated_set(BlockId(1100)));
    assert_eq!(engine.replaced_at(&qc), Some(1200));
    tip_at(&mut engine, 1200 + 76);
    assert!(engine.holds_row(&qc));
    tip_at(&mut engine, 1200 + 77);
    assert!(!engine.holds_row(&qc));
}

/// The heights held whole after one `tip_changed` at `tip`, with a list held at each of `held`
/// (no commitments), the block at `moved` on another branch, and a ChainLock held at `cl`.
fn whole_after(
    cfg: &LlmqConfig,
    held: &[u32],
    tip: u32,
    moved: Option<u32>,
    cl: Option<u32>,
) -> Vec<u32> {
    let mut engine = MasternodeListEngine::new(Mode::Client);
    for h in held {
        engine.hold_set(BlockId(*h), *h, hash_of(*h), &[]);
    }
    if let Some(h) = cl {
        engine.hold_chainlock(h, hash_of(h));
    }
    let chain = Moved {
        chain: Synth {
            floor: ANCHOR,
            tip,
        },
        at: moved,
        fault: false,
    };
    engine.tip_changed(&chain, cfg).expect("step 5");
    engine.whole_lists()
}

/// The rotated type's cycle length C and mining window end, and a cycle start X four cycles
/// above the anchor.
fn rotated_cycle(cfg: &LlmqConfig) -> (u32, u32, u32) {
    let rotated = lp(cfg, cfg.params().consensus.llmq_type_dip0024_instant_send);
    let (c, end) = (u32::from(rotated.dkg_interval), u32::from(rotated.dkg_mining_window_end));
    (c, end, (ANCHOR / c + 4) * c)
}

/// D29 (2) on mainnet: the rotated type's work blocks k = 0…3 of cycle X stay whole while its
/// mining window is open at the newest list, through X + dkgMiningWindowEnd − 1; from its last
/// block X + end on, k = 3 goes and k = 0…2 stay, which the next cycle reads.
#[test]
fn a_rotated_cycles_fourth_list_goes_when_its_window_closes() {
    let cfg = mainnet();
    let (c, end, x) = rotated_cycle(&cfg);
    let k: Vec<u32> = (0..4).map(|i| x - i * c - WORK_DIFF_DEPTH).collect();
    let at = |tip: u32| {
        let held: Vec<u32> = k.iter().copied().chain([tip]).collect();
        whole_after(&cfg, &held, tip, None, None)
    };
    let open = at(x + end - 1);
    assert!(k.iter().all(|h| open.contains(h)), "{open:?}");
    for tip in [x + end, x + end + 1] {
        let closed = at(tip);
        assert!(k[..3].iter().all(|h| closed.contains(h)), "{tip}: {closed:?}");
        assert!(!closed.contains(&k[3]), "{tip}: {closed:?}");
    }
}

/// D29 (2) on mainnet, Platform's `llmq_100_67` (interval 24, mining window end 18): two cycles'
/// windows overlap. With the newest list at X + 17 the work blocks of cycles X and X + 24 are
/// whole; from X + 18 cycle X's window has closed and its work block goes.
#[test]
fn two_open_platform_cycles_keep_both_work_blocks() {
    let cfg = mainnet();
    let platform = lp(&cfg, cfg.params().consensus.llmq_type_platform);
    let (c, end) = (u32::from(platform.dkg_interval), u32::from(platform.dkg_mining_window_end));
    assert_eq!((c, end), (24, 18));
    // No other type's work block: X mod 288 is neither 0 nor 24.
    let x = (ANCHOR / 288 + 4) * 288 + 4 * c;
    let (older, newer) = (x - WORK_DIFF_DEPTH, x + c - WORK_DIFF_DEPTH);
    let at = |tip: u32| whole_after(&cfg, &[older, newer, tip], tip, None, None);
    assert_eq!(at(x + end - 1), [older, newer, x + end - 1]);
    assert_eq!(at(x + end), [newer, x + end]);
}

/// D29 (1): the newest list on the active chain is kept whole, not a newer one on a branch the
/// chain left; with a ChainLock held between them (D29 (4), DEC-387), the newest list at or below
/// it on its block's chain too. The heights are no type's work block.
#[test]
fn the_tip_and_chainlock_lists_are_the_newest_on_their_chains() {
    let cfg = mainnet();
    let y = ANCHOR + 1000;
    let (a, cl, b, tip) = (y, y + 2, y + 4, y + 6);
    assert_eq!(whole_after(&cfg, &[a, b], tip, Some(b), None), [a]);
    assert_eq!(whole_after(&cfg, &[a, b], tip, None, None), [b]);
    assert_eq!(whole_after(&cfg, &[a, b], tip, None, Some(cl)), [a, b]);
}

/// D31: `replaced` reads the active chain's authenticated sets in height order: a commitment a
/// later set holds again is not replaced, and a set on a branch the chain left replaces nothing.
#[test]
fn replaced_reads_the_active_chains_sets_in_order() {
    let cfg = config(Network::Regtest, &[]);
    let cl = cfg.params().consensus.llmq_type_chain_locks;
    let qc = commitment(cl, 10, None);
    let replaced = |sets: &[(u32, bool)], moved: Option<u32>| {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        for (h, holds) in sets {
            let held: &[Commitment] = if *holds {
                std::slice::from_ref(&qc)
            } else {
                &[]
            };
            engine.hold_set(BlockId(*h), *h, hash_of(*h), held);
        }
        let chain = Moved {
            chain: Synth {
                floor: 1000,
                tip: 1105,
            },
            at: moved,
            fault: false,
        };
        engine.tip_changed(&chain, &cfg).expect("step 5");
        engine.replaced_at(&qc)
    };
    assert_eq!(replaced(&[(1100, true), (1101, false)], None), Some(1101));
    assert_eq!(replaced(&[(1100, true), (1101, false), (1102, true)], None), None);
    assert_eq!(replaced(&[(1100, true), (1101, false)], Some(1101)), None);
}

/// D29 (3), validator r1 M1: a disputed commitment's lists stay whole after its window closes, as
/// a waiting one's do: a new row reopens it, and its `qrinfo` was answered (D20). One whose input
/// was not served lets its fourth list go.
#[test]
fn a_disputed_commitments_lists_outlive_its_window() {
    let cfg = mainnet();
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let (c, end, x) = rotated_cycle(&cfg);
    let k3 = x - 3 * c - WORK_DIFF_DEPTH;
    let tip = x + end + 1;
    let qc = commitment(rotated, x, Some(0));
    let whole = |state: Unavailable| {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        engine.hold_set(BlockId(k3), k3, hash_of(k3), &[]);
        engine.hold_set(BlockId(tip), tip, hash_of(tip), std::slice::from_ref(&qc));
        engine.set_state(&qc, QuorumState::Unavailable(state));
        let chain = Synth {
            floor: ANCHOR,
            tip,
        };
        engine.tip_changed(&chain, &cfg).expect("step 5");
        engine.whole_lists()
    };
    assert_eq!(whole(Unavailable::RowsDisputed), [k3, tip]);
    assert_eq!(whole(Unavailable::NotServed), [tip]);
}

/// D30, validator r2 finding 1: a cycle's `qrinfo` answered while its lists were held is asked
/// again once one of them goes, not taken as `NotServed`: a commitment step 5 reads after (a
/// branch the chain switched to mined it, or a reorg reopens one) needs it again. In a call
/// where none goes, the mark stays.
#[test]
fn a_cycles_qrinfo_is_asked_again_once_its_lists_went() {
    let cfg = mainnet();
    let (c, end, x) = rotated_cycle(&cfg);
    let k3 = x - 3 * c - WORK_DIFF_DEPTH;
    let key = RequestKey::QrInfo(hash_of(x));
    let answered = |tip: u32| {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        engine.hold_set(BlockId(k3), k3, hash_of(k3), &[]);
        engine.hold_set(BlockId(tip), tip, hash_of(tip), &[]);
        engine.mark_answered(key);
        let chain = Synth {
            floor: ANCHOR,
            tip,
        };
        engine.tip_changed(&chain, &cfg).expect("step 5");
        (engine.whole_lists().len(), engine.is_answered(key))
    };
    assert_eq!(answered(x + end - 1), (2, true));
    assert_eq!(answered(x + end), (1, false));
}

/// Core's CL commitment for 144 and its state after the step diffs: verified.
fn cores_144() -> (Arc<Commitment>, QuorumState) {
    let q = core_quorums(CHAIN_LOCKS).into_iter().find(|q| q.height == 144).expect("Core's");
    let capture = FixtureChain::full();
    stepped(&capture).quorum(llmq(CHAIN_LOCKS), &QuorumHash::from(q.hash)).expect("held")
}

/// `qc` with another vvec hash: the commitment of a rival branch for the same quorum.
fn rival(qc: &Commitment) -> Commitment {
    let mut other = qc.clone();
    other.quorum_vvec_hash = QuorumVvecHash::from([7; 32]);
    other
}

/// The sets of `qc` a test holds at a height: it, or none.
fn held(qc: &Commitment, present: bool) -> &[Commitment] {
    if present {
        std::slice::from_ref(qc)
    } else {
        &[]
    }
}

/// `engine`'s lookup of `qc`'s quorum at `h` on `chain`.
fn lookup_on(
    engine: &MasternodeListEngine,
    qc: &Commitment,
    h: u32,
    chain: &dyn MnChain,
    cfg: &LlmqConfig,
) -> Result<Lookup, MnReject> {
    engine.lookup_quorum(qc.llmq_type, &qc.quorum_hash, h, chain, cfg)
}

/// D36: a row answers SB0 §5.3's predicate at its edges (regtest W_P = 38). Core's CL commitment
/// for 144, verified, first held at 1100 and replaced at 1101, is found from 1100 through
/// 1101 + W_P − 1 = 1138 (Sol r6's witness on a row) and not at 1139; not at 1099 (first held
/// above it, it may be older: the row decides nothing, and DEC-116 keeps not found for other
/// types); `Unavailable` above the header tip, where headers may yet reach h (DEC-444, Opus s6
/// r2 M1); `Fetch` while step 5 waits; `Unavailable` while its input cannot be had (DEC-444); not
/// found once `Unverified`.
#[test]
fn a_row_answers_the_lookup_predicate_at_its_edges() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, verified) = cores_144();
    let hash = qc.quorum_hash;
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1101), 1101, hash_of(1101), &[]);
    let at = |tip| Synth {
        floor: 1000,
        tip,
    };
    engine.tip_changed(&at(1150), &cfg).expect("step 5");
    assert_eq!(engine.replaced_at(&qc), Some(1101));
    engine.set_state(&qc, verified);
    let lookup = |engine: &MasternodeListEngine, h, tip| {
        engine.lookup_quorum(qc.llmq_type, &hash, h, &at(tip), &cfg).expect("held")
    };
    for h in [1100, 1138] {
        assert!(matches!(lookup(&engine, h, 1150), Lookup::Found(_)), "{h}");
    }
    for h in [1099, 1139] {
        assert_eq!(lookup(&engine, h, 1150), Lookup::NotFound, "{h}");
    }
    assert!(matches!(lookup(&engine, 1120, 1120), Lookup::Found(_)));
    assert_eq!(lookup(&engine, 1121, 1120), Lookup::Unavailable, "above the tip");
    engine.set_state(&qc, QuorumState::Pending(RequestKey::MnListDiff(hash_of(1092))));
    assert_eq!(lookup(&engine, 1138, 1150), Lookup::Fetch);
    // Replaced by h − W_P: no step 5 changes the answer.
    assert_eq!(lookup(&engine, 1139, 1150), Lookup::NotFound);
    engine.set_state(&qc, QuorumState::Unavailable(Unavailable::FetchFailed));
    assert_eq!(lookup(&engine, 1138, 1150), Lookup::Unavailable, "DEC-444");
    assert_eq!(lookup(&engine, 1139, 1150), Lookup::NotFound, "replaced: decided");
    engine.set_state(&qc, QuorumState::Unverified);
    assert_eq!(lookup(&engine, 1138, 1150), Lookup::NotFound);
}

/// D36, validator r5 M1: a commitment for the same quorum on a branch the chain left (its set at
/// 1100, verified) answers nothing; the active chain's (at 1105, `Unverified`) decides. And with
/// the active chain's sets ending at 1105, a lookup at 1450 has no set in h − W_P ..= h and no
/// replaced row: not found (m2), where any held row would once have answered. A verdict whose
/// mining scheme the left branch decided gives no key (m4): step 5 runs it again, so `Fetch`
/// (validator r6 m5).
#[test]
fn a_lookup_reads_the_active_chain_only() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, verified) = cores_144();
    let other = rival(&qc);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&other));
    engine.hold_set(BlockId(1105), 1105, hash_of(1105), std::slice::from_ref(&qc));
    engine.set_state(&other, verified.clone());
    engine.set_state(&qc, QuorumState::Unverified);
    let lookup = |engine: &MasternodeListEngine, h, tip| {
        lookup_on(engine, &qc, h, &reorged(tip, true), &cfg).expect("held")
    };
    assert_eq!(lookup(&engine, 1108, 1110), Lookup::NotFound);
    engine.set_state(&qc, verified);
    assert!(matches!(lookup(&engine, 1108, 1110), Lookup::Found(_)));
    assert_eq!(lookup(&engine, 1450, 1500), Lookup::NotFound);
    // Its scheme decided by the branch the chain left (D12): no key until step 5 runs again.
    engine.set_decider(&qc, BlockId(1100), 1100, hash_of(1100));
    assert_eq!(lookup(&engine, 1108, 1110), Lookup::Fetch);
}

/// D36, SB0 §5.3 (R6-M3) on the rows: after its sets are pruned, a commitment held at 1100 and
/// replaced at 1101 is found at r + W_P − 1 = 1138 from its row alone and not at 1139.
#[test]
fn a_pruned_rows_replacement_answers_the_lookup() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, verified) = cores_144();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    let at = |tip| Synth {
        floor: 1000,
        tip,
    };
    for tip in [1101, 1160] {
        engine.hold_set(BlockId(tip), tip, hash_of(tip), &[]);
        engine.tip_changed(&at(tip), &cfg).expect("step 5");
    }
    assert!(!engine.has_authenticated_set(BlockId(1100)));
    assert_eq!(engine.replaced_at(&qc), Some(1101));
    engine.set_state(&qc, verified);
    let lookup = |h| engine.lookup_quorum(qc.llmq_type, &qc.quorum_hash, h, &at(1160), &cfg);
    assert!(matches!(lookup(1138).expect("held"), Lookup::Found(_)));
    assert_eq!(lookup(1139).expect("held"), Lookup::NotFound);
}

/// D38 on D36: after a restart a pruned row and its verdict still answer, at 1138 and not 1139,
/// as before it; and a verdict step 5 was waiting on, stored as none, is `Fetch` again while its
/// set is held.
#[test]
fn a_rows_answers_survive_a_restart() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, verified) = cores_144();
    let at = |tip| Synth {
        floor: 1000,
        tip,
    };
    let lookup = |e: &MasternodeListEngine, h, tip| {
        e.lookup_quorum(qc.llmq_type, &qc.quorum_hash, h, &at(tip), &cfg).expect("held")
    };
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    for tip in [1101, 1160] {
        engine.hold_set(BlockId(tip), tip, hash_of(tip), &[]);
        engine.tip_changed(&at(tip), &cfg).expect("step 5");
    }
    engine.set_state(&qc, verified);
    let restarted = super::persist::reloaded(&mut engine, &at(1160));
    for h in 1095..=1165 {
        assert_eq!(lookup(&restarted, h, 1160), lookup(&engine, h, 1160), "{h}");
    }
    assert!(matches!(lookup(&restarted, 1138, 1160), Lookup::Found(_)));
    assert_eq!(lookup(&restarted, 1139, 1160), Lookup::NotFound);

    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1101), 1101, hash_of(1101), &[]);
    engine.tip_changed(&at(1150), &cfg).expect("step 5");
    engine.set_state(&qc, QuorumState::Pending(RequestKey::MnListDiff(hash_of(1092))));
    assert_eq!(lookup(&engine, 1138, 1150), Lookup::Fetch);
    let restarted = super::persist::reloaded(&mut engine, &at(1150));
    assert_eq!(lookup(&restarted, 1138, 1150), Lookup::Fetch);
    assert_eq!(lookup(&restarted, 1139, 1150), Lookup::NotFound);
}

/// SB0 §5.3 R6-M3 after pruning and a restart, through the fetch path (PLAN SB11 row, D52): the
/// served commitment held at 1100 and replaced at 1101 is pruned with its sets (tip 1300, below
/// tip − R) and the engine restarts. At r + W_P − 1 = 1138 the lookup fetches the active chain's
/// blocks at 1100 and 1138 and is found; at r + W_P = 1139 it fetches 1101 and 1139, neither
/// holding it, and is not found.
#[test]
fn a_pruned_replacement_is_fetched_after_a_restart() {
    let (qc, verified, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1101), 1101, hash_of(1101), &[]);
    engine.tip_changed(&reorged(1150, false), &cfg).expect("step 5");
    assert_eq!(engine.replaced_at(&qc), Some(1101));
    engine.set_state(&qc, verified.clone());
    engine.hold_set(BlockId(1300), 1300, hash_of(1300), &[]);
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert!(!engine.holds_row(&qc), "pruned");
    assert!(!engine.has_authenticated_set(BlockId(1100)));
    let mut engine = super::persist::reloaded(&mut engine, &chain);
    for (h, found) in [(1138, true), (1139, false)] {
        let low = h - cfg.w_p();
        assert_eq!(lookup_on(&engine, &qc, h, &chain, &cfg).expect("held"), Lookup::Fetch, "{h}");
        fetch(&mut engine, &qc, h, &chain, &cfg);
        assert_eq!(asked_heights(&engine), [low, h], "{h}");
        engine.hold_set(BlockId(low), low, hash_of(low), held(&qc, found));
        engine.hold_set(BlockId(h), h, hash_of(h), &[]);
        if found {
            engine.set_state(&qc, verified.clone());
        }
        for at in [low, h] {
            engine.answer_request(RequestKey::MnListDiff(hash_of(at)));
        }
        engine.tip_changed(&chain, &cfg).expect("step 5");
        let answer = lookup_on(&engine, &qc, h, &chain, &cfg).expect("held");
        if found {
            assert!(matches!(answer, Lookup::Found(_)), "{h}: {answer:?}");
        } else {
            assert_eq!(answer, Lookup::NotFound, "{h}");
        }
    }
}

/// The chain at `tip`: through the block at 1100 (`left` false) or through the branch replacing
/// it (`Moved`'s block `1100 | 1 << 31`).
fn reorged(tip: u32, left: bool) -> Moved {
    Moved {
        chain: Synth {
            floor: 1000,
            tip,
        },
        at: left.then_some(1100),
        fault: false,
    }
}

/// D36, validator r6 M1′: a commitment a branch the chain left held at 1100, replaced at 1101
/// there, is unmarked once the chain leaves it, and goes with its set: after pruning (tip 1160),
/// the lookup at 1120 reads the active chain's commitment for the quorum, or with none, nothing,
/// where both rows once answered as survivors. Before retention reads the new branch, its marks
/// answer nothing either: its mining set left the chain (r7 M1″).
#[test]
fn a_pruned_row_of_a_branch_the_chain_left_answers_nothing() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, verified) = cores_144();
    let other = rival(&qc);
    for mined_on_b in [true, false] {
        let mut engine = MasternodeListEngine::new(Mode::Client);
        engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
        engine.hold_set(BlockId(1101), 1101, hash_of(1101), &[]);
        engine.tip_changed(&reorged(1110, false), &cfg).expect("step 5");
        assert_eq!(engine.replaced_at(&qc), Some(1101));
        let b = BlockId(1100 | 1 << 31);
        engine.hold_set(b, 1100, hash_of(b.0), held(&other, mined_on_b));
        engine.set_state(&qc, verified.clone());
        let stale = lookup_on(&engine, &qc, 1105, &reorged(1112, true), &cfg);
        assert!(!matches!(stale.expect("held"), Lookup::Found(_)));
        engine.tip_changed(&reorged(1110, true), &cfg).expect("step 5");
        assert_eq!(engine.replaced_at(&qc), None);
        engine.hold_set(BlockId(1160), 1160, hash_of(1160), &[]);
        engine.tip_changed(&reorged(1160, true), &cfg).expect("step 5");
        assert!(!engine.holds_row(&qc));
        if mined_on_b {
            engine.set_state(&other, verified.clone());
            assert_eq!(engine.replaced_at(&other), Some(1101));
        }
        let lookup = lookup_on(&engine, &qc, 1120, &reorged(1160, true), &cfg).expect("held");
        if mined_on_b {
            assert!(matches!(lookup, Lookup::Found(_)));
        } else {
            assert_eq!(lookup, Lookup::NotFound);
        }
    }
}

/// D36, validator r6 M2′: a commitment mined at 1100 on a branch the chain left and at 1110 on
/// the active chain, replaced there at 1130, was mined at 1110 for the lookup once its sets are
/// pruned (tip 1180): not found at 1105, found at 1110.
#[test]
fn a_pruned_rows_mining_height_is_the_active_chains() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, verified) = cores_144();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1101), 1101, hash_of(1101), &[]);
    engine.tip_changed(&reorged(1101, false), &cfg).expect("step 5");
    engine.hold_set(BlockId(1110), 1110, hash_of(1110), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1130), 1130, hash_of(1130), &[]);
    engine.tip_changed(&reorged(1130, true), &cfg).expect("step 5");
    engine.hold_set(BlockId(1180), 1180, hash_of(1180), &[]);
    engine.tip_changed(&reorged(1180, true), &cfg).expect("step 5");
    assert!(!engine.has_authenticated_set(BlockId(1110)));
    assert_eq!(engine.replaced_at(&qc), Some(1130));
    engine.set_state(&qc, verified);
    let lookup = |h| lookup_on(&engine, &qc, h, &reorged(1180, true), &cfg);
    assert_eq!(lookup(1105).expect("held"), Lookup::NotFound);
    assert!(matches!(lookup(1110).expect("held"), Lookup::Found(_)));
}

/// D36, validator r6 m6: a row a set at or above the horizon still holds answers a lookup whose
/// window lies below every held set from its marks. Held at 1100 and 1110, replaced at 1111, at
/// tip 1160 (horizon 1104) the set at 1100 is pruned and 1110's is not: found at 1101. At tip
/// 1167 the lowest set read holds it (1110): mined 1100 as marked before, still found at 1101.
#[test]
fn a_row_still_held_answers_a_lookup_below_its_sets() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, verified) = cores_144();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    for (height, in_set) in [(1100, true), (1110, true), (1111, false), (1160, false)] {
        engine.hold_set(BlockId(height), height, hash_of(height), held(&qc, in_set));
    }
    engine.tip_changed(&reorged(1160, false), &cfg).expect("step 5");
    assert!(!engine.has_authenticated_set(BlockId(1100)));
    assert!(engine.has_authenticated_set(BlockId(1110)));
    engine.set_state(&qc, verified);
    let lookup = |h| lookup_on(&engine, &qc, h, &reorged(1160, false), &cfg);
    assert!(matches!(lookup(1101).expect("held"), Lookup::Found(_)));
    assert_eq!(lookup(1099).expect("held"), Lookup::NotFound);
    engine.hold_set(BlockId(1167), 1167, hash_of(1167), &[]);
    engine.tip_changed(&reorged(1167, false), &cfg).expect("step 5");
    assert!(!engine.has_authenticated_set(BlockId(1110)));
    let lookup = |h| lookup_on(&engine, &qc, h, &reorged(1167, false), &cfg);
    assert!(matches!(lookup(1101).expect("held"), Lookup::Found(_)));
}

/// D31, validator r7 m7: a mining height marked on a branch the chain left is not kept as the
/// lower one. Marked at 1100 on the left branch, then held at 1110 on the active chain as its
/// lowest set read, the commitment was mined at 1110.
#[test]
fn a_left_branchs_mining_height_is_not_kept() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, _) = cores_144();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    engine.tip_changed(&reorged(1100, false), &cfg).expect("step 5");
    assert_eq!(engine.mined_at(&qc), Some(1100));
    engine.hold_set(BlockId(1110), 1110, hash_of(1110), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1111), 1111, hash_of(1111), &[]);
    engine.tip_changed(&reorged(1111, true), &cfg).expect("step 5");
    assert_eq!(engine.mined_at(&qc), Some(1110));
}

/// Regtest with `llmq_type` as the served (Platform) type: the capture mined no quorum of
/// Platform's own by its tip (D36), and the CL type's commitment outlives W_P as Platform's does.
fn serving(llmq_type: LlmqType) -> LlmqConfig {
    let mut params =
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("params");
    params.consensus.llmq_type_platform = llmq_type;
    LlmqConfig::new(Arc::new(params)).expect("it outlives W_P")
}

/// The capture's CL commitment for 144 under a config serving its type, on the chain at `tip`.
fn served(tip: u32) -> (Arc<Commitment>, QuorumState, LlmqConfig, Moved) {
    let (qc, verified) = cores_144();
    let cfg = serving(qc.llmq_type);
    (qc, verified, cfg, reorged(tip, false))
}

/// The served lookup at `h`'s fetch.
fn fetch(
    engine: &mut MasternodeListEngine,
    qc: &Commitment,
    h: u32,
    chain: &Moved,
    cfg: &LlmqConfig,
) {
    engine.fetch_lookup(qc.llmq_type, &qc.quorum_hash, h, chain, cfg).expect("held");
}

/// PLAN SB11 row item 16, DEC-116 (D52, slice 8): a type not served, after its sets are pruned,
/// reads not found where no row answers and asks nothing, with its row (tip 1160, which still
/// finds it at 1120) and with the row gone too (tip 1300, where 1120, at which it was active,
/// reads not found). The served type's control at the same state fetches.
#[test]
fn a_type_not_served_after_pruning_is_not_found_and_asks_nothing() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, verified) = cores_144();
    assert!(!cfg.roles(qc.llmq_type).platform, "the CL type is not regtest's Platform type");
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    for tip in [1101, 1160] {
        engine.hold_set(BlockId(tip), tip, hash_of(tip), &[]);
        engine.tip_changed(&reorged(tip, false), &cfg).expect("step 5");
    }
    engine.set_state(&qc, verified);
    assert!(!engine.has_authenticated_set(BlockId(1100)), "pruned");
    for tip in [1160, 1300] {
        let chain = reorged(tip, false);
        if tip == 1300 {
            engine.hold_set(BlockId(tip), tip, hash_of(tip), &[]);
            engine.tip_changed(&chain, &cfg).expect("retention");
            assert!(!engine.holds_row(&qc), "the row went too");
        } else {
            assert!(engine.holds_row(&qc));
        }
        for h in [1120, 1139, 1150] {
            let answer = lookup_on(&engine, &qc, h, &chain, &cfg).expect("held");
            if h == 1120 && tip == 1160 {
                assert!(matches!(answer, Lookup::Found(_)), "the row answers: {answer:?}");
            } else {
                assert_eq!(answer, Lookup::NotFound, "{h}, tip {tip}");
            }
            fetch(&mut engine, &qc, h, &chain, &cfg);
            assert!(engine.requests().is_empty(), "no request at {h}, tip {tip}");
            assert_eq!(engine.lookup_waits(), 0);
        }
        // The served type at the same state fetches the window's ends.
        let served = serving(qc.llmq_type);
        assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &served).expect("held"), Lookup::Fetch);
        fetch(&mut engine, &qc, 1150, &chain, &served);
        assert_eq!(asked_heights(&engine), [1112, 1150], "tip {tip}");
        for h in [1112, 1150] {
            engine.request_failed(RequestKey::MnListDiff(hash_of(h)));
        }
        engine.tip_changed(&chain, &served).expect("the wait goes");
    }
}

/// PLAN SB11 row item 13 (D52, slice 8): the same answers from a row before pruning and from
/// the LRU after it. Core's CL commitment for 144, verified, is held in the sets 1100 through
/// 1150 and replaced at 1151 (a lifetime above W_P, as the served types' invariant asks). Its row,
/// read as a type not served decides it (DEC-116; served, a row reads `Fetch` while a target is
/// not held, D50), answers at 1099, 1100, 1120, 1188 and 1189;
/// with the row pruned, each lookup's fetch of h − W_P and h (sets holding it iff in 1100..=1150)
/// leaves the same answer in the LRU, read once retention dropped the fetched sets.
#[test]
fn a_row_and_the_fetched_answers_agree() {
    let (qc, verified) = cores_144();
    let rows = config(Network::Regtest, &[]);
    let served = serving(qc.llmq_type);
    let holds = |h: u32| (1100..=1150).contains(&h);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    for h in [1100, 1150, 1151] {
        engine.hold_set(BlockId(h), h, hash_of(h), held(&qc, holds(h)));
    }
    engine.tip_changed(&reorged(1200, false), &rows).expect("step 5");
    engine.set_state(&qc, verified.clone());
    assert_eq!(engine.replaced_at(&qc), Some(1151));
    assert!(!engine.has_authenticated_set(BlockId(1100)));
    let queries = [1099, 1100, 1120, 1188, 1189];
    let from_row: Vec<Lookup> = queries
        .iter()
        .map(|&h| lookup_on(&engine, &qc, h, &reorged(1200, false), &rows).expect("held"))
        .collect();
    assert!(matches!(from_row[1..4], [Lookup::Found(_), Lookup::Found(_), Lookup::Found(_)]));
    assert_eq!((&from_row[0], &from_row[4]), (&Lookup::NotFound, &Lookup::NotFound));
    // Each query at its own tip, the next one 200 blocks on: past R, so the row a fetched set
    // leaves (it held the commitment at 1100 or 1150) is pruned before the answer is read.
    let mut tip = 1300;
    let retain = |engine: &mut MasternodeListEngine, tip: u32| {
        engine.hold_set(BlockId(tip), tip, hash_of(tip), &[]);
        engine.tip_changed(&reorged(tip, false), &served).expect("retention");
    };
    retain(&mut engine, tip);
    assert!(!engine.holds_row(&qc), "pruned");
    for (&h, row) in queries.iter().zip(&from_row) {
        let chain = reorged(tip, false);
        assert_eq!(
            lookup_on(&engine, &qc, h, &chain, &served).expect("held"),
            Lookup::Fetch,
            "{h}"
        );
        fetch(&mut engine, &qc, h, &chain, &served);
        for target in [h - served.w_p(), h] {
            engine.hold_set(BlockId(target), target, hash_of(target), held(&qc, holds(target)));
            if holds(target) {
                engine.set_state(&qc, verified.clone());
            }
            engine.answer_request(RequestKey::MnListDiff(hash_of(target)));
        }
        engine.tip_changed(&chain, &served).expect("resolved");
        assert_eq!(engine.lookup_waits(), 0);
        tip += 200;
        retain(&mut engine, tip);
        assert!(!engine.holds_row(&qc), "no row at {h}");
        for target in [h - served.w_p(), h] {
            assert!(!engine.has_authenticated_set(BlockId(target)), "{target} went");
        }
        let chain = reorged(tip, false);
        assert_eq!(&lookup_on(&engine, &qc, h, &chain, &served).expect("kept"), row, "at {h}");
    }
}

/// D37: the fetch's replies (the commitment at 1112, verified, and none at 1150) decide the
/// lookup at 1150: found. At tip 1300 the set at 1112 and the row are gone, and the kept answer
/// still finds it; on a chain replacing the block at 1112 it is no answer: `Fetch`.
#[test]
fn a_fetched_lookup_answers_once_its_sets_went() {
    let (qc, verified, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1150), 1150, hash_of(1150), &[]);
    engine.set_state(&qc, verified);
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert!(!engine.has_authenticated_set(BlockId(1112)));
    assert!(!engine.holds_row(&qc));
    assert!(matches!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Found(_)));
    // Decided: the host asking again changes nothing.
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert!(matches!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Found(_)));
    let moved = Moved {
        at: Some(1112),
        ..reorged(1300, false)
    };
    assert_eq!(lookup_on(&engine, &qc, 1150, &moved, &cfg).expect("held"), Lookup::Fetch);
}

/// D37: one reply (1112, the commitment) before the other keeps its list whole while the lookup
/// waits, though below the horizon (tip 1300) and not the newest; the second (1150) then decides
/// it.
#[test]
fn a_lookup_waiting_on_one_target_keeps_the_other() {
    let (qc, verified, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1300), 1300, hash_of(1300), &[]);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), std::slice::from_ref(&qc));
    engine.answer_request(RequestKey::MnListDiff(hash_of(1112)));
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert!(engine.whole_lists().contains(&1112), "{:?}", engine.whole_lists());
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Fetch);
    engine.set_state(&qc, verified);
    engine.hold_set(BlockId(1150), 1150, hash_of(1150), &[]);
    engine.answer_request(RequestKey::MnListDiff(hash_of(1150)));
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert!(!engine.whole_lists().contains(&1112), "{:?}", engine.whole_lists());
    assert!(matches!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Found(_)));
}

/// D37, SB0 §5.3: a target below the anchor (1120) is not asked, one at it is. The diff to h
/// shows no commitment, but one may have been active below the anchor: `Unavailable`, not a
/// decided not found (DEC-444, Opus s6 r2 m1); so is h below the anchor.
#[test]
fn a_target_below_the_anchor_is_not_asked() {
    let (qc, _, cfg, _) = served(1200);
    let chain = Moved {
        chain: Synth {
            floor: 1120,
            tip: 1200,
        },
        at: None,
        fault: false,
    };
    let mut engine = MasternodeListEngine::new(Mode::Client);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1150]);
    engine.hold_set(BlockId(1150), 1150, hash_of(1150), &[]);
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Unavailable);
    let before = engine.requests();
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert_eq!(engine.requests(), before, "nothing asked decides it");
    assert_eq!(engine.lookup_waits(), 0);
    // The kept answer, read once its set is gone, decides nothing either (mutant U10).
    engine.forget_list(BlockId(1150));
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("kept"), Lookup::Unavailable);
    // A target at the anchor is asked (h − W_P = 1120); h below it has none and asks nothing
    // (Opus s6 r3 n2: mutants Q2, Q4).
    let mut engine = MasternodeListEngine::new(Mode::Client);
    fetch(&mut engine, &qc, 1158, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1120, 1158]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    assert_eq!(lookup_on(&engine, &qc, 1110, &chain, &cfg).expect("held"), Lookup::Unavailable);
    fetch(&mut engine, &qc, 1110, &chain, &cfg);
    assert!(engine.requests().is_empty());
}

/// D37: 64 fetched answers are kept; the 65th evicts the oldest, asked again as `Fetch`.
#[test]
fn the_fetched_answers_are_bounded() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    for h in 1150..1150 + 65 {
        fetch(&mut engine, &qc, h, &chain, &cfg);
        for key in engine.requests().iter().map(MnRequest::key) {
            engine.request_failed(key);
        }
    }
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Fetch);
    assert_eq!(lookup_on(&engine, &qc, 1151, &chain, &cfg).expect("held"), Lookup::Unavailable);
}

/// D37: a waiting lookup whose target left the active chain goes, and keeps nothing: after a
/// reorg replacing the block at 1112, the reply held there is no longer kept whole.
#[test]
fn a_lookup_whose_target_left_the_chain_goes() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), std::slice::from_ref(&qc));
    engine.answer_request(RequestKey::MnListDiff(hash_of(1112)));
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert!(engine.whole_lists().contains(&1112));
    let moved = Moved {
        at: Some(1112),
        ..reorged(1300, false)
    };
    engine.tip_changed(&moved, &cfg).expect("step 5");
    assert!(!engine.whole_lists().contains(&1112), "{:?}", engine.whole_lists());
}

/// D37, validator D37 r1 m2: the same lookup asked again on a chain replacing a target's block
/// (1112) replaces its wait rather than adding one. The moved block is asked; a bound of one
/// does not refuse it; and the replaced wait, whose target left the chain, is not kept: after a
/// call on the moved chain the lookup still waits, now on the moved block.
#[test]
fn a_lookup_asked_again_on_another_chain_replaces_its_wait() {
    let (qc, _, cfg, chain) = served(1200);
    let mut engine = MasternodeListEngine::new(Mode::Client).with_lookup_waits(NonZeroUsize::MIN);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1112, 1150]);
    let moved = Moved {
        at: Some(1112),
        ..reorged(1200, false)
    };
    fetch(&mut engine, &qc, 1150, &moved, &cfg);
    assert_eq!(asked_heights(&engine), [1112, 1112, 1150]);
    let moved_diff = RequestKey::MnListDiff(moved_hash(1112));
    assert!(engine.requests().iter().any(|r| r.key() == moved_diff));
    assert_eq!(engine.waiting_lookups(), [1150]);
    engine.tip_changed(&moved, &cfg).expect("step 5");
    assert_eq!(engine.waiting_lookups(), [1150], "its targets are the active chain's");
}

/// D37: a target held when its lookup was asked, so not asked, that retention drops before the
/// other target's reply comes (a set only, below the horizon once the tip moves on) is gone. Once
/// that reply is held, or fails (validator s6 M1), the wait goes rather than answering not found,
/// and the host's next call asks for the dropped target.
#[test]
fn a_target_dropped_since_asked_drops_its_wait() {
    for fail in [false, true] {
        let (qc, _, cfg, low) = served(1160);
        let mut engine = MasternodeListEngine::new(Mode::Client);
        engine.hold_set(BlockId(1112), 1112, hash_of(1112), &[]);
        engine.hold_set(BlockId(1160), 1160, hash_of(1160), &[]);
        engine.tip_changed(&low, &cfg).expect("step 5");
        assert!(engine.has_authenticated_set(BlockId(1112)));
        assert!(!engine.whole_lists().contains(&1112), "a set only");
        fetch(&mut engine, &qc, 1150, &low, &cfg);
        assert_eq!(asked_heights(&engine), [1150]);
        let high = reorged(1300, false);
        engine.tip_changed(&high, &cfg).expect("step 5");
        assert!(!engine.has_authenticated_set(BlockId(1112)), "dropped below the horizon");
        assert_eq!(engine.waiting_lookups(), [1150]);
        if fail {
            engine.request_failed(RequestKey::MnListDiff(hash_of(1150)));
        } else {
            engine.hold_set(BlockId(1150), 1150, hash_of(1150), &[]);
            engine.answer_request(RequestKey::MnListDiff(hash_of(1150)));
            engine.tip_changed(&high, &cfg).expect("step 5");
        }
        assert!(engine.waiting_lookups().is_empty(), "failed: {fail}");
        assert_eq!(lookup_on(&engine, &qc, 1150, &high, &cfg).expect("held"), Lookup::Fetch);
        fetch(&mut engine, &qc, 1150, &high, &cfg);
        assert!(asked_heights(&engine).contains(&1112), "{:?}", asked_heights(&engine));
    }
}

/// D37, validator s6 M1: a failed request resolves only the waits that asked it. The wait at
/// 1120 asks nothing (step 5 has the commitment at 1112 open) and the one at 1160 asks 1122 and
/// 1160: 1122 failing leaves both, and 1160 failing then answers 1160 `Unavailable` (DEC-444)
/// while 1120 still waits.
#[test]
fn a_failure_resolves_only_the_waits_that_asked_it() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), std::slice::from_ref(&qc));
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    fetch(&mut engine, &qc, 1160, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1160 - cfg.w_p(), 1160]);
    engine.request_failed(RequestKey::MnListDiff(hash_of(1160 - cfg.w_p())));
    assert_eq!(engine.waiting_lookups(), [1120, 1160]);
    engine.request_failed(RequestKey::MnListDiff(hash_of(1160)));
    assert_eq!(engine.waiting_lookups(), [1120]);
    assert_eq!(lookup_on(&engine, &qc, 1120, &chain, &cfg).expect("held"), Lookup::Fetch);
    assert_eq!(lookup_on(&engine, &qc, 1160, &chain, &cfg).expect("held"), Lookup::Unavailable);
}

/// D36, validator r7 M1″: marks read on a chain the active one left answer nothing until
/// retention runs on it. Held at 1100 and 1160 and not at 1170, a commitment is found at 1150
/// (no set in 1112..=1150) from its row on the chain retention read; on one replacing the block
/// at 1170, retention's tip, it is not.
#[test]
fn a_rows_marks_answer_only_on_the_chain_they_were_read_on() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, verified) = cores_144();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    for (height, holds) in [(1100, true), (1160, true), (1170, false)] {
        engine.hold_set(BlockId(height), height, hash_of(height), held(&qc, holds));
    }
    engine.tip_changed(&reorged(1170, false), &cfg).expect("step 5");
    engine.set_state(&qc, verified);
    let at = |moved: Option<u32>| Moved {
        at: moved,
        ..reorged(1175, false)
    };
    let lookup = |engine: &MasternodeListEngine, moved| {
        lookup_on(engine, &qc, 1150, &at(moved), &cfg).expect("held")
    };
    assert!(matches!(lookup(&engine, None), Lookup::Found(_)));
    assert_eq!(lookup(&engine, Some(1170)), Lookup::Unavailable, "undecided, DEC-444");
    // A row answers as a held set does (DEC-444): undecided states are never not found.
    engine.set_state(&qc, QuorumState::Unavailable(Unavailable::FetchFailed));
    assert_eq!(lookup(&engine, None), Lookup::Unavailable);
    engine.set_state(&qc, QuorumState::Pending(RequestKey::MnListDiff(hash_of(1092))));
    assert_eq!(lookup(&engine, None), Lookup::Fetch);
    engine.set_state(&qc, QuorumState::Unverified);
    assert_eq!(lookup(&engine, None), Lookup::NotFound);
    assert_eq!(lookup(&engine, Some(1170)), Lookup::Unavailable);
    // Retention faulted before it read the marks: as unread (Opus s6 r2 n4).
    engine.set_state(&qc, cores_144().1);
    engine.forget_retained();
    assert_eq!(lookup(&engine, None), Lookup::Unavailable, "unread marks");
}

/// D50's precedence (Opus s6 r2 n1): with the marks unread on the active chain, a window set
/// holding the commitment `Pending` keeps the answer `Fetch`; step 5 will decide it.
#[test]
fn unread_marks_keep_a_sets_fetch() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, _) = cores_144();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    for (height, holds) in [(1100, true), (1160, true), (1170, false)] {
        engine.hold_set(BlockId(height), height, hash_of(height), held(&qc, holds));
    }
    engine.tip_changed(&reorged(1170, false), &cfg).expect("step 5");
    engine.set_state(&qc, QuorumState::Pending(RequestKey::MnListDiff(hash_of(1092))));
    let chain = Moved {
        at: Some(1170),
        ..reorged(1175, false)
    };
    assert_eq!(lookup_on(&engine, &qc, 1160, &chain, &cfg).expect("held"), Lookup::Fetch);
}

/// DEC-444 (Opus s6 r2 n2, m2): a window set holding the commitment for any `Unavailable`
/// reason, or `Skipped` (held, but no key is had), is `Unavailable`, never not found.
#[test]
fn every_undecided_holder_is_unavailable() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, _) = cores_144();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    engine.tip_changed(&reorged(1120, false), &cfg).expect("step 5");
    let chain = reorged(1120, false);
    let reasons = [
        Unavailable::BelowFloor {
            height: 1000,
        },
        Unavailable::FetchFailed,
        Unavailable::NotServed,
        Unavailable::UnknownBase,
        Unavailable::RowsDisputed,
        Unavailable::MiningScheme,
        Unavailable::OffChain,
    ];
    let states = reasons.map(QuorumState::Unavailable).into_iter().chain([QuorumState::Skipped]);
    for state in states {
        engine.set_state(&qc, state.clone());
        assert_eq!(
            lookup_on(&engine, &qc, 1110, &chain, &cfg).expect("held"),
            Lookup::Unavailable,
            "{state:?}"
        );
    }
    engine.set_state(&qc, QuorumState::Unverified);
    assert_eq!(lookup_on(&engine, &qc, 1110, &chain, &cfg).expect("held"), Lookup::NotFound);
}

/// D36: `replaced` bounds a replacement from above only when the sets held are not consecutive:
/// held at 1100 and not at 1150, a commitment may have gone at 1101. A lookup at 1180 (window
/// 1142..=1180) reads the highest set holding it (1100), so it is not found, where `replaced`
/// (1150) would have answered; the served type, undecided, fetches.
#[test]
fn sparse_sets_bound_a_replacement_from_above_only() {
    let cfg = config(Network::Regtest, &[]);
    let (qc, verified) = cores_144();
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1100), 1100, hash_of(1100), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1150), 1150, hash_of(1150), &[]);
    engine.tip_changed(&reorged(1200, false), &cfg).expect("step 5");
    assert_eq!(engine.replaced_at(&qc), Some(1150));
    engine.set_state(&qc, verified);
    let chain = reorged(1200, false);
    assert_eq!(lookup_on(&engine, &qc, 1180, &chain, &cfg).expect("held"), Lookup::NotFound);
    assert!(matches!(lookup_on(&engine, &qc, 1138, &chain, &cfg).expect("held"), Lookup::Found(_)));
    let served = serving(qc.llmq_type);
    assert_eq!(lookup_on(&engine, &qc, 1180, &chain, &served).expect("held"), Lookup::Fetch);
}

/// D37 (SB0 §5.3, R6-M3): with nothing held, the served type's lookup at 1150 is `Fetch`, and its
/// fetch asks for the active chain's blocks at h − W_P = 1112 and 1150; a lookup at 1188 asks
/// for 1188 only, sharing 1150. Another type's is not found (DEC-116) and asks nothing; one above
/// the header tip is `Unavailable` and asks nothing (DEC-444, Opus s6 r2 M1). While step 5 has
/// not decided a commitment a window set holds, it decides alone: nothing is asked (validator D37
/// r1 m3).
#[test]
fn a_served_lookup_fetches_its_two_targets_once() {
    let (qc, _, cfg, chain) = served(1200);
    let other = config(Network::Regtest, &[]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Fetch);
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &other).expect("held"), Lookup::NotFound);
    fetch(&mut engine, &qc, 1150, &chain, &other);
    assert!(engine.requests().is_empty());
    assert_eq!(lookup_on(&engine, &qc, 1201, &chain, &cfg).expect("held"), Lookup::Unavailable);
    fetch(&mut engine, &qc, 1201, &chain, &cfg);
    assert!(engine.requests().is_empty());
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1112, 1150]);
    fetch(&mut engine, &qc, 1188, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1112, 1150, 1188]);

    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1140), 1140, hash_of(1140), std::slice::from_ref(&qc));
    for config in [&cfg, &other] {
        assert_eq!(lookup_on(&engine, &qc, 1150, &chain, config).expect("held"), Lookup::Fetch);
        fetch(&mut engine, &qc, 1150, &chain, config);
        assert!(engine.requests().is_empty());
    }
}

/// D37, SB0 §5.3, validator D37 r1 M1, r2 m2, DEC-444: a failed fetch is `Unavailable` (never
/// `NotFound`: nothing decided it) once no target is asked for (one failing while the other is
/// out still waits), and stays so across calls until the host asks again; so is a target whose
/// reply held no authenticated set.
#[test]
fn a_failed_or_unauthenticated_fetch_is_unavailable_until_asked_again() {
    let (qc, _, cfg, chain) = served(1200);
    let lookup =
        |engine: &MasternodeListEngine| lookup_on(engine, &qc, 1150, &chain, &cfg).expect("held");
    let mut engine = MasternodeListEngine::new(Mode::Client);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    engine.request_failed(RequestKey::MnListDiff(hash_of(1112)));
    assert_eq!(lookup(&engine), Lookup::Fetch);
    engine.request_failed(RequestKey::MnListDiff(hash_of(1150)));
    assert_eq!(lookup(&engine), Lookup::Unavailable);
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(lookup(&engine), Lookup::Unavailable);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1112, 1150]);

    let mut engine = MasternodeListEngine::new(Mode::Client);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1150), 1150, hash_of(1150), &[]);
    engine.forget_authentication(BlockId(1150));
    for height in [1112, 1150] {
        engine.answer_request(RequestKey::MnListDiff(hash_of(height)));
    }
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(lookup(&engine), Lookup::Unavailable);
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(lookup(&engine), Lookup::Unavailable);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert_eq!(lookup(&engine), Lookup::Fetch);
}

/// Validator D37 r1 M1 (a): the lower target failing does not hide the upper one's commitment,
/// verified: found, and kept.
#[test]
fn a_failed_lower_target_leaves_the_upper_ones_answer() {
    let (qc, verified, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1300), 1300, hash_of(1300), &[]);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    engine.request_failed(RequestKey::MnListDiff(hash_of(1112)));
    engine.hold_set(BlockId(1150), 1150, hash_of(1150), std::slice::from_ref(&qc));
    engine.set_state(&qc, verified);
    engine.answer_request(RequestKey::MnListDiff(hash_of(1150)));
    for _ in 0..2 {
        engine.tip_changed(&chain, &cfg).expect("step 5");
        assert!(matches!(
            lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"),
            Lookup::Found(_)
        ));
    }
}

/// Validator D37 r1 M1 (b): a commitment step 5 left undecided at a target (here
/// `Unavailable(UnknownBase)`: its quorum's block is unknown) is `Unavailable` (DEC-444): the
/// host's next fetch asks again, where a decided answer asks nothing.
#[test]
fn an_undecided_target_commitment_is_not_kept_as_not_found() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1300), 1300, hash_of(1300), &[]);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), std::slice::from_ref(&qc));
    engine.hold_set(BlockId(1150), 1150, hash_of(1150), &[]);
    for height in [1112, 1150] {
        engine.answer_request(RequestKey::MnListDiff(hash_of(height)));
    }
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Unavailable);
    assert!(asked_heights(&engine).is_empty());
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1112, 1150]);
}

/// Validator D37 r1 m1: step 5 runs only on the looked-up commitment of a fetched set below the
/// horizon; another one there is never run, and goes with its set, though D31 marks it replaced.
#[test]
fn a_fetched_sets_other_commitments_run_no_step_5() {
    let (qc, _, cfg, chain) = served(1300);
    let mut other = (*qc).clone();
    other.quorum_hash = QuorumHash::from([9; 32]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1300), 1300, hash_of(1300), &[]);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), &[(*qc).clone(), other.clone()]);
    engine.answer_request(RequestKey::MnListDiff(hash_of(1112)));
    engine.tip_changed(&chain, &cfg).expect("step 5");
    let state = |c: &Commitment| engine.quorum(c.llmq_type, &c.quorum_hash).map(|(_, s)| s);
    assert_eq!(state(&other), Some(QuorumState::Unauthenticated));
    assert_ne!(state(&qc), Some(QuorumState::Unauthenticated));
    // The other target fails: the set at 1112, the newest below the horizon, goes, and the
    // replacement it marks keeps no row step 5 never ran on.
    engine.request_failed(RequestKey::MnListDiff(hash_of(1150)));
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert!(!engine.has_authenticated_set(BlockId(1112)));
    assert!(!engine.holds_row(&other));
}

/// Validator D37 r2 m1: a lookup whose window holds a commitment only a fetched set below the
/// horizon holds (here at 1112, fetched for another quorum still waiting on 1150) names it:
/// step 5 runs on it, asking nothing.
#[test]
fn a_lookup_names_a_commitment_another_lookups_fetch_holds() {
    let (qc, _, cfg, chain) = served(1300);
    let mut other = (*qc).clone();
    other.quorum_hash = QuorumHash::from([9; 32]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1300), 1300, hash_of(1300), &[]);
    fetch(&mut engine, &other, 1150, &chain, &cfg);
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), &[(*qc).clone(), other.clone()]);
    engine.answer_request(RequestKey::MnListDiff(hash_of(1112)));
    engine.tip_changed(&chain, &cfg).expect("step 5");
    let state = |engine: &MasternodeListEngine| {
        engine.quorum(qc.llmq_type, &qc.quorum_hash).map(|(_, s)| s)
    };
    assert_eq!(state(&engine), Some(QuorumState::Unauthenticated));
    assert_eq!(lookup_on(&engine, &qc, 1120, &chain, &cfg).expect("held"), Lookup::Fetch);
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1150]);
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_ne!(state(&engine), Some(QuorumState::Unauthenticated));
}

/// Validator D37 r1 m1, Opus s3 r1 nit 4 (mutant M8): a commitment a fetched set below the
/// horizon holds (at 1112, for another quorum still waiting on 1150) that a set above it also
/// holds (at 1300) is not a lookup's only: step 5 runs on it.
#[test]
fn a_commitment_a_recent_set_also_holds_is_stepped() {
    let (qc, _, cfg, chain) = served(1300);
    let mut other = (*qc).clone();
    other.quorum_hash = QuorumHash::from([9; 32]);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1300), 1300, hash_of(1300), std::slice::from_ref(&qc));
    fetch(&mut engine, &other, 1150, &chain, &cfg);
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), &[(*qc).clone(), other.clone()]);
    engine.answer_request(RequestKey::MnListDiff(hash_of(1112)));
    engine.tip_changed(&chain, &cfg).expect("step 5");
    let state = engine.quorum(qc.llmq_type, &qc.quorum_hash).map(|(_, s)| s);
    assert!(state.as_ref().is_some_and(|s| *s != QuorumState::Unauthenticated), "{state:?}");
}

/// D44, Opus s3 r1 m3, DEC-412: up to the bound every served lookup waits on its fetch, and one
/// already waiting asks again however full the waits are.
#[test]
fn lookups_wait_up_to_the_bound() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client)
        .with_lookup_waits(NonZeroUsize::new(3).expect("non-zero"));
    for h in [1150, 1160, 1170] {
        fetch(&mut engine, &qc, h, &chain, &cfg);
    }
    assert_eq!(engine.waiting_lookups(), [1150, 1160, 1170]);
    let requests = engine.requests();
    fetch(&mut engine, &qc, 1160, &chain, &cfg);
    assert_eq!(engine.waiting_lookups(), [1150, 1160, 1170]);
    assert_eq!(engine.requests(), requests);
}

/// D49 (Opus s3 r2 nit 3): the wait count reaches the bound as lookups wait, where a new wait
/// is refused. It falls where a wait resolves: `request_failed` once the wait's last target
/// failed (one failure while the other is out leaves it), or a call holding its replies. The
/// refused lookup then waits. A bound above the 64 answers kept is 64 (validator s6 m2).
#[test]
fn the_wait_count_falls_only_where_waits_resolve() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client)
        .with_lookup_waits(NonZeroUsize::new(2).expect("non-zero"));
    assert_eq!((engine.lookup_waits(), engine.max_lookup_waits()), (0, 2));
    for h in [1150, 1160] {
        fetch(&mut engine, &qc, h, &chain, &cfg);
    }
    assert_eq!(engine.lookup_waits(), engine.max_lookup_waits());
    let busy = engine.fetch_lookup(qc.llmq_type, &qc.quorum_hash, 1170, &chain, &cfg);
    assert_eq!(
        busy,
        Err(FetchLookupError::Busy {
            max: 2
        })
    );
    engine.request_failed(RequestKey::MnListDiff(hash_of(1150 - cfg.w_p())));
    assert_eq!(engine.lookup_waits(), 2, "1150 is still out");
    engine.request_failed(RequestKey::MnListDiff(hash_of(1150)));
    assert_eq!(engine.lookup_waits(), 1);
    fetch(&mut engine, &qc, 1170, &chain, &cfg);
    assert_eq!(engine.waiting_lookups(), [1160, 1170]);
    for h in [1160 - cfg.w_p(), 1160] {
        engine.hold_set(BlockId(h), h, hash_of(h), &[]);
        engine.answer_request(RequestKey::MnListDiff(hash_of(h)));
    }
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(engine.waiting_lookups(), [1170]);
    let wide = MasternodeListEngine::new(Mode::Client)
        .with_lookup_waits(NonZeroUsize::new(100).expect("non-zero"));
    assert_eq!(wide.max_lookup_waits(), 64);
}

/// D37, Sol s6 r1 M1: a wait made while step 5 had its commitment open asked nothing. Once that
/// request fails, the host's next call asks both targets, and their failures are the wait's own:
/// it answers `Unavailable` rather than holding its slot.
#[test]
fn a_pending_wait_asked_again_owns_its_failures() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let step5 = RequestKey::MnListDiff(hash_of(1100));
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), std::slice::from_ref(&qc));
    engine.set_state(&qc, QuorumState::Pending(step5));
    engine.ask(MnRequest::MnListDiff {
        base: None,
        target: hash_of(1100),
        target_height: 1100,
    });
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1100], "a pending wait asks nothing");
    assert_eq!(engine.lookup_waits(), 1, "and counts (Opus s6 r1 n1)");
    engine.request_failed(step5);
    assert_eq!(engine.waiting_lookups(), [1120]);
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1120 - cfg.w_p(), 1120]);
    for h in [1120 - cfg.w_p(), 1120] {
        engine.request_failed(RequestKey::MnListDiff(hash_of(h)));
    }
    assert_eq!(engine.lookup_waits(), 0, "both targets failed");
    assert_eq!(lookup_on(&engine, &qc, 1120, &chain, &cfg).expect("held"), Lookup::Unavailable);
}

/// D37: a target that failed while the other was out and is asked again is waited on again.
/// The other's reply then leaves the wait open, not `Unavailable` (Sol s6 r1 mutant).
#[test]
fn a_failed_target_asked_again_is_waited_on() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    let low = 1150 - cfg.w_p();
    engine.request_failed(RequestKey::MnListDiff(hash_of(low)));
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [low, 1150]);
    engine.hold_set(BlockId(1150), 1150, hash_of(1150), &[]);
    engine.answer_request(RequestKey::MnListDiff(hash_of(1150)));
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(engine.waiting_lookups(), [1150], "{low} is out again");
    engine.hold_set(BlockId(low), low, hash_of(low), &[]);
    engine.answer_request(RequestKey::MnListDiff(hash_of(low)));
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert!(engine.waiting_lookups().is_empty());
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::NotFound);
}

/// D37, Opus s6 r1 m2: a lookup asked again while step 5 has its commitment open asks nothing,
/// and keeps what it asked before: their failures still resolve it.
#[test]
fn a_lookup_asked_again_while_pending_keeps_its_requests() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let low = 1120 - cfg.w_p();
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [low, 1120]);
    let step5 = RequestKey::MnListDiff(hash_of(1100));
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), std::slice::from_ref(&qc));
    engine.set_state(&qc, QuorumState::Pending(step5));
    engine.ask(MnRequest::MnListDiff {
        base: None,
        target: hash_of(1100),
        target_height: 1100,
    });
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [low, 1100, 1120], "pending: nothing new");
    engine.request_failed(RequestKey::MnListDiff(hash_of(low)));
    assert_eq!(engine.lookup_waits(), 1, "1120 is out");
    engine.request_failed(RequestKey::MnListDiff(hash_of(1120)));
    assert_eq!(engine.lookup_waits(), 0);
}

/// D37, Opus s6 r1 m3: a target whose request failed waits again once another lookup asks it.
/// The wait at 1150 lost 1150's request; the one at 1188 asks 1150 (its h − W_P) again, so
/// 1112's reply leaves the first waiting on that request, not answering not found.
#[test]
fn a_target_another_lookup_asks_again_is_waited_on() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    engine.request_failed(RequestKey::MnListDiff(hash_of(1150)));
    assert_eq!(engine.waiting_lookups(), [1150], "1112 is out");
    fetch(&mut engine, &qc, 1150 + cfg.w_p(), &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1150 - cfg.w_p(), 1150, 1150 + cfg.w_p()]);
    let low = 1150 - cfg.w_p();
    engine.hold_set(BlockId(low), low, hash_of(low), &[]);
    engine.answer_request(RequestKey::MnListDiff(hash_of(low)));
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(engine.waiting_lookups(), [1150, 1150 + cfg.w_p()]);
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Fetch);
}

/// DEC-444, Opus s6 r1 m1: with both targets held, a commitment whose step 5 request failed
/// (`Unavailable(FetchFailed)`) is `Unavailable`, not a decided not found: asking again asks
/// nothing, and the pending wait goes at the next call with the answer still undecided.
#[test]
fn a_commitment_step_5_gave_up_on_is_unavailable() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let low = 1120 - cfg.w_p();
    for h in [low, 1120] {
        engine.hold_set(BlockId(h), h, hash_of(h), std::slice::from_ref(&qc));
    }
    let step5 = RequestKey::MnListDiff(hash_of(1100));
    engine.set_state(&qc, QuorumState::Pending(step5));
    engine.ask(MnRequest::MnListDiff {
        base: None,
        target: hash_of(1100),
        target_height: 1100,
    });
    let lookup =
        |engine: &MasternodeListEngine| lookup_on(engine, &qc, 1120, &chain, &cfg).expect("held");
    assert_eq!(lookup(&engine), Lookup::Fetch);
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    assert_eq!(engine.lookup_waits(), 1, "a pending wait");
    engine.request_failed(step5);
    assert_eq!(lookup(&engine), Lookup::Unavailable);
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [] as [u32; 0], "nothing to ask");
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(engine.lookup_waits(), 0);
    assert_eq!(lookup(&engine), Lookup::Unavailable);
}

/// D53 (Opus s7 r1 MAJOR-1, r2 MINOR-1): the set at 1150 held without the commitment (the held
/// answer `NotFound`) and the fetch of 1112 failed: the wait stays, as a held target may decide at
/// the next call, but it is spent, so the lookup reads `Unavailable` and a call asks nothing until
/// the wait is answered at the next call; then the host's retry asks again. Another quorum's
/// lookup at 1150, which no wait names, still reads `Fetch`.
#[test]
fn a_spent_wait_beside_a_held_target_asks_nothing() {
    let (qc, _, cfg, chain) = served(1200);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1150), 1150, hash_of(1150), &[]);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1112]);
    engine.request_failed(RequestKey::MnListDiff(hash_of(1112)));
    assert_eq!(engine.lookup_waits(), 1, "the target at 1150 is held");
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Unavailable);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert!(asked_heights(&engine).is_empty(), "nothing asked again");
    let mut other = (*qc).clone();
    other.quorum_hash = QuorumHash::from([9; 32]);
    assert_eq!(lookup_on(&engine, &other, 1150, &chain, &cfg).expect("held"), Lookup::Fetch);
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(engine.lookup_waits(), 0);
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Unavailable);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1112], "the host's retry asks again");
}

/// BL-390, D51: `fetch_lookup` runs step 5 again on a holder whose own input failed, a window set's
/// (both targets held) or a row's, and asks nothing for one answered without it (`NotServed`), with
/// rows disputed or its scheme decided off the active chain, beside a holder found `Verified`, for
/// a type not served, or when refused at the bound. On these synthetic sets step 5 finds Core's
/// base no header: `UnknownBase` shows it ran; Core's bytes are
/// `testnet::a_platform_lookup_asks_again_for_its_holders_failed_input`.
#[test]
fn a_lookup_runs_step_5_again_on_a_holders_failed_input() {
    let (qc, verified, cfg, chain) = served(1300);
    let failed = QuorumState::Unavailable(Unavailable::FetchFailed);
    let ran = QuorumState::Unavailable(Unavailable::UnknownBase);
    let state = |e: &MasternodeListEngine| e.quorum(qc.llmq_type, &qc.quorum_hash).expect("held").1;
    let mut engine = MasternodeListEngine::new(Mode::Client).with_lookup_waits(NonZeroUsize::MIN);
    let low = 1120 - cfg.w_p();
    let hold_window = |engine: &mut MasternodeListEngine| {
        for h in [low, 1120] {
            engine.hold_set(BlockId(h), h, hash_of(h), std::slice::from_ref(&qc));
        }
    };
    hold_window(&mut engine);
    for kept in [Unavailable::NotServed, Unavailable::RowsDisputed] {
        engine.set_state(&qc, QuorumState::Unavailable(kept));
        fetch(&mut engine, &qc, 1120, &chain, &cfg);
        assert_eq!(state(&engine), QuorumState::Unavailable(kept));
    }
    engine.set_state(&qc, failed.clone());
    let other = config(Network::Regtest, &[]);
    fetch(&mut engine, &qc, 1120, &chain, &other);
    assert_eq!(state(&engine), failed, "not served");
    // Another lookup's wait fills the bound: refused, nothing changes.
    fetch(&mut engine, &qc, 1250, &chain, &cfg);
    assert_eq!(engine.lookup_waits(), 1);
    let busy = engine.fetch_lookup(qc.llmq_type, &qc.quorum_hash, 1120, &chain, &cfg);
    assert!(matches!(busy, Err(FetchLookupError::Busy { .. })), "{busy:?}");
    assert_eq!(state(&engine), failed);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    hold_window(&mut engine);
    // Its scheme decided at a block the active chain does not reach: `Fetch`, so nothing is
    // asked again (validator s7 m1).
    engine.set_state(&qc, failed.clone());
    engine.set_decider(&qc, BlockId(1400), 1400, hash_of(1400));
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    assert_eq!(state(&engine), failed, "decided off the active chain");
    let mut engine = MasternodeListEngine::new(Mode::Client);
    hold_window(&mut engine);
    // A wait for the lookup on a branch the chain left (the target at 1120 is another block there)
    // is no wait on these targets: the active chain's call asks again (Opus s7 r1 O1).
    engine.set_state(&qc, QuorumState::Unavailable(Unavailable::NotServed));
    let left = Moved {
        chain: Synth {
            floor: 1000,
            tip: 1300,
        },
        at: Some(1120),
        fault: false,
    };
    fetch(&mut engine, &qc, 1120, &left, &cfg);
    assert_eq!(engine.lookup_waits(), 1, "a wait on the left branch");
    engine.set_state(&qc, failed.clone());
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    assert_eq!(state(&engine), ran, "a window set's holder");
    // A spent wait on the branch the chain left answers nothing on the active chain, whose
    // target at 1120 is not held: `Fetch` there (D53).
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(low), low, hash_of(low), std::slice::from_ref(&qc));
    engine.set_state(&qc, QuorumState::Unavailable(Unavailable::NotServed));
    fetch(&mut engine, &qc, 1120, &left, &cfg);
    engine.request_failed(RequestKey::MnListDiff(moved_hash(1120)));
    assert_eq!(lookup_on(&engine, &qc, 1120, &left, &cfg).expect("held"), Lookup::Unavailable);
    assert_eq!(lookup_on(&engine, &qc, 1120, &chain, &cfg).expect("held"), Lookup::Fetch);
    // A holder found `Verified` answers: a rival commitment for the quorum read before it (no
    // chain Core accepts holds both) asks nothing again though it failed, and no wait is made.
    let other = rival(&qc);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(low), low, hash_of(low), std::slice::from_ref(&other));
    engine.hold_set(BlockId(1120), 1120, hash_of(1120), std::slice::from_ref(&qc));
    engine.set_state(&other, failed.clone());
    engine.set_state(&qc, verified);
    assert!(matches!(lookup_on(&engine, &qc, 1120, &chain, &cfg), Ok(Lookup::Found(_))));
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    assert_eq!(engine.lookup_waits(), 0);
    engine.forget_list(BlockId(1120));
    assert_eq!(state(&engine), failed, "the rival's, read once 1120 went");
    // A row: held at 1100 and 1160, not at 1170, looked up at 1150 (window 1112..=1150).
    let mut engine = MasternodeListEngine::new(Mode::Client);
    for (height, holds) in [(1100, true), (1160, true), (1170, false)] {
        engine.hold_set(BlockId(height), height, hash_of(height), held(&qc, holds));
    }
    engine.tip_changed(&reorged(1170, false), &cfg).expect("step 5");
    engine.set_state(&qc, failed.clone());
    fetch(&mut engine, &qc, 1150, &reorged(1170, false), &cfg);
    assert_eq!(state(&engine), ran, "a row's holder");
}

/// DEC-444, D50: a fetched lookup resolved while a window set between its targets (1130) holds
/// the commitment undecided is kept undecided, though both targets lack it. Retention then drops
/// every set (tip 1300), and the kept answer still reads `Unavailable`, never a decided not
/// found; asking again asks for the targets.
#[test]
fn an_answer_resolved_past_an_undecided_holder_is_unavailable() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(1300), 1300, hash_of(1300), &[]);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    engine.hold_set(BlockId(1130), 1130, hash_of(1130), std::slice::from_ref(&qc));
    for h in [1112, 1150] {
        engine.hold_set(BlockId(h), h, hash_of(h), &[]);
        engine.answer_request(RequestKey::MnListDiff(hash_of(h)));
    }
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert!(engine.waiting_lookups().is_empty());
    assert_eq!(engine.quorum(qc.llmq_type, &qc.quorum_hash), None, "every set went");
    assert_eq!(lookup_on(&engine, &qc, 1150, &chain, &cfg).expect("held"), Lookup::Unavailable);
    fetch(&mut engine, &qc, 1150, &chain, &cfg);
    assert_eq!(asked_heights(&engine), [1112, 1150]);
}

/// DEC-444, D50: a kept decided answer (both targets and the window set at 1270 hold the
/// commitment `Unverified`) reads not found only while nothing held reads the commitment
/// undecided: once the holder's input cannot be had and a target is no longer authenticated, it
/// is `Unavailable`.
#[test]
fn a_kept_not_found_yields_to_an_undecided_holder() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    let (h, low) = (1290, 1290 - cfg.w_p());
    engine.hold_set(BlockId(1300), 1300, hash_of(1300), &[]);
    fetch(&mut engine, &qc, h, &chain, &cfg);
    engine.hold_set(BlockId(1270), 1270, hash_of(1270), std::slice::from_ref(&qc));
    engine.tip_changed(&chain, &cfg).expect("step 5");
    engine.set_state(&qc, QuorumState::Unverified);
    for at in [low, h] {
        engine.hold_set(BlockId(at), at, hash_of(at), &[]);
        engine.answer_request(RequestKey::MnListDiff(hash_of(at)));
    }
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert!(engine.waiting_lookups().is_empty());
    let lookup =
        |engine: &MasternodeListEngine| lookup_on(engine, &qc, h, &chain, &cfg).expect("held");
    assert_eq!(lookup(&engine), Lookup::NotFound);
    engine.forget_authentication(BlockId(h));
    assert_eq!(lookup(&engine), Lookup::NotFound, "kept");
    engine.set_state(&qc, QuorumState::Unavailable(Unavailable::FetchFailed));
    assert_eq!(lookup(&engine), Lookup::Unavailable);
}

/// D44, Opus s3 r2 nit 1: a wait that asks nothing (the commitment at 1112, undecided, in
/// 1120's window) counts toward the bound, and at the bound a lookup that needs no fetch (both
/// targets held, no commitment: a decided `NotFound`) is not refused.
#[test]
fn the_bound_counts_every_wait_and_refuses_no_answer() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client)
        .with_lookup_waits(NonZeroUsize::new(3).expect("non-zero"));
    engine.hold_set(BlockId(1112), 1112, hash_of(1112), std::slice::from_ref(&qc));
    fetch(&mut engine, &qc, 1120, &chain, &cfg);
    assert_eq!(engine.requests(), [], "a pending wait asks nothing");
    for h in [1160, 1170] {
        fetch(&mut engine, &qc, h, &chain, &cfg);
    }
    assert_eq!(engine.waiting_lookups(), [1120, 1160, 1170]);
    assert_eq!(
        engine.fetch_lookup(qc.llmq_type, &qc.quorum_hash, 1180, &chain, &cfg),
        Err(FetchLookupError::Busy {
            max: 3
        })
    );
    let decided = 1250;
    for h in [decided - cfg.w_p(), decided] {
        engine.hold_set(BlockId(h), h, hash_of(h), &[]);
    }
    let lookup = engine.lookup_quorum(qc.llmq_type, &qc.quorum_hash, decided, &chain, &cfg);
    assert_eq!(lookup, Ok(Lookup::NotFound));
    fetch(&mut engine, &qc, decided, &chain, &cfg);
    assert_eq!(engine.waiting_lookups(), [1120, 1160, 1170]);
}

/// D44, Opus s3 r1 m3, DEC-412: a new wait past the bound is refused as busy, asking nothing and
/// dropping no wait (Core's RPC work queue refuses new work, `httpserver.cpp` rc.3:91-101); the
/// lookup still answers `Fetch`, and once a wait resolves it is asked. A refused lookup's
/// undecided answer (its fetch at 1190 failed) is kept: it still reads `Unavailable`.
#[test]
fn a_lookup_past_the_bound_is_refused() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client)
        .with_lookup_waits(NonZeroUsize::new(3).expect("non-zero"));
    let lookup = |engine: &MasternodeListEngine, h| {
        engine.lookup_quorum(qc.llmq_type, &qc.quorum_hash, h, &chain, &cfg)
    };
    fetch(&mut engine, &qc, 1190, &chain, &cfg);
    for r in engine.requests() {
        engine.request_failed(r.key());
    }
    engine.tip_changed(&chain, &cfg).expect("step 5");
    assert_eq!(lookup(&engine, 1190), Ok(Lookup::Unavailable), "undecided");
    for h in [1150, 1160, 1170] {
        fetch(&mut engine, &qc, h, &chain, &cfg);
    }
    let requests = engine.requests();
    for h in [1180, 1190] {
        assert_eq!(
            engine.fetch_lookup(qc.llmq_type, &qc.quorum_hash, h, &chain, &cfg),
            Err(FetchLookupError::Busy {
                max: 3
            })
        );
    }
    assert_eq!(engine.waiting_lookups(), [1150, 1160, 1170]);
    assert_eq!(engine.requests(), requests);
    assert_eq!(lookup(&engine, 1180), Ok(Lookup::Fetch));
    assert_eq!(lookup(&engine, 1190), Ok(Lookup::Unavailable));
    let first = [1150 - cfg.w_p(), 1150];
    for r in engine.requests() {
        if matches!(r, MnRequest::MnListDiff { target_height, .. } if first.contains(&target_height))
        {
            engine.request_failed(r.key());
        }
    }
    engine.tip_changed(&chain, &cfg).expect("step 5");
    fetch(&mut engine, &qc, 1180, &chain, &cfg);
    assert_eq!(engine.waiting_lookups().last(), Some(&1180));
}

/// D47 (validator s4 n5): a waiting lookup's failed request keeps its marks below the bookkeeping
/// floor while the wait lasts, so a late reply to it is not taken for the host's tip diff (D34).
#[test]
fn a_waiting_lookups_failed_request_keeps_its_marks() {
    let (qc, _, cfg, chain) = served(1300);
    let mut engine = MasternodeListEngine::new(Mode::Client);
    fetch(&mut engine, &qc, 1190, &chain, &cfg);
    let asked: Vec<_> = engine.requests().iter().map(MnRequest::key).collect();
    assert_eq!(asked.len(), 2, "both targets");
    engine.request_failed(asked[0]);
    assert_eq!(engine.waiting_lookups(), [1190], "the other target is out");
    engine.forget_bookkeeping_below(&chain, u32::MAX);
    assert_eq!(engine.bookkeeping_of(asked[0]), [false, true, false, false]);
}

/// D47 (validator s4 M1, Opus s4 r1 m2), end to end: a held rotated commitment far below the tip
/// (an index whose DKG keeps failing) keeps its `qrinfo`'s failed, dropped and retry marks
/// through retention beside a recent one, as the floor follows the oldest held base.
#[test]
fn an_old_held_commitments_retries_survive_retention() {
    let cfg = mainnet();
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let c = interval(&cfg, rotated);
    let old = ANCHOR - ANCHOR % c + 2 * c;
    let tip = old + cfg.r() + 3 * cfg.l_est();
    let new = tip - tip % c;
    let chain = Synth {
        floor: ANCHOR,
        tip,
    };
    let (a, b) = (commitment(rotated, old + 5, Some(5)), commitment(rotated, new, Some(0)));
    let mut engine = MasternodeListEngine::new(Mode::Client);
    engine.hold_set(BlockId(tip), tip, hash_of(tip), &[a.clone(), b.clone()]);
    for qc in [&a, &b] {
        engine.set_state(qc, QuorumState::Unverified);
    }
    let key = RequestKey::QrInfo(hash_of(old));
    engine.mark_every_bookkeeping(key, old);
    engine.tip_changed(&chain, &cfg).expect("retention");
    assert_eq!(engine.bookkeeping_of(key)[1..], [true; 3]);
}
