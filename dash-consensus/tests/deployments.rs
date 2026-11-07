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
    Deployments, EhfReadError, NoEhfSignals, ThresholdState, VERSIONBITS_TOP_BITS,
    VersionBitsCache, WarningCache,
};
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_params::DeploymentPos;
use versionbits::{
    Answer, Chain, FaultingSignals, Injected, Network, Query, Refused, Run, Scenario,
};

/// [`NoEhfSignals`] reads nothing, so no query over it meets a read error.
const NEVER: &str = "NoEhfSignals never faults";

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
        let state = d.state(Some(BlockId(h)), DeploymentPos::TestDummy).expect(NEVER);
        let active = state == ThresholdState::Active;
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
            assert_eq!(answer.state, direct.state(prev, pos).expect(NEVER), "{prev:?} {pos:?}");
            let since = direct.state_since_height(prev, pos).expect(NEVER);
            assert_eq!(answer.since, since, "{prev:?} {pos:?}");
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
            let expected = alone.state(Some(BlockId(h)), pos).expect(NEVER);
            assert_eq!(
                on_fork.state(prev, pos).expect(NEVER),
                expected,
                "branch b {on_b}, height {h}"
            );
            assert_eq!(
                on_fork.state_since_height(prev, pos).expect(NEVER),
                alone.state_since_height(Some(BlockId(h)), pos).expect(NEVER),
                "branch b {on_b}, height {h}"
            );
        }
    }
    assert_eq!(on_fork.state(Some(fork.id(false, 49)), pos).ok(), Some(ThresholdState::Active));
    assert_eq!(on_fork.state(Some(fork.id(true, 49)), pos).ok(), Some(ThresholdState::Started));
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
    let _ = d.state(Some(BlockId(19)), DeploymentPos::TestDummy);
}

#[test]
fn deployment_info_reports_enabled_deployments_in_cores_order() {
    let mut s = regtest_testdummy();
    s.headers = vec![run(9, 4), run(14, SIGNAL_28)];
    let params = s.params().expect("params");
    let chain = Chain::new(0, &s.header_fields());
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &NoEhfSignals, &cache);
    let info = d.deployment_info(BlockId(22)).expect(NEVER);
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

/// V24's EHF signal mined at 200 over 2000 blocks that set bits 13 (V24) and 5 (no deployment's),
/// on regtest with V20 at 100 and V24's window 100, threshold 80 down to 60.
fn ehf_chain() -> (dash_params::ChainSetup, Chain) {
    let scenario = Scenario {
        network: Network::Regtest,
        args: vec![
            "-dip3params=100:100".into(),
            "-testactivationheight=v20@100".into(),
            "-testactivationheight=mn_rr@100".into(),
            "-vbparams=v24:0:9223372036854775807:0:100:80:60:5:1".into(),
        ],
        first_height: 0,
        headers: vec![Run {
            count: 2000,
            version: VERSIONBITS_TOP_BITS | 1 << 13 | 1 << UNKNOWN_BIT,
            time: versionbits::BASE_TIME,
            step: 150,
        }],
        signals: vec![],
        queries: vec![],
    };
    let params = scenario.params().expect("params");
    let chain = Chain::new(0, &scenario.header_fields());
    (params, chain)
}

/// A version bit no regtest deployment uses: `UpdateTip`'s warning counts it.
const UNKNOWN_BIT: i32 = 5;

const V24: DeploymentPos = DeploymentPos::V24;

fn at(h: u32) -> Option<BlockId> {
    Some(BlockId(h))
}

/// The error is the source's injected one, at `h`.
fn assert_injected(e: Option<EhfReadError>, h: u32) {
    let e = e.expect("the read error escapes");
    assert_eq!(e.downcast_ref::<Injected>(), Some(&Injected(BlockId(h))), "{e}");
}

/// The cache's entries, as its `Debug`.
fn entries(cache: &impl std::fmt::Debug) -> String {
    format!("{cache:?}")
}

/// U1: a fault on the first read of a fresh cache: `state`, `statistics` and
/// `state_since_height` are `Err`, and the cache stays empty.
#[test]
fn a_first_read_fault_caches_nothing() {
    let (params, chain) = ehf_chain();
    let ehf = FaultingSignals::at(1999);
    let fresh = entries(&VersionBitsCache::new());
    for query in ["state", "statistics", "state_since_height"] {
        let cache = VersionBitsCache::new();
        let d = Deployments::new(&params, &chain, &ehf, &cache);
        let error = match query {
            "state" => d.state(at(1999), V24).err(),
            "statistics" => d.statistics(at(1999), V24, None).err(),
            _ => d.state_since_height(at(1999), V24).err(),
        };
        assert_injected(error, 1999);
        assert_eq!(entries(&cache), fresh, "{query}");
    }
}

/// U2: `state_since_height` computes and caches the initial state, then its walk's parent
/// period's signal read faults: `Err`, and the cache is the initial state's computation.
#[test]
fn a_fault_in_the_since_walk_keeps_the_initial_states() {
    let (params, chain) = ehf_chain();
    let control = VersionBitsCache::new();
    let clean = FaultingSignals::none();
    let d = Deployments::new(&params, &chain, &clean, &control);
    assert_eq!(d.state(at(1999), V24).ok(), Some(ThresholdState::Active));
    assert_eq!(d.state_since_height(at(1999), V24).ok(), Some(500));

    let initial = VersionBitsCache::new();
    let ok = Deployments::new(&params, &chain, &clean, &initial).state(at(1999), V24);
    assert!(ok.is_ok());
    let ehf = FaultingSignals::at(1899);
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &ehf, &cache);
    assert_injected(d.state_since_height(at(1999), V24).err(), 1899);
    assert_eq!(entries(&cache), entries(&initial));
    assert_eq!(ehf.reads(), [BlockId(1999), BlockId(1899)], "the initial state, then the walk");
}

/// U3: `statistics` in V24's first `STARTED` period: its period-end state answers, then
/// `state_since_height`'s walk faults at the period before. `Err`, no statistics.
#[test]
fn a_fault_in_the_statistics_since_walk_is_an_error() {
    let (params, chain) = ehf_chain();
    let clean = FaultingSignals::none();
    let control = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &clean, &control);
    assert_eq!(d.state(at(299), V24).ok(), Some(ThresholdState::Started));
    assert!(d.statistics(at(350), V24, None).is_ok());

    let ehf = FaultingSignals::at(199);
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &ehf, &cache);
    let mut blocks = Vec::new();
    assert_injected(d.statistics(at(350), V24, Some(&mut blocks)).err(), 199);
    let started = VersionBitsCache::new();
    let ok = Deployments::new(&params, &chain, &clean, &started).state(at(299), V24);
    assert!(ok.is_ok());
    assert_eq!(entries(&cache), entries(&started));
}

/// U4: `compute_block_version` caches TestDummy's state (no EHF), then V24's read faults: `Err`,
/// TestDummy's entries equal a control's and V24's and EVO_SHARES' are absent.
#[test]
fn a_fault_in_compute_block_version_keeps_earlier_deployments() {
    let (params, chain) = ehf_chain();
    assert_eq!(
        params.release.deployments(),
        [DeploymentPos::TestDummy, V24, DeploymentPos::EvoShares]
    );
    let ehf = FaultingSignals::at(1999);
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &ehf, &cache);
    assert_injected(d.compute_block_version(at(1999)).err(), 1999);
    let clean = FaultingSignals::none();
    let control = VersionBitsCache::new();
    let c = Deployments::new(&params, &chain, &clean, &control);
    assert!(c.state(at(1999), DeploymentPos::TestDummy).is_ok());
    assert_eq!(entries(&cache), entries(&control));
    assert_ne!(entries(&control), entries(&VersionBitsCache::new()), "TestDummy cached");
}

/// U5: `UpdateTip`'s warning walk for an unknown bit: the seed `DEFINED` and the first period's
/// `STARTED` are cached, then the next period's count meets a block whose condition's
/// `ComputeBlockVersion` read faults, at its parent. That period is absent, the rest equals a
/// control's, and the read was the counted block's.
#[test]
fn a_fault_in_the_warning_count_keeps_the_earlier_periods() {
    let (params, chain) = ehf_chain();
    let clean = FaultingSignals::none();
    let control_cache = VersionBitsCache::new();
    let c = Deployments::new(&params, &chain, &clean, &control_cache);
    // 143 ends the first warning period (144 blocks), 287 the second
    let mut first = WarningCache::new();
    assert_eq!(
        c.warning_state(UNKNOWN_BIT, at(200), &mut first).ok(),
        Some(ThresholdState::Started)
    );
    let mut both = WarningCache::new();
    assert_eq!(
        c.warning_state(UNKNOWN_BIT, at(300), &mut both).ok(),
        Some(ThresholdState::LockedIn)
    );
    assert_ne!(entries(&first), entries(&both), "the second period is cached by the control");

    let ehf = FaultingSignals::at(286);
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &ehf, &cache);
    let mut warning = WarningCache::new();
    assert_injected(d.warning_state(UNKNOWN_BIT, at(300), &mut warning).err(), 286);
    assert_eq!(entries(&warning), entries(&first));
    assert_eq!(ehf.reads(), [BlockId(286)], "the counted block 287's parent");
}

/// U6: the node's view as a trait object over a faulting source: the `try_*` answers are `Err`.
#[test]
fn the_views_try_answers_carry_the_read_error() {
    let (params, chain) = ehf_chain();
    let ehf = FaultingSignals::at(1999);
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &ehf, &cache);
    let view: &dyn DeploymentView = &d;
    assert_injected(view.try_active_after(at(1999), Deployment::Versionbits(V24)).err(), 1999);
    assert_injected(view.try_versionbits(at(1999), V24).err(), 1999);
    let v20 = Deployment::Buried(dash_params::BuriedDeployment::V20);
    assert_eq!(view.try_active_after(at(1999), v20).ok(), Some(true), "buried reads no row");
}

/// U6: the infallible answers keep the backstop: a read error panics with `GetForBlock`'s
/// context.
#[test]
#[should_panic(expected = "GetForBlock: injected EHF read failure")]
fn the_views_infallible_answer_panics_on_a_read_error() {
    let (params, chain) = ehf_chain();
    let ehf = FaultingSignals::at(1999);
    let cache = VersionBitsCache::new();
    let view: &dyn DeploymentView = &Deployments::new(&params, &chain, &ehf, &cache);
    view.active_after(at(1999), Deployment::Versionbits(V24));
}

/// U7: the report with a read fault in V24's entry (its parent's state) is `Err`, not a report
/// without V24.
#[test]
fn a_fault_in_one_deployments_entry_fails_the_whole_report() {
    let (params, chain) = ehf_chain();
    let clean = FaultingSignals::none();
    let control = VersionBitsCache::new();
    let report = Deployments::new(&params, &chain, &clean, &control).deployment_info(BlockId(350));
    assert!(report.expect("the control").iter().any(|i| i.name() == "v24"));
    let ehf = FaultingSignals::at(349);
    let cache = VersionBitsCache::new();
    let d = Deployments::new(&params, &chain, &ehf, &cache);
    assert_injected(d.deployment_info(BlockId(350)).err(), 349);
}
