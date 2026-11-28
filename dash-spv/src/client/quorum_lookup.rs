//! Platform's lookup and its bounded re-ask rule, over the masternode owner's status and `Resolve`.

use std::collections::HashSet;
use std::mem;
use std::sync::PoisonError;
use std::time::Duration;

use async_trait::async_trait;
use dash_mnlist::{FoundQuorum, Lookup};
use dash_params::LlmqType;
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use tokio::sync::{oneshot, watch};
use tokio::time::timeout;

use super::mn_status::{MnCommand, MnHandle, MnStatus, Resolved};
use crate::error::SpvError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuorumQuery {
    pub llmq_type: LlmqType,
    pub quorum_hash: QuorumHash,
    pub height: u32,
}

/// The masternode owner as a lookup sees it.
#[async_trait]
pub trait QuorumLookupEngine: Send + Sync {
    /// The owner's status. A lookup clones it before its first command.
    fn status(&self) -> watch::Receiver<MnStatus>;
    /// Runs [`MnCommand::Resolve`]. `None` when the owner stopped or is gone.
    async fn resolve(&self, query: QuorumQuery, reask: bool) -> Option<(Resolved, u64)>;
}

#[async_trait]
impl QuorumLookupEngine for MnHandle {
    fn status(&self) -> watch::Receiver<MnStatus> {
        self.status.clone()
    }

    async fn resolve(&self, query: QuorumQuery, reask: bool) -> Option<(Resolved, u64)> {
        let (reply, answer) = oneshot::channel();
        self.cmd
            .send(MnCommand::Resolve {
                query,
                reask,
                reply,
            })
            .await
            .ok()?;
        answer.await.ok()
    }
}

/// One re-ask per quorum per published tip, shared by every query.
struct ReaskBudget {
    tip: Option<BlockHash>,
    asked: HashSet<(u8, [u8; 32])>,
}

impl ReaskBudget {
    fn try_reserve(&mut self, query: QuorumQuery, tip: Option<BlockHash>) -> bool {
        if self.tip != tip {
            self.tip = tip;
            self.asked.clear();
        }
        self.asked.insert(budget_key(query))
    }

    /// Returns a reservation the owner did not use, while its tip is current. After the tip moves
    /// away and back, it may return a newer one for the same quorum: one extra re-ask at most.
    fn release(&mut self, query: QuorumQuery, tip: Option<BlockHash>) {
        if self.tip == tip {
            self.asked.remove(&budget_key(query));
        }
    }
}

#[expect(
    clippy::disallowed_types,
    reason = "never held across an await, and a dropped `Reservation` must take it"
)]
type BudgetLock = std::sync::Mutex<ReaskBudget>;

fn budget_key(query: QuorumQuery) -> (u8, [u8; 32]) {
    (query.llmq_type.to_base(), *query.quorum_hash.as_bytes())
}

/// A reserved re-ask, returned on drop unless kept: the lookup future can be dropped while its
/// `Resolve` is pending.
struct Reservation<'a> {
    budget: &'a BudgetLock,
    query: QuorumQuery,
    tip: Option<BlockHash>,
}

impl Reservation<'_> {
    fn keep(self) {
        mem::forget(self);
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.budget.lock().unwrap_or_else(PoisonError::into_inner).release(self.query, self.tip);
    }
}

/// Shared by all queries to one owner.
pub struct QuorumLookupBridge {
    budget: BudgetLock,
}

impl Default for QuorumLookupBridge {
    fn default() -> Self {
        Self {
            budget: BudgetLock::new(ReaskBudget {
                tip: None,
                asked: HashSet::new(),
            }),
        }
    }
}

impl QuorumLookupBridge {
    fn reserve(&self, query: QuorumQuery, tip: Option<BlockHash>) -> Option<Reservation<'_>> {
        let reserved =
            self.budget.lock().unwrap_or_else(PoisonError::into_inner).try_reserve(query, tip);
        reserved.then(|| Reservation {
            budget: &self.budget,
            query,
            tip,
        })
    }

    /// Returns the found quorum, or tells a decided absence from unavailable data.
    pub async fn lookup(
        &self,
        engine: &impl QuorumLookupEngine,
        query: QuorumQuery,
        deadline: Duration,
    ) -> Result<FoundQuorum, SpvError> {
        timeout(deadline, self.run(engine, query)).await.unwrap_or_else(|_| Err(unavailable(query)))
    }

    async fn run(
        &self,
        engine: &impl QuorumLookupEngine,
        query: QuorumQuery,
    ) -> Result<FoundQuorum, SpvError> {
        let mut status = engine.status();
        let mut reasked = false;
        let mut recheck = true;
        loop {
            let tip = {
                let current = status.borrow_and_update();
                if current.stopped {
                    return Err(unavailable(query));
                }
                current.snapshot.tip.map(|t| t.hash)
            };
            let reservation = if reasked {
                None
            } else {
                self.reserve(query, tip)
            };
            let reply = engine.resolve(query, reservation.is_some()).await;
            let used = matches!(
                reply,
                Some((
                    Resolved::Answer {
                        reasked: true,
                        ..
                    } | Resolved::Asked {
                        reasked: true
                    },
                    _
                ))
            );
            match reservation {
                Some(reservation) if used => reservation.keep(),
                unused => drop(unused),
            }
            reasked |= used;
            let Some((resolved, gen)) = reply else {
                return Err(unavailable(query));
            };
            match resolved {
                Resolved::Answer {
                    lookup,
                    ..
                } => match lookup {
                    Lookup::Found(found) => return Ok(found),
                    Lookup::NotFound => {
                        return Err(SpvError::QuorumLookupError(format!(
                            "not found: type {} hash {:?} at height {}",
                            query.llmq_type, query.quorum_hash, query.height
                        )))
                    }
                    Lookup::Unavailable => return Err(unavailable(query)),
                    // Only when the owner read `Fetch` with no wait registered: bounded by one
                    // immediate recheck.
                    Lookup::Fetch if recheck => recheck = false,
                    Lookup::Fetch => return Err(unavailable(query)),
                },
                Resolved::Asked {
                    ..
                } => {
                    status
                        .wait_for(|s| s.gen > gen || s.stopped)
                        .await
                        .map_err(|_| unavailable(query))?;
                }
                Resolved::Busy => {
                    status
                        .wait_for(|s| {
                            s.lookup_waits < s.max_lookup_waits || s.gen > gen || s.stopped
                        })
                        .await
                        .map_err(|_| unavailable(query))?;
                }
            }
        }
    }
}

fn unavailable(query: QuorumQuery) -> SpvError {
    SpvError::QuorumLookupError(format!(
        "unavailable: type {} hash {:?} at height {}",
        query.llmq_type, query.quorum_hash, query.height
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use dash_chain::BlockRef;
    use dash_consensus::BlockId;
    use dash_dml::codec::core_disk::decode_list_s3;
    use dash_mnlist::{replay_rotated_capture, ListDecoder, MnSnapshot, QuorumState};
    use tokio::sync::{mpsc, Mutex};
    use tokio::task::yield_now;

    use super::*;
    use crate::client::anchor::Phase;

    /// Replies with its script, then as an owner whose lookup reads `Unavailable`.
    struct Script {
        replies: Mutex<VecDeque<Resolved>>,
        calls: AtomicUsize,
        fetches: AtomicUsize,
        status: watch::Sender<MnStatus>,
    }

    impl Script {
        fn new(replies: Vec<Resolved>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                calls: AtomicUsize::new(0),
                fetches: AtomicUsize::new(0),
                status: watch::channel(status()).0,
            }
        }

        fn set_tip(&self, height: u32) {
            self.status.send_modify(|s| {
                s.gen += 1;
                s.snapshot = Arc::new(MnSnapshot {
                    tip: Some(BlockRef {
                        id: BlockId(height),
                        hash: BlockHash::from([u8::try_from(height).unwrap(); 32]),
                        height,
                    }),
                    ..MnSnapshot::default()
                });
            });
        }

        /// Yields until `n` calls came, then lets the other tasks settle. It panics if they never
        /// come, as no deadline fires on a paused clock while this task yields.
        async fn wait_calls(&self, n: usize) {
            for _ in 0..10_000 {
                if self.calls.load(Ordering::SeqCst) >= n {
                    for _ in 0..10 {
                        yield_now().await;
                    }
                    return;
                }
                yield_now().await;
            }
            panic!("fewer than {n} calls");
        }
    }

    #[async_trait]
    impl QuorumLookupEngine for Script {
        fn status(&self) -> watch::Receiver<MnStatus> {
            self.status.subscribe()
        }

        async fn resolve(&self, _: QuorumQuery, reask: bool) -> Option<(Resolved, u64)> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let resolved = self.replies.lock().await.pop_front().unwrap_or(Resolved::Answer {
                lookup: Lookup::Unavailable,
                reasked: reask,
            });
            if matches!(
                resolved,
                Resolved::Answer {
                    reasked: true,
                    ..
                } | Resolved::Asked {
                    reasked: true
                }
            ) {
                self.fetches.fetch_add(1, Ordering::SeqCst);
            }
            Some((resolved, self.status.borrow().gen))
        }
    }

    /// An owner on `Script`'s status whose `Resolve` never replies.
    struct Hang<'a>(&'a Script);

    #[async_trait]
    impl QuorumLookupEngine for Hang<'_> {
        fn status(&self) -> watch::Receiver<MnStatus> {
            self.0.status.subscribe()
        }

        async fn resolve(&self, _: QuorumQuery, reask: bool) -> Option<(Resolved, u64)> {
            assert!(reask, "the pending Resolve holds a reservation");
            self.0.calls.fetch_add(1, Ordering::SeqCst);
            std::future::pending().await
        }
    }

    fn status() -> MnStatus {
        MnStatus {
            gen: 0,
            tip_seq: 0,
            snapshot: Arc::new(MnSnapshot::default()),
            lookup_waits: 0,
            max_lookup_waits: 5,
            phase: Phase::Established,
            stopped: false,
        }
    }

    fn answer(lookup: Lookup) -> Resolved {
        Resolved::Answer {
            lookup,
            reasked: false,
        }
    }

    fn query() -> QuorumQuery {
        QuorumQuery {
            llmq_type: LlmqType::LlmqDevnetPlatform,
            quorum_hash: QuorumHash::from([1; 32]),
            height: 1000,
        }
    }

    fn is_unavailable(result: Result<FoundQuorum, SpvError>) {
        assert!(
            matches!(result, Err(SpvError::QuorumLookupError(message)) if message.starts_with("unavailable"))
        );
    }

    fn is_not_found(result: Result<FoundQuorum, SpvError>) {
        assert!(
            matches!(result, Err(SpvError::QuorumLookupError(message)) if message.starts_with("not found"))
        );
    }

    const TIMEOUT: Duration = Duration::from_secs(10);

    #[tokio::test]
    async fn unavailable_reasks_once_per_query_and_per_quorum_per_tip() {
        let bridge = QuorumLookupBridge::default();
        let engine = Script::new(vec![]);
        is_unavailable(bridge.lookup(&engine, query(), TIMEOUT).await);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 1);
        let mut same_quorum = query();
        same_quorum.height += 1;
        is_unavailable(bridge.lookup(&engine, same_quorum, TIMEOUT).await);
        assert_eq!(
            engine.fetches.load(Ordering::SeqCst),
            1,
            "height is not part of the retry budget"
        );
        engine.status.send_modify(|s| s.gen += 1);
        is_unavailable(bridge.lookup(&engine, query(), TIMEOUT).await);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 1, "progress alone keeps the budget");
        engine.set_tip(2);
        is_unavailable(bridge.lookup(&engine, query(), TIMEOUT).await);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 2, "a new published tip resets it");
        let mut another = query();
        another.quorum_hash = QuorumHash::from([3; 32]);
        is_unavailable(bridge.lookup(&engine, another, TIMEOUT).await);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 3);
        another.llmq_type = LlmqType::LlmqDevnet;
        is_unavailable(bridge.lookup(&engine, another, TIMEOUT).await);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 4);
    }

    const DML_LISTS: ListDecoder = |bytes| {
        let list = decode_list_s3(&mut &bytes[..]).expect("an evodb list");
        list.iter().map(|d| d.to_sml_entry()).collect()
    };

    #[tokio::test]
    async fn found_preserves_the_verified_key_and_returns_the_reservation() {
        let replay = replay_rotated_capture(
            include_bytes!("../../../test-vectors/lq1-rot/mainnet-1738656.json.gz"),
            DML_LISTS,
        );
        let (_, _, state, commitment) = replay.into_iter().next().unwrap();
        let QuorumState::Verified(quorum) = state else {
            panic!("not verified")
        };
        let found = FoundQuorum {
            quorum: quorum.clone(),
            commitment: Arc::new(dash_consensus::primitives::P2pCommitment::new(commitment)),
        };
        let bridge = QuorumLookupBridge::default();
        let engine = Script::new(vec![answer(Lookup::Found(found.clone()))]);
        let query = QuorumQuery {
            llmq_type: quorum.llmq_type(),
            quorum_hash: *quorum.quorum_hash(),
            height: 1_738_700,
        };
        assert_eq!(bridge.lookup(&engine, query, TIMEOUT).await.unwrap(), found);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 0);
        is_unavailable(bridge.lookup(&engine, query, TIMEOUT).await);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 1, "the unused re-ask was returned");
    }

    /// A query dropped while its `Resolve` is pending, by its deadline or by its caller, returns its
    /// reservation: the next query at the same tip may re-ask.
    #[tokio::test(start_paused = true)]
    async fn a_cancelled_query_returns_its_reservation() {
        let bridge = QuorumLookupBridge::default();
        let engine = Script::new(vec![]);
        is_unavailable(bridge.lookup(&Hang(&engine), query(), TIMEOUT).await);
        is_unavailable(bridge.lookup(&engine, query(), TIMEOUT).await);
        assert_eq!(engine.calls.load(Ordering::SeqCst), 2);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 1, "after a deadline");

        engine.set_tip(1);
        {
            let hang = Hang(&engine);
            let lookup = bridge.lookup(&hang, query(), TIMEOUT);
            tokio::pin!(lookup);
            tokio::select! {
                biased;
                _ = &mut lookup => panic!("a pending Resolve answers nothing"),
                () = engine.wait_calls(3) => {}
            }
        }
        is_unavailable(bridge.lookup(&engine, query(), TIMEOUT).await);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 2, "after the caller dropped it");
    }

    #[tokio::test]
    async fn concurrent_queries_share_the_retry_budget() {
        let bridge = QuorumLookupBridge::default();
        let engine = Script::new(vec![]);
        let (a, b) = tokio::join!(
            bridge.lookup(&engine, query(), TIMEOUT),
            bridge.lookup(&engine, query(), TIMEOUT)
        );
        is_unavailable(a);
        is_unavailable(b);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn busy_waits_for_capacity_then_asks_again() {
        let bridge = Arc::new(QuorumLookupBridge::default());
        let engine = Arc::new(Script::new(vec![Resolved::Busy, answer(Lookup::NotFound)]));
        engine.status.send_modify(|s| s.lookup_waits = 5);
        let task = tokio::spawn({
            let bridge = bridge.clone();
            let engine = engine.clone();
            async move { bridge.lookup(engine.as_ref(), query(), TIMEOUT).await }
        });
        engine.wait_calls(1).await;
        assert_eq!(engine.calls.load(Ordering::SeqCst), 1, "Busy does not ask again unprompted");
        engine.status.send_modify(|s| s.lookup_waits = 4);
        is_not_found(task.await.unwrap());
        assert_eq!(engine.calls.load(Ordering::SeqCst), 2);

        let engine = Script::new(vec![Resolved::Busy]);
        is_unavailable(bridge.lookup(&engine, query(), TIMEOUT).await);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 1, "Busy returns the reserved re-ask");

        let bridge = Arc::new(QuorumLookupBridge::default());
        let engine = Arc::new(Script::new(vec![Resolved::Busy]));
        engine.status.send_modify(|s| s.lookup_waits = 5);
        let waiting = tokio::spawn({
            let bridge = bridge.clone();
            let engine = engine.clone();
            async move { bridge.lookup(engine.as_ref(), query(), TIMEOUT).await }
        });
        engine.wait_calls(1).await;
        engine.set_tip(1);
        is_unavailable(bridge.lookup(engine.as_ref(), query(), TIMEOUT).await);
        engine.status.send_modify(|s| s.lookup_waits = 4);
        is_unavailable(waiting.await.unwrap());
        assert_eq!(
            engine.fetches.load(Ordering::SeqCst),
            1,
            "a query that waited out Busy shares the new tip's spent budget"
        );
    }

    /// A query that re-asked at one tip and whose fetch then failed at a later tip answers
    /// `unavailable`: its one re-ask is spent, though the new tip renewed the quorum's.
    #[tokio::test]
    async fn a_query_reasks_once_across_a_tip_change() {
        let bridge = QuorumLookupBridge::default();
        let engine = Script::new(vec![Resolved::Asked {
            reasked: true,
        }]);
        engine.set_tip(1);
        let (result, ()) = tokio::join!(bridge.lookup(&engine, query(), TIMEOUT), async {
            engine.wait_calls(1).await;
            engine.set_tip(2);
        });
        is_unavailable(result);
        assert_eq!(engine.calls.load(Ordering::SeqCst), 2);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 1, "no second re-ask at the new tip");
    }

    /// Two waits hold every slot and each still waits on another target. Their replies move
    /// `gen` and can decide the busy query's lookup, so it asks again before its deadline.
    #[tokio::test(start_paused = true)]
    async fn busy_asks_again_on_progress_while_the_waits_stay_full() {
        let bridge = QuorumLookupBridge::default();
        let engine = Script::new(vec![Resolved::Busy, Resolved::Busy, answer(Lookup::NotFound)]);
        engine.status.send_modify(|s| {
            s.lookup_waits = 2;
            s.max_lookup_waits = 2;
        });
        let (result, ()) = tokio::join!(bridge.lookup(&engine, query(), TIMEOUT), async {
            for calls in 1..=2 {
                engine.wait_calls(calls).await;
                assert_eq!(engine.calls.load(Ordering::SeqCst), calls, "Busy moves no gen");
                engine.status.send_modify(|s| s.gen += 1);
            }
        });
        is_not_found(result);
        assert_eq!(engine.calls.load(Ordering::SeqCst), 3);
        assert_eq!(engine.status.borrow().lookup_waits, 2);
    }

    #[tokio::test]
    async fn asked_waits_for_a_later_generation() {
        let bridge = Arc::new(QuorumLookupBridge::default());
        let engine = Arc::new(Script::new(vec![
            Resolved::Asked {
                reasked: false,
            },
            answer(Lookup::NotFound),
        ]));
        engine.status.send_modify(|s| s.gen = 3);
        let task = tokio::spawn({
            let bridge = bridge.clone();
            let engine = engine.clone();
            async move { bridge.lookup(engine.as_ref(), query(), TIMEOUT).await }
        });
        engine.wait_calls(1).await;
        engine.status.send_modify(|s| s.lookup_waits = 1);
        engine.wait_calls(1).await;
        assert_eq!(engine.calls.load(Ordering::SeqCst), 1, "no wake without a later gen");
        engine.status.send_modify(|s| s.gen = 4);
        is_not_found(task.await.unwrap());
        assert_eq!(engine.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn fetch_with_no_wait_is_rechecked_once() {
        let bridge = QuorumLookupBridge::default();
        let engine = Script::new(vec![answer(Lookup::Fetch), answer(Lookup::NotFound)]);
        is_not_found(bridge.lookup(&engine, query(), TIMEOUT).await);
        assert_eq!(engine.calls.load(Ordering::SeqCst), 2);
        let engine = Script::new(vec![answer(Lookup::Fetch), answer(Lookup::Fetch)]);
        is_unavailable(bridge.lookup(&engine, query(), TIMEOUT).await);
        assert_eq!(engine.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_stopped_and_closed_owner_return_unavailable() {
        let bridge = QuorumLookupBridge::default();
        let engine = Script::new(vec![Resolved::Asked {
            reasked: false,
        }]);
        is_unavailable(bridge.lookup(&engine, query(), TIMEOUT).await);
        assert_eq!(engine.calls.load(Ordering::SeqCst), 1);

        let engine = Script::new(vec![]);
        engine.status.send_modify(|s| s.stopped = true);
        is_unavailable(bridge.lookup(&engine, query(), TIMEOUT).await);
        assert_eq!(engine.calls.load(Ordering::SeqCst), 0, "a stopped owner is not asked");

        let (cmd, commands) = mpsc::channel(1);
        drop(commands);
        let closed = MnHandle {
            status: watch::channel(status()).1,
            cmd,
        };
        is_unavailable(bridge.lookup(&closed, query(), TIMEOUT).await);

        let engine = Script::new(vec![answer(Lookup::NotFound)]);
        is_not_found(bridge.lookup(&engine, query(), TIMEOUT).await);
        assert_eq!(engine.fetches.load(Ordering::SeqCst), 0);
    }
}
