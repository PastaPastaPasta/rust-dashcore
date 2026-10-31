//! The forward run's subscriber assertions (CH2 §6.2.1, ST3a's toy `forward_subscriber`): FA1
//! and FA3–FA6 over the marks of b3's test subscriber, its state marks around step 6
//! ([`Subs::step`](super::subs::Subs)), its locator commits, and step 0's prune decisions.
//!
//! - **FA1**: step 0's dry run uses the predicted post-commit lock (§4.2; T32 uses the actual,
//!   pre-commit one): Synced at `new`, otherwise `LCA(L, new)`, or the start height with no
//!   locator. Its "limited pruning to height h" line names `height_first − PRUNE_LOCK_BUFFER − 1`
//!   exactly when that is in `[1, h(new)]` (`prune_lock_bound`).
//! - **FA3**: a Synced subscriber serving a flush's commit commits at `LCA(p, new)`: at `new`
//!   while p is on `new`'s path (case B drains to it), or at the fork when p is off it, or when
//!   V's tip left `new` during the flush (case A after the disconnects, `D d`).
//! - **FA4**: a Syncing subscriber makes a cadence commit within `cadence` blocks of progress,
//!   capped at `LCA(progress, T_ack)` (or at `LCA(progress, LCA(T_ack, new))` from a flush's step
//!   3b to its step 5, D33), and turns Synced with a capped commit, or with L at its target
//!   already.
//! - **FA5**: a Synced subscriber never holds pruning below `new − MIN_BLOCKS_TO_KEEP`; a disabled
//!   one holds none (FA1's line, S11c).
//! - **FA6**: the drain makes at most `⌈(h(new) − h(F))/D_max⌉` checkpoints, and W never waits in
//!   step 6 on a subscriber parked at its limit.

use dashd_store::faulty::InvariantId::{FA1, FA3, FA4, FA5, FA6};
use dashd_store::faulty::{Hash, LocatorKind, Mark, Trace, Violation};
use dashd_store::flatfile::MIN_BLOCKS_TO_KEEP;

use super::subs::{DISABLED, SubConfig, SubMode};
use super::{PATH_DISCONNECT, SUB_STORE};
use crate::flush::PRUNE_LOCK_BUFFER;

/// The "limited pruning" line's prefix after the subscriber's name (Core: validation.cpp:2992-3006).
const LIMITED: &str = " limited pruning to height ";

/// Heights and ancestry of the blocks the marks name.
pub(super) trait Chain {
    fn height(&self, h: &Hash) -> Option<u32>;
    /// The height of the last common block.
    fn lca(&self, a: &Hash, b: &Hash) -> Option<u32>;
    fn is_ancestor(&self, a: &Hash, b: &Hash) -> bool {
        self.lca(a, b).is_some_and(|l| Some(l) == self.height(a))
    }
}

/// A state mark's (synced, durable, progress).
type State = (bool, Option<Hash>, Option<Hash>);

/// Step 6 of a flush: `new`, the state at its start, its commit and checkpoints.
struct Step6 {
    new: Hash,
    start: Option<State>,
    committed: Option<Hash>,
    checkpoints: u32,
    /// V's chain left `new` after the freeze (a disconnect below it).
    left: bool,
    /// The lowest rewind commit in the step: V's disconnects reaching the subscriber during step 6
    /// lower the drain's F to it (D52).
    low: Option<i64>,
}

/// The forward run's subscriber assertions, on `cfg`'s subscriber named `name`.
pub(super) fn forward(
    cfg: SubConfig,
    name: &str,
    chain: &impl Chain,
    trace: &Trace,
) -> Vec<Violation> {
    let mut v = Vec::new();
    let h = |x: &Hash| i64::from(chain.height(x).unwrap_or(0));
    let lca = |a: &Hash, b: &Hash| chain.lca(a, b).map_or(-1, i64::from);
    let limited = format!("{name}{LIMITED}");
    let cadence = match cfg.mode {
        SubMode::Syncing {
            cadence,
        } => Some(i64::from(cadence)),
        SubMode::Synced {
            ..
        } => None,
    };
    let mut state: Option<State> = None;
    let mut line: Option<i64> = None;
    // The open flush's number and `new`.
    let mut flush_new: Option<(u64, Hash)> = None;
    // Since its disable, until it opens again (S11c).
    let mut disabled = false;
    let mut open_new: Option<Hash> = None;
    let mut t_ack: Option<Hash> = None;
    let mut step6: Option<Step6> = None;
    // V's active chain, from its connects and disconnects.
    let mut v_chain: Vec<Hash> = Vec::new();
    let mut cadence_from: Option<i64> = None;
    let mut cadence_seen = false;
    // A cadence commit, judged at the next state mark (its progress): its tip, T_ack and the
    // open flush's `new` then.
    let mut pending: Option<(Hash, Option<Hash>, Option<Hash>)> = None;

    let close = |s6: Option<Step6>, v: &mut Vec<Violation>| {
        let Some(Step6 {
            new,
            start: Some((true, durable, p)),
            committed,
            checkpoints,
            left,
            low,
        }) = s6
        else {
            return;
        };
        let after = committed.or(durable);
        let on_chain = p.is_none_or(|p| chain.is_ancestor(&p, &new) || chain.is_ancestor(&new, &p));
        let at_lca =
            |a: &Hash| p.is_some_and(|p| h(a) == lca(&p, &new)) && chain.is_ancestor(a, &new);
        if !(after == Some(new) || ((!on_chain || left) && after.as_ref().is_some_and(at_lca))) {
            v.push(Violation::new(
                FA3,
                format!(
                    "a synced subscriber did not commit at LCA(p, new) = {} in step 6 (its locator \
                     is at {:?})",
                    h(&new),
                    after.map(|a| h(&a))
                ),
            ));
        }
        let f = p.map_or(-1, |p| lca(&p, &new));
        let f = low.map_or(f, |low| f.min(low));
        let max = ((h(&new) - f).max(0) as u64).div_ceil(u64::from(cfg.d_max));
        if u64::from(checkpoints) > max {
            v.push(Violation::new(
                FA6,
                format!("the drain made {checkpoints} checkpoints, more than ⌈(h(new) − h(F))/D_max⌉ = {max}"),
            ));
        }
    };

    for m in trace.marks() {
        match m {
            // A fail-stopped run owes no further progress (R8 judges it).
            Mark::Abort {
                ..
            } => return v,
            Mark::SubscriberState {
                synced,
                durable,
                progress,
                blocked,
                ..
            } => {
                if let Some((tip, t, open)) = pending.take()
                    && let Some(p) = progress
                {
                    cadence_from = Some(h(p));
                    let caps = [t, open].into_iter().flatten().map(|c| lca(p, &c));
                    if !caps.clone().any(|c| c == h(&tip)) || !chain.is_ancestor(&tip, p) {
                        let caps: Vec<_> = caps.collect();
                        v.push(Violation::new(
                            FA4,
                            format!(
                                "a cadence commit at {} is not capped at LCA(progress {}, T_ack): {caps:?}",
                                h(&tip),
                                h(p)
                            ),
                        ));
                    }
                }
                if *synced && let Some(s6) = step6.as_mut() {
                    if *blocked {
                        v.push(Violation::new(
                            FA6,
                            "W waits in step 6 on a subscriber blocked at its limit",
                        ));
                    }
                    if s6.start.is_none() && !*blocked {
                        s6.start = Some((*synced, *durable, *progress));
                    } else {
                        close(step6.take(), &mut v);
                    }
                }
                disabled = false;
                // The capped commit's target from p: `LCA(p, T_ack)`, or while a flush is open
                // possibly `LCA(p, LCA(T_ack, new))`, the lower of the two LCAs (D33).
                let cap_at = |p: &Hash, t: &Hash| {
                    let a = lca(p, t);
                    open_new.map_or(a, |n| a.min(lca(p, &n)))
                };
                if let Some(c) = cadence {
                    let was_synced = state.is_some_and(|(s, ..)| s);
                    if !*synced {
                        let from = cadence_from.get_or_insert(durable.map_or(0, |d| h(&d)));
                        let floor = durable.map_or(-1, |d| h(&d));
                        // No capped commit can land before T_ack, nor move L while its target is
                        // below L, or is L itself (H2).
                        let stuck = |p: &Hash| {
                            t_ack.is_none_or(|t| {
                                let x = cap_at(p, &t);
                                x < floor
                                    || (x == floor
                                        && durable.is_some_and(|l| chain.is_ancestor(&l, p)))
                            })
                        };
                        match progress {
                            Some(p) if stuck(p) => *from = h(p),
                            Some(p) if h(p) - *from > c => v.push(Violation::new(
                                FA4,
                                format!(
                                    "a syncing subscriber reached {} with no cadence commit since \
                                     {from}",
                                    h(p)
                                ),
                            )),
                            _ => {}
                        }
                    } else if !was_synced
                        && state.is_some()
                        && !cadence_seen
                        && let Some(t) = t_ack
                        // The handoff's capped commit is a no-op when its target is at or below L
                        // on p's path (L may sit above it, as after a reorg away and back).
                        && !(*progress).zip(*durable).is_some_and(|(p, l)| {
                            chain.is_ancestor(&l, &p) && h(&l) >= cap_at(&p, &t)
                        })
                    {
                        v.push(Violation::new(
                            FA4,
                            "a syncing subscriber turned synced without a capped commit",
                        ));
                    }
                    cadence_seen = false;
                }
                state = Some((*synced, *durable, *progress));
            }
            // A disabled index holds no lock (S11c).
            Mark::Log {
                line,
            } if line == DISABLED => {
                state = None;
                disabled = true;
            }
            Mark::Log {
                line: l,
            } => {
                if let Some(x) = l.strip_prefix(&limited) {
                    line = x.parse().ok();
                }
            }
            Mark::PruneDecision {
                new,
                ..
            } => {
                if let Some((synced, durable, _)) = state {
                    // FA1: the predicted post-commit lock, and its line.
                    let first = if synced {
                        h(new)
                    } else {
                        durable.map_or(0, |d| lca(&d, new))
                    };
                    let lock = first - i64::from(PRUNE_LOCK_BUFFER) - 1;
                    let want = (1..=h(new)).contains(&lock).then_some(lock);
                    if line != want {
                        v.push(Violation::new(
                            FA1,
                            format!(
                                "the dry run's limited-pruning line names {line:?}, the predicted \
                                 post-commit lock {want:?}"
                            ),
                        ));
                    }
                    // FA5: a synced subscriber's lock is never below new − K.
                    if synced && line.is_some_and(|l| l < h(new) - i64::from(MIN_BLOCKS_TO_KEEP)) {
                        v.push(Violation::new(
                            FA5,
                            format!("a synced subscriber limits pruning to {line:?} at {}", h(new)),
                        ));
                    }
                }
                if disabled && line.is_some() {
                    v.push(Violation::new(
                        FA1,
                        format!("a disabled index limits pruning to {line:?} at {}", h(new)),
                    ));
                }
                line = None;
            }
            Mark::FlushBegin {
                flush,
                new,
                ..
            } => {
                close(step6.take(), &mut v);
                flush_new = Some((*flush, *new));
                open_new = Some(*new);
            }
            Mark::Durable {
                flush,
            } => {
                if let Some((f, new)) = flush_new
                    && f == *flush
                {
                    // Step 5 captures the subscribers Synced now (H3), a Syncing one handed off
                    // since the freeze included. Synced-mode starts so.
                    let synced = state.map_or(cadence.is_none(), |(s, ..)| s);
                    t_ack = Some(new);
                    open_new = None;
                    step6 = synced.then(|| Step6 {
                        new,
                        start: None,
                        committed: None,
                        checkpoints: 0,
                        left: !v_chain.contains(&new),
                        low: None,
                    });
                }
            }
            Mark::Connected {
                block,
                applied: true,
                ..
            } => v_chain.push(*block),
            Mark::PathBegin {
                path,
            } if *path == PATH_DISCONNECT => {
                v_chain.pop();
                if let Some(s6) = step6.as_mut() {
                    s6.left |= !v_chain.contains(&s6.new);
                }
            }
            Mark::Locator {
                store,
                tip,
                kind,
            } if *store == SUB_STORE => {
                if let Some(s6) = step6.as_mut()
                    // A rewind commit lowers L to the fork first, which case A then keeps.
                    && matches!(
                        kind,
                        LocatorKind::CommitStep | LocatorKind::Checkpoint | LocatorKind::Rewind
                    )
                {
                    s6.committed = Some(*tip);
                    match kind {
                        LocatorKind::Checkpoint => s6.checkpoints += 1,
                        LocatorKind::Rewind => {
                            s6.low = Some(s6.low.map_or(h(tip), |l| l.min(h(tip))))
                        }
                        _ => {}
                    }
                }
                if *kind == LocatorKind::Cadence {
                    cadence_seen = true;
                    pending = Some((*tip, t_ack, open_new));
                }
            }
            _ => {}
        }
    }
    close(step6, &mut v);
    v
}
