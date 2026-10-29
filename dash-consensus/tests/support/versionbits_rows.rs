//! CS4's boundary rows: hand-built versionbits scenarios at the state machine's edges, the
//! dynamic threshold's table (L0) and quirks 31 (CS4's part) and 32. `node-l1`'s
//! `versionbits_record` runs each through the oracle and records Core's answers in
//! `tests/data/oracle/versionbits.json`; `node-l1`'s `l1_versionbits` compares them live.

#![allow(dead_code, reason = "each includer uses a subset")]

use dash_params::DeploymentPos;

use super::versionbits::{BASE_TIME, Network, POSITIONS, Query, Run, Scenario};

const TOP: i32 = 0x2000_0000;
const BIT_DUMMY: i32 = 1 << 28;
const BIT_V24: i32 = 1 << 13;
const BIT_EVO: i32 = 1 << 14;
/// Regtest's genesis time.
const T0: u32 = BASE_TIME;
const NO_TIMEOUT: i64 = i64::MAX;

fn run(count: u32, version: i32, time: u32) -> Run {
    Run {
        count,
        version,
        time,
        step: 150,
    }
}

/// `count` headers from height `from`, `signal_in_10` of each 10 with `signal`, the rest with
/// the top bits only; times continue at 150 s from `T0`.
fn pattern(from: u32, count: u32, signal: i32, signal_in_10: u32) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut h = from;
    while h < from + count {
        let in_cycle = (h - from) % 10;
        let (n, version) = if in_cycle < signal_in_10 {
            (signal_in_10 - in_cycle, TOP | signal)
        } else {
            (10 - in_cycle, TOP)
        };
        let n = n.min(from + count - h);
        runs.push(run(n, version, T0 + 150 * h));
        h += n;
    }
    runs
}

/// Every query kind at each height, in the heights' order: the states, the since-heights and
/// the statistics of each deployment, then the block version and the stage signals.
fn sweep(heights: impl IntoIterator<Item = Option<u32>>) -> Vec<Query> {
    let mut queries = Vec::new();
    for h in heights {
        for pos in POSITIONS {
            queries.push(Query::State(pos, h));
        }
        for pos in POSITIONS {
            queries.push(Query::StateSinceHeight(pos, h));
        }
        for pos in POSITIONS {
            queries.push(Query::Statistics(pos, h));
        }
        queries.push(Query::ComputeBlockVersion(h));
        queries.push(Query::SignalsStage(h));
    }
    queries
}

/// The last block of each period of `period` up to `tip`, and the blocks on either side.
fn boundaries(period: u32, from: u32, tip: u32) -> Vec<Option<u32>> {
    let mut heights = Vec::new();
    let mut end = from + period - 1 - from % period;
    while end <= tip {
        for h in [end.saturating_sub(1), end, end + 1] {
            if h <= tip && heights.last() != Some(&Some(h)) {
                heights.push(Some(h));
            }
        }
        end += period;
    }
    heights
}

fn regtest(args: &[String], headers: Vec<Run>, signals: Vec<(u32, Vec<(u8, i32)>)>) -> Scenario {
    Scenario {
        network: Network::Regtest,
        args: args.to_vec(),
        first_height: 0,
        headers,
        signals,
        queries: Vec::new(),
    }
}

fn tip(s: &Scenario) -> u32 {
    s.tip().expect("a row has headers")
}

/// Regtest's V20 (and DIP3, MN_RR) at `height`.
fn v20_at(height: u32) -> Vec<String> {
    vec![
        format!("-dip3params={height}:{height}"),
        format!("-testactivationheight=v20@{height}"),
        format!("-testactivationheight=mn_rr@{height}"),
    ]
}

fn vbparams(pos: DeploymentPos, fields: &str) -> String {
    format!("-vbparams={}:{fields}", pos.name())
}

/// The dynamic threshold's table (L0): a deployment started from the first period and never
/// signalled, so each period's statistics give `Threshold(nAttempt)` for attempts 0 to 16.
fn threshold_rows(rows: &mut Vec<(String, Scenario)>) {
    // (window, nThresholdStart, nThresholdMin, nFalloffCoeff): mainnet/testnet/devnet/regtest's
    // v24 shapes, a min or coefficient of 0 (the start threshold throughout), a start of 0
    // (nRuleChangeActivationThreshold, above the window here), steep and flat falloffs
    let sets: [(u32, i64, i64, i64); 9] = [
        (250, 200, 150, 5),
        (100, 80, 60, 5),
        (120, 96, 72, 5),
        (10, 9, 1, 1),
        (48, 40, 0, 3),
        (48, 40, 10, 0),
        (48, 0, 10, 3),
        (13, 13, 1, 2),
        (40, 39, 20, 50),
    ];
    for (window, start, min, falloff) in sets {
        let mut s = regtest(
            &[vbparams(
                DeploymentPos::TestDummy,
                &format!("0:{NO_TIMEOUT}:0:{window}:{start}:{min}:{falloff}:0"),
            )],
            vec![run(window * 17, TOP, T0)],
            vec![],
        );
        s.queries = (0..17)
            .map(|p| Query::Statistics(DeploymentPos::TestDummy, Some(p * window + window - 1)))
            .collect();
        rows.push((format!("threshold_{window}_{start}_{min}_{falloff}"), s));
    }
}

/// Every row, labelled.
pub fn rows() -> Vec<(String, Scenario)> {
    let mut rows = Vec::new();
    threshold_rows(&mut rows);

    // regtest's own v24 and evo_shares (window 250, 200 -> 150 over 5, EHF): v24 signalled by 7
    // of 10 blocks once its EHF signal is mined at 600, evo_shares by all from 1100 with its
    // signal at 1100; testdummy (144, 108, no EHF) never
    {
        let mut headers = vec![run(600, TOP, T0)];
        headers.extend(pattern(600, 500, BIT_V24, 7));
        headers.extend(pattern(1100, 2900, BIT_V24 | BIT_EVO, 7));
        let signals = vec![(600, vec![(13, 600)]), (1100, vec![(13, 600), (14, 1100)])];
        let mut s = regtest(&v20_at(432), headers, signals);
        let t = tip(&s);
        let mut heights = boundaries(250, 0, t);
        heights.extend(boundaries(144, 0, t));
        heights.sort();
        heights.dedup();
        let mut up = s.clone();
        up.queries = sweep(heights.iter().copied());
        rows.push(("regtest_v24_ascending".into(), up));
        // the same queries from the tip down: the cache keeps what each query computed under its
        // own signal height
        s.queries = sweep(heights.iter().rev().copied());
        rows.push(("regtest_v24_descending".into(), s));
    }

    // the signal height read at the queried block only: a late query first, then early ones
    {
        let mut headers = vec![run(500, TOP, T0)];
        headers.extend(pattern(500, 1500, BIT_V24, 10));
        let signals = vec![(900, vec![(13, 900)])];
        let mut s = regtest(&v20_at(100), headers, signals);
        let order =
            [Some(1999), Some(749), Some(999), Some(1249), Some(1499), Some(899), Some(499)];
        s.queries = sweep(order);
        rows.push(("regtest_signal_height_order".into(), s));
    }

    // quirk 31 (CS4's part): the stage filter drops a signal mined before its deployment's start
    // time (v24 from the time of height 800 here), keeps a historic bit's (10, no deployment)
    // and a bit whose deployment has no EHF (28)
    {
        let start = i64::from(T0) + 150 * 800;
        let args = {
            let mut a = v20_at(100);
            a.push(vbparams(DeploymentPos::V24, &format!("{start}:{NO_TIMEOUT}:0:100:80:60:5:1")));
            a
        };
        let headers = vec![run(1600, TOP | BIT_V24, T0)];
        let signals = vec![
            (300, vec![(10, 250)]),
            (600, vec![(10, 250), (13, 600), (28, 590)]),
            (1200, vec![(10, 250), (13, 600), (14, 1150), (28, 590)]),
        ];
        let mut s = regtest(&args, headers, signals);
        s.queries = sweep(boundaries(100, 0, 1599));
        // the same with v24's signal mined after its start: kept, and the deployment moves
        let mut kept = s.clone();
        kept.signals = vec![(300, vec![(10, 250)]), (900, vec![(10, 250), (13, 900)])];
        rows.push(("quirk31_stage_expiry".into(), s));
        rows.push(("quirk31_stage_kept".into(), kept));
    }

    // the stage filter's equality: v24's signal mined at 600, whose MTP is exactly v24's start
    // time, is kept (expiry is MTP < start), so v24 starts from the period after 699
    {
        let start = T0 + 150 * (600 - 5);
        let args = {
            let mut a = v20_at(100);
            a.push(vbparams(DeploymentPos::V24, &format!("{start}:{NO_TIMEOUT}:0:100:80:60:5:1")));
            a
        };
        let mut s =
            regtest(&args, vec![run(1000, TOP | BIT_V24, T0)], vec![(600, vec![(13, 600)])]);
        s.queries = sweep(boundaries(100, 0, 999));
        rows.push(("ehf_signal_at_start_time".into(), s));
    }

    // quirk 32 (a): MinBIP9WarningHeight is a floor. Devnet (2018; v24 window 120, EHF): every
    // block signals v24 and its EHF signal is mined at 100, but nothing moves below the floor
    {
        let time = 1_751_400_000;
        let headers = vec![Run {
            count: 2700,
            version: TOP | BIT_V24 | BIT_EVO,
            time,
            step: 150,
        }];
        let s = Scenario {
            network: Network::Devnet,
            args: vec![],
            first_height: 0,
            headers,
            signals: vec![(100, vec![(13, 100)]), (1000, vec![(13, 100), (14, 1000)])],
            queries: sweep(boundaries(120, 1600, 2699)),
        };
        rows.push(("quirk32_floor_devnet".into(), s));
    }
    // testnet (1150516; v24 from 1790208000, window 100): a window of real-chain shape whose
    // first header is below the floor, placeholders under it
    {
        let first = 1_150_516 - 700;
        let headers = vec![Run {
            count: 1500,
            version: TOP | BIT_V24,
            time: 1_790_208_000 - 150 * 200,
            step: 150,
        }];
        let s = Scenario {
            network: Network::Test,
            args: vec![],
            first_height: first,
            headers,
            signals: vec![(first + 300, vec![(13, (first + 300) as i32)])],
            // from the second period of the window: a block's statistics read the signal height
            // at the end of the period before its own
            queries: sweep(boundaries(100, first + 120, first + 1499)),
        };
        rows.push(("quirk32_floor_testnet".into(), s));
    }

    // quirk 32 (b): the threshold's int overflow. Period 1, threshold 1 -> 1 - attempt^2 / 100:
    // attempt * attempt * period wraps at attempt 46341, and the threshold jumps above the period
    {
        let args = [vbparams(DeploymentPos::TestDummy, &format!("0:{NO_TIMEOUT}:0:1:1:1:1:0"))];
        let mut s = regtest(&args, vec![run(46_400, TOP, T0)], vec![]);
        s.queries = [100, 46_339, 46_340, 46_341, 46_342, 46_399]
            .into_iter()
            .flat_map(|h| {
                [
                    Query::Statistics(DeploymentPos::TestDummy, Some(h)),
                    Query::State(DeploymentPos::TestDummy, Some(h)),
                ]
            })
            .collect();
        rows.push(("quirk32_threshold_overflow".into(), s));
    }

    // state edges on regtest's testdummy (window 10, threshold 8, no EHF)
    let dummy = |fields: &str| vec![vbparams(DeploymentPos::TestDummy, fields)];
    // `mtp(h)`: a block's MTP at 150 s spacing from T0 (h >= 10), for the exact-equality edges
    let mtp = |h: u32| T0 + 150 * (h - 5);
    let edges: [(&str, Vec<String>, Vec<Run>); 9] = [
        ("always_active", dummy(&format!("-1:{NO_TIMEOUT}:0:10:8")), vec![run(40, TOP, T0)]),
        (
            "never_active",
            dummy(&format!("-2:{NO_TIMEOUT}:0:10:8")),
            vec![run(40, TOP | BIT_DUMMY, T0)],
        ),
        (
            // starts at the time of height 25, times out at 60's before lock-in: failed
            "timeout_failed",
            dummy(&format!("{}:{}:0:10:8", T0 + 150 * 25, T0 + 150 * 60)),
            pattern(0, 100, BIT_DUMMY, 5),
        ),
        (
            // locks in at once; min_activation_height 75 holds it locked in until then
            "min_activation_height",
            dummy(&format!("0:{NO_TIMEOUT}:75:10:8")),
            vec![run(100, TOP | BIT_DUMMY, T0)],
        ),
        (
            // the start time equals the MTP of the period end 19: started from 20, not 30
            "start_time_exact",
            dummy(&format!("{}:{NO_TIMEOUT}:0:10:8", mtp(19))),
            vec![run(60, TOP | BIT_DUMMY, T0)],
        ),
        (
            // the timeout equals the MTP of the period end 29, never signalled: failed from 30,
            // not 40
            "timeout_exact",
            dummy(&format!("0:{}:0:10:8", mtp(29))),
            vec![run(60, TOP, T0)],
        ),
        (
            // min_activation_height a period multiple (30): locked in from 20, active from 30
            // (the period end 29's height + 1), not 40
            "min_activation_height_period",
            dummy(&format!("0:{NO_TIMEOUT}:30:10:8")),
            vec![run(60, TOP | BIT_DUMMY, T0)],
        ),
        (
            // exactly the threshold (8 of 10), and one short of it (7 of 10)
            "threshold_exact",
            dummy(&format!("0:{NO_TIMEOUT}:0:10:8")),
            [pattern(0, 30, BIT_DUMMY, 7), pattern(30, 30, BIT_DUMMY, 8)].concat(),
        ),
        (
            // the old block versions (4) and other top bits never signal, whatever bit 28 holds
            "top_mask",
            dummy(&format!("0:{NO_TIMEOUT}:0:10:8")),
            vec![
                run(20, 4 | BIT_DUMMY, T0),
                run(10, 0x6000_0000 | BIT_DUMMY, T0 + 3000),
                run(10, 0x4000_0000 | BIT_DUMMY, T0 + 4500),
                run(20, TOP | BIT_DUMMY, T0 + 6000),
            ],
        ),
    ];
    for (label, args, headers) in edges {
        let mut s = regtest(&args, headers, vec![]);
        s.queries = sweep(std::iter::once(None).chain(boundaries(10, 0, tip(&s))));
        rows.push((label.into(), s));
    }

    // non-monotonic times around a start time: the MTP, not the block time, starts the
    // deployment
    {
        let start = T0 + 150 * 30;
        let headers = vec![
            run(25, TOP | BIT_DUMMY, T0),
            Run {
                count: 10,
                version: TOP | BIT_DUMMY,
                time: start + 600,
                step: -300,
            },
            run(45, TOP | BIT_DUMMY, start - 3000),
        ];
        let mut s = regtest(&dummy(&format!("{start}:{NO_TIMEOUT}:0:10:8")), headers, vec![]);
        s.queries = sweep(boundaries(10, 0, tip(&s)));
        rows.push(("mtp_not_block_time".into(), s));
    }

    // mainnet: every deployment never active
    {
        let first = 2_000_000;
        let s = Scenario {
            network: Network::Main,
            args: vec![],
            first_height: first,
            headers: vec![Run {
                count: 100,
                version: TOP | BIT_V24 | BIT_EVO | BIT_DUMMY,
                time: 1_700_000_000,
                step: 150,
            }],
            signals: vec![(first + 50, vec![(13, (first + 40) as i32)])],
            // no statistics: they read the signal height at the end of the previous 4032-block
            // window, below these headers
            queries: [Some(first + 20), Some(first + 99)]
                .into_iter()
                .flat_map(|h| {
                    POSITIONS
                        .map(|pos| Query::State(pos, h))
                        .into_iter()
                        .chain([Query::ComputeBlockVersion(h), Query::SignalsStage(h)])
                })
                .collect(),
        };
        rows.push(("mainnet_never_active".into(), s));
    }

    // genesis's null prev, and a query at genesis
    {
        let mut s = regtest(&[], vec![run(3, TOP | BIT_DUMMY, T0)], vec![]);
        s.queries = sweep([None, Some(0), Some(2)]);
        rows.push(("genesis".into(), s));
    }
    rows
}
