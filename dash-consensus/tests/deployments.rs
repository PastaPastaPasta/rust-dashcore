//! CS4's deployments: Dash's versionbits state machine, `ComputeBlockVersion`, the EHF stage
//! filter and the `getdeploymentinfo` report.
//!
//! `recorded_cases_reproduce_cores_answers` replays Core's answers recorded through the oracle
//! (`data/oracle/versionbits.json`, written by node-l1's `versionbits_record`). The other tests
//! check the port's own invariants on hand-built chains.

#[path = "support/versionbits.rs"]
mod versionbits;
#[path = "support/versionbits_json.rs"]
mod versionbits_json;
#[path = "support/versionbits_rows.rs"]
mod versionbits_rows;

mod common;

use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::deployments::info::{BIP9_ORDER, DeploymentInfo};
use dash_consensus::deployments::{
    Deployments, NoEhfSignals, ThresholdState, VERSIONBITS_TOP_BITS, VersionBitsCache,
};
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_params::DeploymentPos;
use versionbits::{Answer, Chain, Network, Query, Refused, Run, Scenario};

#[test]
fn recorded_cases_reproduce_cores_answers() {
    let cases = common::oracle_cases("versionbits.json");
    assert!(!cases.is_empty());
    let mut differing = Vec::new();
    let mut labels = Vec::new();
    for case in &cases {
        let (label, scenario, core) = versionbits_json::case_from_json(case);
        if scenario.ours().as_ref() != Ok(&core) {
            differing.push(label.clone());
        }
        labels.push((label, scenario));
    }
    assert!(differing.is_empty(), "the node differs from Core on {differing:?}");
    // every row is recorded as it stands (`versionbits_record` re-records an edited one)
    let stale: Vec<String> = versionbits_rows::rows()
        .into_iter()
        .filter(|(label, row)| !labels.iter().any(|(l, s)| l == label && s == row))
        .map(|(label, _)| label)
        .collect();
    assert!(stale.is_empty(), "rows not recorded as they stand: {stale:?}");
}

#[test]
fn every_row_runs_on_the_node() {
    for (label, row) in versionbits_rows::rows() {
        assert!(row.ours().is_ok(), "{label}: {:?}", row.ours().err());
    }
}

/// Regtest with `testdummy` (bit 28) in a 10-block window at threshold 8 and no EHF.
fn regtest_testdummy() -> Scenario {
    Scenario {
        network: Network::Regtest,
        args: vec!["-vbparams=testdummy:0:9223372036854775807:0:10:8".into()],
        first_height: 0,
        headers: Vec::new(),
        signals: Vec::new(),
        queries: Vec::new(),
    }
}

const SIGNAL_28: i32 = VERSIONBITS_TOP_BITS | 1 << 28;

fn run(count: u32, version: i32) -> Run {
    Run {
        count,
        version,
        time: versionbits::BASE_TIME,
        step: 150,
    }
}

fn states(scenario: &Scenario) -> Vec<u8> {
    scenario
        .ours()
        .expect("answers")
        .into_iter()
        .map(|a| match a {
            Answer::State(s) => s,
            other => panic!("{other:?}"),
        })
        .collect()
}

#[test]
fn signalling_chain_moves_through_every_state() {
    let mut s = regtest_testdummy();
    s.headers = vec![run(9, 4), run(40, SIGNAL_28)];
    // the state of the block after each period's last block: 9, 19, 29, 39
    s.queries = [None, Some(8), Some(9), Some(18), Some(19), Some(28), Some(38), Some(48)]
        .map(|h| Query::State(DeploymentPos::TestDummy, h))
        .to_vec();
    let [d, s_, l, a] = [
        ThresholdState::Defined,
        ThresholdState::Started,
        ThresholdState::LockedIn,
        ThresholdState::Active,
    ]
    .map(|t| t as u8);
    // the first period (to 9) is DEFINED; start time 0 starts the second at its boundary; the
    // second (10..19) signals 10 of 10; then locked in for a period, then active
    assert_eq!(states(&s), [d, d, s_, s_, l, l, a, a]);
}

#[test]
fn answers_depend_on_nothing_but_the_query_order_core_uses() {
    let mut s = regtest_testdummy();
    s.headers = vec![run(9, 4), run(120, SIGNAL_28)];
    let heights: Vec<Option<u32>> = (0..129).map(Some).collect();
    s.queries = heights.iter().map(|&h| Query::State(DeploymentPos::TestDummy, h)).collect();
    let ascending = s.ours().expect("answers");
    s.queries.reverse();
    let mut descending = s.ours().expect("answers");
    descending.reverse();
    // no EHF: the signal height is 0 everywhere, so the cache cannot make the order matter
    assert_eq!(ascending, descending);
}

#[test]
fn compute_block_version_sets_started_and_locked_in_bits() {
    let mut s = regtest_testdummy();
    s.headers = vec![run(9, 4), run(30, SIGNAL_28)];
    s.queries = [Some(8), Some(9), Some(19), Some(29)].map(Query::ComputeBlockVersion).to_vec();
    let versions = s.ours().expect("answers");
    let v = |x: i32| Answer::ComputeBlockVersion(x);
    // defined after 8; started after 9 and locked in after 19 signal the bit; active after 29
    // does not
    assert_eq!(
        versions,
        [v(VERSIONBITS_TOP_BITS), v(SIGNAL_28), v(SIGNAL_28), v(VERSIONBITS_TOP_BITS)]
    );
}

#[test]
fn statistics_count_the_period_so_far() {
    let mut s = regtest_testdummy();
    s.headers = vec![run(9, 4), run(3, SIGNAL_28), run(2, VERSIONBITS_TOP_BITS)];
    s.queries = vec![Query::Statistics(DeploymentPos::TestDummy, Some(13))];
    let [
        Answer::Statistics {
            period,
            threshold,
            elapsed,
            count,
            possible,
            blocks,
        },
    ] = &s.ours().expect("answers")[..]
    else {
        panic!("one statistics answer")
    };
    // the period 10..19 so far: 10 and 11 signal, 12 and 13 do not
    assert_eq!((*period, *threshold, *elapsed, *count, *possible), (10, 8, 4, 2, true));
    assert_eq!(blocks, &[true, true, false, false]);
}

#[test]
fn the_oracles_refusals_are_mirrored() {
    let mut s = regtest_testdummy();
    s.headers = vec![run(10, 4)];
    s.queries = vec![Query::State(DeploymentPos::TestDummy, Some(10))];
    assert_eq!(s.ours(), Err(Refused::Malformed), "a query above the tip");
    s.queries = vec![Query::State(DeploymentPos::TestDummy, Some(9))];
    s.args = vec!["-vbparams=testdummy:0:1:0:4294967296:1".into()];
    assert_eq!(s.ours(), Err(Refused::Period), "a window of 2^32 is a period of 0 as int");
    s.args = vec!["-vbparams=nosuch:0:1".into()];
    assert_eq!(s.ours(), Err(Refused::Params));
    // V20 active at a placeholder whose signals the stage reads
    s.args = vec![
        "-dip3params=2:2".into(),
        "-testactivationheight=v20@2".into(),
        "-testactivationheight=mn_rr@2".into(),
    ];
    s.first_height = 20;
    s.queries = vec![Query::SignalsStage(Some(5))];
    assert_eq!(s.ours(), Err(Refused::Context));
}

#[test]
fn every_fuzz_input_is_total() {
    for seed in 0u64..2_000 {
        let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let len = 8 + (seed % 120) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let s = Scenario::from_bytes(&bytes).expect("a scenario");
        // the node's side never panics on a scenario the format produces
        let _ = s.ours();
    }
    assert_eq!(Scenario::from_bytes(&[0; 7]), None);
}

#[test]
fn deployment_view_matches_the_state_queries() {
    let mut s = regtest_testdummy();
    s.headers = vec![run(9, 4), run(40, SIGNAL_28)];
    let params = s.params().expect("params");
    let chain = Chain::new(0, &s.header_fields());
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let dep = Deployment::Versionbits(DeploymentPos::TestDummy);
    for h in 0..49 {
        let active = d.state(Some(BlockId(h)), DeploymentPos::TestDummy) == ThresholdState::Active;
        assert_eq!(d.active_after(Some(BlockId(h)), dep), active, "{h}");
        if h > 0 {
            assert_eq!(d.active_at(BlockId(h), dep), d.active_after(Some(BlockId(h - 1)), dep));
        }
    }
    assert!(!d.active_at(BlockId(0), dep));
}

/// Every `DeploymentPos`, in Core's enum order (`consensus/params.h:42-47`).
const POSITIONS: [DeploymentPos; 3] =
    [DeploymentPos::TestDummy, DeploymentPos::V24, DeploymentPos::EvoShares];

/// A view that answers activation only, as the test and evo views do.
struct ActivationOnly;

impl DeploymentView for ActivationOnly {
    fn active_after(&self, _: Option<BlockId>, _: Deployment) -> bool {
        true
    }

    fn active_at(&self, _: BlockId, _: Deployment) -> bool {
        true
    }
}

#[test]
fn a_view_without_a_cache_gives_no_versionbits_answers() {
    for prev in [None, Some(BlockId(0)), Some(BlockId(7))] {
        for pos in POSITIONS {
            assert_eq!(ActivationOnly.versionbits(prev, pos), None);
        }
    }
}

#[test]
fn deployments_answer_versionbits_from_their_cache() {
    let mut s = regtest_testdummy();
    s.headers = vec![run(9, 4), run(40, SIGNAL_28)];
    let params = s.params().expect("params");
    let chain = Chain::new(0, &s.header_fields());
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    // the view answers what the state queries answer, on a second cache so neither warms the other
    let other = VersionBitsCache::new();
    let direct = Deployments::new(&params, &chain, &NoEhfSignals, &other);
    let view: &dyn DeploymentView = &d;
    let mut seen = Vec::new();
    for prev in std::iter::once(None).chain((0..49).map(|h| Some(BlockId(h)))) {
        for pos in POSITIONS {
            let answer = view.versionbits(prev, pos).expect("the node's view answers");
            assert_eq!(answer.state, direct.state(prev, pos), "{prev:?} {pos:?}");
            assert_eq!(answer.since, direct.state_since_height(prev, pos), "{prev:?} {pos:?}");
            if pos == DeploymentPos::TestDummy {
                seen.push((answer.state, answer.since));
            }
        }
    }
    // testdummy: defined from 0, started at 10, locked in at 20, active from 30
    let at = |h: usize| seen[h + 1];
    assert_eq!(seen[0], (ThresholdState::Defined, 0));
    assert_eq!(at(9), (ThresholdState::Started, 10));
    assert_eq!(at(19), (ThresholdState::LockedIn, 20));
    assert_eq!(at(29), (ThresholdState::Active, 30));
    assert_eq!(at(48), (ThresholdState::Active, 30));
}

/// Two branches over one block index: `a`'s blocks keep their heights as ids, `b`'s above the
/// fork are `B_IDS + height`.
struct Fork<'a> {
    a: &'a Chain,
    b: &'a Chain,
    fork: u32,
}

const B_IDS: u32 = 1 << 20;

impl Fork<'_> {
    fn id(&self, on_b: bool, height: u32) -> BlockId {
        BlockId(if on_b && height > self.fork {
            B_IDS + height
        } else {
            height
        })
    }

    /// The branch and height of `id`.
    fn split(&self, id: BlockId) -> (bool, u32) {
        match id.0.checked_sub(B_IDS) {
            Some(height) => (true, height),
            None => (false, id.0),
        }
    }
}

impl ChainAncestry for Fork<'_> {
    fn header(&self, id: BlockId) -> HeaderCtx {
        let (on_b, height) = self.split(id);
        if on_b {
            self.b
        } else {
            self.a
        }
        .header(BlockId(height))
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        let (on_b, at) = self.split(id);
        (height <= at).then(|| self.id(on_b, height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        let (on_b, height) = self.split(id);
        height.checked_sub(1).map(|h| self.id(on_b, h))
    }
}

#[test]
fn one_cache_serves_both_branches_of_a_fork() {
    // a shared trunk to 9, then `a` signals every block and `b` none
    let mut a = regtest_testdummy();
    a.headers = vec![run(10, 4), run(40, SIGNAL_28)];
    let mut b = regtest_testdummy();
    b.headers = vec![run(10, 4), run(40, VERSIONBITS_TOP_BITS)];
    let params = a.params().expect("params");
    let (chain_a, chain_b) = (Chain::new(0, &a.header_fields()), Chain::new(0, &b.header_fields()));
    let fork = Fork {
        a: &chain_a,
        b: &chain_b,
        fork: 9,
    };
    let shared = VersionBitsCache::new();
    let on_fork = Deployments::new(&params, &fork, &NoEhfSignals, &shared);
    let (cache_a, cache_b) = (VersionBitsCache::new(), VersionBitsCache::new());
    let alone_a = Deployments::new(&params, &chain_a, &NoEhfSignals, &cache_a);
    let alone_b = Deployments::new(&params, &chain_b, &NoEhfSignals, &cache_b);
    let pos = DeploymentPos::TestDummy;
    // alternate the branches so each one's walks meet the other's cached periods
    for h in 0..50 {
        for (on_b, alone) in [(false, alone_a), (true, alone_b)] {
            let prev = Some(fork.id(on_b, h));
            let expected = alone.state(Some(BlockId(h)), pos);
            assert_eq!(on_fork.state(prev, pos), expected, "branch b {on_b}, height {h}");
            assert_eq!(
                on_fork.state_since_height(prev, pos),
                alone.state_since_height(Some(BlockId(h)), pos),
                "branch b {on_b}, height {h}"
            );
        }
    }
    assert_eq!(on_fork.state(Some(fork.id(false, 49)), pos), ThresholdState::Active);
    assert_eq!(on_fork.state(Some(fork.id(true, 49)), pos), ThresholdState::Started);
}

/// A chain whose period end 19 reports dash-chain's unknown MTP.
struct UnknownMtp<'a>(&'a Chain);

impl ChainAncestry for UnknownMtp<'_> {
    fn header(&self, id: BlockId) -> HeaderCtx {
        let mut header = self.0.header(id);
        if id.0 == 19 {
            header.mtp = i64::MIN;
        }
        header
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.0.ancestor(id, height)
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        self.0.prev(id)
    }
}

#[test]
#[should_panic(expected = "the ancestry must give real MTPs")]
fn a_stand_in_mtp_panics_rather_than_caching_a_state() {
    let mut s = regtest_testdummy();
    s.headers = vec![run(9, 4), run(40, SIGNAL_28)];
    let params = s.params().expect("params");
    let chain = Chain::new(0, &s.header_fields());
    let unknown = UnknownMtp(&chain);
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &unknown, &NoEhfSignals, &cache);
    // the walk back from the key 19 reads its MTP (Core: `pindexPrev->GetMedianTimePast()`)
    d.state(Some(BlockId(19)), DeploymentPos::TestDummy);
}

#[test]
fn deployment_info_reports_enabled_deployments_in_cores_order() {
    let mut s = regtest_testdummy();
    s.headers = vec![run(9, 4), run(14, SIGNAL_28)];
    let params = s.params().expect("params");
    let chain = Chain::new(0, &s.header_fields());
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let info = d.deployment_info(BlockId(22));
    let bip9: Vec<DeploymentPos> = info
        .iter()
        .filter_map(|i| match i {
            DeploymentInfo::Bip9(b) => Some(b.pos),
            DeploymentInfo::Buried(_) => None,
        })
        .collect();
    assert_eq!(bip9, BIP9_ORDER);
    let DeploymentInfo::Bip9(dummy) =
        info.iter().find(|i| i.name() == "testdummy").expect("testdummy")
    else {
        panic!("bip9")
    };
    // block 22's own state (period 20..29, locked in by 10 of 10 signals in 10..19)
    assert_eq!(dummy.status, ThresholdState::LockedIn);
    assert_eq!(dummy.since, 20);
    assert_eq!(dummy.activation_height, Some(30));
    let stats = dummy.statistics.expect("statistics while locked in");
    assert_eq!((stats.period, stats.elapsed, stats.count), (10, 3, 3));
    assert_eq!((stats.threshold, stats.possible), (None, None));
    assert_eq!(dummy.signalling.as_deref(), Some("###"));
    assert_eq!(dummy.bit, Some(28));
}

/// CS4's part of quirk 31 (the register's test is EV3's): `GetSignalsStage` drops a signal mined
/// before its deployment's start time, and keeps one whose bit has no deployment (historic bit
/// 10) or whose deployment does not use EHF (28). Core's answers, recorded.
#[test]
fn stage_filter_expires_signals_mined_before_their_start() {
    let cases = common::oracle_cases("versionbits.json");
    let stage = |label: &str, height: u32| -> Vec<(u8, i32)> {
        let (scenario, core) = versionbits_json::recorded_case(&cases, label);
        let at = scenario.queries.iter().position(|q| *q == Query::SignalsStage(Some(height)));
        match &core[at.expect("a stage query")] {
            Answer::SignalsStage(signals) => signals.clone(),
            other => panic!("{other:?}"),
        }
    };
    // v24 (bit 13) starts at the time of height 800: its signal mined at 600 is expired, the
    // others stay
    assert_eq!(stage("quirk31_stage_expiry", 1299), [(10, 250), (14, 1150), (28, 590)]);
    // mined at 900, after the start: kept
    assert_eq!(stage("quirk31_stage_kept", 1299), [(10, 250), (13, 900)]);
}
