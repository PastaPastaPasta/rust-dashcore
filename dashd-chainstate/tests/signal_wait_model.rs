//! V's waits on P3's signal queue (C5, C5′; CH1-node §8.4, D67, A-1b (5)) as an explicit-state
//! model checker.
//!
//! This is an ABSTRACTION of `chain::actor`'s waits, not a test of the implementation. It
//! models:
//!
//! - **V**: the startup hold's connects (D38: no hook, commands wait), init's ABC of `I`
//!   connects, the idle point (C5's hook), a `ProcessBlock`'s accept, and its ABC tail of `C`
//!   connects. Before each connect comes a loop head, where Core's `LimitValidationInterfaceQueue`
//!   sits (`v:3760`) and C5's hook follows it (R4). Outside IBD each accept first waits in C5′'s
//!   `before_accept`. Every wait serves the lane: a `ProcessHeaders` runs, a `ProcessBlock` is
//!   deferred whole inside a wait under C5′ (R3), and otherwise accepted with its tail deferred.
//!   Waits never nest. Each connect pushes one `BlockChecked`; `NewPoWValidBlock` and
//!   `ProcessHeaders` push nothing (C8).
//! - **P3's queue**: `PreStart` until the actor's first drain, then `Live`, `Closed` once the
//!   actor has gone (its drop guard). A C5 wait ends when the queue is within its bound `B`, a C5′
//!   wait when nothing is outstanding, and both at once unless the queue is `Live` (DEC-230).
//! - **The net actor**: starts only once V has replied to init's ABC (ND1's order). It sends one
//!   `ProcessBlock` at a time and awaits its `Activated` reply through a draining wait (DEC-229),
//!   and sends `ProcessHeaders` and awaits that reply without draining (D111 R2). It may go at any
//!   point, which closes the queue.
//! - **N RPC senders**: each sends one `ProcessBlock` at a time and blocks on its reply without
//!   draining.
//!
//! **Checked in every reachable state:** the queue holds at most `max(H, B + 1)` entries (the
//! hold's connects push without waiting; every later connect follows a loop head within `B`), no
//! accept outside IBD starts while a `Live` queue holds an entry (P3's I5), and every reachable
//! state can still reach completion (AG EF done), so no schedule deadlocks or starves.
//!
//! **Negative controls:** an actor whose reply wait does not drain must reach a state that cannot
//! complete, which shows the checker can see the cycle DEC-229 rules out. Four mutants of V each
//! fail a named property (Opus r1 N5): an accept inside a wait outside IBD (MM1) and an idle
//! point with no C5′ wait (MM2) break I5; waits that serve nothing (MM10) and a C5 wait on a
//! `PreStart` queue (MM9) cannot complete.
//!
//! **Not liveness-bearing here, so unchecked:** the idle point's C5 wait (MM4) keeps P3's bound
//! between commands, which the queue bound above does not need, and the hold's exemption from the
//! hooks (MM11) keeps D38's order (held commands see the startup ABC's tip): a hook in the hold
//! meets a `PreStart` queue, which never waits.

use std::collections::{HashMap, HashSet, VecDeque};

/// The model's parameters.
#[derive(Clone, Copy, Debug)]
struct Params {
    /// P3's queue bound.
    bound: u8,
    /// Connects per block's ABC tail.
    connects: u8,
    /// The startup hold's connects (D38).
    hold: u8,
    /// Init's ABC connects, after the hold; the actor starts when it replies.
    init: u8,
    /// `ProcessHeaders` the actor sends.
    headers: u8,
    /// RPC senders besides the actor.
    rpc: u8,
    /// Blocks each sender submits.
    blocks: u8,
    /// V's IBD latch, fixed for the run.
    ibd: bool,
    /// The actor drains while it awaits its reply (DEC-229); false is the negative control.
    draining: bool,
    /// The actor may go (and its drop guard close the queue). Without it, completion cannot rest
    /// on the actor's exit.
    may_go: bool,
    mutant: Mutant,
}

/// A deliberate defect in V, for the negative controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mutant {
    None,
    /// MM1: a block met in a wait is accepted outside IBD, not deferred whole.
    AcceptInWait,
    /// MM2: no C5′ wait before an accept from the idle point.
    NoAcceptWait,
    /// MM10: waits serve nothing.
    NoServe,
    /// MM9: C5's wait does not end on a `PreStart` queue.
    WaitOnPreStart,
}

/// Init's ABC, as the sender of a tail.
const INIT: u8 = u8::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Queue {
    PreStart,
    Live,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Sender {
    /// Blocks still to send.
    Ready(u8),
    /// One `ProcessBlock` queued or running at V; then `Ready(n)`.
    Waiting(u8),
    /// The actor's `ProcessHeaders` queued at V, awaited without draining; then `Ready(n)`.
    Heading(u8),
    /// The actor has gone (only the actor).
    Gone,
}

/// A command in V's lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Cmd {
    /// Sender `s`'s `ProcessBlock`.
    Block(u8),
    /// The actor's `ProcessHeaders`: class M, served wherever V takes a command.
    Headers,
}

/// Work V has deferred: a whole `ProcessBlock` (C5′ inside a wait), or an accepted block's tail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Deferred {
    Command(u8),
    Tail(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum V {
    /// Between operations: deferred work first, then the idle point, then the lane.
    Idle,
    /// C5's hook at the idle point, waiting for queue space.
    IdleWait,
    /// C5′'s `before_accept` for sender `s`'s block, outside any operation.
    AcceptWait(u8),
    /// Sender `s`'s ABC tail with `left` connects to go, at a loop head.
    Tail {
        s: u8,
        left: u8,
        /// In C5's hook at the loop head, waiting for queue space.
        waiting: bool,
        /// In C5′'s `before_accept` for `s2`, met at this release point.
        accept: Option<u8>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    v: V,
    hold: u8,
    q: u8,
    /// Pushes made while the queue was `PreStart` after the hold: V does not wait on a
    /// `PreStart` queue (DEC-230), so these are over the bound.
    pre: u8,
    queue: Queue,
    /// Sender 0 is the actor.
    senders: Vec<Sender>,
    /// V has replied to init's ABC: the actor runs.
    started: bool,
    /// `ProcessHeaders` the actor has still to send.
    headers: u8,
    lane: VecDeque<Cmd>,
    deferred: VecDeque<Deferred>,
}

struct Model {
    p: Params,
}

impl Model {
    fn initial(&self) -> State {
        State {
            v: if self.p.init > 0 {
                V::Tail {
                    s: INIT,
                    left: self.p.init,
                    waiting: false,
                    accept: None,
                }
            } else {
                V::Idle
            },
            hold: self.p.hold,
            q: 0,
            pre: 0,
            queue: Queue::PreStart,
            senders: vec![Sender::Ready(self.p.blocks); usize::from(self.p.rpc) + 1],
            started: self.p.init == 0,
            headers: self.p.headers,
            lane: VecDeque::new(),
            deferred: VecDeque::new(),
        }
    }

    fn done(&self, s: &State) -> bool {
        s.v == V::Idle
            && s.hold == 0
            && s.lane.is_empty()
            && s.deferred.is_empty()
            && s.started
            && (s.headers == 0 || s.senders[0] == Sender::Gone)
            && s.senders.iter().all(|x| matches!(x, Sender::Ready(0) | Sender::Gone))
    }

    /// C5's wait is over: the queue is within its bound, or not `Live`.
    fn space(&self, s: &State) -> bool {
        match self.p.mutant {
            Mutant::WaitOnPreStart => s.queue == Queue::Closed || s.q <= self.p.bound,
            _ => s.queue != Queue::Live || s.q <= self.p.bound,
        }
    }

    /// C5′'s wait is over: nothing is outstanding, or the queue is not `Live`.
    fn quiet(s: &State) -> bool {
        s.queue != Queue::Live || s.q == 0
    }

    /// An accept: P3's I5 holds.
    fn accept(&self, s: &State) -> Result<(), String> {
        if !self.p.ibd && s.queue == Queue::Live && s.q > 0 {
            return Err(format!("an accept overtook {} outstanding entries: {s:?}", s.q));
        }
        Ok(())
    }

    /// A connect's `BlockChecked`: dropped by a closed queue.
    fn push(s: &mut State, in_hold: bool) {
        match s.queue {
            Queue::Closed => {}
            Queue::PreStart if !in_hold => {
                s.q += 1;
                s.pre += 1;
            }
            Queue::PreStart | Queue::Live => s.q += 1,
        }
    }

    /// A lane command served inside a wait: headers run; a block is deferred whole under C5′,
    /// else accepted with its tail deferred.
    fn serve_in_wait(&self, s: &mut State) -> Result<(), String> {
        match s.lane.pop_front().expect("a queued command") {
            Cmd::Headers => Self::reply(s, 0),
            Cmd::Block(c) if self.p.ibd || self.p.mutant == Mutant::AcceptInWait => {
                self.accept(s)?;
                s.deferred.push_back(Deferred::Tail(c));
            }
            Cmd::Block(c) => s.deferred.push_back(Deferred::Command(c)),
        }
        Ok(())
    }

    /// V starts sender `c`'s block from the idle point: C5′'s wait outside IBD, else the accept.
    fn start_block(&self, s: &mut State, c: u8) -> Result<(), String> {
        if self.p.ibd || self.p.mutant == Mutant::NoAcceptWait {
            self.accept(s)?;
            s.v = self.tail(c);
        } else {
            s.v = V::AcceptWait(c);
        }
        Ok(())
    }

    fn tail(&self, s: u8) -> V {
        V::Tail {
            s,
            left: self.p.connects,
            waiting: false,
            accept: None,
        }
    }

    fn reply(s: &mut State, c: u8) {
        if c == INIT {
            s.started = true;
            return;
        }
        let x = &mut s.senders[usize::from(c)];
        if let Sender::Waiting(n) | Sender::Heading(n) = *x {
            *x = Sender::Ready(n);
        }
    }

    fn successors(&self, s: &State) -> Result<Vec<State>, String> {
        let mut out = Vec::new();
        // Senders.
        for (i, x) in s.senders.iter().enumerate() {
            let i = u8::try_from(i).expect("few senders");
            if i == 0 && !s.started {
                continue;
            }
            if let Sender::Ready(n @ 1..) = *x {
                let mut t = s.clone();
                t.senders[usize::from(i)] = Sender::Waiting(n - 1);
                t.lane.push_back(Cmd::Block(i));
                out.push(t);
            }
        }
        // The actor: sends headers, drains (the first drain makes the queue `Live`), or goes.
        let actor = s.senders[0];
        if let Sender::Ready(n) = actor
            && s.started
            && s.headers > 0
        {
            let mut t = s.clone();
            t.senders[0] = Sender::Heading(n);
            t.headers -= 1;
            t.lane.push_back(Cmd::Headers);
            out.push(t);
        }
        let drains = s.started
            && match actor {
                Sender::Gone | Sender::Heading(_) => false,
                Sender::Waiting(_) => self.p.draining,
                Sender::Ready(_) => true,
            };
        if drains && (s.queue == Queue::PreStart || s.q > 0) {
            let mut t = s.clone();
            t.queue = Queue::Live;
            t.q = t.q.saturating_sub(1);
            out.push(t);
        }
        if self.p.may_go && actor != Sender::Gone {
            // Its queued command still runs; the reply is dropped.
            let mut t = s.clone();
            t.senders[0] = Sender::Gone;
            t.queue = Queue::Closed;
            t.q = 0;
            out.push(t);
        }
        // V.
        let mut t = s.clone();
        let moved = match s.v {
            // D38: the hold's connects push without waiting; commands wait in the lane.
            _ if s.hold > 0 => {
                t.hold -= 1;
                Self::push(&mut t, true);
                true
            }
            V::Idle => match t.deferred.pop_front() {
                Some(Deferred::Tail(c)) => {
                    t.v = self.tail(c);
                    true
                }
                Some(Deferred::Command(c)) => {
                    self.start_block(&mut t, c)?;
                    true
                }
                None if !self.space(s) => {
                    t.v = V::IdleWait;
                    true
                }
                None => match t.lane.pop_front() {
                    Some(Cmd::Headers) => {
                        Self::reply(&mut t, 0);
                        true
                    }
                    Some(Cmd::Block(c)) => {
                        self.start_block(&mut t, c)?;
                        true
                    }
                    None => false,
                },
            },
            V::IdleWait if self.space(s) => {
                t.v = V::Idle;
                true
            }
            V::AcceptWait(c) if Self::quiet(s) => {
                self.accept(&t)?;
                t.v = self.tail(c);
                true
            }
            V::IdleWait | V::AcceptWait(_) => false,
            V::Tail {
                s: c,
                left,
                waiting,
                accept,
            } => match (accept, waiting) {
                (Some(c2), _) if Self::quiet(s) => {
                    self.accept(&t)?;
                    t.deferred.push_back(Deferred::Tail(c2));
                    t.v = V::Tail {
                        s: c,
                        left,
                        waiting,
                        accept: None,
                    };
                    true
                }
                (Some(_), _) => false,
                (None, true) if self.space(s) => {
                    t.v = V::Tail {
                        s: c,
                        left,
                        waiting: false,
                        accept: None,
                    };
                    true
                }
                (None, true) => false,
                (None, false) if left == 0 => {
                    Self::reply(&mut t, c);
                    t.v = V::Idle;
                    true
                }
                // The loop head: Core's limit, then C5's hook (R4).
                (None, false) if !self.space(s) => {
                    t.v = V::Tail {
                        s: c,
                        left,
                        waiting: true,
                        accept: None,
                    };
                    true
                }
                (None, false) => {
                    Self::push(&mut t, false);
                    t.v = V::Tail {
                        s: c,
                        left: left - 1,
                        waiting: false,
                        accept: None,
                    };
                    true
                }
            },
        };
        if moved {
            out.push(t);
        }
        // A release point outside a wait serves a queued command: headers run; a block meets
        // C5′'s wait outside IBD, else its accept with the tail deferred.
        if let V::Tail {
            s: c,
            left,
            waiting: false,
            accept: None,
        } = s.v
            && s.hold == 0
            && let Some(&cmd) = s.lane.front()
        {
            let mut t = s.clone();
            t.lane.pop_front();
            match cmd {
                Cmd::Headers => Self::reply(&mut t, 0),
                Cmd::Block(c2) if self.p.ibd => {
                    self.accept(&t)?;
                    t.deferred.push_back(Deferred::Tail(c2));
                }
                Cmd::Block(c2) => {
                    t.v = V::Tail {
                        s: c,
                        left,
                        waiting: false,
                        accept: Some(c2),
                    };
                }
            }
            out.push(t);
        }
        // Every wait serves the lane (never nested: a block met there is deferred whole).
        let in_wait = matches!(
            s.v,
            V::IdleWait
                | V::AcceptWait(_)
                | V::Tail {
                    waiting: true,
                    ..
                }
                | V::Tail {
                    accept: Some(_),
                    ..
                }
        );
        if in_wait && s.hold == 0 && !s.lane.is_empty() && self.p.mutant != Mutant::NoServe {
            let mut t = s.clone();
            self.serve_in_wait(&mut t)?;
            out.push(t);
        }
        Ok(out)
    }

    /// Explores every reachable state: the invariants in each, then AG EF done. `Err` names the
    /// first violation.
    fn check(&self) -> Result<usize, String> {
        let cap = (self.p.bound + 1).max(self.p.hold);
        let init = self.initial();
        let mut seen: HashMap<State, Vec<State>> = HashMap::new();
        let mut todo = VecDeque::from([init]);
        while let Some(s) = todo.pop_front() {
            if seen.contains_key(&s) {
                continue;
            }
            if s.q > cap + s.pre {
                return Err(format!("the queue holds {} > {cap} + {}: {s:?}", s.q, s.pre));
            }
            let next = self.successors(&s)?;
            todo.extend(next.iter().filter(|t| !seen.contains_key(*t)).cloned());
            seen.insert(s, next);
        }
        // AG EF done: walk back from the done states.
        let mut back: HashMap<&State, Vec<&State>> = HashMap::new();
        for (s, next) in &seen {
            for t in next {
                back.entry(seen.get_key_value(t).expect("explored").0).or_default().push(s);
            }
        }
        let mut good: HashSet<&State> = seen.keys().filter(|s| self.done(s)).collect();
        let mut walk: Vec<&State> = good.iter().copied().collect();
        while let Some(s) = walk.pop() {
            for &p in back.get(s).into_iter().flatten() {
                if good.insert(p) {
                    walk.push(p);
                }
            }
        }
        if let Some(stuck) = seen.keys().find(|s| !good.contains(*s)) {
            return Err(format!("cannot complete from {stuck:?}"));
        }
        Ok(seen.len())
    }
}

fn params() -> impl Iterator<Item = Params> {
    let mut all = Vec::new();
    for (ibd, may_go) in [(false, false), (false, true), (true, false), (true, true)] {
        for rpc in 0..=2 {
            // Init's ABC needs a connect past the bound to meet a `PreStart` queue at a loop head.
            for (hold, init, headers) in [(0, 0, 0), (2, 0, 0), (0, 0, 1), (2, 3, 1)] {
                if headers > 0 && rpc > 1 {
                    continue;
                }
                for connects in [1, 3] {
                    for blocks in [1, 2] {
                        all.push(Params {
                            bound: 1,
                            connects,
                            hold,
                            init,
                            headers,
                            rpc,
                            blocks,
                            ibd,
                            draining: true,
                            may_go,
                            mutant: Mutant::None,
                        });
                    }
                }
            }
        }
    }
    all.into_iter()
}

/// C5 and C5′ with N senders, the startup hold, init's ABC and `PreStart`: no deadlock or
/// starvation, the queue stays within its bound, and no accept outside IBD overtakes an
/// outstanding entry.
#[test]
fn v_waits_on_p3s_queue_always_end() {
    for p in params() {
        let states = Model {
            p,
        }
        .check()
        .unwrap_or_else(|e| panic!("{p:?}: {e}"));
        assert!(states > 1, "{p:?}");
    }
}

/// Each mutant of V fails the property it breaks, at the first parameters that show it (Opus r1
/// N5): the checker sees C5′, R3, the serving and the `PreStart` exemption, not just the
/// generic cycle.
#[test]
fn each_mutant_of_v_fails_its_property() {
    for (mutant, property) in [
        (Mutant::AcceptInWait, "an accept overtook"),
        (Mutant::NoAcceptWait, "an accept overtook"),
        (Mutant::NoServe, "cannot complete"),
        (Mutant::WaitOnPreStart, "cannot complete"),
    ] {
        let failed = params().find_map(|p| {
            let p = Params {
                mutant,
                ..p
            };
            Model {
                p,
            }
            .check()
            .err()
            .map(|e| (p, e))
        });
        let (p, e) = failed.unwrap_or_else(|| panic!("{mutant:?} survives every parameter"));
        assert!(e.starts_with(property), "{mutant:?} {p:?}: {e}");
    }
}

/// Negative control: an actor that awaits its reply without draining deadlocks against V's wait
/// for queue space (the cycle DEC-229 rules out).
#[test]
fn a_non_draining_actor_deadlocks() {
    for ibd in [false, true] {
        let p = Params {
            bound: 1,
            connects: 3,
            hold: 0,
            init: 0,
            headers: 0,
            rpc: 0,
            blocks: 1,
            ibd,
            draining: false,
            may_go: false,
            mutant: Mutant::None,
        };
        let r = Model {
            p,
        }
        .check();
        assert!(r.as_ref().is_err_and(|e| e.starts_with("cannot complete")), "{p:?}: {r:?}");
    }
}
