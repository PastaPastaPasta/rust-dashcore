//! The O10 precedence predictor (O10 plan rev2.1 §4.3): which of several defects in one block
//! Core reports, from the call tree `l3-census.py calltree` scans out of the tag (§4.2,
//! `contrib/node/l3-census/precedence-v24.0.0.json`).
//!
//! A defect is an [`Instance`]: a row and its trigger, the index of every loop on the row's path
//! (the block's transaction index, the input index), measured from the block's bytes. The
//! reference walker ([`walk`]) defines the semantics: it executes a mode's tree in source order,
//! children by position, each loop iteration's whole body before the next, and the first
//! terminating row holding an instance at the current trigger fires and stops. With script
//! workers (`-par` > 1) a script failure is queued, not fired (the defer at v:2206), and the
//! queue's wait (ConnectBlock#7, v:2789-2791) fires if anything queued to it is pending.
//!
//! The execution-path key is the walk's visit order as a value: from the root to the firing node,
//! each node's position among its parent's children, a loop's position and then its iteration
//! ([`flat`]). [`argmin`] derives the winner from keys alone (the least key of any instance's
//! reachable firings, a deferred one taking its wait's key), and the seeded property test holds it
//! equal to the walker on random trees at both `-par` regimes. [`Model::predict`] reports the
//! walker's result, or an interaction from the declared rules only.

use std::collections::{BTreeMap, BTreeSet};

use bitcoin_hashes::sha256;
use serde::Deserialize;

use crate::chain::mutate::{Mode, Par};

/// The v24.0.0 model, compiled in beside the inventory it was scanned with.
pub const FIXTURE: &str = include_str!("../../../contrib/node/l3-census/precedence-v24.0.0.json");

/// One guard term: the attribute must have this value for the node to run.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Term(pub String, pub bool);

/// How a guard attribute gets its value (the fixture's `attrs`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Attr {
    /// A static value per mode (`fCheckPOW`: false on Proposal).
    Mode {
        values: BTreeMap<String, bool>,
    },
    /// A measured block or chain fact; `default` is the regtest regime's.
    Fact {
        default: bool,
    },
    /// True at the measured indices of `domain`'s current iteration (`coinbase`: index 0).
    Iter {
        domain: String,
        default: Vec<usize>,
    },
    /// True iff `-par` > 1 (script checks queued).
    Par,
    /// True wherever the walk gets (the checks before it passed).
    Pass,
}

/// A row's kind: a reject (terminating), a wrapper (never wins: the callee's row is the bound
/// occurrence), an RPC precheck (terminating) or the queue's wait (fires if anything is pending).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Reject,
    Wrapper,
    Pre,
    Wait,
}

/// One node of a tree, its callees expanded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub guards: Vec<Term>,
    /// Its line in Core's source at the tag (0 for a synthetic node).
    pub line: u32,
    pub what: What,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    /// A call; `pruned` keeps a re-call's position with no body.
    Call {
        callee: String,
        body: Vec<Node>,
        pruned: bool,
    },
    Loop {
        domain: String,
        start: usize,
        body: Vec<Node>,
    },
    /// A guarded group (`pruned`: an evo subtree kept for its position).
    Block {
        body: Vec<Node>,
        pruned: bool,
    },
    Row {
        key: String,
        kind: RowKind,
    },
    /// Leaves the function.
    Return,
    /// Ends the loop's iteration.
    Skip,
    /// The queued form of `row`, reported by the wait row `wait`.
    Defer {
        row: String,
        wait: String,
    },
}

impl Node {
    pub fn new(what: What) -> Node {
        Node {
            guards: Vec::new(),
            line: 0,
            what,
        }
    }

    pub fn guarded(mut self, guards: &[(&str, bool)]) -> Node {
        self.guards = guards.iter().map(|&(a, v)| Term(a.into(), v)).collect();
        self
    }

    fn body(&self) -> &[Node] {
        match &self.what {
            What::Call {
                body,
                ..
            }
            | What::Loop {
                body,
                ..
            }
            | What::Block {
                body,
                ..
            } => body,
            _ => &[],
        }
    }
}

/// One defect: a row and its trigger, the index of each loop on its path, outermost first, as
/// measured from the block's bytes (a10 §3.5 step 3's facts). The rest is the declared
/// interaction data (§4.3): the fields it touches, the guard attributes it changes, and the
/// transactions it removes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Instance {
    pub row: String,
    pub trigger: Vec<(String, usize)>,
    pub footprint: BTreeSet<String>,
    pub touches: BTreeSet<String>,
    pub removes: BTreeSet<usize>,
}

impl Instance {
    pub fn new(row: &str, trigger: &[(&str, usize)]) -> Instance {
        Instance {
            row: row.into(),
            trigger: trigger.iter().map(|&(d, i)| (d.into(), i)).collect(),
            ..Instance::default()
        }
    }
}

/// The measured facts a walk reads, over the attributes' defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub facts: BTreeMap<String, bool>,
    /// Per `iter` attribute, the indices of its domain where it holds.
    pub iter: BTreeMap<String, BTreeSet<usize>>,
    /// A domain's length (by default one past the highest index any instance or `iter`
    /// attribute names).
    pub lengths: BTreeMap<String, usize>,
}

/// One step of an execution path: a node's position among its parent's children, and a loop's
/// iteration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Step {
    pub pos: u32,
    pub iter: Option<u32>,
}

/// The execution-path key of a path: each step's position, a loop's position then its iteration
/// (§4.3). Keys compare lexicographically.
pub fn flat(path: &[Step]) -> Vec<u32> {
    let mut key = Vec::with_capacity(path.len() * 2);
    for step in path {
        key.push(step.pos);
        key.extend(step.iter);
    }
    key
}

/// What fired: the bound occurrence (a row key), its execution-path key, the instances it binds
/// (indices into the walk's instances), and whether it is a queued check reported at the wait.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fired {
    pub occurrence: String,
    pub key: Vec<u32>,
    pub instances: Vec<usize>,
    pub deferred: bool,
}

/// A walk's setting: the attributes, the mode's name, `-par` > 1, and the measured facts.
#[derive(Clone, Copy, Debug)]
pub struct Env<'a> {
    pub attrs: &'a BTreeMap<String, Attr>,
    pub mode: &'a str,
    pub parallel: bool,
    pub facts: &'a Facts,
}

impl Env<'_> {
    fn term(&self, term: &Term, ctx: &[(String, usize)]) -> Result<bool, String> {
        let attr =
            self.attrs.get(&term.0).ok_or_else(|| format!("unknown guard attribute {}", term.0))?;
        let value = match attr {
            Attr::Mode {
                values,
            } => *values.get(self.mode).ok_or_else(|| {
                format!("attribute {} has no value for mode {}", term.0, self.mode)
            })?,
            Attr::Fact {
                default,
            } => self.facts.facts.get(&term.0).copied().unwrap_or(*default),
            Attr::Iter {
                domain,
                default,
            } => {
                let (_, index) = ctx
                    .iter()
                    .rev()
                    .find(|(d, _)| d == domain)
                    .ok_or_else(|| format!("iter guard {} outside a {domain} loop", term.0))?;
                match self.facts.iter.get(&term.0) {
                    Some(set) => set.contains(index),
                    None => default.contains(index),
                }
            }
            Attr::Par => self.parallel,
            Attr::Pass => true,
        };
        Ok(value == term.1)
    }

    fn guards(&self, guards: &[Term], ctx: &[(String, usize)]) -> Result<bool, String> {
        for term in guards {
            if !self.term(term, ctx)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A loop over `domain`'s length: measured, or one past the highest index any instance's
    /// trigger or any `iter` attribute of the domain names.
    fn length(&self, domain: &str, instances: &[Instance]) -> usize {
        if let Some(&n) = self.facts.lengths.get(domain) {
            return n;
        }
        let triggers =
            instances.iter().flat_map(|i| &i.trigger).filter(|(d, _)| d == domain).map(|(_, i)| *i);
        let iters = self.attrs.iter().filter_map(|(name, attr)| match attr {
            Attr::Iter {
                domain: d,
                default,
            } if d == domain => Some(
                self.facts
                    .iter
                    .get(name)
                    .map_or_else(|| default.clone(), |s| s.iter().copied().collect()),
            ),
            _ => None,
        });
        triggers.chain(iters.flatten()).max().map_or(0, |m| m + 1)
    }
}

/// The instances of `row` whose trigger is the current loop context.
fn matching(instances: &[Instance], row: &str, ctx: &[(String, usize)]) -> Vec<usize> {
    instances
        .iter()
        .enumerate()
        .filter(|(_, i)| i.row == row && i.trigger == ctx)
        .map(|(n, _)| n)
        .collect()
}

enum Flow {
    Next,
    Return,
    Skip,
    Fired(Fired),
}

struct Walker<'a> {
    env: Env<'a>,
    instances: &'a [Instance],
    ctx: Vec<(String, usize)>,
    path: Vec<Step>,
    /// The queued instances, each with the wait row that reports it.
    pending: Vec<(usize, String)>,
}

impl Walker<'_> {
    fn nodes(&mut self, nodes: &[Node]) -> Result<Flow, String> {
        for (pos, node) in nodes.iter().enumerate() {
            let flow = self.node(pos as u32, node)?;
            if !matches!(flow, Flow::Next) {
                return Ok(flow);
            }
        }
        Ok(Flow::Next)
    }

    fn node(&mut self, pos: u32, node: &Node) -> Result<Flow, String> {
        if !self.env.guards(&node.guards, &self.ctx)? {
            return Ok(Flow::Next);
        }
        if let What::Loop {
            domain,
            start,
            body,
        } = &node.what
        {
            for i in *start..self.env.length(domain, self.instances) {
                self.path.push(Step {
                    pos,
                    iter: Some(i as u32),
                });
                self.ctx.push((domain.clone(), i));
                let flow = self.nodes(body)?;
                self.ctx.pop();
                self.path.pop();
                match flow {
                    Flow::Next | Flow::Skip => {}
                    other => return Ok(other),
                }
            }
            return Ok(Flow::Next);
        }
        self.path.push(Step {
            pos,
            iter: None,
        });
        let flow = self.step(node);
        self.path.pop();
        flow
    }

    fn step(&mut self, node: &Node) -> Result<Flow, String> {
        Ok(match &node.what {
            What::Call {
                pruned: true,
                ..
            }
            | What::Block {
                pruned: true,
                ..
            } => Flow::Next,
            What::Call {
                body,
                callee,
                ..
            } => match self.nodes(body)? {
                Flow::Return => Flow::Next,
                Flow::Skip => return Err(format!("a continue leaves {callee}")),
                flow => flow,
            },
            What::Block {
                body,
                ..
            } => self.nodes(body)?,
            What::Loop {
                ..
            } => unreachable!("loops step in `node`"),
            What::Row {
                kind: RowKind::Wrapper,
                ..
            } => Flow::Next,
            What::Row {
                key,
                kind: RowKind::Wait,
            } => {
                let queued: Vec<usize> =
                    self.pending.iter().filter(|(_, w)| w == key).map(|(n, _)| *n).collect();
                if queued.is_empty() {
                    Flow::Next
                } else {
                    Flow::Fired(Fired {
                        occurrence: key.clone(),
                        key: flat(&self.path),
                        instances: queued,
                        deferred: true,
                    })
                }
            }
            What::Row {
                key,
                ..
            } => {
                let hits = matching(self.instances, key, &self.ctx);
                if hits.is_empty() {
                    Flow::Next
                } else {
                    Flow::Fired(Fired {
                        occurrence: key.clone(),
                        key: flat(&self.path),
                        instances: hits,
                        deferred: false,
                    })
                }
            }
            What::Return => Flow::Return,
            What::Skip => Flow::Skip,
            What::Defer {
                row,
                wait,
            } => {
                for n in matching(self.instances, row, &self.ctx) {
                    if !self.pending.iter().any(|(m, w)| *m == n && w == wait) {
                        self.pending.push((n, wait.clone()));
                    }
                }
                Flow::Next
            }
        })
    }
}

/// The reference walker (§4.3): execute `root` in source order; the first firing, or `None` if
/// nothing fires (the block passes every check holding an instance).
pub fn walk(root: &[Node], env: Env<'_>, instances: &[Instance]) -> Result<Option<Fired>, String> {
    for instance in instances {
        check_instance(root, &env, instance)?;
    }
    let mut walker = Walker {
        env,
        instances,
        ctx: Vec::new(),
        path: Vec::new(),
        pending: Vec::new(),
    };
    Ok(match walker.nodes(root)? {
        Flow::Fired(fired) => Some(fired),
        _ => None,
    })
}

/// A harness error: an instance of a row the tree lacks, of a wait or wrapper row (only a
/// deferred check reaches a wait), or at a trigger that fits none of the row's loop paths (the
/// same domains in order, each index at or past its loop's start and below a measured length).
fn check_instance(root: &[Node], env: &Env<'_>, instance: &Instance) -> Result<(), String> {
    type Site = (RowKind, Vec<(String, usize)>);
    fn sites(nodes: &[Node], row: &str, loops: &mut Vec<(String, usize)>, out: &mut Vec<Site>) {
        for n in nodes {
            match &n.what {
                What::Row {
                    key,
                    kind,
                } if key == row => out.push((*kind, loops.clone())),
                What::Defer {
                    row: r,
                    ..
                } if r == row => out.push((RowKind::Reject, loops.clone())),
                What::Loop {
                    domain,
                    start,
                    body,
                } => {
                    loops.push((domain.clone(), *start));
                    sites(body, row, loops, out);
                    loops.pop();
                }
                _ => sites(n.body(), row, loops, out),
            }
        }
    }
    let mut found = Vec::new();
    sites(root, &instance.row, &mut Vec::new(), &mut found);
    found.retain(|(kind, _)| matches!(kind, RowKind::Reject | RowKind::Pre));
    if found.is_empty() {
        return Err(format!("an instance of {}, not a firing row of the tree", instance.row));
    }
    let fits = |loops: &[(String, usize)]| {
        loops.len() == instance.trigger.len()
            && loops.iter().zip(&instance.trigger).all(|((domain, start), (d, i))| {
                d == domain && i >= start && env.facts.lengths.get(domain).is_none_or(|n| i < n)
            })
    };
    if !found.iter().any(|(_, loops)| fits(loops)) {
        return Err(format!(
            "an instance of {} at {:?}, outside each of its loop paths",
            instance.row, instance.trigger
        ));
    }
    Ok(())
}

/// One node visit of the static enumeration: its path, guards held along it, and its frame (the
/// path of its innermost call) and loop scope (the path of its innermost loop iteration in that
/// frame).
struct Visit<'a> {
    node: &'a Node,
    path: Vec<Step>,
    ctx: Vec<(String, usize)>,
    frame: usize,
    scope: Option<usize>,
}

/// The static enumeration: every node visit in key order, each loop over its domain's length.
struct Enumerator<'a, 'e> {
    env: Env<'e>,
    instances: &'e [Instance],
    path: Vec<Step>,
    ctx: Vec<(String, usize)>,
    out: Vec<Visit<'a>>,
}

impl<'a> Enumerator<'a, '_> {
    fn nodes(
        &mut self,
        nodes: &'a [Node],
        frame: usize,
        scope: Option<usize>,
    ) -> Result<(), String> {
        for (pos, node) in nodes.iter().enumerate() {
            if !self.env.guards(&node.guards, &self.ctx)? {
                continue;
            }
            let pos = pos as u32;
            if let What::Loop {
                domain,
                start,
                body,
            } = &node.what
            {
                for i in *start..self.env.length(domain, self.instances) {
                    self.path.push(Step {
                        pos,
                        iter: Some(i as u32),
                    });
                    self.ctx.push((domain.clone(), i));
                    self.nodes(body, frame, Some(self.path.len()))?;
                    self.ctx.pop();
                    self.path.pop();
                }
                continue;
            }
            self.path.push(Step {
                pos,
                iter: None,
            });
            self.out.push(Visit {
                node,
                path: self.path.clone(),
                ctx: self.ctx.clone(),
                frame,
                scope,
            });
            match &node.what {
                What::Call {
                    pruned: false,
                    body,
                    ..
                } => self.nodes(body, self.path.len(), None)?,
                What::Block {
                    pruned: false,
                    body,
                } => self.nodes(body, frame, scope)?,
                _ => {}
            }
            self.path.pop();
        }
        Ok(())
    }
}

/// What [`argmin`] saw, for the property test's non-vacuity counts.
#[derive(Clone, Debug, Default)]
pub struct ArgminStats {
    /// Firings of instances that some earlier return or continue cut off.
    pub preempted: usize,
    /// Instances with a reachable firing.
    pub firing: usize,
}

/// The winner from keys alone (§4.3): every instance's reachable firings (a row at its trigger
/// with every guard on the path holding, and no return of its frame or continue of its loop
/// iteration at a lesser key), a deferred one taking the key of the first reachable wait after
/// it; the least key wins, and instances at one key are one occurrence.
pub fn argmin(
    root: &[Node],
    env: Env<'_>,
    instances: &[Instance],
) -> Result<Option<Fired>, String> {
    argmin_stats(root, env, instances).map(|(f, _)| f)
}

pub fn argmin_stats(
    root: &[Node],
    env: Env<'_>,
    instances: &[Instance],
) -> Result<(Option<Fired>, ArgminStats), String> {
    for instance in instances {
        check_instance(root, &env, instance)?;
    }
    let mut enumerator = Enumerator {
        env,
        instances,
        path: Vec::new(),
        ctx: Vec::new(),
        out: Vec::new(),
    };
    enumerator.nodes(root, 0, None)?;
    let visits = enumerator.out;
    // A return or continue cuts the visits after it in its frame or loop iteration, once it is
    // reached itself: visits are in key order, so each is decided by the cutters before it.
    let cuts = |c: &Visit<'_>, v: &Visit<'_>| {
        let prefix = match c.node.what {
            What::Return => c.frame,
            _ => match c.scope {
                Some(scope) => scope,
                None => return false,
            },
        };
        c.path.len() > prefix
            && v.path.len() > prefix
            && v.path[..prefix] == c.path[..prefix]
            && flat(&c.path) < flat(&v.path)
    };
    let mut cutters: Vec<usize> = Vec::new();
    let mut reach = Vec::with_capacity(visits.len());
    for (i, v) in visits.iter().enumerate() {
        let reached = !cutters.iter().any(|&c| cuts(&visits[c], v));
        if reached && matches!(v.node.what, What::Return | What::Skip) {
            cutters.push(i);
        }
        reach.push(reached);
    }
    let waits: Vec<&Visit<'_>> = visits
        .iter()
        .zip(&reach)
        .filter(|(v, reached)| {
            **reached
                && matches!(
                    v.node.what,
                    What::Row {
                        kind: RowKind::Wait,
                        ..
                    }
                )
        })
        .map(|(v, _)| v)
        .collect();
    let mut stats = ArgminStats::default();
    // (key, occurrence, deferred, instance)
    let mut firings: Vec<(Vec<u32>, String, bool, usize)> = Vec::new();
    for (n, instance) in instances.iter().enumerate() {
        let mut best: Option<(Vec<u32>, String, bool)> = None;
        for (v, reached) in visits.iter().zip(&reach) {
            let fired = match &v.node.what {
                What::Row {
                    key,
                    kind: RowKind::Reject | RowKind::Pre,
                } if *key == instance.row => Some((flat(&v.path), key.clone(), false)),
                What::Defer {
                    row,
                    wait,
                } if *row == instance.row => {
                    let at = flat(&v.path);
                    waits
                        .iter()
                        .filter(|w| matches!(&w.node.what, What::Row { key, .. } if key == wait))
                        .map(|w| flat(&w.path))
                        .filter(|k| *k > at)
                        .min()
                        .map(|k| (k, wait.clone(), true))
                }
                _ => None,
            };
            let Some(fired) = fired else {
                continue;
            };
            if v.ctx != instance.trigger {
                continue;
            }
            if !reached {
                stats.preempted += 1;
                continue;
            }
            if best.as_ref().is_none_or(|b| fired.0 < b.0) {
                best = Some(fired);
            }
        }
        if let Some((key, occurrence, deferred)) = best {
            stats.firing += 1;
            firings.push((key, occurrence, deferred, n));
        }
    }
    let Some(min) = firings.iter().map(|f| f.0.clone()).min() else {
        return Ok((None, stats));
    };
    let at: Vec<&(Vec<u32>, String, bool, usize)> = firings.iter().filter(|f| f.0 == min).collect();
    Ok((
        Some(Fired {
            occurrence: at[0].1.clone(),
            key: min,
            instances: at.iter().map(|f| f.3).collect(),
            deferred: at[0].2,
        }),
        stats,
    ))
}

/// A declared interaction rule (§4.3): only these make a pair interacting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// The two defects touch one field.
    Footprint {
        a: usize,
        b: usize,
        field: String,
    },
    /// `a` changes a guard attribute on `b`'s path.
    Guard {
        a: usize,
        b: usize,
        attr: String,
    },
    /// `a` removes the transaction `b` sits in.
    Removed {
        a: usize,
        b: usize,
        tx: usize,
    },
}

/// The model's answer for a block: the walker's firing, an interaction, or nothing fires.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prediction {
    Winner(Fired),
    Interacting(Rule),
    None,
}

/// A cutter inside a node that leaves it: a return (out of its function, from inside a loop
/// too) or a skip (out of its loop's iteration), reached through blocks and loops but never a
/// call (a callee's return leaves only the callee).
pub struct Cutter {
    /// The guards from the node down to the cutter, outermost first: the nodes after it run
    /// only where these do not all hold.
    pub guards: Vec<Term>,
    /// It sits inside a loop of the node (its guards are per iteration there).
    pub in_loop: bool,
}

/// The cutters in `node` that leave it.
pub fn escaping_cutters(node: &Node) -> Vec<Cutter> {
    fn go(node: &Node, in_loop: bool, above: &mut Vec<Term>, out: &mut Vec<Cutter>) {
        let n = above.len();
        above.extend(node.guards.iter().cloned());
        match &node.what {
            What::Return => out.push(Cutter {
                guards: above.clone(),
                in_loop,
            }),
            What::Skip if !in_loop => out.push(Cutter {
                guards: above.clone(),
                in_loop,
            }),
            What::Block {
                body,
                ..
            } => body.iter().for_each(|c| go(c, in_loop, above, out)),
            What::Loop {
                body,
                ..
            } => body.iter().for_each(|c| go(c, true, above, out)),
            _ => {}
        }
        above.truncate(n);
    }
    let mut out = Vec::new();
    go(node, false, &mut Vec::new(), &mut out);
    out
}

/// Every guard attribute on any path from `root` to `row`, and on every cutter before the row
/// along such a path (whether the row is reached depends on both).
pub fn path_attrs(root: &[Node], row: &str) -> BTreeSet<String> {
    fn go(nodes: &[Node], row: &str, above: &mut Vec<String>, out: &mut BTreeSet<String>) {
        let start = above.len();
        for node in nodes {
            let n = above.len();
            above.extend(node.guards.iter().map(|t| t.0.clone()));
            let here = match &node.what {
                What::Row {
                    key,
                    ..
                } => key == row,
                What::Defer {
                    row: r,
                    wait,
                } => r == row || wait == row,
                _ => false,
            };
            if here {
                out.extend(above.iter().cloned());
            }
            go(node.body(), row, above, out);
            above.truncate(n);
            // The later siblings (and all below them) run only where its cutters do not fire.
            for cutter in escaping_cutters(node) {
                above.extend(cutter.guards.into_iter().map(|t| t.0));
            }
        }
        above.truncate(start);
    }
    let mut out = BTreeSet::new();
    go(root, row, &mut Vec::new(), &mut out);
    out
}

/// The first declared rule a pair of `instances` meets, if any.
pub fn interaction(root: &[Node], instances: &[Instance]) -> Option<Rule> {
    for (a, x) in instances.iter().enumerate() {
        for (b, y) in instances.iter().enumerate() {
            if a == b {
                continue;
            }
            if a < b
                && let Some(field) = x.footprint.intersection(&y.footprint).next()
            {
                return Some(Rule::Footprint {
                    a,
                    b,
                    field: field.clone(),
                });
            }
            if let Some(attr) = x.touches.intersection(&path_attrs(root, &y.row)).next() {
                return Some(Rule::Guard {
                    a,
                    b,
                    attr: attr.clone(),
                });
            }
            let tx = y.trigger.iter().find(|(d, _)| d == "block.vtx").map(|(_, i)| *i);
            if let Some(tx) = tx.filter(|t| x.removes.contains(t)) {
                return Some(Rule::Removed {
                    a,
                    b,
                    tx,
                });
            }
        }
    }
    None
}

/// A case's label checked against the rules: an `interacting` label on a pair no rule makes
/// interacting fails (a model disagreement is never fixed by reclassification, §4.3).
pub fn check_label(root: &[Node], instances: &[Instance], interacting: bool) -> Result<(), String> {
    match (interaction(root, instances), interacting) {
        (None, true) => Err("labelled interacting, but no declared rule holds".into()),
        (Some(rule), false) => Err(format!("labelled independent, but {rule:?} holds")),
        _ => Ok(()),
    }
}

/// [`walk`] behind the interaction rules (§4.3).
pub fn predict_in(
    root: &[Node],
    env: Env<'_>,
    instances: &[Instance],
) -> Result<Prediction, String> {
    if let Some(rule) = interaction(root, instances) {
        return Ok(Prediction::Interacting(rule));
    }
    Ok(match walk(root, env, instances)? {
        Some(fired) => Prediction::Winner(fired),
        None => Prediction::None,
    })
}

#[derive(Debug, Deserialize)]
struct RawNode {
    n: String,
    #[serde(default)]
    line: u32,
    #[serde(default)]
    guards: Vec<Term>,
    #[serde(default)]
    callee: Option<String>,
    #[serde(default)]
    pruned: Option<String>,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    start: usize,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    row: Option<String>,
    #[serde(default)]
    wait: Option<String>,
    #[serde(default)]
    body: Vec<RawNode>,
}

#[derive(Debug, Deserialize)]
struct RawFunction {
    body: Vec<RawNode>,
}

#[derive(Debug, Deserialize)]
pub struct Provenance {
    pub tag: String,
    pub commit: String,
    pub inventory_sha256: String,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    provenance: Provenance,
    attrs: BTreeMap<String, Attr>,
    modes: BTreeMap<String, Vec<RawNode>>,
    functions: BTreeMap<String, RawFunction>,
}

fn convert(
    raw: &RawNode,
    functions: &BTreeMap<String, RawFunction>,
    stack: &mut Vec<String>,
) -> Result<Node, String> {
    let field = |f: &Option<String>, name: &str| {
        f.clone().ok_or_else(|| format!("a {} node with no {name}", raw.n))
    };
    let body = |stack: &mut Vec<String>| -> Result<Vec<Node>, String> {
        raw.body.iter().map(|n| convert(n, functions, stack)).collect()
    };
    let what = match raw.n.as_str() {
        "call" => {
            let callee = field(&raw.callee, "callee")?;
            if raw.pruned.is_some() {
                What::Call {
                    callee,
                    body: Vec::new(),
                    pruned: true,
                }
            } else {
                if stack.contains(&callee) {
                    return Err(format!("recursion through {callee}"));
                }
                let f = functions.get(&callee).ok_or_else(|| format!("no function {callee}"))?;
                stack.push(callee.clone());
                let body: Result<Vec<Node>, String> =
                    f.body.iter().map(|n| convert(n, functions, stack)).collect();
                stack.pop();
                What::Call {
                    callee,
                    body: body?,
                    pruned: false,
                }
            }
        }
        "loop" => What::Loop {
            domain: field(&raw.domain, "domain")?,
            start: raw.start,
            body: body(stack)?,
        },
        "block" => What::Block {
            pruned: raw.pruned.is_some(),
            body: body(stack)?,
        },
        "row" => What::Row {
            key: field(&raw.key, "key")?,
            kind: match field(&raw.kind, "kind")?.as_str() {
                "R" => RowKind::Reject,
                "W" => RowKind::Wrapper,
                "pre" => RowKind::Pre,
                "wait" => RowKind::Wait,
                other => return Err(format!("unknown row kind {other}")),
            },
        },
        "return" => What::Return,
        "skip" => What::Skip,
        "defer" => What::Defer {
            row: field(&raw.row, "row")?,
            wait: field(&raw.wait, "wait")?,
        },
        other => return Err(format!("unknown node {other}")),
    };
    Ok(Node {
        guards: raw.guards.clone(),
        line: raw.line,
        what,
    })
}

/// The model: per mode, the expanded tree, and the attributes its guards read.
#[derive(Debug)]
pub struct Model {
    pub provenance: Provenance,
    pub attrs: BTreeMap<String, Attr>,
    roots: BTreeMap<String, Vec<Node>>,
}

pub fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Submit => "Submit",
        Mode::Proposal => "Proposal",
        Mode::Header => "Header",
    }
}

impl Model {
    /// The compiled-in v24.0.0 model, checked against the compiled-in inventory.
    pub fn load() -> Result<Model, String> {
        Model::from_json(FIXTURE, super::inventory::FIXTURE)
    }

    /// The model in `text`, refused unless it was scanned beside the inventory `inventory`
    /// (byte for byte, by sha256) and every row it names is that inventory's, so a stale model
    /// is never read (Q-11).
    pub fn from_json(text: &str, inventory: &str) -> Result<Model, String> {
        let raw: RawModel =
            serde_json::from_str(text).map_err(|e| format!("the precedence model: {e}"))?;
        let sha = super::trial::to_hex(&sha256::Hash::hash(inventory.as_bytes()).to_byte_array());
        if raw.provenance.inventory_sha256 != sha {
            return Err(format!(
                "the precedence model was scanned beside inventory {}, this one is {sha}: regenerate it",
                raw.provenance.inventory_sha256
            ));
        }
        let inv: serde_json::Value =
            serde_json::from_str(inventory).map_err(|e| format!("the inventory: {e}"))?;
        if inv["provenance"]["commit"] != raw.provenance.commit.as_str() {
            return Err("the precedence model's commit is not the inventory's".into());
        }
        let keys: BTreeSet<&str> = inv["rows"]
            .as_array()
            .map(|rows| rows.iter().filter_map(|r| r["key"].as_str()).collect())
            .unwrap_or_default();
        let mut roots = BTreeMap::new();
        for mode in ["Submit", "Proposal", "Header"] {
            let raw_root = raw.modes.get(mode).ok_or_else(|| format!("no {mode} root"))?;
            let root: Vec<Node> = raw_root
                .iter()
                .map(|n| convert(n, &raw.functions, &mut Vec::new()))
                .collect::<Result<_, _>>()?;
            validate(&root, &raw.attrs, &keys)?;
            roots.insert(mode.to_owned(), root);
        }
        Ok(Model {
            provenance: raw.provenance,
            attrs: raw.attrs,
            roots,
        })
    }

    pub fn root(&self, mode: Mode) -> &[Node] {
        &self.roots[mode_name(mode)]
    }

    pub fn env<'a>(&'a self, mode: Mode, par: Par, facts: &'a Facts) -> Env<'a> {
        Env {
            attrs: &self.attrs,
            mode: mode_name(mode),
            parallel: par != Par::One,
            facts,
        }
    }

    /// The prediction for `instances` in `mode` at `par` (§4.3).
    pub fn predict(
        &self,
        mode: Mode,
        par: Par,
        facts: &Facts,
        instances: &[Instance],
    ) -> Result<Prediction, String> {
        predict_in(self.root(mode), self.env(mode, par, facts), instances)
    }
}

/// Every guard names a known attribute, every non-RPC row is an inventory row, and every defer's
/// wait is a wait row of the same tree.
fn validate(
    root: &[Node],
    attrs: &BTreeMap<String, Attr>,
    keys: &BTreeSet<&str>,
) -> Result<(), String> {
    fn rows(nodes: &[Node], out: &mut Vec<(String, RowKind)>) {
        for n in nodes {
            if let What::Row {
                key,
                kind,
            } = &n.what
            {
                out.push((key.clone(), *kind));
            }
            rows(n.body(), out);
        }
    }
    fn walk_nodes(
        nodes: &[Node],
        f: &mut dyn FnMut(&Node) -> Result<(), String>,
    ) -> Result<(), String> {
        for n in nodes {
            f(n)?;
            walk_nodes(n.body(), f)?;
        }
        Ok(())
    }
    /// A skip ends its loop's iteration: one with no loop of its own function around it has no
    /// meaning (the walker would end the walk, `argmin` cut nothing).
    fn skips(nodes: &[Node], in_loop: bool) -> Result<(), String> {
        for n in nodes {
            match &n.what {
                What::Skip if !in_loop => {
                    return Err(format!(
                        "line {}: a skip outside any loop of its function",
                        n.line
                    ));
                }
                What::Loop {
                    body,
                    ..
                } => skips(body, true)?,
                What::Call {
                    body,
                    ..
                } => skips(body, false)?,
                _ => skips(n.body(), in_loop)?,
            }
        }
        Ok(())
    }
    skips(root, false)?;
    let mut all = Vec::new();
    rows(root, &mut all);
    walk_nodes(root, &mut |n| {
        for t in &n.guards {
            if !attrs.contains_key(&t.0) {
                return Err(format!("line {}: unknown guard attribute {}", n.line, t.0));
            }
        }
        match &n.what {
            What::Row {
                key,
                kind,
            } if *kind != RowKind::Pre && !keys.contains(key.as_str()) => {
                Err(format!("row {key} is not an inventory row"))
            }
            What::Defer {
                row,
                wait,
            } => {
                if !keys.contains(row.as_str()) {
                    return Err(format!("defer of {row}, not an inventory row"));
                }
                if !all.iter().any(|(k, kind)| k == wait && *kind == RowKind::Wait) {
                    return Err(format!("defer of {row} to {wait}, not a wait row of the tree"));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    })
}

// The placement of defects on a mode's tree (O10-1 `witnesses`, used by O10-2's pairs).

/// One path from a root to a row (or its defer): the guards and loops on it, outermost first,
/// and the guards of each cutter before it on the way (none may all hold: the row is reached).
#[derive(Clone, Debug, Default)]
struct Path {
    guards: Vec<Term>,
    loops: Vec<(String, usize)>,
    unless: Vec<Vec<Term>>,
}

fn paths(nodes: &[Node], row: &str, above: &Path, out: &mut Vec<Path>) {
    let mut before = Vec::new();
    for node in nodes {
        let mut here = above.clone();
        here.unless.extend(before.iter().cloned());
        for cutter in escaping_cutters(node) {
            // A return inside an earlier loop would need its guards to fail at every
            // iteration; the model has none, so `place` does not model it.
            assert!(!cutter.in_loop, "a return inside a loop before {row}");
            before.push(cutter.guards);
        }
        here.guards.extend(node.guards.iter().cloned());
        match &node.what {
            What::Row {
                key,
                kind: RowKind::Reject | RowKind::Pre,
            } if key == row => out.push(here),
            What::Defer {
                row: r,
                ..
            } if r == row => out.push(here),
            What::Loop {
                domain,
                start,
                body,
            } => {
                here.loops.push((domain.clone(), *start));
                paths(body, row, &here, out);
            }
            _ => paths(node.body(), row, &here, out),
        }
    }
}

/// Where an instance sits: explicit indices by domain (the block's transaction, the
/// transaction's input or output), else the coinbase (0) where a path requires it, else index 1
/// (after the coinbase), else a loop's first index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct At {
    pub vtx: Option<usize>,
    pub vin: Option<usize>,
    pub vout: Option<usize>,
}

/// No block holds the defects: placing the last of `rows` needs `fact` with the value the
/// others' paths (or its own) already fixed the other way, e.g. bad-cb-height (v:4591,
/// `!fDIP0003Active_context`) with bad-cb-type (v:4600, `fDIP0003Active_context`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Infeasible {
    pub fact: String,
    pub rows: Vec<String>,
}

/// Why no block holds a set of defects as placed ([`setting`]): never a firing, never `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unplaced {
    /// Two paths need one fact both ways ([`Infeasible`]).
    Infeasible(Infeasible),
    /// An earlier defect's path fixed a fact that makes a return before `row` fire on every
    /// path to it (`row` alone has a block): `row` is unreachable after that return, e.g.
    /// anything after AcceptBlockHeader's `duplicate` (v:4619-4625, `known_header`).
    AfterReturn {
        row: String,
        after: Vec<String>,
    },
    /// Two defects' paths need the iteration guard `attr` both ways at one index of its domain
    /// (O10-1 open item 8b): refused, never guessed.
    IterConflict {
        attr: String,
        index: usize,
        rows: Vec<String>,
    },
}

impl Unplaced {
    /// The reason's short name for a count: `infeasible:<fact>`, `unreachable-after-return`,
    /// `iter-conflict:<attr>`.
    pub fn reason(&self) -> String {
        match self {
            Unplaced::Infeasible(e) => format!("infeasible:{}", e.fact),
            Unplaced::AfterReturn {
                ..
            } => "unreachable-after-return".into(),
            Unplaced::IterConflict {
                attr,
                ..
            } => format!("iter-conflict:{attr}"),
        }
    }
}

/// `row`'s instance on its first path viable in `mode` at `par`, and the facts that path's
/// guards need (each guard on the defect's own path holds: the defect is there). `None` when
/// no path is viable (a mode guard, or the other `-par`, shuts every one); [`Infeasible`]
/// when every path a mode or `-par` guard leaves open needs a fact `facts` fixes the other way.
pub fn place(
    model: &Model,
    mode: Mode,
    par: Par,
    row: &str,
    at: At,
    facts: &mut Facts,
) -> Result<Option<Instance>, Infeasible> {
    place_needing(model, mode, par, row, at, facts, &mut Vec::new())
}

/// [`place`], also returning the iteration guards the chosen path needs: `(attr, index, value)`.
fn place_needing(
    model: &Model,
    mode: Mode,
    par: Par,
    row: &str,
    at: At,
    facts: &mut Facts,
    needs: &mut Vec<(String, usize, bool)>,
) -> Result<Option<Instance>, Infeasible> {
    let mut conflict = None;
    let mut found = Vec::new();
    paths(model.root(mode), row, &Path::default(), &mut found);
    assert!(!found.is_empty(), "{row} is not in the {mode:?} tree");
    'path: for path in found {
        let mut trigger = Vec::new();
        for (domain, start) in &path.loops {
            let coinbase = path.guards.iter().any(|t| t.0 == "coinbase" && t.1);
            let index = match domain.as_str() {
                "block.vtx" => at.vtx.unwrap_or(if coinbase {
                    0
                } else {
                    (*start).max(1)
                }),
                "tx.vin" => at.vin.unwrap_or(*start),
                "tx.vout" => at.vout.unwrap_or(*start),
                _ => *start,
            };
            trigger.push((domain.clone(), index));
        }
        // The trigger is measured from the bytes: an input or output index on a path with no
        // loop over that domain places nothing there.
        let looped = |domain: &str| path.loops.iter().any(|(d, _)| d == domain);
        if (at.vin.is_some() && !looped("tx.vin")) || (at.vout.is_some() && !looped("tx.vout")) {
            continue 'path;
        }
        let mut want = facts.clone();
        let mut need = Vec::new();
        for term in &path.guards {
            match &model.attrs[&term.0] {
                Attr::Mode {
                    values,
                } => {
                    if values[mode_name(mode)] != term.1 {
                        continue 'path;
                    }
                }
                Attr::Par => {
                    if (par != Par::One) != term.1 {
                        continue 'path;
                    }
                }
                Attr::Pass => {}
                Attr::Fact {
                    ..
                } => {
                    if want.facts.insert(term.0.clone(), term.1).is_some_and(|v| v != term.1) {
                        conflict.get_or_insert_with(|| term.0.clone());
                        continue 'path;
                    }
                }
                Attr::Iter {
                    domain,
                    default,
                } => {
                    let (_, index) =
                        trigger.iter().rev().find(|(d, _)| d == domain).expect("its loop");
                    let set = want
                        .iter
                        .entry(term.0.clone())
                        .or_insert_with(|| default.iter().copied().collect());
                    if term.1 {
                        set.insert(*index);
                    } else {
                        set.remove(index);
                    }
                    need.push((term.0.clone(), *index, term.1));
                }
            }
        }
        let fixed: BTreeSet<&str> = path.guards.iter().map(|t| t.0.as_str()).collect();
        for cutter in &path.unless {
            if !avoid(model, mode, par, &trigger, cutter, &fixed, &mut want) {
                continue 'path;
            }
        }
        *facts = want;
        *needs = need;
        return Ok(Some(Instance {
            row: row.to_owned(),
            trigger,
            ..Instance::default()
        }));
    }
    match conflict {
        Some(fact) => Err(Infeasible {
            fact,
            rows: vec![row.to_owned()],
        }),
        None => Ok(None),
    }
}

/// Keeps a cutter before the row from firing: true when one of its guards already fails in
/// `want`, or once the first fact (unset so far) or iteration guard the path does not fix is
/// negated there; false when none can fail (the cutter shuts the path).
fn avoid(
    model: &Model,
    mode: Mode,
    par: Par,
    trigger: &[(String, usize)],
    cutter: &[Term],
    fixed: &BTreeSet<&str>,
    want: &mut Facts,
) -> bool {
    let index = |domain: &str| trigger.iter().rev().find(|(d, _)| d == domain).map(|(_, i)| *i);
    let holds = |term: &Term, want: &Facts| {
        let value = match &model.attrs[&term.0] {
            Attr::Mode {
                values,
            } => values[mode_name(mode)],
            Attr::Par => par != Par::One,
            Attr::Pass => true,
            Attr::Fact {
                default,
            } => want.facts.get(&term.0).copied().unwrap_or(*default),
            Attr::Iter {
                domain,
                default,
            } => {
                let i = index(domain).expect("an iter guard inside its loop");
                want.iter.get(&term.0).map_or(default.contains(&i), |set| set.contains(&i))
            }
        };
        value == term.1
    };
    if cutter.iter().any(|t| !holds(t, want)) {
        return true;
    }
    let Some(term) = cutter.iter().find(|t| {
        !fixed.contains(t.0.as_str())
            && match model.attrs[&t.0] {
                Attr::Fact {
                    ..
                } => !want.facts.contains_key(&t.0),
                Attr::Iter {
                    ..
                } => true,
                _ => false,
            }
    }) else {
        return false;
    };
    match &model.attrs[&term.0] {
        Attr::Iter {
            domain,
            default,
        } => {
            let i = index(domain).expect("an iter guard inside its loop");
            let set = want
                .iter
                .entry(term.0.clone())
                .or_insert_with(|| default.iter().copied().collect());
            if term.1 {
                set.remove(&i);
            } else {
                set.insert(i);
            }
        }
        _ => {
            want.facts.insert(term.0.clone(), !term.1);
        }
    }
    true
}

/// A mode or `-par` guard shuts every path to `row` in `mode` at `par`.
pub fn shut(model: &Model, mode: Mode, par: Par, row: &str) -> bool {
    let mut found = Vec::new();
    paths(model.root(mode), row, &Path::default(), &mut found);
    found.iter().all(|path| {
        path.guards.iter().any(|t| match &model.attrs[&t.0] {
            Attr::Mode {
                values,
            } => values[mode_name(mode)] != t.1,
            Attr::Par => (par != Par::One) != t.1,
            _ => false,
        })
    })
}

/// The rows of `mode`'s tree an instance can sit on (as `paths` finds them): its reject and
/// pre rows and its defers, never a wrapper or a wait row.
pub fn rows(model: &Model, mode: Mode) -> BTreeSet<String> {
    fn go(nodes: &[Node], out: &mut BTreeSet<String>) {
        for n in nodes {
            match &n.what {
                What::Row {
                    key,
                    kind: RowKind::Reject | RowKind::Pre,
                } => {
                    out.insert(key.clone());
                }
                What::Defer {
                    row,
                    ..
                } => {
                    out.insert(row.clone());
                }
                _ => {}
            }
            go(n.body(), out);
        }
    }
    let mut out = BTreeSet::new();
    go(model.root(mode), &mut out);
    out
}

/// The walker's occurrence for `defects` placed on `mode`'s tree, the facts merged from
/// every defect's path and `extra`. A defect no viable path reaches holds no instance; defects
/// no one block can hold are [`Unplaced`], neither a firing nor `None`.
pub fn predicted(
    model: &Model,
    mode: Mode,
    par: Par,
    defects: &[(&str, At)],
    extra: &dyn Fn(&mut Facts),
) -> Result<(Option<(String, bool)>, bool), Unplaced> {
    let (instances, facts) = setting(model, mode, par, defects, Facts::default(), extra)?;
    Ok(walked(model, mode, par, &instances, &facts))
}

/// `defects` placed on `mode`'s tree over `start`, and the whole setting the walk reads: the
/// facts merged from every defect's path, then `extra`, and every loop domain's length made
/// explicit at what `Env::length` reads for the placed instances (the base shape).
pub fn setting(
    model: &Model,
    mode: Mode,
    par: Par,
    defects: &[(&str, At)],
    start: Facts,
    extra: &dyn Fn(&mut Facts),
) -> Result<(Vec<Instance>, Facts), Unplaced> {
    let mut facts = start;
    let present = rows(model, mode);
    // The defect whose path first fixed each fact, for an infeasible result.
    let mut fixed_by: BTreeMap<String, String> = BTreeMap::new();
    // The iteration guards each placed defect's path needs, by (attr, index).
    let mut needed: BTreeMap<(String, usize), (bool, String)> = BTreeMap::new();
    let mut instances = Vec::new();
    let mut placed_rows: Vec<String> = Vec::new();
    for (row, at) in defects.iter().filter(|(row, _)| present.contains(*row)) {
        let before = facts.facts.clone();
        let mut needs = Vec::new();
        let placed = place_needing(model, mode, par, row, *at, &mut facts, &mut needs).map_err(
            |mut e| {
                if let Some(other) = fixed_by.get(&e.fact).filter(|r| *r != row) {
                    e.rows.insert(0, other.clone());
                }
                Unplaced::Infeasible(e)
            },
        )?;
        // A defect sits nowhere where a mode or `-par` guard shuts every path to its row
        // (`high-hash` on Proposal: fCheckPOW false), or where it does alone (its answer is
        // then `None`); otherwise an earlier defect's return shuts it.
        if placed.is_none()
            && !placed_rows.is_empty()
            && !shut(model, mode, par, row)
            && place(model, mode, par, row, *at, &mut Facts::default()).ok().flatten().is_some()
        {
            return Err(Unplaced::AfterReturn {
                row: (*row).to_owned(),
                after: placed_rows,
            });
        }
        for (attr, index, value) in needs {
            match needed.get(&(attr.clone(), index)) {
                Some((v, other)) if *v != value => {
                    return Err(Unplaced::IterConflict {
                        attr,
                        index,
                        rows: vec![other.clone(), (*row).to_owned()],
                    });
                }
                Some(_) => {}
                None => {
                    needed.insert((attr, index), (value, (*row).to_owned()));
                }
            }
        }
        for fact in facts.facts.keys().filter(|f| !before.contains_key(*f)) {
            fixed_by.insert(fact.clone(), (*row).to_owned());
        }
        if placed.is_some() {
            placed_rows.push((*row).to_owned());
        }
        instances.extend(placed);
    }
    extra(&mut facts);
    facts.lengths = shape(model.root(mode), model.env(mode, par, &facts), &instances);
    Ok((instances, facts))
}

/// A defect set's model answer: the walker's occurrence (deferred or not) and whether the key
/// argmin agrees, or why no block holds it.
pub type Answer = Result<(Option<(String, bool)>, bool), Unplaced>;

/// One [`universe`] answer: the mode, the `-par`, the placed rows.
pub type Probe = (Mode, Par, Vec<(String, At)>, Answer);

/// The model-level pair universe (O10-1's lone-row and pair probe, which O10-2's generator
/// counts, §4.5): on each mode at both `-par`, every row of the mode's tree alone and every
/// ordered pair of two rows, the second at `second` (O10-1: transaction 2; item 8b's same-index
/// universe: transaction 1).
pub fn universe(model: &Model, second: At) -> Vec<Probe> {
    let mut out = Vec::new();
    for mode in [Mode::Submit, Mode::Proposal, Mode::Header] {
        let all = rows(model, mode);
        for par in [Par::One, Par::Four] {
            for a in &all {
                for b in std::iter::once(None).chain(all.iter().filter(|b| *b != a).map(Some)) {
                    let mut defects = vec![(a.clone(), At::default())];
                    defects.extend(b.map(|b| (b.clone(), second)));
                    let refs: Vec<(&str, At)> =
                        defects.iter().map(|(r, at)| (r.as_str(), *at)).collect();
                    let answer = predicted(model, mode, par, &refs, &|_| {});
                    out.push((mode, par, defects, answer));
                }
            }
        }
    }
    out
}

/// Every loop domain of `root` and the length a walk of `instances` in `env` gives it.
fn shape(root: &[Node], env: Env<'_>, instances: &[Instance]) -> BTreeMap<String, usize> {
    fn domains(nodes: &[Node], out: &mut BTreeSet<String>) {
        for n in nodes {
            if let What::Loop {
                domain,
                ..
            } = &n.what
            {
                out.insert(domain.clone());
            }
            domains(n.body(), out);
        }
    }
    let mut all = BTreeSet::new();
    domains(root, &mut all);
    all.into_iter().map(|d| (d.clone(), env.length(&d, instances))).collect()
}

/// The walker's occurrence for `instances` in `facts`, and whether the key-based argmin agrees.
pub fn walked(
    model: &Model,
    mode: Mode,
    par: Par,
    instances: &[Instance],
    facts: &Facts,
) -> (Option<(String, bool)>, bool) {
    let env = model.env(mode, par, facts);
    let walked = walk(model.root(mode), env, instances).expect("a walk");
    let keyed = argmin(model.root(mode), env, instances).expect("an argmin");
    let same = walked.as_ref().map(|f| (&f.occurrence, &f.key))
        == keyed.as_ref().map(|f| (&f.occurrence, &f.key));
    (walked.map(|f| (f.occurrence, f.deferred)), same)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOMAINS: [&str; 3] = ["d0", "d1", "d2"];

    /// SplitMix64: the property test's seeded draws.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn chance(&mut self, percent: usize) -> bool {
            self.below(100) < percent
        }
    }

    /// The attributes of a random tree: a mode value, four facts, an iter attribute per domain,
    /// `par` and `pass`.
    fn random_attrs(rng: &mut Rng) -> BTreeMap<String, Attr> {
        let mut attrs = BTreeMap::new();
        attrs.insert(
            "m".into(),
            Attr::Mode {
                values: [("M".to_owned(), rng.chance(50))].into(),
            },
        );
        for f in 0..4 {
            attrs.insert(
                format!("f{f}"),
                Attr::Fact {
                    default: rng.chance(60),
                },
            );
        }
        for d in DOMAINS {
            let default = (0..4).filter(|_| rng.chance(30)).collect();
            attrs.insert(
                format!("i_{d}"),
                Attr::Iter {
                    domain: d.into(),
                    default,
                },
            );
        }
        attrs.insert("par".into(), Attr::Par);
        attrs.insert("pass".into(), Attr::Pass);
        attrs
    }

    struct Gen<'a> {
        rng: &'a mut Rng,
        waits: usize,
        defers: usize,
    }

    impl Gen<'_> {
        fn guards(&mut self, open: &[&str]) -> Vec<Term> {
            if !self.rng.chance(30) {
                return Vec::new();
            }
            (0..1 + self.rng.below(2))
                .map(|_| {
                    let attr = match self.rng.below(5) {
                        0 => "m".to_owned(),
                        1 => "pass".to_owned(),
                        2 if !open.is_empty() => format!("i_{}", open[self.rng.below(open.len())]),
                        _ => format!("f{}", self.rng.below(4)),
                    };
                    Term(attr, self.rng.chance(60))
                })
                .collect()
        }

        /// 1 to 5 nodes at `depth`, inside the loops `open` (`frame_loop`: a loop of this frame
        /// is open, so a continue may appear).
        fn nodes(&mut self, depth: usize, open: &[&'static str], frame_loop: bool) -> Vec<Node> {
            let mut nodes = Vec::new();
            for _ in 0..1 + self.rng.below(5) {
                let guards = self.guards(open);
                let deep = depth < 4;
                let pick = self.rng.below(100);
                let what = if pick < 32 {
                    What::Row {
                        key: format!("r{}", self.rng.below(8)),
                        kind: RowKind::Reject,
                    }
                } else if pick < 37 {
                    What::Row {
                        key: format!("w{}", self.rng.below(3)),
                        kind: RowKind::Wrapper,
                    }
                } else if pick < 50 && deep {
                    What::Call {
                        callee: "f".into(),
                        body: self.nodes(depth + 1, open, false),
                        pruned: self.rng.chance(5),
                    }
                } else if pick < 68 && deep {
                    let free: Vec<&'static str> =
                        DOMAINS.iter().copied().filter(|d| !open.contains(d)).collect();
                    if free.is_empty() {
                        continue;
                    }
                    let domain = free[self.rng.below(free.len())];
                    let inner: Vec<&'static str> = open.iter().copied().chain([domain]).collect();
                    let mut body = self.nodes(depth + 1, &inner, true);
                    // A queued check: a defer under `par`, its inline row under `!par`, and the
                    // wait as a later sibling of the loop.
                    let defer = self.rng.chance(50);
                    let (row, wait) =
                        (format!("q{}", self.defers % 3), format!("wait{}", self.waits));
                    if defer {
                        self.defers += 1;
                        self.waits += 1;
                        let queued = vec![
                            Node::new(What::Block {
                                body: vec![Node::new(What::Defer {
                                    row: row.clone(),
                                    wait: wait.clone(),
                                })],
                                pruned: false,
                            })
                            .guarded(&[("par", true)]),
                            Node::new(What::Block {
                                body: vec![Node::new(What::Row {
                                    key: row,
                                    kind: RowKind::Reject,
                                })],
                                pruned: false,
                            })
                            .guarded(&[("par", false)]),
                        ];
                        let at = self.rng.below(body.len() + 1);
                        // Half the time inside a call in the loop's body, the wait in the
                        // caller (CheckInputScripts' shape, v:2763).
                        let queued = if self.rng.chance(50) {
                            vec![Node::new(What::Call {
                                callee: "g".into(),
                                body: queued,
                                pruned: false,
                            })]
                        } else {
                            queued
                        };
                        body.splice(at..at, queued);
                    }
                    nodes.push(Node {
                        guards,
                        line: 0,
                        what: What::Loop {
                            domain: domain.into(),
                            start: usize::from(self.rng.chance(25)),
                            body,
                        },
                    });
                    if defer {
                        // Any later sibling position: right after the loop, or after more nodes.
                        let mut later = if self.rng.chance(50) {
                            self.nodes(depth + 1, open, frame_loop)
                        } else {
                            Vec::new()
                        };
                        later.insert(
                            self.rng.below(later.len() + 1),
                            Node::new(What::Row {
                                key: wait,
                                kind: RowKind::Wait,
                            }),
                        );
                        nodes.extend(later);
                    }
                    continue;
                } else if pick < 82 && deep {
                    What::Block {
                        body: self.nodes(depth + 1, open, frame_loop),
                        pruned: self.rng.chance(5),
                    }
                } else if pick < 90 {
                    What::Return
                } else if frame_loop {
                    What::Skip
                } else {
                    What::Row {
                        key: format!("r{}", self.rng.below(8)),
                        kind: RowKind::Reject,
                    }
                };
                nodes.push(Node {
                    guards,
                    line: 0,
                    what,
                });
            }
            nodes
        }
    }

    /// Every (row key, its loops' domains and starts) a firing row or defer of `nodes` can hold
    /// an instance at.
    type Site = (String, Vec<(String, usize)>);

    fn sites(nodes: &[Node], ctx: &mut Vec<(String, usize)>, out: &mut Vec<Site>) {
        for n in nodes {
            match &n.what {
                What::Row {
                    key,
                    kind: RowKind::Reject,
                }
                | What::Defer {
                    row: key,
                    ..
                } => {
                    out.push((key.clone(), ctx.clone()));
                }
                What::Loop {
                    domain,
                    start,
                    body,
                } => {
                    ctx.push((domain.clone(), *start));
                    sites(body, ctx, out);
                    ctx.pop();
                }
                _ => sites(n.body(), ctx, out),
            }
        }
    }

    fn random_instances(rng: &mut Rng, root: &[Node]) -> Vec<Instance> {
        let mut found = Vec::new();
        sites(root, &mut Vec::new(), &mut found);
        if found.is_empty() {
            return Vec::new();
        }
        // A third of the picks go to a queued check where the tree has one, so deferred
        // firings are not rare.
        let queued: Vec<&Site> = found.iter().filter(|(r, _)| r.starts_with('q')).collect();
        (0..1 + rng.below(3))
            .map(|_| {
                let (row, loops) = if !queued.is_empty() && rng.chance(33) {
                    queued[rng.below(queued.len())]
                } else {
                    &found[rng.below(found.len())]
                };
                Instance {
                    row: row.clone(),
                    trigger: loops
                        .iter()
                        .map(|(d, start)| (d.clone(), start + rng.below(4 - start)))
                        .collect(),
                    ..Instance::default()
                }
            })
            .collect()
    }

    fn sorted(fired: Option<Fired>) -> Option<Fired> {
        fired.map(|mut f| {
            f.instances.sort_unstable();
            f
        })
    }

    /// §4.3's property: on 10^4 seeded random trees (loops, nested and sibling loops, calls,
    /// guards, W rows, returns, continues, defers and waits), each with 1-3 random instances, the
    /// least key is the walker's firing, at -par = 1 and > 1. The counts keep it from passing
    /// vacuously.
    #[test]
    fn key_argmin_is_the_walker() {
        let mut rng = Rng(0x0010_0001);
        let (mut fired, mut contested, mut deferred, mut preempted, mut none) = (0, 0, 0, 0, 0);
        for draw in 0..10_000 {
            let attrs = random_attrs(&mut rng);
            let mut trees = Gen {
                rng: &mut rng,
                waits: 0,
                defers: 0,
            };
            let root = trees.nodes(0, &[], false);
            let instances = random_instances(&mut rng, &root);
            let mut facts = Facts::default();
            if rng.chance(30) {
                facts.facts.insert(format!("f{}", rng.below(4)), rng.chance(50));
            }
            for parallel in [false, true] {
                let env = Env {
                    attrs: &attrs,
                    mode: "M",
                    parallel,
                    facts: &facts,
                };
                let walked = sorted(walk(&root, env, &instances).expect("a walk"));
                let (keyed, stats) = argmin_stats(&root, env, &instances).expect("an argmin");
                assert_eq!(
                    walked,
                    sorted(keyed),
                    "draw {draw}, par > 1 {parallel}: {instances:?}\n{root:#?}"
                );
                match &walked {
                    Some(f) => {
                        fired += 1;
                        deferred += usize::from(f.deferred);
                    }
                    None => none += 1,
                }
                contested += usize::from(stats.firing >= 2);
                preempted += usize::from(stats.preempted > 0);
            }
        }
        eprintln!(
            "key_argmin_is_the_walker: fired {fired}, none {none}, contested {contested}, deferred {deferred}, preempted {preempted}"
        );
        assert!(fired >= 5_000 && none >= 1_000, "fired {fired}, none {none}");
        assert!(contested >= 2_000, "contested {contested}");
        assert!(deferred >= 300, "deferred {deferred}");
        assert!(preempted >= 300, "preempted {preempted}");
    }

    fn row(key: &str) -> Node {
        Node::new(What::Row {
            key: key.into(),
            kind: RowKind::Reject,
        })
    }

    fn lp(domain: &str, body: Vec<Node>) -> Node {
        Node::new(What::Loop {
            domain: domain.into(),
            start: 0,
            body,
        })
    }

    fn attrs() -> BTreeMap<String, Attr> {
        [
            ("par".to_owned(), Attr::Par),
            (
                "cb".to_owned(),
                Attr::Iter {
                    domain: "block.vtx".into(),
                    default: vec![0],
                },
            ),
        ]
        .into()
    }

    #[test]
    fn a_row_before_a_loop_beats_every_iteration() {
        let root = vec![row("pre"), lp("block.vtx", vec![row("body")])];
        let (attrs, facts) = (attrs(), Facts::default());
        let env = Env {
            attrs: &attrs,
            mode: "M",
            parallel: false,
            facts: &facts,
        };
        let i = [Instance::new("body", &[("block.vtx", 0)]), Instance::new("pre", &[])];
        let w = walk(&root, env, &i).expect("a walk").expect("a firing");
        assert_eq!((w.occurrence.as_str(), w.key.clone()), ("pre", vec![0]));
        assert_eq!(argmin(&root, env, &i).expect("an argmin"), Some(w));
    }

    #[test]
    fn an_iter_guard_skips_its_iteration() {
        let root = vec![lp("block.vtx", vec![row("r").guarded(&[("cb", false)])])];
        let (attrs, facts) = (attrs(), Facts::default());
        let env = Env {
            attrs: &attrs,
            mode: "M",
            parallel: false,
            facts: &facts,
        };
        assert_eq!(
            walk(&root, env, &[Instance::new("r", &[("block.vtx", 0)])]).expect("a walk"),
            None
        );
        assert!(
            walk(&root, env, &[Instance::new("r", &[("block.vtx", 1)])]).expect("a walk").is_some()
        );
    }

    #[test]
    fn instances_on_waits_wrappers_or_unknown_rows_are_refused() {
        let root = vec![
            Node::new(What::Row {
                key: "w".into(),
                kind: RowKind::Wait,
            }),
            Node::new(What::Row {
                key: "x".into(),
                kind: RowKind::Wrapper,
            }),
        ];
        let (attrs, facts) = (attrs(), Facts::default());
        let env = Env {
            attrs: &attrs,
            mode: "M",
            parallel: true,
            facts: &facts,
        };
        for bad in ["w", "x", "nope"] {
            assert!(walk(&root, env, &[Instance::new(bad, &[])]).is_err(), "{bad}");
        }
    }

    #[test]
    fn interacting_comes_from_declared_rules_only() {
        let root = vec![lp("block.vtx", vec![row("a").guarded(&[("cb", false)]), row("b")])];
        let a = Instance::new("a", &[("block.vtx", 1)]);
        let b = Instance::new("b", &[("block.vtx", 2)]);
        assert_eq!(interaction(&root, &[a.clone(), b.clone()]), None);
        assert!(check_label(&root, &[a.clone(), b.clone()], true).is_err());
        assert!(check_label(&root, &[a.clone(), b.clone()], false).is_ok());
        let mut fa = a.clone();
        fa.footprint.insert("tx2.vin".into());
        let mut fb = b.clone();
        fb.footprint.insert("tx2.vin".into());
        assert!(matches!(interaction(&root, &[fa, fb]), Some(Rule::Footprint { .. })));
        let mut ga = b.clone();
        ga.touches.insert("cb".into());
        assert!(
            matches!(interaction(&root, &[ga, a.clone()]), Some(Rule::Guard { attr, .. }) if attr == "cb")
        );
        let mut ra = a.clone();
        ra.removes.insert(2);
        assert!(matches!(
            interaction(&root, &[ra.clone(), b.clone()]),
            Some(Rule::Removed {
                tx: 2,
                ..
            })
        ));
        assert!(check_label(&root, &[ra, b], true).is_ok());
    }

    #[test]
    fn an_earlier_cutter_guards_the_rows_after_it() {
        // `if (cb) return true;` before `b` (AcceptBlockHeader's known header, v:4631): a defect
        // that changes `cb` decides whether `b` is reached.
        let cut = Node::new(What::Block {
            body: vec![Node::new(What::Return)],
            pruned: false,
        })
        .guarded(&[("cb", true)]);
        let root = vec![row("a"), cut, row("b")];
        assert!(path_attrs(&root, "b").contains("cb"));
        assert!(!path_attrs(&root, "a").contains("cb"));
        let mut x = Instance::new("a", &[]);
        x.touches.insert("cb".into());
        let y = Instance::new("b", &[]);
        assert!(matches!(interaction(&root, &[x.clone(), y]), Some(Rule::Guard { .. })));
        // A skip cuts only the rest of its own iteration's body.
        let skip = Node::new(What::Skip).guarded(&[("cb", true)]);
        let root = vec![lp("block.vtx", vec![skip, row("in")]), row("after")];
        assert!(path_attrs(&root, "in").contains("cb"));
        assert!(!path_attrs(&root, "after").contains("cb"));
    }

    #[test]
    fn triggers_off_the_rows_loops_are_refused() {
        let root = vec![Node::new(What::Loop {
            domain: "block.vtx".into(),
            start: 1,
            body: vec![row("r")],
        })];
        let attrs = attrs();
        let facts = Facts {
            lengths: [("block.vtx".to_owned(), 3)].into(),
            ..Facts::default()
        };
        let env = Env {
            attrs: &attrs,
            mode: "M",
            parallel: false,
            facts: &facts,
        };
        for bad in [&[][..], &[("tx.vin", 1)], &[("block.vtx", 0)], &[("block.vtx", 3)]] {
            assert!(walk(&root, env, &[Instance::new("r", bad)]).is_err(), "{bad:?}");
            assert!(argmin(&root, env, &[Instance::new("r", bad)]).is_err(), "{bad:?}");
        }
        assert!(
            walk(&root, env, &[Instance::new("r", &[("block.vtx", 2)])]).expect("a walk").is_some()
        );
    }

    #[test]
    fn a_skip_outside_a_loop_is_refused() {
        let (attrs, keys) = (attrs(), BTreeSet::from(["r"]));
        let call = |body| {
            Node::new(What::Call {
                callee: "f".into(),
                body,
                pruned: false,
            })
        };
        assert!(validate(&[Node::new(What::Skip), row("r")], &attrs, &keys).is_err());
        assert!(
            validate(&[lp("block.vtx", vec![call(vec![Node::new(What::Skip)])])], &attrs, &keys)
                .is_err()
        );
        assert!(
            validate(&[lp("block.vtx", vec![Node::new(What::Skip), row("r")])], &attrs, &keys)
                .is_ok()
        );
    }

    #[test]
    fn the_rc4_model_loads_beside_its_inventory() {
        let model = Model::load().expect("the v24.0.0 model");
        assert_eq!(model.provenance.tag, "v24.0.0");
        for mode in [Mode::Submit, Mode::Proposal, Mode::Header] {
            assert!(!model.root(mode).is_empty());
        }
    }

    #[test]
    fn a_stale_model_is_refused() {
        let other = super::super::inventory::FIXTURE.replacen("rows", "rows ", 1);
        let err = Model::from_json(FIXTURE, &other).expect_err("another inventory");
        assert!(err.contains("regenerate"), "{err}");
        let renamed =
            FIXTURE.replace("ConnectBlock#9:bad-cb-amount", "ConnectBlock#9:bad-cb-amountX");
        let err = Model::from_json(&renamed, super::super::inventory::FIXTURE)
            .expect_err("a renamed row");
        assert!(err.contains("not an inventory row"), "{err}");
    }
}

/// The model witnesses (O10 plan §4.4): the synthetic ones the scanner builds
/// (`precedence-selftest.json`), the real-tree W1–W6 and W3b with their solo controls, the a10
/// cross-checks (group M at a10-5 0560c38ec, the base catalogue's single cases) and every C row
/// predicting itself on each mode it is reached in.
#[cfg(test)]
mod witnesses {
    use std::collections::{BTreeMap, BTreeSet};

    use serde::Deserialize;

    use super::*;
    use crate::chain::mutate::{Expect, Mode, Par, catalogue};

    const SELFTEST: &str = include_str!("../../../contrib/node/l3-census/precedence-selftest.json");

    #[derive(Deserialize)]
    struct Witness {
        attrs: BTreeMap<String, Attr>,
        modes: BTreeMap<String, Vec<RawNode>>,
        functions: BTreeMap<String, RawFunction>,
        cases: Vec<Case>,
    }

    /// A case; a generated solo control carries its witness's facts and every loop length.
    #[derive(Deserialize)]
    struct Case {
        name: String,
        instances: Vec<RawInstance>,
        par: Vec<u8>,
        want: Option<String>,
        deferred: bool,
        #[serde(default)]
        facts: BTreeMap<String, bool>,
        #[serde(default)]
        lengths: BTreeMap<String, usize>,
    }

    impl Case {
        fn instances(&self) -> Vec<Instance> {
            self.instances
                .iter()
                .map(|i| Instance {
                    row: i.row.clone(),
                    trigger: i.trigger.clone(),
                    ..Instance::default()
                })
                .collect()
        }

        fn facts(&self) -> Facts {
            Facts {
                facts: self.facts.clone(),
                lengths: self.lengths.clone(),
                ..Facts::default()
            }
        }
    }

    impl Witness {
        fn root(&self) -> Vec<Node> {
            self.modes["Submit"]
                .iter()
                .map(|r| convert(r, &self.functions, &mut Vec::new()))
                .collect::<Result<_, _>>()
                .expect("a tree")
        }

        /// A case's whole setting: its facts and every loop length its walk reads.
        fn setting(&self, root: &[Node], case: &Case) -> Facts {
            let mut facts = case.facts();
            let env = Env {
                attrs: &self.attrs,
                mode: "Submit",
                parallel: false,
                facts: &facts,
            };
            facts.lengths = shape(root, env, &case.instances());
            facts
        }
    }

    #[derive(Deserialize)]
    struct RawInstance {
        row: String,
        trigger: Vec<(String, usize)>,
    }

    /// §4.4's synthetic witnesses: each case's winner, walked and keyed, at each `-par` it names.
    #[test]
    fn the_synthetic_witnesses_walk_as_predicted() {
        let fixture: serde_json::Value =
            serde_json::from_str(SELFTEST).expect("the selftest fixture");
        let (mut n, mut failed) = (0, Vec::new());
        for (name, value) in fixture["witnesses"].as_object().expect("witnesses") {
            let w: Witness = serde_json::from_value(value.clone()).expect("a witness model");
            let root = w.root();
            for case in &w.cases {
                let instances = case.instances();
                let facts = case.facts();
                for &par in &case.par {
                    let env = Env {
                        attrs: &w.attrs,
                        mode: "Submit",
                        parallel: par > 1,
                        facts: &facts,
                    };
                    let walked = walk(&root, env, &instances).expect("a walk");
                    let keyed = argmin(&root, env, &instances).expect("an argmin");
                    let got = walked.as_ref().map(|f| (f.occurrence.clone(), f.deferred));
                    let want = case.want.clone().map(|w| (w, case.deferred));
                    let same = walked.map(|f| (f.occurrence, f.key))
                        == keyed.map(|f| (f.occurrence, f.key));
                    if got != want || !same {
                        failed.push(format!(
                            "{name}: {} at -par={par}: {got:?}, want {want:?}, key = walker {same}",
                            case.name
                        ));
                    }
                    n += 1;
                }
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
        assert!(n >= 25, "{n} synthetic predictions");
    }

    fn none(_: &mut Facts) {}

    fn at(vtx: usize) -> At {
        At {
            vtx: Some(vtx),
            vin: None,
            vout: None,
        }
    }

    fn at_in(vtx: usize, vin: usize) -> At {
        At {
            vtx: Some(vtx),
            vin: Some(vin),
            vout: None,
        }
    }

    const CB_LENGTH: &str = "validation.cpp:CheckBlock#1:bad-blk-length";
    const CB_SIGOPS: &str = "validation.cpp:CheckBlock#4:bad-blk-sigops";
    const CT_VIN_EMPTY: &str = "tx_check.cpp:CheckTransaction#1:bad-txns-vin-empty";
    const CCB_FINAL: &str = "validation.cpp:ContextualCheckBlock#2:bad-txns-nonfinal";
    const CCT_OVERSIZE: &str = "validation.cpp:ContextualCheckTransaction#5:bad-txns-oversize";
    const CONN_FEES: &str = "validation.cpp:ConnectBlock#4:bad-txns-accumulated-fee-outofrange";
    const CONN_BIP68: &str = "validation.cpp:ConnectBlock#5:bad-txns-nonfinal";
    const CONN_SIGOPS: &str = "validation.cpp:ConnectBlock#6:bad-blk-sigops";
    const CONN_QUEUED: &str = "validation.cpp:ConnectBlock#7:block-validation-failed";
    const CONN_AMOUNT: &str = "validation.cpp:ConnectBlock#9:bad-cb-amount";
    const CIS_SCRIPT: &str =
        "validation.cpp:CheckInputScripts#2:mandatory-script-verify-flag-failed (%s)";
    const CTI_MISSING: &str =
        "tx_verify.cpp:Consensus::CheckTxInputs#1:bad-txns-inputs-missingorspent";
    const CTI_PREMATURE: &str =
        "tx_verify.cpp:Consensus::CheckTxInputs#2:bad-txns-premature-spend-of-coinbase";
    const CTI_VALUES: &str =
        "tx_verify.cpp:Consensus::CheckTxInputs#3:bad-txns-inputvalues-outofrange";
    const CTI_BELOW: &str = "tx_verify.cpp:Consensus::CheckTxInputs#4:bad-txns-in-belowout";
    const STX_DIP3: &str = "specialtxman.cpp:CheckSpecialTxInner#1:bad-tx-type-dip3-inactive";
    const STX_CB_PAYLOAD: &str = "specialtxman.cpp:ProcessSpecialTxsInBlock#2:bad-cbtx-payload";

    /// Where a case's setting starts: its own extra facts over the defaults, or (a solo control)
    /// the whole setting of the named witness, so the control keeps the witness's measured facts
    /// and base shape, every loop length explicit (DEC-998 M-1).
    #[derive(Clone, Copy)]
    enum Base {
        Extra(fn(&mut Facts)),
        Of(&'static str),
    }

    /// (name, modes, defects, base, expected occurrence per -par: (par1, par4)).
    type Real =
        (&'static str, &'static [Mode], Vec<(&'static str, At)>, Base, [(&'static str, bool); 2]);

    const BOTH: &[Mode] = &[Mode::Submit, Mode::Proposal];

    fn twelve(f: &mut Facts) {
        f.lengths.insert("block.vtx".into(), 12);
    }

    /// A case's placed instances and setting in `mode` at `par` ([`setting`]); a solo's starts
    /// from its witness's.
    fn real_setting(
        model: &Model,
        cases: &[Real],
        case: &Real,
        mode: Mode,
        par: Par,
    ) -> Result<(Vec<Instance>, Facts), Unplaced> {
        match case.3 {
            Base::Extra(extra) => setting(model, mode, par, &case.2, Facts::default(), &extra),
            Base::Of(name) => {
                let witness =
                    cases.iter().find(|c| c.0 == name).unwrap_or_else(|| panic!("no case {name}"));
                let (_, start) = real_setting(model, cases, witness, mode, par)?;
                setting(model, mode, par, &case.2, start, &none)
            }
        }
    }

    /// The real-source witnesses of §4.4 (O10-2 runs them against dashd), each with its reversed
    /// placement and, for each constituent, its solo control in the witness's setting.
    fn real_witnesses() -> Vec<Real> {
        use Base::{Extra, Of};
        let same = |row: &'static str| [(row, false), (row, false)];
        let w4r = "W4 reversed: + tx2 BIP68";
        let w5last = "W5 vin_empty at the last index";
        let w6last = "W6 sigops + vin_empty at the last of 12";
        let w6one = "W6 vin_empty at index 1";
        vec![
            (
                "W1",
                BOTH,
                vec![(CONN_BIP68, at(1)), (CTI_BELOW, at(2))],
                Extra(none),
                same(CONN_BIP68),
            ),
            ("W1 solo BIP68", BOTH, vec![(CONN_BIP68, at(1))], Of("W1"), same(CONN_BIP68)),
            ("W1 solo in-belowout", BOTH, vec![(CTI_BELOW, at(2))], Of("W1"), same(CTI_BELOW)),
            (
                "W1 reversed",
                BOTH,
                vec![(CTI_BELOW, at(1)), (CONN_BIP68, at(2))],
                Extra(none),
                same(CTI_BELOW),
            ),
            (
                "W1 reversed solo in-belowout",
                BOTH,
                vec![(CTI_BELOW, at(1))],
                Of("W1 reversed"),
                same(CTI_BELOW),
            ),
            (
                "W1 reversed solo BIP68",
                BOTH,
                vec![(CONN_BIP68, at(2))],
                Of("W1 reversed"),
                same(CONN_BIP68),
            ),
            (
                "W2",
                BOTH,
                vec![(CCT_OVERSIZE, at(1)), (CCB_FINAL, at(2))],
                Extra(none),
                same(CCT_OVERSIZE),
            ),
            ("W2 solo oversize", BOTH, vec![(CCT_OVERSIZE, at(1))], Of("W2"), same(CCT_OVERSIZE)),
            ("W2 solo nonfinal", BOTH, vec![(CCB_FINAL, at(2))], Of("W2"), same(CCB_FINAL)),
            (
                "W2 reversed",
                BOTH,
                vec![(CCB_FINAL, at(1)), (CCT_OVERSIZE, at(2))],
                Extra(none),
                same(CCB_FINAL),
            ),
            (
                "W2 reversed solo nonfinal",
                BOTH,
                vec![(CCB_FINAL, at(1))],
                Of("W2 reversed"),
                same(CCB_FINAL),
            ),
            (
                "W2 reversed solo oversize",
                BOTH,
                vec![(CCT_OVERSIZE, at(2))],
                Of("W2 reversed"),
                same(CCT_OVERSIZE),
            ),
            (
                "W3 [MAX, MAX, immature]",
                BOTH,
                vec![(CTI_VALUES, at_in(1, 1)), (CTI_PREMATURE, at_in(1, 2))],
                Extra(none),
                same(CTI_VALUES),
            ),
            (
                "W3 solo range",
                BOTH,
                vec![(CTI_VALUES, at_in(1, 1))],
                Of("W3 [MAX, MAX, immature]"),
                same(CTI_VALUES),
            ),
            (
                "W3 solo immature",
                BOTH,
                vec![(CTI_PREMATURE, at_in(1, 2))],
                Of("W3 [MAX, MAX, immature]"),
                same(CTI_PREMATURE),
            ),
            (
                "W3 reversed [immature, MAX, MAX]",
                BOTH,
                vec![(CTI_PREMATURE, at_in(1, 0)), (CTI_VALUES, at_in(1, 2))],
                Extra(none),
                same(CTI_PREMATURE),
            ),
            (
                "W3 reversed solo immature",
                BOTH,
                vec![(CTI_PREMATURE, at_in(1, 0))],
                Of("W3 reversed [immature, MAX, MAX]"),
                same(CTI_PREMATURE),
            ),
            (
                "W3 reversed solo range",
                BOTH,
                vec![(CTI_VALUES, at_in(1, 2))],
                Of("W3 reversed [immature, MAX, MAX]"),
                same(CTI_VALUES),
            ),
            (
                "W3b [MAX mature, immature]",
                BOTH,
                vec![(CTI_VALUES, at_in(1, 1)), (CTI_PREMATURE, at_in(1, 1))],
                Extra(none),
                same(CTI_PREMATURE),
            ),
            (
                "W3b solo range",
                BOTH,
                vec![(CTI_VALUES, at_in(1, 1))],
                Of("W3b [MAX mature, immature]"),
                same(CTI_VALUES),
            ),
            (
                "W3b solo immature",
                BOTH,
                vec![(CTI_PREMATURE, at_in(1, 1))],
                Of("W3b [MAX mature, immature]"),
                same(CTI_PREMATURE),
            ),
            (
                "W4",
                BOTH,
                vec![(CIS_SCRIPT, at_in(1, 0))],
                Extra(none),
                [(CIS_SCRIPT, false), (CONN_QUEUED, true)],
            ),
            (
                w4r,
                BOTH,
                vec![(CIS_SCRIPT, at_in(1, 0)), (CONN_BIP68, at(2))],
                Extra(none),
                [(CIS_SCRIPT, false), (CONN_BIP68, false)],
            ),
            (
                "W4 reversed solo script",
                BOTH,
                vec![(CIS_SCRIPT, at_in(1, 0))],
                Of(w4r),
                [(CIS_SCRIPT, false), (CONN_QUEUED, true)],
            ),
            ("W4 reversed solo BIP68", BOTH, vec![(CONN_BIP68, at(2))], Of(w4r), same(CONN_BIP68)),
            (
                "W5 size + tx1 vin_empty",
                BOTH,
                vec![(CB_LENGTH, At::default()), (CT_VIN_EMPTY, at(1))],
                Extra(none),
                same(CB_LENGTH),
            ),
            (
                "W5 solo size",
                BOTH,
                vec![(CB_LENGTH, At::default())],
                Of("W5 size + tx1 vin_empty"),
                same(CB_LENGTH),
            ),
            (
                "W5 solo vin_empty",
                BOTH,
                vec![(CT_VIN_EMPTY, at(1))],
                Of("W5 size + tx1 vin_empty"),
                same(CT_VIN_EMPTY),
            ),
            (
                w5last,
                BOTH,
                vec![(CB_LENGTH, At::default()), (CT_VIN_EMPTY, at(11))],
                Extra(twelve),
                same(CB_LENGTH),
            ),
            (
                "W5 last solo size",
                BOTH,
                vec![(CB_LENGTH, At::default())],
                Of(w5last),
                same(CB_LENGTH),
            ),
            (
                "W5 last solo vin_empty",
                BOTH,
                vec![(CT_VIN_EMPTY, at(11))],
                Of(w5last),
                same(CT_VIN_EMPTY),
            ),
            (
                w6last,
                BOTH,
                vec![(CB_SIGOPS, At::default()), (CT_VIN_EMPTY, at(11))],
                Extra(twelve),
                same(CT_VIN_EMPTY),
            ),
            ("W6 solo sigops", BOTH, vec![(CB_SIGOPS, At::default())], Of(w6last), same(CB_SIGOPS)),
            (
                "W6 solo vin_empty",
                BOTH,
                vec![(CT_VIN_EMPTY, at(11))],
                Of(w6last),
                same(CT_VIN_EMPTY),
            ),
            (
                w6one,
                BOTH,
                vec![(CB_SIGOPS, At::default()), (CT_VIN_EMPTY, at(1))],
                Extra(twelve),
                same(CT_VIN_EMPTY),
            ),
            (
                "W6 index 1 solo sigops",
                BOTH,
                vec![(CB_SIGOPS, At::default())],
                Of(w6one),
                same(CB_SIGOPS),
            ),
            (
                "W6 index 1 solo vin_empty",
                BOTH,
                vec![(CT_VIN_EMPTY, at(1))],
                Of(w6one),
                same(CT_VIN_EMPTY),
            ),
        ]
    }

    /// Every case's prediction against its expectation, each failure named (a model mutant's
    /// run lists every witness it fails).
    fn check(model: &Model, cases: Vec<Real>) -> usize {
        let (mut n, mut failed) = (0, Vec::new());
        for case in &cases {
            let (name, modes, _, _, want) = case;
            for &mode in *modes {
                for (par, &(row, deferred)) in [Par::One, Par::Four].into_iter().zip(want) {
                    let Ok((instances, facts)) = real_setting(model, &cases, case, mode, par)
                    else {
                        failed.push(format!("{name} on {mode:?} at {par:?}: no such block"));
                        continue;
                    };
                    let (got, same) = walked(model, mode, par, &instances, &facts);
                    let want = Some((row.to_owned(), deferred));
                    if got != want || !same {
                        failed.push(format!("{name} on {mode:?} at {par:?}: {got:?}, want {want:?}, key = walker {same}"));
                    }
                    n += 1;
                }
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
        n
    }

    #[test]
    fn the_real_witnesses_predict_as_the_plan_reads_the_source() {
        let model = Model::load().expect("the v24.0.0 model");
        assert_eq!(check(&model, real_witnesses()), 37 * 4);
    }

    /// A row's own bound occurrence alone, per `-par`: a script fault's is the wait at par > 1.
    fn own(row: &'static str) -> [(&'static str, bool); 2] {
        match row {
            CIS_SCRIPT => [(CIS_SCRIPT, false), (CONN_QUEUED, true)],
            _ => [(row, false), (row, false)],
        }
    }

    /// The constituents of `cases`' multi-instance witnesses, and those with no solo control: a
    /// single-defect case at the same placement on each of the witness's modes, expecting its own
    /// bound occurrence, whose instance and whole setting (facts and every loop length, after
    /// `reshape`) equal the witness's in each mode at both `-par` values.
    fn real_missing(
        model: &Model,
        cases: &[Real],
        reshape: &dyn Fn(&str, &mut Facts),
    ) -> (usize, Vec<String>) {
        let (mut constituents, mut missing) = (0, Vec::new());
        for witness in cases.iter().filter(|c| c.2.len() > 1) {
            let (name, modes, defects, _, _) = witness;
            for &(row, at) in defects {
                constituents += 1;
                let solo = cases.iter().any(|s| {
                    s.2.as_slice() == [(row, at)]
                        && s.4 == own(row)
                        && modes.iter().all(|mode| s.1.contains(mode))
                        && modes.iter().all(|&mode| {
                            [Par::One, Par::Four].into_iter().all(|par| {
                                let (Ok((wi, wf)), Ok((si, mut sf))) = (
                                    real_setting(model, cases, witness, mode, par),
                                    real_setting(model, cases, s, mode, par),
                                ) else {
                                    return false;
                                };
                                reshape(s.0, &mut sf);
                                let own: Vec<Instance> =
                                    wi.into_iter().filter(|i| i.row == row).collect();
                                si == own && sf == wf
                            })
                        })
                });
                if !solo {
                    missing.push(format!("{name}: {row} at {at:?}"));
                }
            }
        }
        (constituents, missing)
    }

    /// DEC-998 M-1: every constituent of every multi-instance real witness has its solo control,
    /// at the same placement, measured facts and base shape, on each of the witness's modes,
    /// expecting its own bound occurrence at both `-par` values.
    #[test]
    fn every_real_witness_constituent_has_its_solo() {
        let model = Model::load().expect("the v24.0.0 model");
        let cases = real_witnesses();
        let (constituents, missing) = real_missing(&model, &cases, &|_, _| {});
        assert!(missing.is_empty(), "constituents with no solo control: {missing:#?}");
        assert_eq!(constituents, 2 * 12, "the 12 multi-instance witnesses' constituents");
        // A control that keeps the trigger but runs on a shorter block (W4 reversed's script
        // solo on W4's two transactions), or one deleted, leaves its constituent uncovered.
        let shrunk = |name: &str, f: &mut Facts| {
            if name == "W4 reversed solo script" {
                *f.lengths.get_mut("block.vtx").expect("a block.vtx length") -= 1;
            }
        };
        let gap = vec![format!("W4 reversed: + tx2 BIP68: {CIS_SCRIPT} at {:?}", at_in(1, 0))];
        assert_eq!(real_missing(&model, &cases, &shrunk).1, gap);
        let fewer: Vec<Real> =
            cases.iter().filter(|c| c.0 != "W4 reversed solo script").cloned().collect();
        assert_eq!(real_missing(&model, &fewer, &|_, _| {}).1, gap);
    }

    /// The synthetic fixture's constituents (instance × `-par` of each multi-instance case), and
    /// those with no solo control: a single-instance case of the same group holding the instance
    /// at that `-par`, expecting its own row (a deferred row's wait at par > 1), in the case's
    /// whole setting (facts and every loop length).
    fn synthetic_missing(fixture: &serde_json::Value) -> (usize, Vec<String>) {
        let (mut constituents, mut missing) = (0, Vec::new());
        for (name, value) in fixture["witnesses"].as_object().expect("witnesses") {
            let w: Witness = serde_json::from_value(value.clone()).expect("a witness model");
            let root = w.root();
            let mut waits = BTreeMap::new();
            defers(&root, &mut waits);
            for case in w.cases.iter().filter(|c| c.instances.len() > 1) {
                let setting = w.setting(&root, case);
                for i in &case.instances {
                    for &par in &case.par {
                        constituents += 1;
                        let want = match waits.get(&i.row) {
                            Some(wait) if par > 1 => (Some(wait.clone()), true),
                            _ => (Some(i.row.clone()), false),
                        };
                        let solo = w.cases.iter().any(|c| {
                            c.instances.len() == 1
                                && c.instances[0].row == i.row
                                && c.instances[0].trigger == i.trigger
                                && c.par.contains(&par)
                                && (c.want.clone(), c.deferred) == want
                                && w.setting(&root, c) == setting
                        });
                        if !solo {
                            missing.push(format!("{name}: {}: {} at -par={par}", case.name, i.row));
                        }
                    }
                }
            }
        }
        (constituents, missing)
    }

    /// DEC-998 M-1 for the synthetic fixture: every constituent has its solo control.
    #[test]
    fn every_synthetic_constituent_has_its_solo() {
        let mut fixture: serde_json::Value =
            serde_json::from_str(SELFTEST).expect("the selftest fixture");
        let (constituents, missing) = synthetic_missing(&fixture);
        assert!(missing.is_empty(), "constituents with no solo control: {missing:#?}");
        assert_eq!(constituents, 54, "synthetic constituents (instance × -par)");
        // A control that keeps its trigger but has one loop shorter, or none, leaves a gap.
        let cases = fixture["witnesses"]["connect"]["cases"].as_array_mut().expect("cases");
        let solo = cases
            .iter()
            .position(|c| c["lengths"]["block.vtx"] == 3 && c["deferred"] == true)
            .expect("the deferred script solo on three transactions");
        let mut shrunk = fixture.clone();
        shrunk["witnesses"]["connect"]["cases"][solo]["lengths"]["block.vtx"] = 2.into();
        assert!(!synthetic_missing(&shrunk).1.is_empty(), "a shrunk control passes");
        fixture["witnesses"]["connect"]["cases"].as_array_mut().expect("cases").remove(solo);
        assert!(!synthetic_missing(&fixture).1.is_empty(), "a deleted control passes");
    }

    /// Each `Defer`'s row and the wait that reports it.
    fn defers(nodes: &[Node], out: &mut BTreeMap<String, String>) {
        for n in nodes {
            if let What::Defer {
                row,
                wait,
            } = &n.what
            {
                out.insert(row.clone(), wait.clone());
            }
            defers(n.body(), out);
        }
    }

    /// a10-5 group M (0560c38ec, `orders!` and `matrix`): the script fault is `fund` then
    /// `spend` (the failing input at the spend's vin 0); F's failing transaction is after it
    /// (`script_first`), before it (`f_first`), or the spend itself with the fault as its extra,
    /// last input (`same`; `m_accumulated_fee`'s second fee is the fault's input alone,
    /// 742ce42c7). `script_first` expects the script reason at -par=1 and F's at -par=4; the
    /// other orders F's at both.
    #[test]
    fn the_model_predicts_a10_group_m() {
        let model = Model::load().expect("the v24.0.0 model");
        // (family, F's row, the input F's row fires at: only rows inside CheckTxInputs' input
        // loop have one; F's transaction count, the failing one last; the fault's input index
        // in F's failing transaction under `same`: its last, unpaid in `m_inputvalues`
        // (a648651b2))
        let families: [(&str, &str, Option<usize>, usize, usize); 7] = [
            ("m_missing", CTI_MISSING, None, 1, 1),
            ("m_premature", CTI_PREMATURE, Some(0), 1, 1),
            ("m_in_belowout", CTI_BELOW, None, 1, 1),
            ("m_bip68", CONN_BIP68, None, 1, 1),
            // `legacy_block`'s transaction, then `locked_spend`'s fund and P2SH spend.
            ("m_sigops", CONN_SIGOPS, None, 3, 1),
            ("m_inputvalues", CTI_VALUES, Some(1), 1, 2),
            // Two fee transactions, the second crossing `MAX_MONEY`: in `same`, the fault's input
            // alone, all of it fee (742ce42c7).
            ("m_accumulated_fee", CONN_FEES, None, 2, 0),
        ];
        let f_at = |vtx, vin| At {
            vtx: Some(vtx),
            vin,
            vout: None,
        };
        let mut cases: Vec<Real> = Vec::new();
        for (family, f, j, txs, extra_vin) in families {
            // script_first: fund 1, spend 2 (fault at vin 0), then F's transactions.
            cases.push((
                family,
                BOTH,
                vec![(CIS_SCRIPT, at_in(2, 0)), (f, f_at(2 + txs, j))],
                Base::Extra(none),
                [(CIS_SCRIPT, false), (f, false)],
            ));
            // f_first: F's transactions from 1, then fund and spend.
            cases.push((
                family,
                BOTH,
                vec![(f, f_at(txs, j)), (CIS_SCRIPT, at_in(txs + 2, 0))],
                Base::Extra(none),
                [(f, false), (f, false)],
            ));
            // same: fund 1, then F's transactions, the failing one holding the fault as its
            // extra, last input.
            cases.push((
                family,
                BOTH,
                vec![(f, f_at(1 + txs, j)), (CIS_SCRIPT, at_in(1 + txs, extra_vin))],
                Base::Extra(none),
                [(f, false), (f, false)],
            ));
        }
        // r7: the fault and a coinbase over its reward (v:2791 before v:2857).
        cases.push((
            "m_script_cb_amount",
            BOTH,
            vec![(CIS_SCRIPT, at_in(2, 0)), (CONN_AMOUNT, At::default())],
            Base::Extra(none),
            [(CIS_SCRIPT, false), (CONN_QUEUED, true)],
        ));
        // r7: a version-3 type-1 spend and the fault, either order: the special-tx gate first.
        fn special_at_3(f: &mut Facts) {
            f.iter.insert("has_payload".into(), [3].into());
        }
        fn special_at_1(f: &mut Facts) {
            f.iter.insert("has_payload".into(), [1].into());
        }
        cases.push((
            "m_script_dip3_gate script_first",
            BOTH,
            vec![(CIS_SCRIPT, at_in(2, 0)), (STX_DIP3, at(3))],
            Base::Extra(special_at_3),
            [(STX_DIP3, false), (STX_DIP3, false)],
        ));
        cases.push((
            "m_script_dip3_gate gate_first",
            BOTH,
            vec![(STX_DIP3, at(1)), (CIS_SCRIPT, at_in(3, 0))],
            Base::Extra(special_at_1),
            [(STX_DIP3, false), (STX_DIP3, false)],
        ));
        // r7: the fault under a version-1 type-5 coinbase: its payload check before the loop.
        cases.push((
            "m_script_cbtx_payload",
            BOTH,
            vec![(STX_CB_PAYLOAD, At::default()), (CIS_SCRIPT, at_in(2, 0))],
            Base::Extra(none),
            [(STX_CB_PAYLOAD, false), (STX_CB_PAYLOAD, false)],
        ));
        assert_eq!(check(&model, cases), (7 * 3 + 4) * 4);
    }

    const CCB_HEIGHT: &str = "validation.cpp:ContextualCheckBlock#4:bad-cb-height";
    const CCB_CB_TYPE: &str = "validation.cpp:ContextualCheckBlock#5:bad-cb-type";
    const CCT_TYPE: &str = "validation.cpp:ContextualCheckTransaction#1:bad-txns-type";
    const CCT_CB_TYPE: &str = "validation.cpp:ContextualCheckTransaction#2:bad-txns-cb-type";
    const CCT_VERSION: &str = "validation.cpp:ContextualCheckTransaction#4:bad-txns-version";

    /// The line of `key`'s row in `nodes`' tree.
    fn row_line(nodes: &[Node], key: &str) -> Option<u32> {
        nodes.iter().find_map(|n| match &n.what {
            What::Row {
                key: k,
                ..
            } if k == key => Some(n.line),
            _ => row_line(n.body(), key),
        })
    }

    /// O10-1's probe universe, now the generator's (O10-2 carry-overs 1-3): 18,762 answers; the
    /// 40 pre/post-DIP3 pairs infeasible on `dip3`; the 96 AcceptBlockHeader `duplicate`-first
    /// pairs unreachable after its return (v:4619-4625); no answer a panic. Item 8b's
    /// same-index universe counts its iteration-guard conflicts.
    #[test]
    fn the_pair_universe_counts() {
        let model = Model::load().expect("the v24.0.0 model");
        let count = |answers: &[Probe]| {
            let mut n: BTreeMap<String, usize> = BTreeMap::new();
            for (.., answer) in answers {
                let key = match answer {
                    Ok((Some(_), true)) => "fired".to_owned(),
                    Ok((None, true)) => "none".to_owned(),
                    Ok((_, false)) => "key-disagrees".to_owned(),
                    Err(e) => e.reason(),
                };
                *n.entry(key).or_default() += 1;
            }
            n
        };
        let o10_1 = universe(&model, at(2));
        let n = count(&o10_1);
        assert_eq!(o10_1.len(), 18_762);
        assert_eq!(n.get("unreachable-after-return"), Some(&96), "{n:?}");
        assert_eq!(n.get("infeasible:dip3"), Some(&40), "{n:?}");
        assert_eq!(n.get("key-disagrees"), None, "{n:?}");
        for (mode, par, defects, answer) in &o10_1 {
            if let Err(Unplaced::AfterReturn {
                after,
                ..
            }) = answer
            {
                assert!(
                    after.len() == 1 && after[0].starts_with("validation.cpp:AcceptBlockHeader#"),
                    "{mode:?} {par:?} {defects:?}: {answer:?}"
                );
            }
        }
        // Item 8b: two defects at transaction 1 whose paths need `coinbase` (CheckBlock's
        // v:4410-4412 `IsCoinBase` at i ≥ 1 against a non-coinbase path) or `special_version`
        // (v:358 `IsSpecialTxVersion`, which v:381's `nVersion < 0` rules out, BL-680) both
        // ways are refused, never guessed; at transaction 2 none arises.
        let same = count(&universe(&model, at(1)));
        assert_eq!(same.get("iter-conflict:coinbase"), Some(&38), "{same:?}");
        assert_eq!(same.get("iter-conflict:special_version"), Some(&32), "{same:?}");
        assert!(n.keys().all(|k| !k.starts_with("iter-conflict")), "{n:?}");
    }

    /// v:4591 guards bad-cb-height with `HEIGHTINCB && !fDIP0003Active_context`; v:4600 guards
    /// bad-cb-type, and v:356 CCT's type rows, with `fDIP0003Active_context`. No block is both
    /// before and after DIP3, so such a pair is infeasible, in either order; each side alone, or
    /// two post-DIP3 rows together, is a block.
    #[test]
    fn a_pre_and_post_dip3_pair_is_infeasible() {
        let model = Model::load().expect("the v24.0.0 model");
        let dip3 = |defects: &[(&str, At)], mode, par| {
            let (_, facts) = setting(&model, mode, par, defects, Facts::default(), &none)
                .expect("a feasible block");
            facts.facts.get("dip3").copied()
        };
        for &mode in BOTH {
            assert_eq!(row_line(model.root(mode), CCB_HEIGHT), Some(4596));
            for par in [Par::One, Par::Four] {
                for (a, b) in [(CCB_HEIGHT, CCB_CB_TYPE), (CCB_CB_TYPE, CCB_HEIGHT)] {
                    let pair = [(a, At::default()), (b, At::default())];
                    assert_eq!(
                        predicted(&model, mode, par, &pair, &none),
                        Err(Unplaced::Infeasible(Infeasible {
                            fact: "dip3".into(),
                            rows: vec![a.into(), b.into()],
                        })),
                        "{a} + {b} on {mode:?} at {par:?}"
                    );
                }
                let pair = [(CCB_HEIGHT, At::default()), (CCT_TYPE, at(1))];
                assert!(predicted(&model, mode, par, &pair, &none).is_err());
                let alone = [(CCB_HEIGHT, At::default())];
                assert_eq!(
                    predicted(&model, mode, par, &alone, &none),
                    Ok((Some((CCB_HEIGHT.to_owned(), false)), true))
                );
                assert_eq!(dip3(&alone, mode, par), Some(false));
                let post = [(CCT_TYPE, at(1)), (CCB_CB_TYPE, At::default())];
                assert_eq!(
                    predicted(&model, mode, par, &post, &none),
                    Ok((Some((CCT_TYPE.to_owned(), false)), true))
                );
                assert_eq!(dip3(&post, mode, par), Some(true));
            }
        }
    }

    /// BL-680: v:381-382 rejects a transaction with `nVersion < 0` (CCT#4), and such a
    /// transaction is never `IsSpecialTxVersion()` (`nVersion >= 3`, transaction.h:334). So
    /// CCT#4 at a transaction needs `special_version` false there, and with v:358's special
    /// version rows (CCT#1, #2) at the same index the pair is the typed iteration conflict,
    /// in either order. CCT#4 alone fires, and at two indices the pair is a block whose first
    /// row fires, in either order.
    #[test]
    fn a_negative_version_tx_is_never_special_at_its_index() {
        let model = Model::load().expect("the v24.0.0 model");
        for &mode in BOTH {
            for par in [Par::One, Par::Four] {
                for special in [CCT_TYPE, CCT_CB_TYPE] {
                    for (a, b) in [(special, CCT_VERSION), (CCT_VERSION, special)] {
                        assert_eq!(
                            predicted(&model, mode, par, &[(a, at(1)), (b, at(1))], &none),
                            Err(Unplaced::IterConflict {
                                attr: "special_version".into(),
                                index: 1,
                                rows: vec![a.into(), b.into()],
                            }),
                            "{a} + {b} at transaction 1 on {mode:?} at {par:?}"
                        );
                    }
                }
                let (_, facts) =
                    setting(&model, mode, par, &[(CCT_VERSION, at(1))], Facts::default(), &none)
                        .expect("CCT#4 alone has a block");
                assert_eq!(facts.iter.get("special_version"), Some(&BTreeSet::new()));
                assert_eq!(
                    predicted(&model, mode, par, &[(CCT_VERSION, at(1))], &none),
                    Ok((Some((CCT_VERSION.to_owned(), false)), true))
                );
                for (first, apart) in [
                    (CCT_TYPE, [(CCT_TYPE, at(1)), (CCT_VERSION, at(2))]),
                    (CCT_VERSION, [(CCT_VERSION, at(1)), (CCT_TYPE, at(2))]),
                ] {
                    assert_eq!(
                        predicted(&model, mode, par, &apart, &none),
                        Ok((Some((first.to_owned(), false)), true))
                    );
                }
            }
        }
    }

    /// The pre row of a mode's root whose reply is `message` (an RPC refusal before validation).
    fn pre_row(model: &Model, mode: Mode, message: &str) -> Option<String> {
        model.root(mode).iter().find_map(|n| match &n.what {
            What::Row {
                key,
                kind: RowKind::Pre,
            } if key.ends_with(&format!(":{message}")) => Some(key.clone()),
            _ => None,
        })
    }

    /// Every base-catalogue variant (`chain::mutate::catalogue`), on the modes its legs run
    /// (Submit, Proposal) at both -par: the defects are every occurrence it expects anywhere,
    /// and the model's winner is the one it expects there. `queued_script_then_bip68` puts the
    /// script fault before its BIP68 transaction; `tx_type_dip3`'s spend carries a payload.
    #[test]
    fn the_model_predicts_the_base_catalogue() {
        let model = Model::load().expect("the v24.0.0 model");
        let mut n = 0;
        for op in catalogue() {
            // Group M's two faults need their places (fund, spend, F's transactions):
            // `the_model_predicts_a10_group_m` covers every m_ operator with them.
            if op.name.starts_with("m_") {
                continue;
            }
            for v in op.variants {
                // a10-5's scenarios (`scenario_steps`) answer from the index their earlier ops
                // left (`Expect::Answer`): no one-block prediction covers them.
                if crate::chain::mutate::scenario_steps(op.name, v.name).is_some() {
                    continue;
                }
                // a10-5's expectations are per site (`Variant::expect`); the model's are not.
                for &site in v.sites {
                    let mut defects: Vec<(String, At)> = Vec::new();
                    for mode in [Mode::Submit, Mode::Proposal] {
                        for par in [Par::One, Par::Four] {
                            let row = match (v.expect)(mode, par, site) {
                                Expect::Accept => continue,
                                Expect::Answer(a) => unreachable!("a scenario's answer {a}"),
                                Expect::Reject {
                                    occurrence,
                                    ..
                                } => occurrence.to_owned(),
                                Expect::Refused {
                                    message,
                                    ..
                                } => pre_row(&model, mode, message)
                                    .unwrap_or_else(|| panic!("{}: no pre row {message}", op.name)),
                            };
                            let place = match (op.name, row.as_str()) {
                                ("queued_script_then_bip68", CIS_SCRIPT) => at_in(1, 0),
                                ("queued_script_then_bip68", CONN_BIP68) => at(2),
                                _ => At::default(),
                            };
                            if !defects.iter().any(|(r, _)| *r == row) && row != CONN_QUEUED {
                                defects.push((row, place));
                            }
                        }
                    }
                    let defects: Vec<(&str, At)> =
                        defects.iter().map(|(r, a)| (r.as_str(), *a)).collect();
                    let extra: fn(&mut Facts) = match op.name {
                        "tx_type_dip3" => |f| {
                            f.iter.insert("has_payload".into(), [1].into());
                        },
                        _ => none,
                    };
                    for mode in [Mode::Submit, Mode::Proposal] {
                        for par in [Par::One, Par::Four] {
                            let want = match (v.expect)(mode, par, site) {
                                Expect::Accept => None,
                                Expect::Answer(a) => unreachable!("a scenario's answer {a}"),
                                Expect::Reject {
                                    occurrence,
                                    ..
                                } => Some((occurrence.to_owned(), occurrence == CONN_QUEUED)),
                                Expect::Refused {
                                    message,
                                    ..
                                } => pre_row(&model, mode, message).map(|r| (r, false)),
                            };
                            let (got, same) = predicted(&model, mode, par, &defects, &extra)
                                .expect("a feasible block");
                            assert!(
                                got == want && same,
                                "{}.{} at {site:?} on {mode:?} at {par:?}: {got:?}, want {want:?}, key = walker {same}",
                                op.name,
                                v.name
                            );
                            n += 1;
                        }
                    }
                }
            }
        }
        assert!(n >= 4 * 30, "{n} catalogue predictions");
    }

    /// C rows whose condition is, on one mode, exactly an earlier RPC precheck's: a lone defect
    /// there is the precheck's too, and the precheck refuses first (so a10's census leaves that
    /// mode out of the row's paths).
    const SAME_CONDITION: [(&str, Mode, &str); 3] = [
        // mi:1033 `block.vtx.empty() || !block.vtx[0]->IsCoinBase()`; v:4409.
        (
            "validation.cpp:CheckBlock#2:bad-cb-missing",
            Mode::Submit,
            "mining.cpp:submitblock@pre1033:Block does not start with a coinbase",
        ),
        // mi:1046 the known index's `nStatus & BLOCK_FAILED_MASK`; v:4625.
        (
            "validation.cpp:AcceptBlockHeader#1:duplicate",
            Mode::Submit,
            "mining.cpp:submitblock@pre1046:duplicate-invalid",
        ),
        // mi:1091 the previous header not in the index; v:4644.
        (
            "validation.cpp:AcceptBlockHeader#3:prev-blk-not-found",
            Mode::Header,
            "mining.cpp:submitheader@pre1091:Must submit previous header",
        ),
    ];

    /// Each C row alone, on each mode and `-par`: it binds exactly on the modes a10's census
    /// reaches it (its inventory `paths`), as itself (a script fault at -par=4 as ConnectBlock#7,
    /// the wait row no defect holds alone). Off its paths the model never reaches it, or the
    /// mode is a [`SAME_CONDITION`] one, where the row with its precheck binds the precheck.
    #[test]
    fn every_c_row_predicts_itself() {
        let model = Model::load().expect("the v24.0.0 model");
        let inventory: serde_json::Value =
            serde_json::from_str(super::super::inventory::FIXTURE).expect("the inventory");
        let (mut bound, mut paths_total) = (0, 0);
        for row in inventory["rows"].as_array().expect("rows") {
            if row["decision"] != "C" || row["kind"] != "R" {
                continue;
            }
            let key = row["key"].as_str().expect("a key");
            let paths: BTreeSet<&str> =
                row["paths"].as_array().expect("paths").iter().filter_map(|p| p.as_str()).collect();
            for mode in [Mode::Submit, Mode::Proposal, Mode::Header] {
                let on_path = paths.contains(mode_name(mode));
                paths_total += usize::from(on_path);
                let precheck =
                    SAME_CONDITION.iter().find(|(r, m, _)| *r == key && *m == mode).map(|t| t.2);
                if key == CONN_QUEUED {
                    let (got, same) =
                        predicted(&model, mode, Par::Four, &[(CIS_SCRIPT, at_in(1, 0))], &none)
                            .expect("a feasible block");
                    assert!(same, "{key} on {mode:?}: key = walker");
                    assert_eq!(
                        got.is_some_and(|g| g == (key.to_owned(), true)),
                        on_path,
                        "{key} on {mode:?}"
                    );
                    bound += usize::from(on_path);
                    continue;
                }
                let mut here = false;
                for par in [Par::One, Par::Four] {
                    let (got, same) = predicted(&model, mode, par, &[(key, At::default())], &none)
                        .expect("a feasible block");
                    assert!(same, "{key} on {mode:?} at {par:?}: key = walker");
                    if let Some(pre) = precheck {
                        assert!(!on_path, "{key} on {mode:?}: a same-condition mode on its paths");
                        let pair = [(key, At::default()), (pre, At::default())];
                        let (got, same) =
                            predicted(&model, mode, par, &pair, &none).expect("a feasible block");
                        assert!(same, "{key} + {pre} on {mode:?} at {par:?}: key = walker");
                        assert_eq!(
                            got,
                            Some((pre.to_owned(), false)),
                            "{key} + {pre} on {mode:?} at {par:?}"
                        );
                        continue;
                    }
                    let want = on_path.then(|| match (key, par) {
                        (CIS_SCRIPT, Par::Four) => (CONN_QUEUED.to_owned(), true),
                        _ => (key.to_owned(), false),
                    });
                    assert_eq!(got, want, "{key} on {mode:?} at {par:?}");
                    here |= on_path;
                }
                bound += usize::from(here);
            }
        }
        let same_condition = SAME_CONDITION.len();
        assert_eq!(bound, paths_total, "every C row binds on each of its paths");
        assert!(bound >= 43 && same_condition == 3, "{bound} (row, mode) bindings");
    }
}
