//! CH7b-3 C6's strict replays (X7): four of dashd's recorded runs connect whole on the node's
//! seams with [`ReplayChainHelper`] under `Policy::Strict`, each tip attempt keyed to its block's
//! final connect, so every recorded query, each `cl_best` included, is asked in its place and
//! order, or the replay stops on the miss. Step 10's passes are then checked against what Core's
//! `CheckCbTxBestChainlock` (`stm:139-215`) makes of each CbTx and the best ChainLock the
//! recording holds for it, both decoded here, apart from the helper: shortcut 1 (`stm:152-158`),
//! shortcut 2 (`stm:187-193`), else the verify (`stm:202-208`), which the census counts.
//!
//! `feature_governance_cl` is partial by a declared gap (D18): it replays to 199 only, as at 200,
//! its first triggered superblock, the node refuses with D18's superblock text (EV3, GV1b).

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_statedigest::decisions::{Policy, RecordedDecisions};
use dash_statedigest::{BestChainLock, Hash256, Lines, Origin, Query, Record};

use super::{Run, node0};
use crate::chain::digest_tap::display;
use crate::connect::{AttemptChoice, MissSlot, ReplayChainHelper};
use crate::evo::cl::{AttemptClVerifier, ClPass};

/// Step 10's shortcuts, as C2's recorder keeps them.
type Shortcuts = Vec<(u32, ClPass)>;

/// The verifies, as C4's census keeps them.
type Checks = Vec<(u32, u32, Option<bool>)>;

/// `test`'s node0 `name`, decompressed.
fn read(test: &str, name: &str) -> Vec<u8> {
    let file = std::fs::File::open(node0(test).join(name)).expect(name);
    zstd::decode_all(file).expect("zstd")
}

/// Each block's `cl_best` answer at its last tip attempt, which for a block of the final chain is
/// its final connect, read from the decisions file's `A` records alone. A block is absent where
/// that attempt asked no `cl_best`.
fn recorded_best(decisions: &[u8]) -> BTreeMap<Hash256, Option<BestChainLock>> {
    let mut best = BTreeMap::new();
    for (line, record) in Lines::new(decisions) {
        let record = record.unwrap_or_else(|e| panic!("decisions line {line}: {e:?}"));
        let Record::Attempt(a) = record else {
            continue;
        };
        if a.id.org != Origin::Tip {
            continue;
        }
        let asked = a.q.iter().find_map(|q| match q {
            Query::ClBest(answer) => Some(answer.clone()),
            _ => None,
        });
        match asked {
            Some(answer) => best.insert(a.id.b, answer),
            None => best.remove(&a.id.b),
        };
    }
    best
}

/// Step 10's passes Core makes over `run`'s blocks to `to` from `best`: the shortcuts, in order,
/// and the verifies as the census records them. Only a CbTx with the ChainLock fields (version
/// 3) asks `cl_best`, and each such block's final connect asked it.
fn expected(
    run: &Run,
    to: u32,
    best: &BTreeMap<Hash256, Option<BestChainLock>>,
) -> (Shortcuts, Checks) {
    let (mut shortcuts, mut verifies) = (Vec::new(), Vec::new());
    for m in run.blocks.iter().take_while(|m| m.height <= to) {
        let h = m.height;
        let payload = m.block.txs()[0].to_mutable().extra_payload().clone();
        let asked = best.get(&display(m.hash()).expect("a hash"));
        assert_eq!(asked.is_some(), payload.len() >= 167, "{h}: cl_best asked past the gate");
        let (Some(best), true) = (asked, payload.len() >= 167) else {
            continue;
        };
        let (diff, sig) = (payload[70], &payload[71..167]);
        assert!(diff < 253, "a one-byte difference at {h}");
        if sig == [0; 96] {
            continue;
        }
        let cl_height = h - 1 - u32::from(diff);
        let at = |height: u32| {
            best.as_ref().is_some_and(|b| b.height == i64::from(height) && b.sig[..] == *sig)
        };
        if diff == 0 && at(h - 1) {
            shortcuts.push((h, ClPass::BestAtParent));
        } else if at(cl_height) {
            shortcuts.push((h, ClPass::OutdatedBest));
        } else {
            verifies.push((h, cl_height, Some(true)));
        }
    }
    (shortcuts, verifies)
}

/// `test` replayed strictly, to `to` or its tip: no decision miss, and step 10's shortcuts and
/// verifies are Core's. Returns the run and how many of each.
fn replays_strictly(test: &str, to: Option<u32>) -> (Run, usize, usize) {
    let bytes = read(test, "decisions.jsonl.zst");
    let decisions = RecordedDecisions::load(&read(test, "digest.jsonl.zst"), &bytes);
    let decisions = Arc::new(decisions.expect("the recording joins"));
    let miss = MissSlot::new();
    let helper = ReplayChainHelper::new(
        decisions,
        Policy::Strict,
        AttemptChoice::FinalConnect,
        miss.clone(),
    );
    let (verifier, census) = AttemptClVerifier::with_census();
    let run = Run::of(test, Box::new(helper), |_| Box::new(verifier));
    let to = to.unwrap_or_else(|| u32::try_from(run.blocks.len()).expect("a height"));
    run.connect_to(to);
    assert_eq!(miss.get(), None, "{test}: no decision miss");
    let (shortcuts, verifies) = expected(&run, to, &recorded_best(&bytes));
    assert!(shortcuts.len() + verifies.len() > 0, "{test}: a CbTx ChainLock past step 10");
    assert_eq!(run.counts.root_checks().cl_shortcuts, shortcuts, "{test}: shortcuts");
    assert_eq!(*census.checks.lock(), verifies, "{test}: verifies");
    (run, shortcuts.len(), verifies.len())
}

#[test]
fn cl_best_replays_strictly_on_feature_llmq_chainlocks() {
    let (_, shortcuts, verifies) = replays_strictly("feature_llmq_chainlocks", None);
    assert!(shortcuts > 0 && verifies > 0, "{shortcuts} shortcuts, {verifies} verifies");
}

#[test]
fn cl_best_replays_strictly_on_feature_llmq_rotation() {
    let (_, shortcuts, verifies) = replays_strictly("feature_llmq_rotation", None);
    assert!(shortcuts > 0 && verifies > 0, "{shortcuts} shortcuts, {verifies} verifies");
}

#[test]
fn cl_best_replays_strictly_on_feature_llmq_evo() {
    let (_, shortcuts, verifies) = replays_strictly("feature_llmq_evo", None);
    assert!(shortcuts > 0 && verifies > 0, "{shortcuts} shortcuts, {verifies} verifies");
}

/// Partial by a declared gap (D18): the replay runs to 199 only. 1 to 199 carry shortcuts alone,
/// no `ClVerifier` verify, so this run adds no verify coverage; the chainlocks, rotation and evo
/// replays carry that. 200, the first superblock the recording triggers, is refused with D18's
/// exact text: an `AbortNode`, the only one, with the tip left at 199.
///
/// The pin is a tripwire. When GV1b lands, superblock validation is built, 200 connects and this
/// test goes red; the replay must then run whole, like the other three.
#[test]
fn cl_best_replays_strictly_on_feature_governance_cl() {
    let (run, shortcuts, verifies) = replays_strictly("feature_governance_cl", Some(199));
    assert!(shortcuts > 0 && verifies == 0, "{shortcuts} shortcuts, {verifies} verifies");
    run.rig.submit_stopping(run.at(200), "the superblock at 200");
    let text = format!(
        "rdashd cannot validate block {} at height 200 on {}: superblock validation (EV3, GV1b) \
         is not built (ALLOWED-DIFFS D18)",
        run.at(200).hash(),
        run.p.network.network_id()
    );
    assert_eq!(run.kept.reports(), [(text.clone(), Some(text))], "one report, D18's");
    assert_eq!(run.kept.evo_tip(), Some(run.at(199).hash()), "the tip stays at 199");
}
