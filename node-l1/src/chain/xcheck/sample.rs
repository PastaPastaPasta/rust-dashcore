//! The X-check's sample (O2b-4 PLAN §5.3): seeds, projections, augmented and generated programs,
//! and the proposals each actually sent to dashd, by context, counted after resolution.

use std::collections::BTreeMap;

use dash_oracle_sys::{ChainOp, RestartKind};
use serde_json::Value;

use crate::chain::core::Schedule;
use crate::chain::intent::{Intent, Program, Ref};
use crate::chain::run::Traced;
use crate::chain::step::Step;

/// How a sampled program came about; counted apart in the evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    /// A public seed, replayed literally: every op it ran maps to dashd.
    Seed,
    /// A seed with its excluded intents removed.
    Projection,
    /// A seed, projection or generated program with proposals inserted ([`augment`]).
    Augmented,
    /// A deterministic draw ([`generated`]), its excluded intents removed.
    Generated,
}

impl Category {
    pub fn name(self) -> &'static str {
        match self {
            Category::Seed => "seed",
            Category::Projection => "projection",
            Category::Augmented => "augmented",
            Category::Generated => "generated",
        }
    }
}

/// Where an executed proposal stood (PLAN §5.3's seven contexts).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Context {
    OnTip,
    OffTip,
    AfterInvalidate,
    AfterKill,
    DuplicateAccepted,
    DuplicateInvalid,
    DuplicateInconclusive,
}

pub const CONTEXTS: [Context; 7] = [
    Context::OnTip,
    Context::OffTip,
    Context::AfterInvalidate,
    Context::AfterKill,
    Context::DuplicateAccepted,
    Context::DuplicateInvalid,
    Context::DuplicateInconclusive,
];

impl Context {
    pub fn name(self) -> &'static str {
        match self {
            Context::OnTip => "on_tip",
            Context::OffTip => "off_tip",
            Context::AfterInvalidate => "after_invalidate",
            Context::AfterKill => "after_kill",
            Context::DuplicateAccepted => "duplicate_accepted",
            Context::DuplicateInvalid => "duplicate_invalid",
            Context::DuplicateInconclusive => "duplicate_inconclusive",
        }
    }
}

/// The contexts of the proposals a traced run sent, in order. A proposal is counted when the
/// session answered it (not down) and no load was refused before it. Its context is the
/// duplicate its reply names; else the op before it (proposals, clock moves and flushes left
/// aside), an `INVALIDATE` or a `RESTART KILL`; else whether it builds on the tip.
// Core: src/rpc/mining.cpp:708-720 at v24.0.0 (the duplicate replies, then not on the tip)
pub fn contexts(restore: &Step, trace: &[Traced]) -> Vec<Context> {
    let mut tip = restore.post.tip.as_ref().map(|t| t.1.0);
    let mut last: Option<&ChainOp> = None;
    let mut out = Vec::new();
    for t in trace {
        if t.step.post.down {
            break;
        }
        match &t.op {
            ChainOp::Proposal(bytes) => {
                let context = match &t.step.reply.outcome {
                    Ok(Value::String(s)) if s == "duplicate" => Some(Context::DuplicateAccepted),
                    Ok(Value::String(s)) if s == "duplicate-invalid" => {
                        Some(Context::DuplicateInvalid)
                    }
                    Ok(Value::String(s)) if s == "duplicate-inconclusive" => {
                        Some(Context::DuplicateInconclusive)
                    }
                    _ => None,
                };
                let prev: Option<[u8; 32]> = bytes.get(4..36).and_then(|b| b.try_into().ok());
                out.push(context.unwrap_or(match last {
                    Some(ChainOp::Invalidate(_)) => Context::AfterInvalidate,
                    Some(ChainOp::Restart(RestartKind::Kill)) => Context::AfterKill,
                    _ if prev.is_some() && prev == tip => Context::OnTip,
                    _ => Context::OffTip,
                }));
            }
            ChainOp::SetTime(_)
            | ChainOp::Flush {
                ..
            } => {}
            op => last = Some(op),
        }
        tip = t.step.post.tip.as_ref().map(|b| b.1.0);
    }
    out
}

/// Whether an intent has no dashd counterpart at M3: `ACTIVATE`, a `FLUSH` other than
/// `ALWAYS`, `CL_RECORD` and `CL_ENFORCE`.
pub fn excluded(intent: &Intent) -> bool {
    matches!(intent, Intent::Activate | Intent::Cl(_) | Intent::ClEnforce)
        || matches!(intent, Intent::Flush(m) if m % 3 != 0)
}

/// `program` without its excluded intents.
pub fn project(program: &Program) -> Program {
    Program {
        old: program.old,
        intents: program.intents.iter().filter(|i| !excluded(i)).cloned().collect(),
    }
}

/// A new block proposed on the active chain `k` below its tip.
fn propose_new(k: u8) -> Intent {
    Intent::Propose {
        on: Ref(k),
        mutate: 0,
        again: false,
    }
}

/// A block's own bytes proposed again.
fn propose_again(on: Ref) -> Intent {
    Intent::Propose {
        on,
        mutate: 0,
        again: true,
    }
}

/// `program` with proposals at PLAN §5.3's contexts: after each `INVALIDATE`, its target again
/// (a block it failed) and a new block on the tip; after each `RESTART KILL`, a new block on the
/// tip; and at the end a new block on the tip and one below it, the tip again, the first two
/// blocks the input built again and the first branch's tip again.
pub fn augment(program: &Program) -> Program {
    let mut intents = Vec::new();
    for intent in &program.intents {
        intents.push(intent.clone());
        match intent {
            Intent::Invalidate(r) => intents.extend([propose_again(*r), propose_new(0)]),
            Intent::Restart(k) if k % 2 == 1 => intents.push(propose_new(0)),
            _ => {}
        }
    }
    intents.extend([
        propose_new(0),
        propose_new(1),
        propose_again(Ref(0)),
        propose_again(Ref(64)),
        propose_again(Ref(65)),
        propose_again(Ref(128)),
    ]);
    Program {
        old: program.old,
        intents,
    }
}

/// 64-bit FNV-1a.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// The `i`th generated draw on `schedule`: FNV-1a of `xcheck/<schedule>/<i>` seeds SplitMix64,
/// whose bytes (16 to 400 of them) decode as `chain-v2`; the excluded intents are removed.
pub fn generated(schedule: Schedule, i: u64) -> Program {
    let mut x = fnv1a(format!("xcheck/{}/{i}", schedule.name()).as_bytes());
    let mut next = move || {
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let len = 16 + usize::try_from(next() % 385).expect("below 385");
    let bytes: Vec<u8> = (0..len).map(|_| next().to_le_bytes()[0]).collect();
    project(&Program::decode_v2(&bytes))
}

/// What running one program on the session gave the sampler.
pub enum Executed<T> {
    /// Its run, and the contexts of the proposals it sent.
    Ran(T, Vec<Context>),
    /// The run sent an op dashd does not run at M3 (the reason).
    Excluded(&'static str),
    /// The run fired a tie taint (D30): discarded.
    Tainted,
}

/// One sampled program.
pub struct Sampled<T> {
    pub category: Category,
    pub label: String,
    pub program: Program,
    pub run: T,
    pub contexts: Vec<Context>,
}

/// The per-schedule minimums: programs with at least one executed proposal, and executed
/// proposals in each context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Minimums {
    pub programs: usize,
    pub per_context: usize,
}

/// PLAN §5.3's.
pub const M3: Minimums = Minimums {
    programs: 200,
    per_context: 10,
};

/// The sample's counts beside its programs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub draws: u64,
    pub tainted: u64,
    /// Programs left out because their run sent an excluded op, by the op.
    pub excluded: BTreeMap<&'static str, u64>,
    /// Draws whose projection was empty.
    pub empty: u64,
    /// Sampled programs by how the session's run ended ([`END_KINDS`]): `done`, or cut short at
    /// a reply the generator did not expect, a session-NodeSut divergence or an op NodeSut
    /// refused.
    pub ended: BTreeMap<&'static str, u64>,
}

/// How a sampled program's session run ended: every intent ran, or the run stopped early.
pub const END_KINDS: [&str; 4] = ["done", "unexpected", "diverged", "unsupported"];

/// What became of one program the sampler ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Took {
    Ran,
    Excluded,
    Tainted,
}

/// A schedule's sample: each seed literally where its run maps whole, else, when it sent an
/// excluded op, its projection if not empty, and each of those augmented (a tainted seed is
/// dropped: its projection is mostly itself); then generated draws and their augmentations until
/// [`Minimums`] are met, giving up after `max_draws` draws. `exec` runs a program on the session.
pub fn sample<T>(
    schedule: Schedule,
    seeds: &[(String, Program)],
    min: Minimums,
    max_draws: u64,
    mut exec: impl FnMut(&Program) -> Result<Executed<T>, String>,
) -> Result<(Vec<Sampled<T>>, Tally), String> {
    let mut out: Vec<Sampled<T>> = Vec::new();
    let mut tally = Tally::default();
    let mut take = |category,
                    label: String,
                    program: Program,
                    out: &mut Vec<Sampled<T>>,
                    tally: &mut Tally| {
        Ok::<Took, String>(match exec(&program)? {
            Executed::Ran(run, contexts) => {
                out.push(Sampled {
                    category,
                    label,
                    program,
                    run,
                    contexts,
                });
                Took::Ran
            }
            Executed::Excluded(why) => {
                *tally.excluded.entry(why).or_default() += 1;
                Took::Excluded
            }
            Executed::Tainted => {
                tally.tainted += 1;
                Took::Tainted
            }
        })
    };
    for (name, seed) in seeds {
        let base = match take(Category::Seed, name.clone(), seed.clone(), &mut out, &mut tally)? {
            Took::Ran => Some(seed.clone()),
            Took::Excluded => {
                let projection = project(seed);
                let label = format!("{name}/projection");
                (!projection.intents.is_empty()
                    && take(Category::Projection, label, projection.clone(), &mut out, &mut tally)?
                        == Took::Ran)
                    .then_some(projection)
            }
            Took::Tainted => None,
        };
        if let Some(base) = base {
            take(
                Category::Augmented,
                format!("{name}/augmented"),
                augment(&base),
                &mut out,
                &mut tally,
            )?;
        }
    }
    let met = |out: &[Sampled<T>]| {
        let mut per: BTreeMap<Context, usize> = BTreeMap::new();
        for c in out.iter().flat_map(|s| &s.contexts) {
            *per.entry(*c).or_default() += 1;
        }
        out.iter().filter(|s| !s.contexts.is_empty()).count() >= min.programs
            && CONTEXTS.iter().all(|c| per.get(c).copied().unwrap_or(0) >= min.per_context)
    };
    let mut i = 0;
    while !met(&out) {
        if tally.draws >= max_draws {
            return Err(format!(
                "{}: the minimums are not met after {max_draws} draws: {}",
                schedule.name(),
                summary(&out)
            ));
        }
        tally.draws += 1;
        let program = generated(schedule, i);
        let label = format!("xcheck/{}/{i}", schedule.name());
        i += 1;
        if program.intents.is_empty() {
            tally.empty += 1;
            continue;
        }
        if take(Category::Generated, label.clone(), program.clone(), &mut out, &mut tally)?
            == Took::Ran
        {
            take(
                Category::Augmented,
                format!("{label}/augmented"),
                augment(&program),
                &mut out,
                &mut tally,
            )?;
        }
    }
    Ok((out, tally))
}

/// Programs with a proposal and the executed proposals per context.
pub fn summary<T>(sampled: &[Sampled<T>]) -> String {
    let with = sampled.iter().filter(|s| !s.contexts.is_empty()).count();
    let per: Vec<String> = CONTEXTS
        .iter()
        .map(|c| {
            format!(
                "{} {}",
                c.name(),
                sampled.iter().flat_map(|s| &s.contexts).filter(|x| *x == c).count()
            )
        })
        .collect();
    format!("{with} programs with a proposal; {}", per.join(", "))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use dash_oracle_sys::{ChainHash, ChainOp, Post, Reply, RestartKind};
    use serde_json::{Value, json};

    use super::{
        CONTEXTS, Category, Context, Executed, Minimums, augment, contexts, generated, project,
        sample,
    };
    use crate::chain::core::Schedule;
    use crate::chain::intent::{Intent, Program, Ref};
    use crate::chain::run::Traced;
    use crate::chain::step::Step;

    fn post(tip: [u8; 32], down: bool) -> Post {
        let mut v = json!({
            "best_cl": null, "best_header": null, "deployments": {}, "down": false,
            "flush_delta": {"full": {}, "index_only": {}}, "flushes": {"full": {}, "index_only": {}},
            "ibd": false, "index": "00".repeat(32), "sporks": [], "tie": [],
            "tip": [1, format!("{:?}", ChainHash(tip))], "tips": [],
            "utxo": {"amount": 0, "hash": "00".repeat(32), "txouts": 0},
        });
        if down {
            v = json!({"down": true, "flush_delta": {"full": {}, "index_only": {}}, "tie": []});
        }
        serde_json::from_value(v).expect("a post")
    }

    fn step(tip: [u8; 32], reply: Result<Value, &str>, down: bool) -> Step {
        let outcome = reply.map_err(|m| dash_oracle_sys::RpcError {
            code: None,
            message: m.to_owned(),
        });
        Step {
            reply: Reply {
                checked: Vec::new(),
                outcome,
            },
            records: Vec::new(),
            events: Vec::new(),
            post: post(tip, down),
        }
    }

    /// A block's bytes whose header's previous hash is `prev`.
    fn block_on(prev: [u8; 32]) -> Vec<u8> {
        let mut b = vec![0; 80];
        b[4..36].copy_from_slice(&prev);
        b
    }

    fn traced(op: ChainOp, step: Step) -> Traced {
        Traced {
            op,
            step,
            muhash: None,
        }
    }

    /// 4f test 4: proposals are counted after resolution, in each of the seven contexts; one after
    /// a refused load, or answered by a down session, is not.
    #[test]
    fn executed_contexts_are_counted_after_resolution() {
        let (a, b) = ([1; 32], [2; 32]);
        let null = || Ok(Value::Null);
        let s = |r: &str| Ok(Value::String(r.to_owned()));
        let restore = step(a, null(), false);
        let trace = vec![
            traced(ChainOp::Proposal(block_on(a)), step(a, null(), false)),
            traced(
                ChainOp::Proposal(block_on(b)),
                step(a, s("inconclusive-not-best-prevblk"), false),
            ),
            traced(ChainOp::Proposal(block_on(a)), step(a, s("duplicate"), false)),
            traced(ChainOp::Proposal(block_on(a)), step(a, s("duplicate-invalid"), false)),
            traced(ChainOp::Proposal(block_on(a)), step(a, s("duplicate-inconclusive"), false)),
            traced(ChainOp::Invalidate(a), step(b, null(), false)),
            traced(ChainOp::SetTime(9), step(b, null(), false)),
            traced(ChainOp::Proposal(block_on(b)), step(b, null(), false)),
            traced(
                ChainOp::Restart(RestartKind::Kill),
                step(b, Ok(json!({"status": "success"})), false),
            ),
            traced(ChainOp::Proposal(block_on(b)), step(b, null(), false)),
            traced(ChainOp::Block(block_on(b)), step(b, null(), false)),
            traced(ChainOp::Proposal(block_on(b)), step(b, null(), false)),
            traced(
                ChainOp::Restart(RestartKind::Clean),
                step(b, Ok(json!({"status": "failure"})), true),
            ),
            traced(ChainOp::Proposal(block_on(b)), step(b, Err("session-down"), true)),
        ];
        assert_eq!(
            contexts(&restore, &trace),
            [
                Context::OnTip,
                Context::OffTip,
                Context::DuplicateAccepted,
                Context::DuplicateInvalid,
                Context::DuplicateInconclusive,
                Context::AfterInvalidate,
                Context::AfterKill,
                Context::OnTip,
            ]
        );
        // A proposal intent that resolved to nothing sent no op, so none is counted.
        assert!(contexts(&restore, &[]).is_empty());
    }

    /// 4f test 2: a projection drops exactly the excluded intents; an all-excluded seed projects
    /// to nothing.
    #[test]
    fn projections_drop_excluded_ops_and_keep_the_rest() {
        let p = Program::parse("extend main 2\nflush if_needed\nflush always\nactivate\ncl main@-0\ncl_enforce\nrestart kill\n").unwrap();
        let projected = project(&p);
        assert_eq!(projected.intents.len(), 3);
        assert!(matches!(projected.intents[1], Intent::Flush(m) if m % 3 == 0));
        let all = Program::parse("activate\nflush periodic\n").unwrap();
        assert!(project(&all).intents.is_empty());
        let w = Program::parse(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/chain_seeds/w_p3b.prog"
            ))
            .unwrap(),
        )
        .unwrap();
        let wp = project(&w);
        assert!(!wp.intents.is_empty());
        assert!(
            !wp.intents
                .iter()
                .any(|i| matches!(i, Intent::Propose { .. } | Intent::ProposeX { .. })),
            "W-P3b has no proposal"
        );
        let aug = augment(&Program::parse("invalidate main@-0\nrestart kill\n").unwrap());
        assert!(matches!(
            aug.intents[1],
            Intent::Propose {
                again: true,
                ..
            }
        ));
        assert!(matches!(
            aug.intents[2],
            Intent::Propose {
                on: Ref(0),
                again: false,
                ..
            }
        ));
        assert!(matches!(
            aug.intents[4],
            Intent::Propose {
                on: Ref(0),
                again: false,
                ..
            }
        ));
    }

    /// 4f test 3: the same draws twice, per schedule, and the schedules' draws differ.
    #[test]
    fn generated_draws_are_deterministic() {
        for schedule in [Schedule::S0, Schedule::S1, Schedule::S2] {
            let a: Vec<Program> = (0..32).map(|i| generated(schedule, i)).collect();
            let b: Vec<Program> = (0..32).map(|i| generated(schedule, i)).collect();
            assert_eq!(a, b, "{}", schedule.name());
            assert!(a.iter().all(|p| !p.intents.iter().any(super::excluded)));
        }
        assert_ne!(generated(Schedule::S0, 0), generated(Schedule::S1, 0));
    }

    /// 4f test 5: with a fake executor, the sampler keeps drawing until every minimum is met, and
    /// fails past its draw budget; a seed is projected only when its run sent an excluded op.
    #[test]
    fn the_sampler_redraws_until_the_minimums() {
        let min = Minimums {
            programs: 12,
            per_context: 3,
        };
        let (mut calls, mut sent) = (0, 0);
        // Every third program sends one proposal, the contexts in turn; every fifth is tainted;
        // every seventh sends an excluded op.
        let exec = |_: &Program| {
            calls += 1;
            Ok(match calls {
                c if c % 5 == 0 => Executed::Tainted,
                c if c % 7 == 0 => Executed::Excluded("activate"),
                c if c % 3 == 0 => {
                    sent += 1;
                    Executed::Ran((), vec![CONTEXTS[sent % 7]])
                }
                _ => Executed::Ran((), Vec::new()),
            })
        };
        let seed = Program::parse("extend main 1\n").unwrap();
        let (out, tally) =
            sample(Schedule::S0, &[("basic".into(), seed)], min, 1000, exec).unwrap();
        assert!(out.iter().filter(|s| !s.contexts.is_empty()).count() >= 12);
        for c in CONTEXTS {
            assert!(
                out.iter().flat_map(|s| &s.contexts).filter(|x| **x == c).count() >= 3,
                "{c:?}"
            );
        }
        assert!(tally.draws > 0 && tally.tainted > 0 && tally.excluded["activate"] > 0);
        assert_eq!(out[0].category, Category::Seed);
        assert!(out.iter().any(|s| s.category == Category::Generated));
        // Each draw runs its program and that program augmented: 100 calls at most in 50 draws.
        let mut calls = 0;
        let never = |_: &Program| {
            calls += 1;
            assert!(calls <= 100, "max_draws did not end the loop");
            Ok(Executed::<()>::Ran((), Vec::new()))
        };
        let err = sample(Schedule::S0, &[], min, 50, never).err().expect("unmet");
        assert!(err.contains("after 50 draws"), "{err}");

        // A tainted seed is dropped; an excluded one is projected. Each program after the
        // seeds sends one proposal in every context.
        let seed = Program::parse("extend main 1\n").unwrap();
        let seeds = [("tainted".into(), seed.clone()), ("excluded".into(), seed)];
        let mut calls = 0;
        let exec = |_: &Program| {
            calls += 1;
            Ok(match calls {
                1 => Executed::Tainted,
                2 => Executed::Excluded("activate"),
                _ => Executed::Ran((), CONTEXTS.to_vec()),
            })
        };
        let one = Minimums {
            programs: 1,
            per_context: 1,
        };
        let (out, tally) = sample(Schedule::S0, &seeds, one, 10, exec).unwrap();
        let labels: Vec<&str> = out.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["excluded/projection", "excluded/augmented"]);
        assert_eq!((tally.tainted, tally.excluded["activate"], tally.draws), (1, 1, 0));
    }
}
