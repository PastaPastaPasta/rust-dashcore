//! `diff_check_special_tx` (EV1 §6.2): the generator's seeds, each mutation alone, the D19
//! pairs and a fixed-seed corpus, Core's `CheckSpecialTx` against ours
//! ([`node_l1::specialtx`]).

use std::collections::{BTreeMap, BTreeSet};

use dash_consensus::primitives::Tx;
use dash_consensus::subsidy::COIN;
use dash_consensus::validation::TxValidationResult;
use dash_dml::specialtx::covenant::SHARED_COLLATERAL_SCRIPT as TEMPLATE;
use dash_dml::specialtx::reasons::{
    BAD_PRODIS_TOO_EARLY, BAD_PROTX_COLLATERAL, BAD_PROTX_DUP_PLATFORMNODEID,
    BAD_PROTX_NETINFO_VERSION, BAD_PROTX_SIG, BAD_PROUPSHARE_TOO_EARLY,
    BAD_PROUPSHAREDREG_TOO_EARLY, BAD_SHARED_COLLATERAL_CREATE,
};
use dash_oracle_sys::{Oracle, Pin};
use dash_primitives::{AddrV1, NetInfo, NetInfoV1, ServiceV1, TxOut, TxType};
use dash_types::codec::BaseCodec;
use node_l1::Outcome;
use node_l1::specialtx::{
    Applied, Case, FLAG_EARLY, FLAG_EVO_SHARES, FLAG_LEGACY, FLAG_SIGS, FLAG_THREE_SHARES,
    FLAG_V24, Mutation, Payload, TYPES, Verdicts, World, catalogue, compare, compare_in_block,
    external_key, p2pkh, plain_tx, seed, seed_inputs, serv_migration_case,
    serv_platform_node_id_case, variants, vary, verdicts, walked,
};

const BAD_SPECIAL: i32 = TxValidationResult::BadSpecial as i32;
const CONSENSUS: i32 = TxValidationResult::Consensus as i32;

/// Every flags byte, but those a world before V19 reads as another (V24 and EVO_SHARES off).
fn worlds() -> impl Iterator<Item = u8> {
    let all = FLAG_V24 | FLAG_EVO_SHARES | FLAG_EARLY | FLAG_THREE_SHARES | FLAG_SIGS | FLAG_LEGACY;
    (0..=all).filter(|f| f & FLAG_LEGACY == 0 || f & (FLAG_V24 | FLAG_EVO_SHARES) == 0)
}

/// Every seed of every world.
fn seeds() -> impl Iterator<Item = (u8, TxType, u8, Case)> {
    worlds().flat_map(|flags| {
        TYPES.into_iter().flat_map(move |t| {
            variants(t).filter_map(move |v| Some((flags, t, v, seed(World::new(flags), t, v)?)))
        })
    })
}

fn run(oracle: &Oracle, case: &Case) -> Verdicts {
    verdicts(oracle, case).expect("no divergence in the harness").expect("a context Core takes")
}

/// The reason a shared type gets before V24.
fn too_early(tx_type: TxType) -> Option<&'static str> {
    match tx_type {
        TxType::ProviderDissolve => Some(BAD_PRODIS_TOO_EARLY),
        TxType::ProviderUpdateShare => Some(BAD_PROUPSHARE_TOO_EARLY),
        TxType::ProviderUpdateSharedRegistrar => Some(BAD_PROUPSHAREDREG_TOO_EARLY),
        _ => None,
    }
}

/// Each seed is accepted by Core and by us; a shared type's before V24 is too early on both.
#[test]
fn seeds_are_accepted() {
    let oracle = Oracle::new(Pin::V24_0);
    let mut failures = Vec::new();
    let mut count = 0;
    for (flags, tx_type, variant, case) in seeds() {
        count += 1;
        let v = run(&oracle, &case);
        let expected = match too_early(tx_type) {
            Some(reason) if flags & FLAG_V24 == 0 => Some(reason),
            _ => None,
        };
        let core = v.core.as_ref().map(|(_, r)| r.as_str());
        if core != expected || v.core != v.ours {
            failures.push(format!("flags {flags:#x} {tx_type:?} variant {variant:#x}: {v:?}"));
        }
    }
    assert!(count > 1000, "{count} seeds");
    assert!(failures.is_empty(), "{} of {count}:\n{}", failures.len(), failures.join("\n"));
}

/// A variation (up to two extra inputs and outputs, the outputs before or after the
/// transaction's own) keeps every seed's verdict on both sides: no check of the type reads what
/// it adds, and an internal collateral's index follows its output.
#[test]
fn variations_keep_seed_verdicts() {
    let oracle = Oracle::new(Pin::V24_0);
    let shapes: Vec<u8> = (0..9u8).flat_map(|v| [v, v | 0x80]).collect();
    let mut failures = Vec::new();
    for (i, (flags, tx_type, variant, case)) in seeds().enumerate() {
        let base = run(&oracle, &case);
        for j in 0..3 {
            let variation = shapes[(3 * i + j) % shapes.len()];
            let mut varied = case.clone();
            vary(&mut varied, variation);
            let v = run(&oracle, &varied);
            if v.core != base.core || v.ours != base.ours {
                failures.push(format!(
                    "flags {flags:#x} {tx_type:?} variant {variant:#x} variation {variation:#x}: {v:?}"
                ));
            }
        }
    }
    // A collateral index past the outputs (1c:6i1) stays past them, whichever end gains them.
    let past = catalogue(TxType::ProviderRegister)
        .into_iter()
        .find(|m| m.step == "1c:6i1")
        .expect("1c:6i1");
    let mut moved = 0;
    for case in accepted_seeds(TxType::ProviderRegister).iter().step_by(5) {
        let mut c = case.clone();
        if !past.apply(&mut c, 0) {
            continue;
        }
        let base = run(&oracle, &c);
        for variation in [3u8, 6, 0x83, 0x86] {
            let mut varied = c.clone();
            vary(&mut varied, variation);
            let v = run(&oracle, &varied);
            moved += 1;
            if v.core != base.core || v.ours != base.ours {
                failures.push(format!("1c:6i1 variation {variation:#x}: {base:?} became {v:?}"));
            }
        }
    }
    assert!(moved > 0, "no seed took 1c:6i1");
    assert!(failures.is_empty(), "{}:\n{}", failures.len(), failures.join("\n"));
}

/// The seeds both sides accept, per type.
fn accepted_seeds(tx_type: TxType) -> Vec<Case> {
    seeds()
        .filter(|(flags, t, ..)| {
            *t == tx_type && (flags & FLAG_V24 != 0 || too_early(*t).is_none())
        })
        .map(|(.., case)| case)
        .collect()
}

/// Up to `n` items of `items`, spread over it.
fn spread<T>(items: Vec<T>, n: usize) -> Vec<T> {
    let step = items.len().div_ceil(n).max(1);
    items.into_iter().step_by(step).collect()
}

/// Each mutation alone, on seeds that carry it: Core rejects with one of its step's reasons, and
/// we agree. Every mutation of the catalogue is carried by some seed.
#[test]
fn each_mutation_alone_rejects_at_its_step() {
    let oracle = Oracle::new(Pin::V24_0);
    let (mut failures, mut carried_steps, mut runs) = (Vec::new(), BTreeMap::new(), 0);
    for tx_type in TYPES {
        let seeds = accepted_seeds(tx_type);
        for mutation in catalogue(tx_type) {
            let mut carried = false;
            for arg in 0..4u8 {
                let cases: Vec<Case> = seeds
                    .iter()
                    .filter_map(|s| {
                        let mut c = s.clone();
                        mutation.apply(&mut c, arg).then_some(c)
                    })
                    .collect();
                carried |= !cases.is_empty();
                for case in spread(cases, 6) {
                    runs += 1;
                    let v = run(&oracle, &case);
                    let hit = v
                        .core
                        .as_ref()
                        .is_some_and(|(_, r)| mutation.reasons.contains(&r.as_str()));
                    if !hit || v.core != v.ours {
                        failures.push(format!("{} arg {arg}: {v:?}", mutation.step));
                    }
                }
            }
            *carried_steps.entry(mutation.step).or_insert(false) |= carried;
        }
    }
    failures.dedup();
    let uncarried: Vec<_> = carried_steps.iter().filter(|(_, c)| !**c).map(|(s, _)| s).collect();
    assert!(uncarried.is_empty(), "no seed carries {uncarried:?}");
    assert!(failures.is_empty(), "{} of {runs}:\n{}", failures.len(), failures.join("\n"));
}

/// A step's table: its label up to the colon.
fn table(step: &str) -> &str {
    step.split(':').next().unwrap_or(step)
}

/// The (argument, argument) pairs a step pair runs with: the entities they pick differ in some.
const ARGS: [(u8, u8); 4] = [(0, 0), (1, 1), (0, 3), (2, 0)];

/// A pair of steps, in the order applied, with their entities when in a list loop.
type Pair = (Applied, Applied);

/// Whether `b`'s entity is the one after `a`'s (`Some(true)`) or before it (`Some(false)`).
fn adjacent(a: &Applied, b: &Applied) -> Option<bool> {
    let (i, j) = (a.entity?, b.entity?);
    (i.abs_diff(j) == 1).then_some(j > i)
}

/// What the pair runs found.
#[derive(Default)]
struct PairRuns {
    reached: BTreeSet<Pair>,
    failures: Vec<String>,
    /// Core's reason is neither step's.
    neither: Vec<String>,
}

impl PairRuns {
    fn run(&mut self, oracle: &Oracle, a: &Mutation, b: &Mutation, case: &Case) {
        let v = run(oracle, case);
        if v.core != v.ours {
            self.failures.push(format!("{} then {}: {v:?}", a.step, b.step));
        }
        let Some((_, reason)) = &v.core else {
            self.failures.push(format!("{} then {}: Core accepts", a.step, b.step));
            return;
        };
        self.reached.insert((case.applied[0], case.applied[1]));
        if !a.reasons.contains(&reason.as_str()) && !b.reasons.contains(&reason.as_str()) {
            self.neither.push(format!("{} then {}: {reason}", a.step, b.step));
        }
    }
}

/// `a` then `b` on `seed` with `args`, when both apply.
fn applied(seed: &Case, a: &Mutation, b: &Mutation, (arg_a, arg_b): (u8, u8)) -> Option<Case> {
    let mut c = seed.clone();
    (a.apply(&mut c, arg_a) && b.apply(&mut c, arg_b)).then_some(c)
}

/// Whether two cases are the same: the transaction bytes, then (when those match) the world.
fn same(x: &Case, y: &Case) -> bool {
    let tx_bytes = |c: &Case| {
        let mut bytes = Vec::new();
        c.tx().raw().encode(&mut bytes);
        bytes
    };
    tx_bytes(x) == tx_bytes(y) && x.fingerprint() == y.fingerprint()
}

/// Whether `case` (`a` then `b` on `seed`) carries both defects. A step whose mutation rewrites
/// more than its own field, or cannot apply to its own result, checks its defect itself
/// (`Mutation::holds`). Any other step is idempotent (`unchecked_mutations_are_idempotent`), and
/// survives the other one when the case is not the other applied alone and applying the step
/// again changes nothing: the other neither took away what it changed nor overwrote it.
fn carries_both(
    seed: &Case,
    case: &Case,
    a: &Mutation,
    b: &Mutation,
    (arg_a, arg_b): (u8, u8),
) -> bool {
    let survives = |x: &Mutation, arg: u8, y: &Mutation, arg_y: u8| {
        if let Some(holds) = x.holds(case) {
            return holds;
        }
        let mut y_alone = seed.clone();
        if y.apply(&mut y_alone, arg_y) && same(&y_alone, case) {
            return false;
        }
        let mut again = case.clone();
        x.apply(&mut again, arg) && same(&again, case)
    };
    survives(a, arg_a, b, arg_b) && survives(b, arg_b, a, arg_a)
}

fn both(seed: &Case, a: &Mutation, b: &Mutation, args: (u8, u8)) -> Option<Case> {
    applied(seed, a, b, args).filter(|c| carries_both(seed, c, a, b, args))
}

/// Up to `n` of `seeds`, a stride of 37 apart: the seed kinds (a variant's low bits) and the
/// worlds all occur, where an even stride would keep to a few.
fn sample(seeds: &[Case], n: usize) -> Vec<Case> {
    (0..n.min(seeds.len())).map(|i| seeds[i * 37 % seeds.len()].clone()).collect()
}

/// The pair check's premise: a step that does not check its own defect is idempotent, so that
/// applying it again to a case shows whether the other step took away what it changed.
#[test]
fn unchecked_mutations_are_idempotent() {
    let mut failures = BTreeSet::new();
    for tx_type in TYPES {
        let seeds = sample(&accepted_seeds(tx_type), 64);
        for m in catalogue(tx_type).into_iter().filter(|m| !m.checks_itself()) {
            for s in &seeds {
                for arg in 0..16 {
                    let mut once = s.clone();
                    if !m.apply(&mut once, arg) {
                        continue;
                    }
                    let mut twice = once.clone();
                    if !(m.apply(&mut twice, arg) && same(&twice, &once)) {
                        failures.insert(format!("{} arg {arg}", m.step));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "not idempotent:\n{}",
        failures.into_iter().collect::<Vec<_>>().join("\n")
    );
}

/// D19's first reject: every ordered pair of steps of one table, applied to seeds that carry
/// both (the first found from a rotating start, per argument pair), through Core and us; and
/// for two steps in a list loop, on adjacent entities either way round (on 64 sampled seeds, each
/// argument pair below 16 until one carries it).
fn run_pairs(oracle: &Oracle) -> PairRuns {
    let mut runs = PairRuns::default();
    for tx_type in TYPES {
        let seeds = accepted_seeds(tx_type);
        let few = sample(&seeds, 64);
        let catalogue = catalogue(tx_type);
        let looped: BTreeSet<&str> = catalogue
            .iter()
            .filter(|m| {
                few.iter().any(|s| {
                    (0..16).any(|arg| {
                        let mut c = s.clone();
                        m.apply(&mut c, arg) && c.applied.iter().any(|x| x.entity.is_some())
                    })
                })
            })
            .map(|m| m.step)
            .collect();
        let mut rotate = 0usize;
        for a in &catalogue {
            for b in &catalogue {
                if a.step == b.step || table(a.step) != table(b.step) {
                    continue;
                }
                for args in ARGS {
                    rotate = rotate.wrapping_add(7919);
                    let carried = (0..seeds.len())
                        .find_map(|i| both(&seeds[(rotate + i) % seeds.len()], a, b, args));
                    if let Some(case) = carried {
                        runs.run(oracle, a, b, &case);
                    }
                }
                if !(looped.contains(a.step) && looped.contains(b.step)) {
                    continue;
                }
                for forward in [true, false] {
                    let have = runs.reached.iter().any(|(x, y)| {
                        x.step == a.step && y.step == b.step && adjacent(x, y) == Some(forward)
                    });
                    if have {
                        continue;
                    }
                    let carried = few.iter().find_map(|s| {
                        (0..=255u8).find_map(|args| {
                            let args = (args & 15, args >> 4);
                            let c = applied(s, a, b, args)?;
                            let fits = adjacent(&c.applied[0], &c.applied[1]) == Some(forward);
                            (fits && carries_both(s, &c, a, b, args)).then_some(c)
                        })
                    });
                    if let Some(case) = carried {
                        runs.run(oracle, a, b, &case);
                    }
                }
            }
        }
    }
    runs
}

/// `contrib/node/ev1-pairs.toml` (EV1 §6.2, D19), read in its restricted shape (D29): the steps
/// of each table with their reasons, the excluded steps, and the infeasible pairs.
struct PairTable {
    steps: BTreeMap<String, BTreeSet<String>>,
    /// The steps in a list loop.
    loops: BTreeSet<String>,
    excluded: BTreeSet<String>,
    infeasible: BTreeSet<(String, String)>,
}

/// An unordered pair as an ordered key.
fn unordered(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_owned(), b.to_owned())
    } else {
        (b.to_owned(), a.to_owned())
    }
}

fn pair_table() -> PairTable {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../contrib/node/ev1-pairs.toml");
    let text = std::fs::read_to_string(path).expect("ev1-pairs.toml");
    let mut table = PairTable {
        steps: BTreeMap::new(),
        loops: BTreeSet::new(),
        excluded: BTreeSet::new(),
        infeasible: BTreeSet::new(),
    };
    let (mut kind, mut array) = (String::new(), None::<String>);
    for line in text.lines() {
        if let Some(k) = line.strip_prefix("[[").and_then(|l| l.strip_suffix("]]")) {
            kind = k.to_owned();
        } else if let Some(key) = line.strip_suffix(" = [") {
            array = Some(key.to_owned());
        } else if line == "]" {
            array = None;
        } else if let Some(item) = line.strip_prefix("  \"").and_then(|l| l.strip_suffix("\",")) {
            let mut words = item.split(' ');
            let first = words.next().expect("a word").to_owned();
            match (kind.as_str(), array.as_deref()) {
                ("table", Some("steps")) => {
                    table.steps.insert(first, words.map(str::to_owned).collect());
                }
                ("table", Some("loops")) => {
                    table.loops.insert(first);
                }
                ("unreachable" | "elsewhere", Some("steps")) => {
                    table.excluded.insert(first);
                }
                ("infeasible" | "world", Some("pairs")) => {
                    table.infeasible.insert(unordered(&first, words.next().expect("a pair")));
                }
                _ => {}
            }
        }
    }
    table
}

/// D19's pair coverage against `ev1-pairs.toml`: the file declares every catalogue step with its
/// reasons, and excludes exactly the steps the catalogue leaves out; every pair of driven steps
/// of one table with different reasons is reached, unless it is listed infeasible; and no
/// infeasible pair is reached (a stale entry).
fn check_pair_coverage(reached: &BTreeSet<(String, String)>) -> Vec<String> {
    let file = pair_table();
    let mut problems = Vec::new();
    let mut catalogued = BTreeSet::new();
    for m in TYPES.into_iter().flat_map(catalogue) {
        catalogued.insert(m.step.to_owned());
        let reasons: BTreeSet<String> = m.reasons.iter().map(|r| (*r).to_owned()).collect();
        match file.steps.get(m.step) {
            Some(r) if *r == reasons => {}
            other => problems
                .push(format!("{}: catalogue {:?}, ev1-pairs.toml {other:?}", m.step, m.reasons)),
        }
    }
    let driven: Vec<&String> = file.steps.keys().filter(|s| !file.excluded.contains(*s)).collect();
    for s in &driven {
        if !catalogued.contains(*s) {
            problems.push(format!("{s}: in ev1-pairs.toml, neither catalogued nor excluded"));
        }
    }
    for s in &file.excluded {
        if catalogued.contains(s) {
            problems.push(format!("{s}: excluded in ev1-pairs.toml, but catalogued"));
        }
    }
    let mut by_table: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (i, a) in driven.iter().enumerate() {
        for b in &driven[i + 1..] {
            if table(a) != table(b) || file.steps[*a] == file.steps[*b] {
                continue;
            }
            let pair = unordered(a, b);
            let counts = by_table.entry(table(a)).or_default();
            match (file.infeasible.contains(&pair), reached.contains(&pair)) {
                (false, true) => counts.0 += 1,
                (false, false) => problems.push(format!("{a} {b}: not reached")),
                (true, true) => problems.push(format!("{a} {b}: listed infeasible, but reached")),
                (true, false) => counts.1 += 1,
            }
        }
    }
    for (t, (hit, infeasible)) in by_table {
        println!("pairs {t}: {hit} reached, {infeasible} infeasible");
    }
    problems
}

#[test]
fn pairs_first_reject_d19() {
    let oracle = Oracle::new(Pin::V24_0);
    let PairRuns {
        reached,
        failures,
        neither,
    } = run_pairs(&oracle);
    failures.iter().take(40).for_each(|f| println!("FAIL {f}"));
    neither.iter().take(200).for_each(|f| println!("NEITHER {f}"));
    let steps = reached.iter().map(|(a, b)| unordered(a.step, b.step)).collect();
    let coverage = check_pair_coverage(&steps);
    // Entity pairs: every two loop steps of a table with different reasons, on entities k and
    // k + 1 in both orders (D19). The loop steps are the file's, and the ones that named an
    // entity.
    let mut on: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut looped: BTreeSet<String> = BTreeSet::new();
    for (a, b) in &reached {
        for x in [a, b] {
            if x.entity.is_some() {
                looped.insert(x.step.to_owned());
            }
        }
        match adjacent(a, b) {
            Some(true) => on.insert((a.step, b.step)),
            Some(false) => on.insert((b.step, a.step)),
            None => false,
        };
    }
    let declared = pair_table().loops;
    let reasons: BTreeMap<&str, BTreeSet<&str>> = TYPES
        .into_iter()
        .flat_map(catalogue)
        .map(|m| (m.step, m.reasons.iter().copied().collect()))
        .collect();
    let mut by_table: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for step in &declared {
        by_table.entry(table(step)).or_default().push(step.as_str());
    }
    let mut missing = Vec::new();
    for (t, steps) in &by_table {
        let pairs = steps.iter().flat_map(|x| steps.iter().map(move |y| (*x, *y)));
        let distinct: Vec<(&str, &str)> =
            pairs.filter(|(x, y)| x != y && reasons.get(x) != reasons.get(y)).collect();
        let absent: Vec<String> = distinct
            .iter()
            .filter(|p| !on.contains(*p))
            .map(|(x, y)| format!("{x}@k {y}@k+1"))
            .collect();
        println!(
            "adjacent {t}: {} ordered pairs to reach, {} missing",
            distinct.len(),
            absent.len()
        );
        missing.extend(absent);
    }
    assert!(failures.is_empty(), "{} failures", failures.len());
    assert!(neither.is_empty(), "{} cases where Core's reason is neither step's", neither.len());
    assert!(coverage.is_empty(), "ev1-pairs.toml:\n{}", coverage.join("\n"));
    assert_eq!(looped, declared, "the loop steps that named an entity, against ev1-pairs.toml");
    assert!(missing.is_empty(), "adjacent entity steps not reached:\n{}", missing.join("\n"));
}

/// A splitmix64 step.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A fixed-seed corpus of random inputs (the fuzz target's form, up to three mutations): every
/// one agrees with Core, and both verdicts occur.
#[test]
fn random_inputs_agree() {
    let mut state = 0x0e71_3b00;
    let mut outcomes = BTreeMap::new();
    for _ in 0..3_000 {
        let len = 5 + (next(&mut state) % 8) as usize;
        let input: Vec<u8> = (0..len).map(|_| next(&mut state) as u8).collect();
        let outcome = compare(&input, Pin::V24_0);
        assert!(outcome.is_ok(), "{input:02x?}: {outcome:?}");
        *outcomes.entry(format!("{outcome:?}")).or_insert(0) += 1;
    }
    println!("{outcomes:?}");
    assert!(outcomes.contains_key("Ok(BothAccept)") && outcomes.contains_key("Ok(BothReject)"));
}

/// The public seeds' inputs agree with Core.
#[test]
fn seed_inputs_agree() {
    let inputs = seed_inputs();
    assert!(inputs.len() > 500, "{} seeds", inputs.len());
    for input in &inputs {
        let outcome = compare(input, Pin::V24_0);
        assert!(
            matches!(outcome, Ok(Outcome::BothAccept | Outcome::BothReject)),
            "{input:02x?}: {outcome:?}"
        );
    }
}

/// The block leg's outcome and Core's rejection, if any.
fn in_block(oracle: &Oracle, world: &World, txs: &[Tx]) -> (Outcome, Option<String>) {
    let d = compare_in_block(oracle, world, txs).expect("no divergence");
    (d.outcome, d.core.map(|(_, reason)| reason))
}

/// The seed of `tx_type` in a V24 world, by variant.
fn v24_seed(tx_type: TxType, variant: u8) -> Case {
    seed(World::new(FLAG_V24), tx_type, variant).expect("a seed")
}

/// Quirk 26 (EV1 D40, the L1 half): an external collateral is read from the view at the tip
/// (`specialtxman.cpp` rc.4:1245-1249), so a ProRegTx whose collateral an earlier transaction of
/// the same block creates is `bad-protx-collateral` in Core's check loop and in ours. With the
/// collateral at the tip both accept the block; alone, the transaction gets the same verdict.
#[test]
fn same_block_collateral_is_refused_quirk_26() {
    let oracle = Oracle::new(Pin::V24_0);
    // An external regular registration (variant 2).
    let at_tip = v24_seed(TxType::ProviderRegister, 2);
    assert_eq!(in_block(&oracle, &at_tip.world, &[at_tip.tx()]), (Outcome::BothAccept, None));

    let funding = plain_tx(vec![TxOut {
        value: 1000 * COIN,
        script_pubkey: p2pkh(external_key()).into(),
    }]);
    let mut same_block = at_tip.clone();
    same_block.fund_collateral_from(&funding);
    let tx = same_block.tx();
    let v = verdicts(&oracle, &same_block).unwrap().unwrap();
    assert_eq!(v.core, Some((BAD_SPECIAL, BAD_PROTX_COLLATERAL.to_owned())));
    assert_eq!(v.core, v.ours);
    assert_eq!(
        in_block(&oracle, &same_block.world, &[funding, tx]),
        (Outcome::BothReject, Some(BAD_PROTX_COLLATERAL.to_owned()))
    );
}

/// The covenant's template outputs (EV1 §6.4, §3.17) in Core's check loop and ours: only a shared
/// registration's collateral output pays the template, from V24 on.
#[test]
fn template_outputs_in_a_block() {
    let oracle = Oracle::new(Pin::V24_0);
    let template = || TxOut {
        value: COIN,
        script_pubkey: TEMPLATE.to_vec().into(),
    };
    let create = Some(BAD_SHARED_COLLATERAL_CREATE.to_owned());
    // A shared registration (variant 4) pays it at its collateral index; a second one is refused.
    let shared = v24_seed(TxType::ProviderRegister, 4);
    assert_eq!(in_block(&oracle, &shared.world, &[shared.tx()]), (Outcome::BothAccept, None));
    let mut twice = shared.clone();
    twice.outputs.push(template());
    assert_eq!(
        in_block(&oracle, &twice.world, &[twice.tx()]),
        (Outcome::BothReject, create.clone())
    );
    // Every other type, and a plain transaction, may not pay it.
    for (tx_type, variant) in [
        (TxType::ProviderRegister, 0),
        (TxType::ProviderRegister, 2),
        (TxType::ProviderUpdateRegistrar, 0),
        (TxType::ProviderUpdateShare, 0),
        (TxType::ProviderUpdateSharedRegistrar, 0),
    ] {
        let mut case = v24_seed(tx_type, variant);
        assert_eq!(
            in_block(&oracle, &case.world, &[case.tx()]),
            (Outcome::BothAccept, None),
            "{tx_type:?}"
        );
        case.outputs.push(template());
        assert_eq!(
            in_block(&oracle, &case.world, &[case.tx()]),
            (Outcome::BothReject, create.clone()),
            "{tx_type:?}"
        );
    }
    let world = World::new(FLAG_V24);
    assert_eq!(
        in_block(&oracle, &world, &[plain_tx(vec![template()])]),
        (Outcome::BothReject, create)
    );
    // Before V24 the covenant does not run.
    let world = World::new(0);
    assert_eq!(
        in_block(&oracle, &world, &[plain_tx(vec![template()])]),
        (Outcome::BothAccept, None)
    );
}

/// A dissolution in a block: the seed is accepted, a template output among its outputs is
/// refused by one of the two checks, alike.
#[test]
fn dissolutions_in_a_block() {
    let oracle = Oracle::new(Pin::V24_0);
    for variant in variants(TxType::ProviderDissolve) {
        let mut case = v24_seed(TxType::ProviderDissolve, variant);
        assert_eq!(in_block(&oracle, &case.world, &[case.tx()]), (Outcome::BothAccept, None));
        case.outputs.insert(
            0,
            TxOut {
                value: COIN,
                script_pubkey: TEMPLATE.to_vec().into(),
            },
        );
        let (outcome, reason) = in_block(&oracle, &case.world, &[case.tx()]);
        assert_eq!(outcome, Outcome::BothReject, "{reason:?}");
    }
}

/// Register quirk 21 (EV1-4): Core verifies an operator signature under the global BLS flag
/// (legacy until V19 is active, `bls.cpp` rc.4:296-312), not under the payload's version. A
/// version-1 service update and revocation of OLD, a legacy masternode, are accepted signed
/// under the flag's scheme before V19 and after, and refused `bad-protx-sig` signed under the
/// other one, by Core and by us.
#[test]
fn operator_sig_scheme_follows_the_flag_quirk_21() {
    let oracle = Oracle::new(Pin::V24_0);
    let mut checked = 0;
    for flags in [FLAG_SIGS | FLAG_LEGACY, FLAG_SIGS] {
        // Variant 2 names OLD at its own version (1).
        for tx_type in [TxType::ProviderUpdateService, TxType::ProviderUpdateRevoke] {
            let mut case = seed(World::new(flags), tx_type, 2).expect("a seed");
            let version = match &case.payload {
                Payload::UpServ(p) => p.version,
                Payload::UpRev(p) => p.version,
                _ => unreachable!("an operator-signed update"),
            };
            assert_eq!(version, 1, "{tx_type:?}");
            let signed = run(&oracle, &case);
            assert_eq!((signed.core.clone(), signed.ours.clone()), (None, None), "{signed:?}");
            case.other_scheme = true;
            let other = run(&oracle, &case);
            assert_eq!(other.core, Some((CONSENSUS, BAD_PROTX_SIG.to_owned())), "{other:?}");
            assert_eq!(other.core, other.ours);
            checked += 1;
        }
    }
    assert_eq!(checked, 4);
}

/// Our check loop's step tables admit every signature reject (EV1-4, Sol r1 M-1): with
/// `check_sigs` on, each signature step's mutation is refused at its step with Core's reason when
/// the transaction goes through `dml_checks::check_block`, where a reason its row lacked was a
/// harness divergence.
#[test]
fn check_loop_admits_signature_rejects() {
    let oracle = Oracle::new(Pin::V24_0);
    let flags = FLAG_V24 | FLAG_SIGS;
    let steps = [
        (TxType::ProviderRegister, "1c:14e"),
        (TxType::ProviderRegister, "1c:14s"),
        (TxType::ProviderUpdateService, "2c:13"),
        (TxType::ProviderUpdateRegistrar, "3c:14"),
        (TxType::ProviderUpdateRevoke, "4c:5"),
        (TxType::ProviderDissolve, "10l:13"),
        (TxType::ProviderUpdateShare, "11c:9"),
        (TxType::ProviderUpdateSharedRegistrar, "12c:9"),
    ];
    for (tx_type, step) in steps {
        let m = catalogue(tx_type).into_iter().find(|m| m.step == step).expect("a step");
        let case = variants(tx_type)
            .find_map(|variant| {
                let mut c = seed(World::new(flags), tx_type, variant)?;
                m.apply(&mut c, 0).then_some(c)
            })
            .expect("a seed the step applies to");
        let v = run(&oracle, &case);
        let core = v.core.clone().expect("Core refuses it");
        assert!(m.reasons.contains(&core.1.as_str()), "{step}: {v:?}");
        assert_eq!(v.core, v.ours, "{step}");
        let ours = walked(&case).unwrap_or_else(|d| panic!("{step}: {d}"));
        assert_eq!(ours.map(|(_, reason)| reason), Some(core.1.as_str()), "{step}");
    }
}

/// 2c:10 on the oracle (EV1-4, Opus r1 n-4): an Evo's service update naming the platform node id
/// another Evo holds is `bad-protx-dup-platformnodeid` in Core and in ours
/// (`specialtxman.cpp` rc.4:1458-1462); keeping its own is accepted.
#[test]
fn service_update_platform_node_id_held_by_another() {
    let oracle = Oracle::new(Pin::V24_0);
    let own = run(&oracle, &serv_platform_node_id_case(false).expect("a case"));
    assert_eq!((own.core.clone(), own.ours.clone()), (None, None), "{own:?}");
    let held = run(&oracle, &serv_platform_node_id_case(true).expect("a case"));
    let expected = Some((BAD_SPECIAL, BAD_PROTX_DUP_PLATFORMNODEID.to_owned()));
    assert_eq!(held.core, expected, "{held:?}");
    assert_eq!(held.core, held.ours);
}

/// D29's ProUpServ leg (EV1-4): a version-2 service update of a masternode at version 3 carries a
/// legacy address that `ApplyProUpServTx` raises to ExtNetInfo; Core and we accept a routable
/// port and refuse port 22 with the migration's reason.
#[test]
fn service_update_migration_both_ways() {
    let oracle = Oracle::new(Pin::V24_0);
    let verdict = |port: u16| {
        let mut ip = [0u8; 16];
        ip[10..].copy_from_slice(&[0xff, 0xff, 1, 2, 3, 200]);
        let net_info = NetInfo::Legacy(NetInfoV1(ServiceV1 {
            addr: AddrV1(ip),
            port,
        }));
        let case = serv_migration_case(false, 3, net_info, 2).expect("a case");
        run(&oracle, &case)
    };
    let accepted = verdict(19_899);
    assert_eq!(accepted.core, None, "{accepted:?}");
    assert_eq!(accepted.core, accepted.ours);
    let refused = verdict(22);
    assert_eq!(
        refused.core,
        Some((BAD_SPECIAL, BAD_PROTX_NETINFO_VERSION.to_owned())),
        "{refused:?}"
    );
    assert_eq!(refused.core, refused.ours);
}
