//! `accept` and `accept_header` (SB4 §5, §7.1's acceptance half, §11.4, §12): the Rust halves of
//! A-1, A-3, A-24 and A-28. Headers are mined on regtest, so every hash is X11 and passes
//! `high-hash` unless a test breaks it.

use std::sync::Arc;

use dash_clock::MockClock;
use dash_consensus::BlockValidationResult as R;
use dash_params::{BuriedDeployment, ChainSetup, Network};
use dash_primitives::BlockHeader;

use super::*;
use crate::config::StaticSporks;
use crate::error::LoadError;
use crate::testing::{self, Latch, batch, params};

/// The mock time: well after regtest's genesis (1417713337).
const NOW: i64 = 1_700_000_000;

struct Fixture {
    chain: HeaderChain,
    clock: Arc<MockClock>,
    sporks: Arc<StaticSporks>,
}

fn fixture(cfg: impl FnOnce(&ChainSetup) -> ChainConfig) -> Fixture {
    fixture_with(params(Network::Regtest), cfg)
}

fn fixture_with(p: Arc<ChainSetup>, cfg: impl FnOnce(&ChainSetup) -> ChainConfig) -> Fixture {
    let clock = Arc::new(MockClock::at(NOW));
    let sporks = Arc::new(StaticSporks::for_network(&p, None));
    let svc = Services {
        clock: clock.clone(),
        sporks: sporks.clone(),
        fail: Arc::new(Latch::default()),
    };
    let cfg = cfg(&p);
    let mut chain = HeaderChain::new_genesis(p, cfg, svc, NoStore).expect("a valid configuration");
    if chain.cfg.mode == ChainMode::Node {
        chain.init_active_tip(BlockId(0)).expect("Core LoadChainTip");
    }
    Fixture {
        chain,
        clock,
        sporks,
    }
}

fn node() -> Fixture {
    fixture(ChainConfig::node)
}

fn node_chain() -> HeaderChain {
    node().chain
}

/// `raw` with the first nonce, counting up from its own, whose X11 meets `bits`, or misses it
/// when `!pass`.
fn mine_raw(chain: &HeaderChain, mut raw: BlockHeader, pass: bool) -> Header {
    loop {
        let h = Header::new(raw);
        let ok =
            check_proof_of_work(h.hash().as_bytes(), CompactTarget::new(raw.bits), chain.hp.pow());
        if ok == pass {
            return h;
        }
        raw.nonce += 1;
    }
}

/// A valid header on `prev`: regtest's bits, `time`, a nonce from `salt` (forks differ).
fn mined(chain: &HeaderChain, prev: BlockHash, time: u32, salt: u32) -> Header {
    mine_raw(chain, testing::header(prev, time, salt << 16), true)
}

/// `n` mined headers on `prev`, 150 s apart after `prev`'s time.
fn mined_branch(chain: &HeaderChain, prev: BlockId, n: usize, salt: u32) -> Vec<Header> {
    let (mut hash, mut time) =
        chain.shared.arena.imm(prev, |e| (e.hash, e.header.time)).expect("resident");
    (0..n)
        .map(|_| {
            time += 150;
            let h = mined(chain, hash, time, salt);
            hash = h.hash();
            h
        })
        .collect()
}

fn accept(chain: &mut HeaderChain, headers: &[Header]) -> HeadersResult {
    chain.accept(batch(headers), None).expect("no storage or internal failure")
}

fn id_of(chain: &HeaderChain, h: &Header) -> BlockId {
    chain.reader().lookup(&h.hash()).expect("in the index")
}

fn ids_of(chain: &HeaderChain, hs: &[Header]) -> Vec<BlockId> {
    hs.iter().map(|h| id_of(chain, h)).collect()
}

/// `n` mined headers on genesis, accepted, with their ids.
fn grown(chain: &mut HeaderChain, n: usize) -> (Vec<Header>, Vec<BlockId>) {
    let hs = mined_branch(chain, BlockId(0), n, 1);
    assert_eq!(accept(chain, &hs).stop, None);
    let ids = ids_of(chain, &hs);
    (hs, ids)
}

/// ORs `bits` into `id`'s status.
fn mark(chain: &mut HeaderChain, id: BlockId, bits: u32) {
    chain.set_status(id, |s| Status(s.bits() | bits)).expect("known id");
}

/// Regtest with `trunk[i]` added as the checkpoint at height `h`, for each `(h, i)`.
fn checkpointed(trunk: &[Header], at: &[(u32, usize)]) -> Arc<ChainSetup> {
    let mut p = (*params(Network::Regtest)).clone();
    for &(h, i) in at {
        p.checkpoints.push((h.into(), dash_num::Hash256::from(*trunk[i].hash().as_bytes())));
    }
    Arc::new(p)
}

fn status(chain: &HeaderChain, id: BlockId) -> Status {
    chain.writer_status(id).expect("known id")
}

/// The batch's stop as (index, result, reason).
fn stopped(r: &HeadersResult) -> Option<(usize, R, String)> {
    match &r.stop {
        Some(HeaderStop::Invalid {
            index,
            reject,
        }) => Some((*index, reject.result(), reject.reject_reason().to_owned())),
        Some(HeaderStop::BelowAnchor {
            index,
        }) => Some((*index, R::Unset, "below-anchor".to_owned())),
        None => None,
    }
}

fn one_reject(chain: &mut HeaderChain, h: &Header) -> (R, String) {
    match chain.accept_header(h).expect("no failure").one {
        AcceptOne::Invalid(e) => (e.result(), e.reject_reason().to_owned()),
        other => panic!("accepted: {other:?}"),
    }
}

fn one_accepted(chain: &mut HeaderChain, h: &Header) -> BlockId {
    match chain.accept_header(h).expect("no failure").one {
        AcceptOne::Accepted(id) => id,
        other => panic!("not accepted: {other:?}"),
    }
}

#[test]
fn a_batch_extends_the_index_and_publishes_once() {
    let mut chain = node_chain();
    let epoch = chain.snapshot().epoch;
    let hs = mined_branch(&chain, BlockId(0), 20, 1);
    let r = accept(&mut chain, &hs);
    assert_eq!(r.stop, None);
    let ids = ids_of(&chain, &hs);
    assert_eq!(r.inserted, ids);
    assert_eq!(r.last_accepted, Some(ids[19]));
    assert_eq!(r.floor_moved, None);
    assert_eq!(chain.best_header(), ids[19]);
    // One status publication (R3), same tip_seq, carrying the new best header.
    assert_eq!((r.snap.epoch, r.snap.tip_seq), (epoch + 1, 0));
    assert_eq!(r.snap.tip.best_header, ids[19]);
    let e = chain.reader().entry(ids[19]).expect("entry");
    assert_eq!((e.height, e.status), (20, Status(Status::VALID_TREE)));

    // Resent: every header is known, nothing inserted, nothing published.
    let r = accept(&mut chain, &hs);
    assert_eq!((r.stop, r.inserted.len(), r.last_accepted), (None, 0, Some(ids[19])));
    assert_eq!(r.snap.epoch, epoch + 1);
}

/// Core's genesis short-cut (`:4606`): `AddToBlockIndex` returns the entry and the signal fires.
#[test]
fn the_genesis_header_returns_its_entry() {
    let mut chain = node_chain();
    let g = Header::new(dash_consensus::params::genesis_block(&chain.params).header);
    let r = accept(&mut chain, &[g]);
    assert_eq!((r.stop, r.inserted, r.last_accepted), (None, vec![BlockId(0)], Some(BlockId(0))));
    assert_eq!(chain.next_id, 1);
}

/// `last_accepted` is the last header accepted before the stop; `stop` names the index.
#[test]
fn a_batch_stops_at_the_first_rejection() {
    let mut chain = node_chain();
    let mut hs = mined_branch(&chain, BlockId(0), 5, 1);
    let raw = *hs[3].raw();
    hs[3] = mine_raw(&chain, raw, false);
    let r = accept(&mut chain, &hs);
    assert_eq!(stopped(&r), Some((3, R::InvalidHeader, "high-hash".into())));
    assert_eq!(r.inserted.len(), 3);
    assert_eq!(r.last_accepted, Some(id_of(&chain, &hs[2])));
    assert_eq!(chain.reader().lookup(&hs[4].hash()), None);
}

/// A-1's rule level on a node chain: each contextual rule through acceptance, with Core's
/// result class and reason.
#[test]
fn header_rules_reject_with_cores_strings() {
    let mut chain = node_chain();
    let trunk = mined_branch(&chain, BlockId(0), 12, 1);
    accept(&mut chain, &trunk);
    let tip = trunk[11];
    let tip_time = tip.time();

    let orphan = mined(&chain, BlockHash::from([7; 32]), tip_time + 150, 2);
    assert_eq!(one_reject(&mut chain, &orphan), (R::MissingPrev, "prev-blk-not-found".into()));

    // MTP of the last 11 is trunk[6]'s time.
    let old = mined(&chain, tip.hash(), trunk[6].time(), 2);
    assert_eq!(one_reject(&mut chain, &old), (R::InvalidHeader, "time-too-old".into()));

    let mut raw = testing::header(tip.hash(), tip_time + 150, 3 << 16);
    raw.bits = 0x207f_fffe;
    let diff = mine_raw(&chain, raw, true);
    assert_eq!(one_reject(&mut chain, &diff), (R::InvalidHeader, "bad-diffbits".into()));

    let p = chain.params.clone();
    let cltv = p.consensus.deployment_height(BuriedDeployment::Cltv).to_u32();
    assert!(cltv <= 13, "regtest buries BIP65 early");
    let mut raw = testing::header(tip.hash(), tip_time + 150, 4 << 16);
    raw.version = 3;
    let v3 = mine_raw(&chain, raw, true);
    assert_eq!(one_reject(&mut chain, &v3), (R::InvalidHeader, "bad-version(0x00000003)".into()));

    // Nothing rejected was inserted.
    for h in [orphan, old, diff, v3] {
        assert_eq!(chain.reader().lookup(&h.hash()), None);
    }
}

/// A-3's Rust half: `time-too-new` against `NodeClock::now()` + 7,200, moved by `setmocktime`.
#[test]
fn time_too_new_nodeclock() {
    let Fixture {
        mut chain,
        clock,
        ..
    } = node();
    let genesis = chain.shared.arena.imm(BlockId(0), |e| e.hash).expect("genesis");
    let at = |chain: &HeaderChain, dt: i64, salt| mined(chain, genesis, (NOW + dt) as u32, salt);
    let late = at(&chain, 7_201, 1);
    assert_eq!(one_reject(&mut chain, &late), (R::TimeFuture, "time-too-new".into()));
    let edge = at(&chain, 7_200, 2);
    one_accepted(&mut chain, &edge);
    clock.set_mocktime(NOW + 1);
    one_accepted(&mut chain, &late);
}

/// CH1-A10: `check_header_at` is `ContextualCheckBlockHeader` on `prev` without the insert
/// (`TestBlockValidity`, `validation.cpp:4953`): Core's class and reason for each rule, `now` as
/// `time-too-new`'s reference (`:4509`), the index, its epoch and the best header unchanged, an
/// unknown `prev` refused, and SPV refused (`WrongMode`).
#[test]
fn check_header_at_checks_without_inserting() {
    let mut chain = node_chain();
    let trunk = mined_branch(&chain, BlockId(0), 12, 1);
    accept(&mut chain, &trunk);
    let tip = trunk[11];
    let tip_id = id_of(&chain, &tip);
    let (best, epoch, next) = (chain.best_header(), chain.snapshot().epoch, chain.next_id);
    let check = |chain: &HeaderChain, h: &Header, now| {
        let verdict = chain.check_header_at(tip_id, h, now).expect("no failure");
        verdict.map_err(|e| (e.result(), e.reject_reason().to_owned()))
    };

    let good = mined(&chain, tip.hash(), tip.time() + 150, 2);
    assert_eq!(check(&chain, &good, NOW), Ok(()));
    let old = mined(&chain, tip.hash(), trunk[6].time(), 2);
    assert_eq!(check(&chain, &old, NOW), Err((R::InvalidHeader, "time-too-old".into())));
    let mut raw = testing::header(tip.hash(), tip.time() + 150, 3 << 16);
    raw.bits = 0x207f_fffe;
    let diff = mine_raw(&chain, raw, true);
    assert_eq!(check(&chain, &diff, NOW), Err((R::InvalidHeader, "bad-diffbits".into())));
    // `now` is the reference, not the chain's clock (which stays at NOW).
    let late = mined(&chain, tip.hash(), (NOW + 7_201) as u32, 4);
    assert_eq!(check(&chain, &late, NOW), Err((R::TimeFuture, "time-too-new".into())));
    assert_eq!(check(&chain, &late, NOW + 1), Ok(()));

    for h in [good, old, diff, late] {
        assert_eq!(chain.reader().lookup(&h.hash()), None);
    }
    assert_eq!((chain.best_header(), chain.snapshot().epoch, chain.next_id), (best, epoch, next));
    let unknown = BlockId(chain.next_id + 7);
    assert_eq!(
        chain.check_header_at(unknown, &good, NOW).unwrap_err(),
        ChainError::UnknownBlock(unknown)
    );
    // The accept path still reads the chain's clock: the late header is too new at NOW.
    assert_eq!(one_reject(&mut chain, &late), (R::TimeFuture, "time-too-new".into()));

    let spv = fixture(|p| ChainConfig::spv(p, 10_000)).chain;
    let g = mined(
        &spv,
        Header::new(dash_consensus::params::genesis_block(&spv.params).header).hash(),
        NOW as u32,
        1,
    );
    assert_eq!(
        spv.check_header_at(BlockId(0), &g, NOW).unwrap_err(),
        ChainError::Config(ConfigError::WrongMode("check_header_at"))
    );
}

/// A-24's Rust half: Core's checkpoint list, highest checkpoint in the index, `-checkpoints`.
#[test]
fn checkpoint_rule_core_list() {
    // The trunk is deterministic, so its tenth header can be a checkpoint of a second chain.
    let trunk = mined_branch(&node_chain(), BlockId(0), 12, 1);
    let p = checkpointed(&trunk, &[(10, 9)]);

    for enabled in [true, false] {
        let Fixture {
            mut chain,
            ..
        } = fixture_with(p.clone(), |p| ChainConfig {
            checkpoints_enabled: enabled,
            ..ChainConfig::node(p)
        });
        accept(&mut chain, &trunk[..9]);
        // Regtest lists genesis at 0 (`chainparams.cpp`'s `checkpointData`).
        assert_eq!(chain.highest_checkpoint_in_index(), enabled.then_some(0));
        accept(&mut chain, &trunk[9..]);
        assert_eq!(chain.highest_checkpoint_in_index(), enabled.then_some(10));

        // A fork whose first header is below the checkpoint's height.
        let fork = mined_branch(&chain, id_of(&chain, &trunk[4]), 1, 9);
        let r = accept(&mut chain, &fork);
        match enabled {
            true => assert_eq!(
                stopped(&r),
                Some((0, R::Checkpoint, "bad-fork-prior-to-checkpoint".into()))
            ),
            false => assert_eq!(r.stop, None),
        }
        // At the checkpoint's height it is allowed (`nHeight < pcheckpoint->nHeight`).
        let at = mined_branch(&chain, id_of(&chain, &trunk[8]), 1, 9);
        assert_eq!(accept(&mut chain, &at).stop, None);
    }
}

/// `load` rebuilds the checkpoint cache from the records (§11.4).
#[test]
fn load_rebuilds_the_checkpoint_cache() {
    let trunk = mined_branch(&node_chain(), BlockId(0), 6, 1);
    let p = checkpointed(&trunk, &[(3, 2), (5, 4)]);
    let Fixture {
        mut chain,
        ..
    } = fixture_with(p.clone(), ChainConfig::node);
    accept(&mut chain, &trunk);
    assert_eq!(chain.highest_checkpoint_in_index(), Some(5));
    let records: Vec<_> =
        chain.take_dirty().into_iter().map(|w| Ok::<_, LoadError>((w.hash, w.bytes))).collect();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
    let svc = testing::services(&p);
    let loaded = HeaderChain::<NoStore>::load(
        p.clone(),
        ChainConfig::node(&p),
        svc,
        records.into_iter(),
        &pool,
    )
    .expect("loads");
    assert_eq!(loaded.highest_checkpoint_in_index(), Some(5));
    assert_eq!(loaded.last_checkpoint.map(|(k, _)| k), Some(5));
}

/// A-28: Core's index-state branches in `AcceptBlockHeader`'s order.
#[test]
fn accept_pnbh_index_state() {
    let mut chain = node_chain();
    let (trunk, ids) = grown(&mut chain, 4);

    // Known duplicates: FAILED → cached-invalid, CONFLICT → chainlock, both → FAILED first.
    mark(&mut chain, ids[3], Status::FAILED_VALID);
    assert_eq!(one_reject(&mut chain, &trunk[3]), (R::CachedInvalid, "duplicate".into()));
    mark(&mut chain, ids[3], Status::CONFLICT_CHAINLOCK);
    assert_eq!(one_reject(&mut chain, &trunk[3]), (R::CachedInvalid, "duplicate".into()));
    mark(&mut chain, ids[2], Status::CONFLICT_CHAINLOCK);
    assert_eq!(one_reject(&mut chain, &trunk[2]), (R::ChainLock, "duplicate".into()));

    // A parent both FAILED and CONFLICT: bad-prevblk first. CONFLICT alone: the chainlock one.
    let on3 = mined_branch(&chain, ids[3], 1, 2);
    assert_eq!(one_reject(&mut chain, &on3[0]), (R::InvalidPrev, "bad-prevblk".into()));
    let on2 = mined_branch(&chain, ids[2], 1, 2);
    assert_eq!(one_reject(&mut chain, &on2[0]), (R::ChainLock, "bad-prevblk-chainlock".into()));
}

/// A-28: an invalid parent is rejected at once; a failed ancestor through `m_failed_blocks`
/// marks prev down to the failed block's child, `FAILED_CHILD` and dirty.
#[test]
fn failed_blocks_mark_the_descent_lazily() {
    let mut chain = node_chain();
    let (_, ids) = grown(&mut chain, 6);
    chain.take_dirty();
    mark(&mut chain, ids[1], Status::FAILED_VALID);
    chain.failed_blocks().insert(ids[1]);

    let on5 = mined_branch(&chain, ids[5], 1, 2);
    assert_eq!(one_reject(&mut chain, &on5[0]), (R::InvalidPrev, "bad-prevblk".into()));
    for &id in &ids[2..] {
        assert_eq!(status(&chain, id), Status(Status::VALID_TREE | Status::FAILED_CHILD));
        assert!(chain.dirty.contains(id));
    }
    assert!(!chain.dirty.contains(ids[1]) && !chain.dirty.contains(ids[0]));
    assert_eq!(status(&chain, ids[0]), Status(Status::VALID_TREE));
    // Now the parent itself is failed: the immediate check answers, the same reason.
    assert_eq!(one_reject(&mut chain, &on5[0]), (R::InvalidPrev, "bad-prevblk".into()));
    // A sibling branch off the failed block's parent is untouched.
    let side = mined_branch(&chain, ids[0], 1, 3);
    one_accepted(&mut chain, &side[0]);
}

/// §7.1: a header conflicting with the floor is inserted CONFLICT, never the best header and
/// without the signal; gated by the live spork.
#[test]
fn a_header_conflicting_with_the_floor_is_inserted_conflicting() {
    let Fixture {
        mut chain,
        sporks,
        ..
    } = node();
    let (trunk, ids) = grown(&mut chain, 6);
    chain.cl_floor = Some(chain.block_ref(ids[4]));

    // Spork off (regtest's default): no conflict check.
    let fork = mined_branch(&chain, ids[2], 4, 2);
    assert!(!chain.has_conflicting_chainlock(4, &fork[0].hash()).unwrap());
    assert!(!chain.has_chainlock(5, &trunk[4].hash()).unwrap());
    assert_eq!(accept(&mut chain, &fork[..1]).stop, None);

    sporks.set(true);
    assert!(chain.has_chainlock(5, &trunk[4].hash()).unwrap());
    assert!(chain.has_chainlock(2, &trunk[1].hash()).unwrap());
    assert!(!chain.has_chainlock(6, &trunk[5].hash()).unwrap());
    assert!(chain.has_conflicting_chainlock(5, &fork[1].hash()).unwrap());
    let best = chain.best_header();
    let r = accept(&mut chain, &fork[1..]);
    assert_eq!(stopped(&r), Some((0, R::ChainLock, "bad-chainlock".into())));
    assert!(r.inserted.is_empty());
    let c = id_of(&chain, &fork[1]);
    assert_eq!(status(&chain, c), Status(Status::VALID_TREE | Status::CONFLICT_CHAINLOCK));
    assert!(chain.dirty.contains(c), "AddToBlockIndex dirties it (:121)");
    assert_eq!(chain.best_header(), best);
    // Resent: a known CONFLICT duplicate; its child: the conflicting-parent rule.
    assert_eq!(one_reject(&mut chain, &fork[1]), (R::ChainLock, "duplicate".into()));
    assert_eq!(one_reject(&mut chain, &fork[2]), (R::ChainLock, "bad-prevblk-chainlock".into()));
}

/// §7.1 late header (CH1-A12): the floor moves at the header, also when the batch stops later;
/// not on a height mismatch.
#[test]
fn a_late_header_moves_the_floor() {
    let mut chain = node_chain();
    let mut hs = mined_branch(&chain, BlockId(0), 6, 1);
    chain.cl_best = Some(ChainLockRef {
        height: 3,
        hash: hs[2].hash(),
    });
    let epoch = chain.snapshot().epoch;
    let raw = *hs[4].raw();
    hs[4] = mine_raw(&chain, raw, false);
    let r = accept(&mut chain, &hs);
    let locked = id_of(&chain, &hs[2]);
    assert_eq!(stopped(&r).map(|s| s.0), Some(4));
    assert_eq!((r.floor_moved, chain.cl_floor()), (Some(locked), Some(locked)));
    assert_eq!(r.snap.epoch, epoch + 1);
    assert_eq!(r.snap.cl_floor.map(|b| (b.id, b.height)), Some((locked, 3)));

    // The hash at another height: Core logs and keeps the floor.
    let mut chain = node_chain();
    let hs = mined_branch(&chain, BlockId(0), 3, 1);
    chain.cl_best = Some(ChainLockRef {
        height: 4,
        hash: hs[2].hash(),
    });
    let r = accept(&mut chain, &hs);
    assert_eq!((r.floor_moved, chain.cl_floor()), (None, None));
}

/// `accept_header` publishes nothing; the caller does (O-m9).
#[test]
fn accept_header_does_not_publish() {
    let mut chain = node_chain();
    let epoch = chain.snapshot().epoch;
    let hs = mined_branch(&chain, BlockId(0), 1, 1);
    let one = chain.accept_header(&hs[0]).unwrap();
    assert_eq!(one.one, AcceptOne::Accepted(id_of(&chain, &hs[0])));
    assert_eq!(chain.snapshot().epoch, epoch);
    assert_eq!(chain.accept_header(&hs[0]).unwrap().one, AcceptOne::Known(id_of(&chain, &hs[0])));
}

/// SPV publishes nothing at accept (§9.1 stage 4 does) and refuses a step over B before any
/// work (SB5-A11 (a)).
#[test]
fn spv_accept_publishes_nothing_and_caps_the_step() {
    let Fixture {
        mut chain,
        ..
    } = fixture(|p| ChainConfig::spv(p, 10_000));
    let epoch = chain.snapshot().epoch;
    let hs = mined_branch(&chain, BlockId(0), 3, 1);
    let r = accept(&mut chain, &hs);
    assert_eq!((r.stop, r.inserted.len(), r.snap.epoch), (None, 3, epoch));

    let cap = chain.cfg.feeds.step_headers as usize;
    let over = vec![hs[0]; cap + 1];
    let err = chain.accept(batch(&over), None).unwrap_err();
    assert_eq!(
        err,
        ChainError::Config(ConfigError::StepHeaders {
            headers: cap + 1,
            cap: cap as u32,
        })
    );
}

/// A-9 through `accept`: an SPV rule that reads outside its context (an envelope forced to one
/// entry, while regtest's min-difficulty walk reads back to the interval start) panics in debug
/// builds; release builds return `Internal` with nothing inserted and no blame.
#[test]
#[cfg_attr(debug_assertions, should_panic(expected = "rule read outside its envelope"))]
fn an_spv_rule_read_outside_its_context_is_internal() {
    let Fixture {
        mut chain,
        ..
    } = fixture(|p| ChainConfig::spv(p, 10_000));
    let (_, ids) = grown(&mut chain, 10);
    let next = mined_branch(&chain, ids[9], 1, 1);
    chain.rules.envelope = Some(1);
    let r = chain.accept(batch(&next), None);
    assert_eq!(r.unwrap_err(), ChainError::Internal("rule read outside its envelope"));
    assert_eq!(chain.reader().lookup(&next[0].hash()), None);
}

#[test]
#[cfg_attr(debug_assertions, should_panic(expected = "a batch with 1 headers and 0 hashes"))]
fn a_batch_without_its_hashes_is_internal() {
    let mut chain = node_chain();
    let hs = mined_branch(&chain, BlockId(0), 1, 1);
    let r = chain.accept(
        HeaderBatch {
            headers: hs,
            hashes: vec![],
        },
        None,
    );
    assert!(matches!(r, Err(ChainError::Internal(_))));
}

/// The order of §5.2's steps where two rules both apply.
#[test]
fn checks_run_in_cores_order() {
    let Fixture {
        mut chain,
        sporks,
        ..
    } = node();
    let (trunk, ids) = grown(&mut chain, 6);

    // 2 before 3: an orphan with a high hash is `high-hash`, which carries blame.
    let raw = testing::header(BlockHash::from([7; 32]), trunk[5].time() + 150, 0);
    let orphan = mine_raw(&chain, raw, false);
    assert_eq!(one_reject(&mut chain, &orphan), (R::InvalidHeader, "high-hash".into()));

    mark(&mut chain, ids[1], Status::FAILED_VALID);
    chain.failed_blocks().insert(ids[1]);
    chain.take_dirty();
    // 6 before 7: a contextual failure on a failed block's descendant marks nothing.
    let mut raw = testing::header(trunk[5].hash(), trunk[5].time() + 150, 1 << 16);
    raw.bits = 0x207f_fffe;
    let diff = mine_raw(&chain, raw, true);
    assert_eq!(one_reject(&mut chain, &diff), (R::InvalidHeader, "bad-diffbits".into()));
    assert_eq!(status(&chain, ids[5]), Status(Status::VALID_TREE));
    assert!(chain.take_dirty().is_empty());

    // 7 before 8: a header that also conflicts with the floor is `bad-prevblk`, not inserted.
    chain.cl_floor = Some(chain.block_ref(ids[5]));
    sporks.set(true);
    let on4 = mined_branch(&chain, ids[4], 1, 2);
    assert!(chain.has_conflicting_chainlock(6, &on4[0].hash()).unwrap());
    let before = chain.next_id;
    assert_eq!(one_reject(&mut chain, &on4[0]), (R::InvalidPrev, "bad-prevblk".into()));
    assert_eq!(chain.next_id, before);
}

/// Step 7 runs only when prev is not `VALID_SCRIPTS`, and takes the first failed ancestor in id
/// order (D9).
#[test]
fn the_failed_blocks_loop_is_gated_and_ordered() {
    let mut chain = node_chain();
    let (_, ids) = grown(&mut chain, 6);
    for i in [1, 3] {
        mark(&mut chain, ids[i], Status::FAILED_VALID);
        chain.failed_blocks().insert(ids[i]);
    }
    chain.set_status(ids[5], |_| Status(Status::VALID_SCRIPTS)).unwrap();
    let on5 = mined_branch(&chain, ids[5], 1, 2);
    one_accepted(&mut chain, &on5[0]);

    chain.set_status(ids[5], |_| Status(Status::VALID_TREE)).unwrap();
    let on5 = mined_branch(&chain, ids[5], 1, 3);
    assert_eq!(one_reject(&mut chain, &on5[0]), (R::InvalidPrev, "bad-prevblk".into()));
    // The walk ran to ids[1]'s child, through ids[3].
    let failed_child = |id| status(&chain, id).has(Status::FAILED_CHILD);
    assert!([2, 3, 4, 5].iter().all(|&i| failed_child(ids[i])));
    assert!(!failed_child(ids[1]) && !failed_child(ids[0]));
}

/// The late-header signal is synchronous: a later header in the batch sees the new floor.
#[test]
fn a_header_after_the_late_lock_meets_its_floor() {
    let Fixture {
        mut chain,
        sporks,
        ..
    } = node();
    sporks.set(true);
    let a = mined_branch(&chain, BlockId(0), 3, 1);
    chain.cl_best = Some(ChainLockRef {
        height: 3,
        hash: a[2].hash(),
    });
    accept(&mut chain, &a[..2]);
    let b = mined_branch(&chain, id_of(&chain, &a[1]), 1, 2);
    let r = accept(&mut chain, &[a[2], b[0]]);
    assert_eq!(stopped(&r), Some((1, R::ChainLock, "bad-chainlock".into())));
    assert_eq!(r.floor_moved, Some(id_of(&chain, &a[2])));
    assert_eq!(r.inserted, vec![id_of(&chain, &a[2])]);
}

/// Core's `std::map<int, uint256>` keeps the first entry per height; a hash listed twice
/// answers with its highest key.
#[test]
fn the_checkpoint_map_follows_cores_map() {
    let (h1, h2) = (testing::fake_hash(1), testing::fake_hash(2));
    let mut p = (*params(Network::Regtest)).clone();
    p.checkpoints.clear();
    for (height, h) in [(5u32, h1), (5, h2), (7, h1)] {
        p.checkpoints.push((height.into(), dash_num::Hash256::from(*h.as_bytes())));
    }
    let chain = fixture_with(Arc::new(p), ChainConfig::node).chain;
    assert_eq!(chain.checkpoints.get(&h1), Some(&7));
    assert_eq!(chain.checkpoints.get(&h2), None);
    assert_eq!(chain.checkpoints.len(), 1);
}
