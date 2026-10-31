//! CH3a a8: V's deployments (CH3 §2.16). CS4's `Deployments` on V's complete header chain with one
//! versionbits cache, answering at each deployment's boundary ±1 on every network and through the
//! connector's context, and `UpdateTip`'s unknown-versionbits warning (`validation.cpp:3218-3233`)
//! at Core's thresholds and `MinBIP9WarningHeight`.
//!
//! Mainnet's and testnet's deployment heights are out of a mined chain's reach, so every network
//! runs through V's wiring ([`ChainDeployments`]) over a stand-in ancestry ([`Line`]); regtest
//! and devnet also run on V's own header chain, and the warning through V's `UpdateTip`.

use std::sync::{Arc, Mutex};

use dash_chain::CompleteAncestry;
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::deployments::{
    Deployments, NoEhfSignals, ThresholdState, VERSIONBITS_TOP_BITS, VersionBitsCache,
    bip9_enabled, buried_enabled,
};
use dash_consensus::{Block, BlockId, ChainAncestry, HeaderCtx};
use dash_num::{Arith256, CompactTarget};
use dash_params::{
    BuriedDeployment, ChainOption, ChainParams, ChainParamsBuilder, DeploymentPos, NetworkKind,
    Release,
};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use super::blocks::devnet;
use super::support::{
    Mined, Rig, RigConfig, SPACING, cb_script, coinbase, install_accept_log, lines_with,
    mine_version, regtest,
};
use crate::chain::actor::V;
use crate::chain::deployments::{ChainDeployments, unknown_rules_warning};
use crate::chain::{
    BlockConnector, BlockStatusWriter, ConnectCx, ConnectOutcome, DisconnectResult, Fatal,
    NullConnector, ProposalChecks, SyncState, UiNotifications, VContext, ViewTarget,
    devnet_genesis_block,
};

use ThresholdState::{Active, Defined, LockedIn, Started};

/// A version signalling TESTDUMMY (bit 28, `chainparams.cpp`, every network).
const TESTDUMMY_VERSION: i32 = VERSIONBITS_TOP_BITS | (1 << 28);
/// A bit no v24 deployment uses: the warning's.
const UNKNOWN_BIT: i32 = 27;
const UNKNOWN_VERSION: i32 = VERSIONBITS_TOP_BITS | (1 << UNKNOWN_BIT);

/// `kind`'s v24 parameters; devnet's is [`devnet`]'s.
fn params(kind: NetworkKind) -> Arc<ChainParams> {
    match kind {
        NetworkKind::Devnet => devnet(),
        _ => Arc::new(
            ChainParamsBuilder::new(kind, Release::V24_0).build().expect("Core's parameters"),
        ),
    }
}

/// `p` with `MinBIP9WarningHeight` at `m`.
fn with_min_bip9_warning_height(p: &ChainParams, m: u32) -> Arc<ChainParams> {
    let mut p = p.clone();
    p.consensus.min_bip9_warning_height = m.into();
    Arc::new(p)
}

/// The stand-in's first block time: past testnet's and devnet's EHF deployment starts, so only
/// the missing MNHF signals keep those deployments `DEFINED`.
const T0: u32 = 1_800_000_000;

/// A stand-in chain for heights no test can mine: `BlockId(h)` at height `h`, block `h` at
/// `T0 + SPACING h` with Core's median time past, of version `signal_version` in the inclusive
/// spans of heights `signal` and the bare top bits elsewhere.
struct Line {
    tip: u32,
    signal: Vec<(u32, u32)>,
    signal_version: i32,
}

impl Line {
    fn new(tip: u32, signal: &[(u32, u32)], signal_version: i32) -> Line {
        Line {
            tip,
            signal: signal.to_vec(),
            signal_version,
        }
    }

    fn time(h: u32) -> u32 {
        T0 + SPACING * h
    }
}

impl ChainAncestry for Line {
    fn header(&self, id: BlockId) -> HeaderCtx {
        let h = id.0;
        assert!(h <= self.tip, "an id above the tip");
        // Core's `GetMedianTimePast` (chain.h): the median of the last 11 times.
        let mut times: Vec<u32> = (h.saturating_sub(10)..=h).map(Line::time).collect();
        times.sort_unstable();
        HeaderCtx {
            height: h,
            hash: BlockHash::from([0; 32]),
            time: Line::time(h),
            bits: CompactTarget::new(0),
            version: if self.signal.iter().any(|&(lo, hi)| (lo..=hi).contains(&h)) {
                self.signal_version
            } else {
                VERSIONBITS_TOP_BITS
            },
            mtp: i64::from(times[times.len() / 2]),
            chain_work: Arith256::ZERO,
        }
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

/// Every deployment of `p` and the height from which it is active, on a chain whose every block
/// signals TESTDUMMY and that holds no MNHF signal: a buried one at its height; TESTDUMMY, only
/// enabled on regtest (start 0, Core's window and threshold, `MinBIP9WarningHeight` 0), after
/// one period each `DEFINED`, `STARTED` and `LOCKED_IN`; an EHF deployment never, as no signal
/// sets its `SignalHeight` (`versionbits.cpp:232-246`) until CH7a's source.
fn activations(p: &ChainParams) -> Vec<(Deployment, Option<u32>)> {
    let buried = BuriedDeployment::ALL.into_iter().map(|d| {
        let at = buried_enabled(p, d).then(|| p.consensus.deployment_height(d).to_u32());
        (Deployment::Buried(d), at)
    });
    let testdummy = bip9_enabled(p, DeploymentPos::TestDummy)
        .then(|| 3 * p.consensus.miner_confirmation_window);
    let bip9 = [
        (DeploymentPos::TestDummy, testdummy),
        (DeploymentPos::V24, None),
        (DeploymentPos::EvoShares, None),
    ];
    buried.chain(bip9.map(|(pos, at)| (Deployment::Versionbits(pos), at))).collect()
}

/// Each case's answers through `view` at its boundary ±1, or at heights 0, 1, `tip / 2` and
/// `tip` for one never active: `DeploymentActiveAfter` of the block at `h` (its parent's id) and
/// `DeploymentActiveAt` of it, against `h >= activation`. `id(h)` is the block at `h`. The
/// mismatches, described.
fn mismatches(
    view: &dyn DeploymentView,
    id: &dyn Fn(u32) -> BlockId,
    tip: u32,
    cases: &[(Deployment, Option<u32>)],
) -> Vec<String> {
    let mut out = Vec::new();
    for &(d, from) in cases {
        let heights = match from {
            Some(h) => vec![h.wrapping_sub(1), h, h + 1],
            None => vec![0, 1, tip / 2, tip],
        };
        for h in heights.into_iter().filter(|&h| h <= tip) {
            let after = view.active_after(h.checked_sub(1).map(id), d);
            let at = view.active_at(id(h), d);
            let want = from.is_some_and(|f| h >= f);
            if (after, at) != (want, want) {
                out.push(format!("{d:?} at height {h}: after {after}, at {at}, want {want}"));
            }
        }
    }
    out
}

/// The heights at which regtest's TESTDUMMY changes state on a chain whose every block signals
/// it (`versionbits.cpp:49-169`, 144-block periods, 108 of 144), ±1, for the block at each.
const REGTEST_TESTDUMMY: [(u32, ThresholdState); 9] = [
    (142, Defined),
    (143, Defined),
    (144, Started),
    (287, Started),
    (288, LockedIn),
    (289, LockedIn),
    (431, LockedIn),
    (432, Active),
    (433, Active),
];

/// [`REGTEST_TESTDUMMY`]'s mismatches through `view`, described; `id(h)` is the block at `h`.
fn testdummy_mismatches(view: &Deployments<'_>, id: &dyn Fn(u32) -> BlockId) -> Vec<String> {
    REGTEST_TESTDUMMY
        .iter()
        .filter_map(|&(h, want)| {
            let got = view.state(h.checked_sub(1).map(id), DeploymentPos::TestDummy);
            (got != want).then(|| format!("TESTDUMMY at {h}: {got:?}, want {want:?}"))
        })
        .collect()
}

/// What `unknown_rules` answers for one bit in `state`: nothing unless it is `Some`.
fn reported(bit: i32, state: Option<ThresholdState>) -> Vec<(i32, ThresholdState)> {
    state.map(|s| (bit, s)).into_iter().collect()
}

/// D21 on every network, through V's wiring: each buried deployment turns on at its height, and
/// no versionbits deployment but regtest's TESTDUMMY ever does without MNHF signals.
#[test]
fn every_network_answers_at_each_deployment_boundary() {
    for kind in NetworkKind::ALL {
        let p = params(kind);
        let cases = activations(&p);
        let m = p.consensus.min_bip9_warning_height.to_u32();
        let last = cases.iter().filter_map(|c| c.1).max().unwrap_or(0);
        // Past MinBIP9WarningHeight too, where an EHF deployment's walk reads its signals.
        let tip = last.max(m + 3 * p.consensus.miner_confirmation_window) + 2;
        // Every block above genesis signals TESTDUMMY, as on V's chain below.
        let line = Line::new(tip, &[(1, tip)], TESTDUMMY_VERSION);
        let deployments = ChainDeployments::over(p.clone(), line);
        let view = deployments.view();
        let mut wrong = mismatches(&view, &BlockId, tip, &cases);
        if kind == NetworkKind::Regtest {
            let testdummy = (Deployment::Versionbits(DeploymentPos::TestDummy), Some(432));
            assert!(cases.contains(&testdummy));
            wrong.extend(testdummy_mismatches(&view, &BlockId));
        }
        assert!(wrong.is_empty(), "{kind:?}: {wrong:#?}");
    }
}

/// `n` blocks on `parent`, `spacing` seconds apart, the block at `h` of version `version(h)`.
fn versioned(
    p: &ChainParams,
    parent: &Mined,
    n: u32,
    salt: u32,
    spacing: u32,
    version: impl Fn(u32) -> i32,
) -> Vec<Mined> {
    let mut out: Vec<Mined> = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let prev = out.last().unwrap_or(parent);
        let height = prev.height + 1;
        let cb = coinbase(&cb_script(height, salt));
        let block = mine_version(p, prev.hash(), prev.time() + spacing, vec![cb], version(height));
        out.push(Mined {
            block,
            height,
        });
    }
    out
}

/// `f` on V over its deployments and the ids of `blocks` by height (genesis first), with its
/// result.
fn on_deployments<'a, T: Send + 'static>(
    rig: &Rig,
    blocks: impl IntoIterator<Item = &'a Mined>,
    f: impl FnOnce(&mut V, &dyn Fn(u32) -> BlockId) -> T + Send + 'static,
) -> T {
    let ids = rig.ids(blocks);
    let out = Arc::new(Mutex::new(None));
    let put = out.clone();
    rig.on_v(move |v| {
        let r = f(v, &|h: u32| ids[h as usize]);
        *put.lock().expect("out") = Some(r);
        Ok(())
    });
    let r = out.lock().expect("out").take();
    r.expect("V ran it")
}

/// The same boundaries on V's own header chain (CH3 §2.16): regtest's over 602 headers that
/// all signal TESTDUMMY, with TESTDUMMY's periods; devnet's at its heights 1 and 2, above its
/// devnet genesis block.
#[test]
fn vs_header_chain_answers_at_each_deployment_boundary() {
    let rig = Rig::new();
    let chain = versioned(&rig.p, &rig.genesis, 602, 0, SPACING, |_| TESTDUMMY_VERSION);
    rig.headers(&chain);
    let p = rig.p.clone();
    let blocks = std::iter::once(&rig.genesis).chain(&chain);
    let wrong = on_deployments(&rig, blocks, move |v, id| {
        let view = v.deployments.view();
        let mut wrong = mismatches(&view, id, 602, &activations(&p));
        wrong.extend(testdummy_mismatches(&view, id));
        wrong
    });
    assert!(wrong.is_empty(), "regtest: {wrong:#?}");
    rig.assert_no_abort();

    let p = devnet();
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        no_check_block_index: true,
        ..RigConfig::default()
    });
    let dg = Mined {
        block: devnet_genesis_block(&p).expect("a devnet"),
        height: 1,
    };
    let chain = versioned(&p, &dg, 3, 0, SPACING, |_| TESTDUMMY_VERSION);
    rig.headers(&chain);
    let blocks = [&rig.genesis, &dg].into_iter().chain(&chain);
    let wrong = on_deployments(&rig, blocks, move |v, id| {
        mismatches(&v.deployments.view(), id, 4, &activations(&p))
    });
    assert!(wrong.is_empty(), "devnet: {wrong:#?}");
    rig.assert_no_abort();
}

/// The cache is wired on the complete header chain, never a `RuleContext` (CH3 §2.16, D21): V's
/// deployments are over the chain's [`CompleteAncestry`], whose reads are the chain's own, and
/// every answer equals CS4's over V's `HeaderChain` itself with a fresh cache.
#[test]
fn the_deployments_are_wired_on_vs_header_chain() {
    let rig = Rig::new();
    // Nine in ten signal TESTDUMMY: it locks in at 288 and is active from 432.
    let tip = 450;
    let chain = versioned(&rig.p, &rig.genesis, tip, 0, SPACING, |h| {
        if h % 10 == 0 {
            VERSIONBITS_TOP_BITS
        } else {
            TESTDUMMY_VERSION
        }
    });
    rig.headers(&chain);
    let blocks = std::iter::once(&rig.genesis).chain(&chain);
    let wrong = on_deployments(&rig, blocks, move |v, id| {
        let wired: &ChainDeployments<CompleteAncestry> = &v.deployments;
        let ours = wired.view();
        let handle = v.chain.complete_ancestry().expect("V's chain is complete");
        let whole: &dyn ChainAncestry = &v.chain;
        let cache = VersionBitsCache::new();
        let direct = Deployments::new(&v.params, &v.chain, &NoEhfSignals, &cache);
        let mut wrong = Vec::new();
        for h in 0..=tip {
            let (block, prev) = (id(h), h.checked_sub(1).map(id));
            if handle.header(block) != whole.header(block)
                || handle.prev(block) != whole.prev(block)
                || handle.ancestor(block, h / 2) != whole.ancestor(block, h / 2)
            {
                wrong.push(format!("the handle's reads at {h}"));
            }
            for pos in [DeploymentPos::TestDummy, DeploymentPos::V24, DeploymentPos::EvoShares] {
                if ours.state(prev, pos) != direct.state(prev, pos) {
                    wrong.push(format!("{pos:?} at {h}"));
                }
            }
            if ours.compute_block_version(prev) != direct.compute_block_version(prev) {
                wrong.push(format!("ComputeBlockVersion at {h}"));
            }
        }
        wrong
    });
    assert!(wrong.is_empty(), "{wrong:#?}");
    rig.assert_no_abort();
}

/// What the connector's context answered for one connected block: its hash and TESTDUMMY's
/// `DeploymentActiveAfter` (of its parent) and `DeploymentActiveAt`.
type Answer = (BlockHash, bool, bool);

/// Asks the connector's context about TESTDUMMY at each connect, then connects as
/// [`NullConnector`].
struct Asks(Arc<Mutex<Vec<Answer>>>);

impl BlockConnector for Asks {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        let prev = cx.reader.entry(id).expect("indexed").prev;
        let d = Deployment::Versionbits(DeploymentPos::TestDummy);
        let answer =
            (block.hash(), cx.deployments.active_after(prev, d), cx.deployments.active_at(id, d));
        self.0.lock().expect("answers").push(answer);
        NullConnector.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        NullConnector.disconnect_block(cx, v, target, block, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        pow: bool,
        merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, block, prev, pow, merkle)
    }
}

/// Regtest with `-vbparams=testdummy:<start>:<no timeout>:0:10:8`: TESTDUMMY on 10-block periods
/// needing 8 signals, from `start`.
fn regtest_testdummy_10_of_8(start: i64) -> Arc<ChainParams> {
    let vbparams = format!("testdummy:{start}:{}:0:10:8", i64::MAX);
    Arc::new(
        ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
            .arg(ChainOption::VbParams, vbparams)
            .build()
            .expect("regtest with -vbparams"),
    )
}

/// `ConnectCx::deployments` is V's (CH3 §2.16): with `-vbparams=testdummy:0:…:0:10:8` (10-block
/// periods, 8 of 10), TESTDUMMY is active from block 30 of a signalling branch. A reorg onto a
/// longer branch that never signals answers false at the same heights, and the reorg back answers
/// true again: V's one cache, which no reorg clears (Core keys it by block), stays right across
/// both.
#[test]
fn the_connectors_context_stays_right_across_reorgs() {
    let p = regtest_testdummy_10_of_8(0);
    let asks = Arc::new(Mutex::new(Vec::new()));
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        connector: Some(Box::new(Asks(asks.clone()))),
        ..RigConfig::default()
    });
    let a = versioned(&p, &rig.genesis, 31, 0, SPACING, |_| TESTDUMMY_VERSION);
    let b = versioned(&p, &rig.genesis, 32, 1, SPACING, |_| VERSIONBITS_TOP_BITS);
    let a_more = versioned(&p, &a[30], 2, 0, SPACING, |_| TESTDUMMY_VERSION);
    let answers = |blocks: &[Mined], from: Option<u32>| -> Vec<Answer> {
        let on = |m: &Mined| from.is_some_and(|f| m.height >= f);
        blocks.iter().map(|m| (m.hash(), on(m), on(m))).collect()
    };
    asks.lock().expect("answers").clear();

    for m in a.iter().chain(&b).chain(&a_more) {
        rig.accept(m);
    }
    let a_all: Vec<Mined> = a.iter().chain(&a_more).cloned().collect();
    let want: Vec<Answer> = [answers(&a, Some(30)), answers(&b, None), answers(&a_all, Some(30))]
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(*asks.lock().expect("answers"), want);
    assert_eq!(rig.v.view().snap.tip.tip, rig.id(&a_more[1]));
    rig.assert_no_abort();
}

/// The unknown-bit warning's threshold on each network (`validation.cpp:2395-2417`): bit 27 set in
/// `nRuleChangeActivationThreshold` blocks of the first period the walk counts above
/// `MinBIP9WarningHeight` is `LOCKED_IN` from the period's end and `ACTIVE` one period later; one
/// block fewer leaves it `STARTED`, which warns of nothing.
#[test]
fn the_unknown_bit_warning_needs_the_networks_threshold() {
    // `chainparams.cpp:197,205-206`, `:406,414-415`, `:588,596-597`, `:831,839-840`.
    let core = [
        (NetworkKind::Mainnet, 2016, 1815, 2_201_472 + 2016),
        (NetworkKind::Testnet, 2016, 1512, 1_148_500 + 2016),
        (NetworkKind::Devnet, 2016, 1512, 2 + 2016),
        (NetworkKind::Regtest, 144, 108, 0),
    ];
    for (kind, period, threshold, m) in core {
        let p = params(kind);
        let c = &p.consensus;
        let ours = (
            c.miner_confirmation_window,
            c.rule_change_activation_threshold,
            c.min_bip9_warning_height.to_u32(),
        );
        assert_eq!(ours, (period, threshold, m), "{kind:?}");
        let first = first_period_end(m, period);
        for count in [threshold, threshold - 1] {
            let locked = count == threshold;
            let line =
                Line::new(first + 2 * period, &[(first + 1, first + count)], UNKNOWN_VERSION);
            let mut deployments = ChainDeployments::over(p.clone(), line);
            for (tip, want) in [
                (first + period - 1, None),
                (first + period, locked.then_some(LockedIn)),
                (first + 2 * period - 1, locked.then_some(LockedIn)),
                (first + 2 * period, locked.then_some(Active)),
            ] {
                let got = deployments.unknown_rules(BlockId(tip));
                assert_eq!(
                    got,
                    reported(UNKNOWN_BIT, want),
                    "{kind:?}, {count} signals, tip {tip}"
                );
            }
        }
    }
}

/// Condition's parent clause (`validation.cpp:2415`): a block counts only if `ComputeBlockVersion`
/// of its parent leaves the bit clear, so a bit the node itself signals (a known deployment
/// `STARTED` or `LOCKED_IN`) warns only once that deployment has moved on. With every block
/// signalling TESTDUMMY, the node sets bit 28 too while TESTDUMMY is STARTED or LOCKED_IN (blocks
/// 144 to 431), so nothing counts; once it is ACTIVE the node stops, and the miners' bit 28 is
/// unknown rules: LOCKED_IN from 575, ACTIVE from 719.
#[test]
fn the_unknown_bit_warning_skips_a_bit_the_node_signals() {
    let line = Line::new(719, &[(1, 719)], TESTDUMMY_VERSION);
    let mut d = ChainDeployments::over(regtest(), line);
    for (tip, want) in
        [(287, None), (431, None), (574, None), (575, Some(LockedIn)), (719, Some(Active))]
    {
        assert_eq!(d.unknown_rules(BlockId(tip)), reported(28, want), "tip {tip}");
    }
}

/// The parent's `ComputeBlockVersion`, not the block's (`validation.cpp:2415`): TESTDUMMY
/// (10-block periods, 8 of 10) starts at block 200, its start time being block 194's, so the node
/// sets bit 28 for blocks 200 to 219 (STARTED, then LOCKED_IN). Of 144..287, blocks 144..209 and
/// 220..271 set bit 28: 199 counts (its parent's version leaves the bit clear) and 200..209 do
/// not, so the period counts exactly the threshold, 108 (56 + 52). Asking the block's own
/// `ComputeBlockVersion` would drop 199 and count 107.
#[test]
fn the_unknown_bit_warning_asks_the_parents_block_version() {
    let p = regtest_testdummy_10_of_8(i64::from(Line::time(194)));
    let line = Line::new(287, &[(144, 209), (220, 271)], TESTDUMMY_VERSION);
    let mut d = ChainDeployments::over(p, line);
    let state = |h: u32| d.view().state(Some(BlockId(h - 1)), DeploymentPos::TestDummy);
    let edges = [state(199), state(200), state(219), state(220)];
    assert_eq!(edges, [Defined, Started, LockedIn, Active]);
    assert_eq!(d.unknown_rules(BlockId(287)), [(28, LockedIn)]);
}

/// Condition's top-bits clause (`validation.cpp:2413`): bit 27 in a whole period under the top
/// bits `011` counts nowhere.
#[test]
fn the_unknown_bit_warning_needs_the_top_bits() {
    let first = first_period_end(0, 144);
    let wrong_top = 0x6000_0000 | (1 << UNKNOWN_BIT);
    let line = Line::new(first + 2 * 144, &[(first + 1, first + 144)], wrong_top);
    let mut d = ChainDeployments::over(regtest(), line);
    assert!(d.unknown_rules(BlockId(first + 144)).is_empty());
    assert!(d.unknown_rules(BlockId(first + 2 * 144)).is_empty());
}

/// The first period end (a height one below a multiple of `period`) at or above `m`: the walk's
/// first computed state (`versionbits.cpp:64-67` cuts below `m`), so the period after it is the
/// first the warning counts.
fn first_period_end(m: u32, period: u32) -> u32 {
    m + (period - 1 - m % period) % period
}

/// `MinBIP9WarningHeight` ±1 on each network's own value: the walk stops below it
/// (`versionbits.cpp:64-67`), so a period whose end is `M - 1` is never counted. Mainnet's `M` is
/// a period start, so one less counts a whole period more; on the others ±1 moves no period end.
/// (Condition's own height test, `validation.cpp:2412`, is implied: counted blocks lie above the
/// walk's cut.)
#[test]
fn the_unknown_bit_warning_starts_at_min_bip9_warning_height() {
    for (kind, counted) in [
        (NetworkKind::Mainnet, [true, false, false]),
        (NetworkKind::Testnet, [true, true, true]),
        (NetworkKind::Devnet, [true, true, true]),
        (NetworkKind::Regtest, [true, true, true]),
    ] {
        let p = params(kind);
        let (period, threshold) =
            (p.consensus.miner_confirmation_window, p.consensus.rule_change_activation_threshold);
        let m = p.consensus.min_bip9_warning_height.to_u32();
        // Regtest's M is 0: its -1 is 0 too.
        let first = first_period_end(m.saturating_sub(1), period);
        for (warning_height, counts) in [m.saturating_sub(1), m, m + 1].into_iter().zip(counted) {
            let line =
                Line::new(first + period, &[(first + 1, first + threshold)], UNKNOWN_VERSION);
            let p = with_min_bip9_warning_height(&p, warning_height);
            let got = ChainDeployments::over(p, line).unknown_rules(BlockId(first + period));
            let want = reported(UNKNOWN_BIT, counts.then_some(LockedIn));
            assert_eq!(got, want, "{kind:?}, MinBIP9WarningHeight {warning_height}");
        }
    }
}

/// A UI notification the warning test sees.
#[derive(Clone, Debug, PartialEq, Eq)]
enum UiEv {
    Tip(u32),
    Misc(String),
    Alert(String),
}

/// Records block tips and `DoWarning`'s calls.
#[derive(Default)]
struct WarnUi(Mutex<Vec<UiEv>>);

impl WarnUi {
    fn take(&self) -> Vec<UiEv> {
        std::mem::take(&mut *self.0.lock().expect("ui"))
    }

    fn push(&self, e: UiEv) {
        self.0.lock().expect("ui").push(e);
    }
}

impl UiNotifications for WarnUi {
    fn notify_block_tip(&self, _state: SyncState, _hash: BlockHash, height: u32) {
        self.push(UiEv::Tip(height));
    }

    fn set_misc_warning(&self, warning: &str) {
        self.push(UiEv::Misc(warning.to_owned()));
    }

    fn alert_notify(&self, message: &str) {
        self.push(UiEv::Alert(message.to_owned()));
    }
}

/// `UpdateTip`'s log line for the tip `m`.
fn update_tip_line(m: &Mined) -> String {
    let lines = lines_with(&format!("UpdateTip: new best={} ", m.hash()));
    assert_eq!(lines.len(), 1, "{lines:?}");
    lines.into_iter().next().expect("one line")
}

/// The warning through V's `UpdateTip` (`validation.cpp:3218-3233`, D30) on regtest (144-block
/// periods, 108 of 144): with 108 of blocks 144..287 setting bit 27, tip 287 logs the
/// locked-in warning and tip 431 runs `DoWarning`, the misc warning then `AlertNotify` once
/// (`:3150-3158`), before the step's `NotifyBlockTip` (N1). 107 blocks, IBD, or a
/// `MinBIP9WarningHeight` above 143, the first period end the walk computes, warn of nothing.
#[test]
fn update_tip_warns_of_unknown_rules_at_cores_thresholds() {
    install_accept_log();
    assert_eq!(unknown_rules_warning(UNKNOWN_BIT), "Unknown new rules activated (versionbit 27)");
    for (salt, (m, count, ibd, warns)) in (0..).zip([
        (0, 108, false, true),
        (0, 107, false, false),
        (0, 108, true, false),
        (142, 108, false, true),
        (143, 108, false, true),
        (144, 108, false, false),
    ]) {
        update_tip_case(salt, m, count, ibd, warns);
    }
}

/// One case of [`update_tip_warns_of_unknown_rules_at_cores_thresholds`]: regtest with
/// `MinBIP9WarningHeight` `m`, in IBD or out of it, over 431 blocks (salted by `salt`) of which
/// the `count` from 144 set bit 27; `warns` is whether Core warns.
fn update_tip_case(salt: u32, m: u32, count: u32, ibd: bool, warns: bool) {
    let case = format!("MinBIP9WarningHeight {m}, {count} signals, IBD {ibd}");
    let warning = unknown_rules_warning(UNKNOWN_BIT);
    let locked_in = format!(" warning='{warning}'");
    let p = with_min_bip9_warning_height(&regtest(), m);
    let ui = Arc::new(WarnUi::default());
    let rig = Rig::with(RigConfig {
        params: Some(p.clone()),
        // Ten minutes past genesis is out of IBD; a day (the default) is in it.
        after_genesis: (!ibd).then_some(600),
        ui: Some(ui.clone()),
        no_check_block_index: true,
        ..RigConfig::default()
    });
    // One second apart, so 431 blocks stay within two hours of the clock.
    let signals = 144..144 + count;
    let chain = versioned(&p, &rig.genesis, 431, salt, 1, |h| {
        if signals.contains(&h) {
            UNKNOWN_VERSION
        } else {
            VERSIONBITS_TOP_BITS
        }
    });
    // Not `Rig::accept`: its table check after every block would cost 431 index scans a case.
    for block in &chain {
        let r = rig.v.process_block(block.block.clone(), true, None, true).recv();
        assert!(r.as_ref().is_ok_and(|r| r.accepted && r.state.is_valid()), "{case}: {r:?}");
    }
    assert_eq!(rig.v.view().snap.tip.tip, rig.id(&chain[430]), "{case}");

    let at = |h: usize| update_tip_line(&chain[h - 1]);
    assert!(!at(286).contains(" warning="), "{case}: {}", at(286));
    assert_eq!(at(287).ends_with(&locked_in), warns, "{case}: {}", at(287));
    assert_eq!(at(430).ends_with(&locked_in), warns, "{case}: {}", at(430));
    assert!(!at(431).contains(" warning="), "{case}: {}", at(431));
    let events = ui.take();
    let warned: Vec<&UiEv> = events.iter().filter(|e| !matches!(e, UiEv::Tip(_))).collect();
    if !warns {
        assert!(warned.is_empty(), "{case}: {warned:?}");
        return;
    }
    let misc = UiEv::Misc(warning.clone());
    let alert = UiEv::Alert(warning);
    let last_step = events.iter().skip_while(|e| **e != UiEv::Tip(430)).skip(1);
    assert_eq!(last_step.collect::<Vec<_>>(), [&misc, &alert, &UiEv::Tip(431)], "{case}");
    // `fWarned`: a later `DoWarning` sets the misc warning only. Disconnecting 431 leaves 430's
    // LOCKED_IN; reconnecting it is ACTIVE again.
    let tip = chain[430].hash();
    assert_eq!(rig.v.invalidate_block(tip).recv(), Ok(Ok(())), "{case}");
    assert_eq!(rig.v.reconsider_block(tip, false).recv(), Ok(Ok(())), "{case}");
    assert_eq!(ui.take(), [UiEv::Tip(430), misc, UiEv::Tip(431)], "{case}");
    rig.assert_no_abort();
}
