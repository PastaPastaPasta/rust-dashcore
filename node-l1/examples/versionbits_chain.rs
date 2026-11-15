//! CS4's real-chain check: the node's versionbits answers against Core's
//! (`oracle_versionbits_state`) on testnet's own headers and EHF signals, read from the
//! reference node with pure reads only by `contrib/node/cs4-versionbits-chain.py`. CS4 ran it at
//! tip 1571956: headers.bin sha256 63a1e32d...9568, signals.json 1b335b62...e142 (one signal,
//! bit 13 at 1565132).
//!
//! The window starts 2000 blocks below testnet's `MinBIP9WarningHeight` (Core's walk never goes
//! below it). Queried, in ascending order on one cache per side: every 576th block and the last
//! block of each 100-block v24 period with its neighbours, each with every deployment's state,
//! since-height and statistics, and the block version. The signal changes hold the signals mined
//! from 2 h before v24's start time on. An earlier bit-13/14 signal (Core's `IsValidMNActivation`
//! accepts one before its deployment's start, as an unknown fork bit) is expired at every stage
//! from its next block on, since its MTP is below the start, so leaving it out gives Core's
//! state answers. The stage query is left out: it would also report historic bits, which the
//! changes do not hold.
//!
//! `cargo run --release --config 'profile.release.package.dashd-masternode.debug-assertions=true' -p node-l1 --features oracle --example versionbits_chain --
//! <headers.bin> <signals.json>` prints one summary line and fails if any answer differs.

use node_l1::versionbits::core_answers_for;
use node_l1::versionbits::scenario::{Answer, Network, POSITIONS, Query, Run, Scenario};

const MIN_BIP9: u32 = 1_150_516;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).expect("headers");
    let signals: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&args[2]).expect("signals")).expect("json");
    assert_eq!(data.len() % 80, 0);
    let all: Vec<[u8; 80]> = data.chunks(80).map(|c| c.try_into().expect("80 bytes")).collect();
    let tip = all.len() as u32 - 1;
    let first = MIN_BIP9 - 2000;
    let headers: Vec<[u8; 80]> = all[first as usize..].to_vec();
    let field = |h: &[u8; 80], at: usize| u32::from_le_bytes(h[at..at + 4].try_into().expect("4"));

    // the signals held from each signal's block on (`ProcessBlock`'s map). A bit Core accepts
    // again was expired in the previous block's stage, so its new height replaces the old one.
    let mut held: Vec<(u8, i32)> = Vec::new();
    let mut changes = Vec::new();
    let mut found: Vec<(u32, u8)> = signals["signals"]
        .as_array()
        .expect("signals")
        .iter()
        .map(|s| {
            (s["height"].as_u64().expect("height") as u32, s["bit"].as_u64().expect("bit") as u8)
        })
        .collect();
    found.sort();
    for (height, bit) in found {
        held.retain(|&(b, _)| b != bit);
        held.push((bit, height as i32));
        held.sort();
        match changes.last_mut() {
            Some((h, set)) if *h == height => *set = held.clone(),
            _ => changes.push((height, held.clone())),
        }
    }

    let mut heights: Vec<u32> = (first + 200..=tip).filter(|h| h % 576 == 0).collect();
    let mut end = (MIN_BIP9 / 100) * 100 - 1;
    while end <= tip {
        heights.extend([end - 1, end, end + 1].into_iter().filter(|&h| h <= tip));
        end += 100;
    }
    heights.sort();
    heights.dedup();
    let mut queries = Vec::new();
    for &h in &heights {
        for pos in POSITIONS {
            queries.push(Query::State(pos, Some(h)));
            queries.push(Query::StateSinceHeight(pos, Some(h)));
            queries.push(Query::Statistics(pos, Some(h)));
        }
        queries.push(Query::ComputeBlockVersion(Some(h)));
    }

    let scenario = Scenario {
        network: Network::Test,
        args: vec![],
        first_height: first,
        headers: headers
            .iter()
            .map(|h| Run {
                count: 1,
                version: field(h, 0) as i32,
                time: field(h, 68),
                step: 0,
            })
            .collect(),
        signals: changes,
        queries,
    };
    let core = core_answers_for(&scenario, headers).expect("Core's answers");
    let ours = scenario.ours().expect("the node's answers");
    let differing = core.iter().zip(&ours).filter(|(a, b)| a != b).count();
    let mut states = [0usize; 5];
    for (q, a) in scenario.queries.iter().zip(&core) {
        if let (Query::State(..), Answer::State(s)) = (q, a) {
            states[usize::from(*s)] += 1;
        }
    }
    println!(
        "versionbits_chain testnet window {first}..={tip} heights={} queries={} signal_changes={:?} \
         core_states(defined,started,locked_in,active,failed)={states:?} differing={differing}",
        heights.len(),
        scenario.queries.len(),
        scenario.signals,
    );
    assert_eq!(core.len(), ours.len());
    assert_eq!(differing, 0, "the node differs from Core");
}
