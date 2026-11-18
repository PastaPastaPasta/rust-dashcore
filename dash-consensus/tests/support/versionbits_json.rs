//! The JSON form of CS4's versionbits scenarios and answers (`tests/data/oracle/versionbits.json`),
//! shared by the recorder (`node-l1`'s `versionbits_record` example) and this crate's replay.
//!
//! A case is `{"label", "network", "args", "first_height", "headers": [[count, version, time,
//! step], ...], "signals": [[height, [[bit, signal height], ...]], ...], "queries": [[op, pos,
//! height], ...], "core": [answer, ...]}`. `op` is `state`, `since`, `statistics`, `version` or
//! `stage`; `pos` is the deployment's name (`null` for `version` and `stage`); `height` is
//! `null` for `nullptr`. Answers: a state's name in `getdeploymentinfo`'s spelling, an integer,
//! `{"period", "threshold", "elapsed", "count", "possible", "blocks"}` with `blocks` in
//! `getdeploymentinfo`'s `#`/`-` spelling, or `[[bit, height], ...]`.

#![allow(dead_code, reason = "each includer uses a subset")]

use dash_params::DeploymentPos;
use serde_json::{Value, json};

use super::versionbits::{Answer, Network, POSITIONS, Query, Run, Scenario};

/// `ThresholdState`'s names in `getdeploymentinfo`, by Core's enum value.
pub const STATE_NAMES: [&str; 5] = ["defined", "started", "locked_in", "active", "failed"];

fn int<T: TryFrom<i64>>(v: &Value) -> T {
    T::try_from(v.as_i64().expect("an integer")).ok().expect("in range")
}

fn height(v: &Value) -> Option<u32> {
    (!v.is_null()).then(|| int(v))
}

fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v[key].as_array().unwrap_or_else(|| panic!("{key}"))
}

fn pos(v: &Value) -> DeploymentPos {
    let name = v.as_str().expect("a deployment name");
    POSITIONS.into_iter().find(|p| p.name() == name).expect("a deployment")
}

pub fn scenario_to_json(s: &Scenario) -> Value {
    let queries: Vec<Value> = s
        .queries
        .iter()
        .map(|q| match *q {
            Query::State(p, h) => json!(["state", p.name(), h]),
            Query::StateSinceHeight(p, h) => json!(["since", p.name(), h]),
            Query::Statistics(p, h) => json!(["statistics", p.name(), h]),
            Query::ComputeBlockVersion(h) => json!(["version", null, h]),
            Query::SignalsStage(h) => json!(["stage", null, h]),
        })
        .collect();
    json!({
        "network": s.network.core_name(),
        "args": s.args,
        "first_height": s.first_height,
        "headers": s.headers.iter().map(|r| json!([r.count, r.version, r.time, r.step])).collect::<Vec<_>>(),
        "signals": s.signals,
        "queries": queries,
    })
}

pub fn scenario_from_json(v: &Value) -> Scenario {
    Scenario {
        network: Network::from_core_name(v["network"].as_str().expect("network"))
            .expect("a network"),
        args: list(v, "args").iter().map(|a| a.as_str().expect("an argument").to_owned()).collect(),
        first_height: int(&v["first_height"]),
        headers: list(v, "headers")
            .iter()
            .map(|r| Run {
                count: int(&r[0]),
                version: int(&r[1]),
                time: int(&r[2]),
                step: int(&r[3]),
            })
            .collect(),
        signals: list(v, "signals")
            .iter()
            .map(|c| {
                let set = c[1].as_array().expect("signals");
                (int(&c[0]), set.iter().map(|p| (int(&p[0]), int(&p[1]))).collect())
            })
            .collect(),
        queries: list(v, "queries")
            .iter()
            .map(|q| {
                let h = height(&q[2]);
                match q[0].as_str().expect("an op") {
                    "state" => Query::State(pos(&q[1]), h),
                    "since" => Query::StateSinceHeight(pos(&q[1]), h),
                    "statistics" => Query::Statistics(pos(&q[1]), h),
                    "version" => Query::ComputeBlockVersion(h),
                    "stage" => Query::SignalsStage(h),
                    other => panic!("unknown op {other}"),
                }
            })
            .collect(),
    }
}

pub fn answer_to_json(a: &Answer) -> Value {
    match a {
        Answer::State(s) => json!(STATE_NAMES[usize::from(*s)]),
        Answer::StateSinceHeight(h) => json!(h),
        Answer::Statistics {
            period,
            threshold,
            elapsed,
            count,
            possible,
            blocks,
        } => json!({
            "period": period,
            "threshold": threshold,
            "elapsed": elapsed,
            "count": count,
            "possible": possible,
            "blocks": blocks.iter().map(|&b| if b { '#' } else { '-' }).collect::<String>(),
        }),
        Answer::ComputeBlockVersion(v) => json!(v),
        Answer::SignalsStage(s) => json!(s),
    }
}

/// The answer to `query` from its JSON form.
pub fn answer_from_json(query: Query, v: &Value) -> Answer {
    match query {
        Query::State(..) => {
            let name = v.as_str().expect("a state");
            Answer::State(STATE_NAMES.iter().position(|n| *n == name).expect("a state name") as u8)
        }
        Query::StateSinceHeight(..) => Answer::StateSinceHeight(int(v)),
        Query::Statistics(..) => Answer::Statistics {
            period: int(&v["period"]),
            threshold: int(&v["threshold"]),
            elapsed: int(&v["elapsed"]),
            count: int(&v["count"]),
            possible: v["possible"].as_bool().expect("possible"),
            blocks: v["blocks"].as_str().expect("blocks").chars().map(|c| c == '#').collect(),
        },
        Query::ComputeBlockVersion(..) => Answer::ComputeBlockVersion(int(v)),
        Query::SignalsStage(..) => Answer::SignalsStage(
            v.as_array().expect("signals").iter().map(|p| (int(&p[0]), int(&p[1]))).collect(),
        ),
    }
}

/// A recorded case: its label, scenario and Core's answers.
pub fn case_from_json(v: &Value) -> (String, Scenario, Vec<Answer>) {
    let scenario = scenario_from_json(v);
    let core = list(v, "core");
    assert_eq!(core.len(), scenario.queries.len(), "one answer per query");
    let answers = scenario.queries.iter().zip(core).map(|(&q, a)| answer_from_json(q, a)).collect();
    (v["label"].as_str().expect("a label").to_owned(), scenario, answers)
}

/// The recorded case `label` of `cases`, with Core's answers, after checking that the node
/// reproduces them.
pub fn recorded_case(cases: &[Value], label: &str) -> (Scenario, Vec<Answer>) {
    let value = cases.iter().find(|c| c["label"] == label).unwrap_or_else(|| panic!("{label}"));
    let (_, scenario, core) = case_from_json(value);
    assert_eq!(scenario.ours().as_ref(), Ok(&core), "{label}: the node differs from Core");
    (scenario, core)
}
