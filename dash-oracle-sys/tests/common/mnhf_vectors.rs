//! What the MNHF step tests share, and dash-dml's MNHF step vectors
//! (`dash-dml/tests/data/mnhf/*.txt`): one line per `oracle_mnhf_step`, with everything
//! `ProcessBlock()` and the next block's `GetSignalsStage()` read, so EV3's port is checked
//! without an oracle. The format is in `dash-dml/tests/data/mnhf/README.md`.

use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, MnList, MnhfStep, Oracle, Rejection, ResultCode,
    Verdict,
};

/// A chain context of `headers` from `first_height` to the tip, with nothing else.
pub fn context(headers: &[[u8; 80]], first_height: i32) -> Context {
    Context::new(
        first_height,
        headers[first_height as usize..].to_vec(),
        false,
        vec![],
        0,
        Checkpoints::Disabled,
        CoinsView::Unchecked(vec![]),
        MnList::Absent,
        vec![],
    )
}

/// A `ProcessBlock()` reject: `BLOCK_CONSENSUS` with `reason` and no debug message.
pub fn reject(reason: &str) -> Verdict {
    Verdict::Reject(Rejection {
        result: ResultCode::Block(1),
        reason: reason.into(),
        debug_message: String::new(),
        reason_truncated: false,
        debug_truncated: false,
    })
}

/// Core's deployments (`vDeployments`) as `(bit, nStartTime, nTimeout, useEHF)`, and
/// `V20Height`.
fn chain_facts(oracle: &Oracle, chain: &Chain) -> (Vec<(i64, i64, i64, bool)>, i32) {
    let json: serde_json::Value =
        serde_json::from_str(&oracle.chainparams_json(chain).unwrap()).unwrap();
    let c = &json["consensus"];
    let deps = c["vDeployments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            let n = |k: &str| d[k].as_i64().unwrap_or_else(|| panic!("{k}"));
            (n("bit"), n("nStartTime"), n("nTimeout"), d["useEHF"].as_bool().unwrap())
        })
        .collect();
    (deps, c["V20Height"].as_i64().unwrap() as i32)
}

/// The chain's `V20Height`: Core stores a block's signals row only from there.
pub fn v20_height(oracle: &Oracle, chain: &Chain) -> i32 {
    chain_facts(oracle, chain).1
}

/// The median time past of the last of `times` (`CBlockIndex::GetMedianTimePast()`).
pub fn median_time_past(times: &[u32]) -> i64 {
    let mut last: Vec<u32> = times[times.len().saturating_sub(11)..].to_vec();
    last.sort_unstable();
    i64::from(last[last.len() / 2])
}

fn pairs(list: &[(u8, i32)]) -> String {
    if list.is_empty() {
        return "-".into();
    }
    list.iter().map(|(b, h)| format!("{b}:{h}")).collect::<Vec<_>>().join(" ")
}

/// A vector's block: at `height` with median time past `mtp`, its MNHF transactions signalling
/// `bits` in block order, stepped with `just_check`.
pub struct VectorBlock<'a> {
    pub name: &'a str,
    pub height: i32,
    pub mtp: i64,
    pub bits: &'a [u8],
    pub just_check: bool,
}

/// One vector line: the block and Core's step on it.
pub fn line(oracle: &Oracle, chain: &Chain, block: &VectorBlock, step: &MnhfStep) -> String {
    let VectorBlock {
        name,
        height,
        mtp,
        bits,
        just_check,
    } = *block;
    assert!(!name.contains('|'));
    let (deps, v20_height) = chain_facts(oracle, chain);
    let deps: Vec<String> =
        deps.iter().map(|(b, s, t, e)| format!("{b}:{s}:{t}:{}", u8::from(*e))).collect();
    let bits = match bits {
        [] => "-".into(),
        _ => bits.iter().map(u8::to_string).collect::<Vec<_>>().join(" "),
    };
    let verdict = match &step.verdict {
        Verdict::Accept => "ok".into(),
        Verdict::Reject(r) => r.reason.clone(),
    };
    let signals = step.signals.as_deref().map_or("none".into(), pairs);
    let after = step.connected.as_ref().map_or("none".into(), |c| pairs(&c.stage_after));
    // DeploymentActiveAfter(block, V20): the next block's height reaches V20Height.
    let v20 = height + 1 >= v20_height;
    format!(
        "{name} | deps {} | v20 {} | height {height} | mtp {mtp} | stage {} | bits {bits} | check {} | {verdict} | signals {signals} | after {after}",
        deps.join(" "),
        u8::from(v20),
        pairs(&step.stage_before),
        u8::from(just_check),
    )
}

/// The committed `file` holds `lines` under its header; `DASH_ORACLE_REGENERATE=1` rewrites
/// it.
pub fn check_or_write(oracle: &Oracle, file: &str, source: &str, lines: &[String]) {
    let path = format!("{}/../dash-dml/tests/data/mnhf/{file}", env!("CARGO_MANIFEST_DIR"));
    let text = format!(
        "# Written by dash-oracle-sys {source} (DASH_ORACLE_REGENERATE=1): every answer is Core's\n\
         # oracle_mnhf_step at the {} pin. Format: README.md.\n{}\n",
        oracle.pin(),
        lines.join("\n")
    );
    if std::env::var("DASH_ORACLE_REGENERATE").is_ok_and(|v| v == "1") {
        std::fs::write(&path, text).expect(&path);
        return;
    }
    let committed = std::fs::read_to_string(&path).expect(&path);
    assert_eq!(committed, text, "the oracle no longer computes {path}");
}
