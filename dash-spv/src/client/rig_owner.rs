//! A single-loop masternode owner over dash-mnlist's capture rig, implementing the owner contract
//! in `mn_status` so host tests drive real lookups before the client's own owner exists.

use std::mem;
use std::sync::Arc;

use dash_mnlist::{CaptureRig, FetchLookupError, Lookup, MnReject, MnViewError, RequestKey};
use tokio::sync::{mpsc, oneshot, watch};

use super::anchor::Phase;
use super::mn_status::{MnCommand, MnHandle, MnStatus, Resolved};
use super::quorum_lookup::QuorumQuery;

/// A host input the test queues: what the client's header and network tasks would deliver.
pub(crate) enum Host {
    /// Answers every request the captures hold a reply to.
    Pump,
    /// Reports each key as failed by every peer, in one input.
    Fail(Vec<RequestKey>),
    /// Accepts headers up to `height` with no masternode payload.
    SetTip(u32),
}

type Read = Box<dyn FnOnce(&CaptureRig) + Send>;

enum Input {
    Now(Host, oneshot::Sender<()>),
    /// Runs right after the next command's reply, before any other input.
    AfterNextCommand(Host),
    Read(Read),
}

/// The test's side of the owner's input queue.
pub(crate) struct RigHost {
    inputs: mpsc::UnboundedSender<Input>,
}

impl RigHost {
    /// Queues `host` and waits until the owner handled it.
    pub(crate) async fn run(&self, host: Host) {
        let (ack, done) = oneshot::channel();
        self.inputs.send(Input::Now(host, ack)).expect("the owner runs");
        done.await.expect("the owner handled it");
    }

    /// Makes the owner handle `host` right after it replies to its next command.
    pub(crate) fn after_next_command(&self, host: Host) {
        self.inputs.send(Input::AfterNextCommand(host)).expect("the owner runs");
    }

    /// Reads the rig between two owner inputs.
    pub(crate) async fn read<T: Send + 'static>(
        &self,
        read: impl FnOnce(&CaptureRig) -> T + Send + 'static,
    ) -> T {
        let (tx, rx) = oneshot::channel();
        self.inputs
            .send(Input::Read(Box::new(move |rig| {
                let _ = tx.send(read(rig));
            })))
            .expect("the owner runs");
        rx.await.expect("the owner read it")
    }
}

struct RigOwner {
    rig: CaptureRig,
    status: watch::Sender<MnStatus>,
    progress: u64,
    tip_seq: u64,
    after: Vec<Host>,
}

/// Starts the owner on `rig`, with the rig's chain as its pinned view.
pub(crate) fn spawn(rig: CaptureRig) -> (MnHandle, RigHost) {
    let (status, status_rx) = watch::channel(MnStatus {
        gen: 0,
        tip_seq: 0,
        snapshot: rig.engine.snapshot(),
        lookup_waits: rig.engine.lookup_waits(),
        max_lookup_waits: rig.engine.max_lookup_waits(),
        phase: Phase::Established,
        stopped: false,
    });
    let (cmd, commands) = mpsc::channel(16);
    let (inputs, host) = mpsc::unbounded_channel();
    let owner = RigOwner {
        progress: rig.engine.progress(),
        rig,
        status,
        tip_seq: 0,
        after: Vec::new(),
    };
    tokio::spawn(owner.run(commands, host));
    (
        MnHandle {
            status: status_rx,
            cmd,
        },
        RigHost {
            inputs,
        },
    )
}

impl RigOwner {
    async fn run(
        mut self,
        mut commands: mpsc::Receiver<MnCommand>,
        mut host: mpsc::UnboundedReceiver<Input>,
    ) {
        loop {
            tokio::select! {
                biased;
                Some(input) = host.recv() => match input {
                    Input::Now(h, ack) => {
                        self.host(h);
                        let _ = ack.send(());
                    }
                    Input::AfterNextCommand(h) => self.after.push(h),
                    Input::Read(read) => read(&self.rig),
                },
                command = commands.recv() => {
                    let Some(command) = command else { return };
                    if !self.command(command) {
                        self.status.send_modify(|s| s.stopped = true);
                        return;
                    }
                    for h in mem::take(&mut self.after) {
                        self.host(h);
                    }
                }
            }
        }
    }

    /// Handles one command and replies. `false` stops the owner.
    fn command(&mut self, command: MnCommand) -> bool {
        let CaptureRig {
            engine,
            chain,
            config,
        } = &mut self.rig;
        match command {
            MnCommand::VerifyChainLock {
                cl,
                reply,
                ..
            } => {
                let outcome = engine.verify_chainlock(&cl, chain, config);
                let _ = reply.send((outcome, self.after_call()));
            }
            MnCommand::VerifyIsLock {
                is,
                reply,
                ..
            } => {
                let outcome = engine.verify_islock(&is, chain, config);
                let _ = reply.send((outcome, self.after_call()));
            }
            MnCommand::Resolve {
                query,
                reask,
                reply,
            } => {
                let Some(resolved) = self.resolve(query, reask) else {
                    return false;
                };
                let _ = reply.send((resolved, self.after_call()));
            }
            MnCommand::ClearPendingChainLock {
                cl,
                reply,
            } => {
                let cleared = engine.clear_pending_chainlock(&cl);
                let _ = reply.send((cleared, self.after_call()));
            }
            MnCommand::Stop(ack) => {
                self.after_call();
                self.status.send_modify(|s| s.stopped = true);
                let _ = ack.send(());
                return false;
            }
        }
        true
    }

    /// Reads the lookup and, on `Fetch` or a re-ask, fetches it, with no other input between.
    /// `None` on a store fault.
    fn resolve(&mut self, query: QuorumQuery, reask: bool) -> Option<Resolved> {
        let first = match self.lookup(query) {
            Ok(lookup) => lookup,
            Err(e) => return unavailable(&e),
        };
        let reasked = match first {
            Lookup::Fetch => false,
            Lookup::Unavailable if reask => true,
            lookup => {
                return Some(Resolved::Answer {
                    lookup,
                    reasked: false,
                })
            }
        };
        let CaptureRig {
            engine,
            chain,
            config,
        } = &mut self.rig;
        match engine.fetch_lookup(query.llmq_type, &query.quorum_hash, query.height, chain, config)
        {
            Ok(()) => {}
            Err(FetchLookupError::Busy {
                ..
            }) => return Some(Resolved::Busy),
            Err(FetchLookupError::Reject(e)) => return unavailable(&e),
        }
        let lookup = match self.lookup(query) {
            Ok(lookup) => lookup,
            Err(e) => return unavailable(&e),
        };
        let waiting = self.rig.engine.is_waiting(query.llmq_type, &query.quorum_hash, query.height);
        Some(if lookup == Lookup::Fetch && waiting {
            Resolved::Asked {
                reasked,
            }
        } else {
            Resolved::Answer {
                lookup,
                reasked,
            }
        })
    }

    fn lookup(&self, query: QuorumQuery) -> Result<Lookup, MnReject> {
        let rig = &self.rig;
        rig.engine.lookup_quorum(
            query.llmq_type,
            &query.quorum_hash,
            query.height,
            &rig.chain,
            &rig.config,
        )
    }

    fn host(&mut self, host: Host) {
        match host {
            Host::Pump => {
                self.rig.pump();
            }
            Host::Fail(keys) => {
                for key in keys {
                    self.rig.fail(key);
                }
            }
            Host::SetTip(height) => {
                self.rig.chain.set_tip(height);
                self.tip_seq += 1;
                let CaptureRig {
                    engine,
                    chain,
                    config,
                } = &mut self.rig;
                engine.tip_changed(chain, config).expect("the rig's chain reads");
            }
        }
        self.after_call();
    }

    /// Publishes what the last engine call changed and returns `gen` after it.
    fn after_call(&mut self) -> u64 {
        let engine = &self.rig.engine;
        let progress = engine.progress();
        let moved = progress != self.progress || self.tip_seq != self.status.borrow().tip_seq;
        self.progress = progress;
        let (tip_seq, waits, snapshot) = (self.tip_seq, engine.lookup_waits(), engine.snapshot());
        self.status.send_if_modified(|s| {
            let changed = moved || s.lookup_waits != waits || s.snapshot != snapshot;
            if moved {
                s.gen += 1;
                s.tip_seq = tip_seq;
            }
            s.lookup_waits = waits;
            s.snapshot = Arc::clone(&snapshot);
            changed
        });
        self.status.borrow().gen
    }
}

/// A refused lookup answers `Unavailable`; a store fault stops the owner.
fn unavailable(reject: &MnReject) -> Option<Resolved> {
    (!matches!(reject, MnReject::View(MnViewError::Storage(_)))).then_some(Resolved::Answer {
        lookup: Lookup::Unavailable,
        reasked: false,
    })
}

mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use dash_mnlist::MnRequest;
    use dash_params::LlmqType;
    use dash_pkc::bls::BlsSchemeKind;
    use tokio::sync::Mutex;
    use tokio::task::yield_now;
    use tokio::time::{timeout, Instant};

    use super::*;
    use crate::client::quorum_lookup::{QuorumLookupBridge, QuorumLookupEngine};
    use crate::error::SpvError;

    /// A Platform quorum mined at this height, one block above the synced lists.
    const MINED: u32 = 1_574_531;
    /// The deadline of every lookup. The clock is paused, so it fires only once nothing can run.
    const DEADLINE: Duration = Duration::from_secs(10);

    /// The rig synced to `MINED - 1`, with the header at `MINED` accepted and no list for it.
    fn rig() -> CaptureRig {
        let mut rig = CaptureRig::testnet(1_574_496);
        rig.advance(MINED - 1);
        rig.chain.set_tip(MINED);
        let CaptureRig {
            engine,
            chain,
            config,
        } = &mut rig;
        engine.tip_changed(chain, config).expect("the rig's chain reads");
        rig
    }

    fn platform(rig: &CaptureRig) -> LlmqType {
        rig.config.params().consensus.llmq_type_platform
    }

    /// The lookup at `height` of Core's Platform quorum mined at `MINED`.
    fn query(rig: &CaptureRig, height: u32) -> QuorumQuery {
        let llmq_type = platform(rig);
        QuorumQuery {
            llmq_type,
            quorum_hash: rig.quorum(llmq_type, 1_574_520).expect("Core lists it").0,
            height,
        }
    }

    /// Waits until the owner published `lookup_waits == n`.
    async fn waits(handle: &MnHandle, n: usize) {
        timeout(DEADLINE, handle.status.clone().wait_for(|s| s.lookup_waits == n))
            .await
            .expect("the waits reach n")
            .expect("the owner runs");
    }

    /// Fails the outstanding `getmnlistd`s to `heights` in one owner input, so no command runs
    /// between the failures.
    async fn fail_targets(host: &RigHost, heights: &'static [u32]) {
        let keys = host
            .read(|rig| {
                rig.engine
                    .requests()
                    .into_iter()
                    .filter(|r| {
                        matches!(r, MnRequest::MnListDiff { target_height, .. } if heights.contains(target_height))
                    })
                    .map(|r| r.key())
                    .collect::<Vec<_>>()
            })
            .await;
        assert_eq!(keys.len(), heights.len(), "each target is asked");
        host.run(Host::Fail(keys)).await;
    }

    /// The handle, recording each `Resolve` the bridge sends and the owner's answer.
    struct Recorder<'a> {
        handle: &'a MnHandle,
        calls: Mutex<Vec<(bool, Resolved)>>,
    }

    impl<'a> Recorder<'a> {
        fn new(handle: &'a MnHandle) -> Self {
            Self {
                handle,
                calls: Mutex::new(Vec::new()),
            }
        }

        /// Yields until `n` calls are recorded. It panics if they never come, as no deadline
        /// fires on the paused clock while this task yields.
        async fn wait_calls(&self, n: usize) {
            for _ in 0..10_000 {
                if self.calls.lock().await.len() >= n {
                    return;
                }
                yield_now().await;
            }
            panic!("fewer than {n} calls");
        }

        async fn take(&self) -> Vec<(bool, Resolved)> {
            mem::take(&mut *self.calls.lock().await)
        }
    }

    #[async_trait]
    impl QuorumLookupEngine for Recorder<'_> {
        fn status(&self) -> watch::Receiver<MnStatus> {
            self.handle.status.clone()
        }

        async fn resolve(&self, query: QuorumQuery, reask: bool) -> Option<(Resolved, u64)> {
            let reply = self.handle.resolve(query, reask).await;
            if let Some((resolved, _)) = &reply {
                self.calls.lock().await.push((reask, resolved.clone()));
            }
            reply
        }
    }

    /// A lookup waits for its fetch, Core's reply finds the quorum with its commitment, and a later
    /// query is answered with no new request and no wait.
    #[tokio::test(start_paused = true)]
    async fn a_fetched_quorum_is_found_then_answered_at_once() {
        let rig = rig();
        let q = query(&rig, MINED);
        let core_key = rig.quorum(q.llmq_type, 1_574_520).expect("Core lists it").1;
        let (handle, host) = spawn(rig);
        let bridge = QuorumLookupBridge::default();

        let (found, ()) = tokio::join!(bridge.lookup(&handle, q, DEADLINE), async {
            waits(&handle, 1).await;
            host.run(Host::Pump).await;
        });
        let found = found.expect("found");
        assert_eq!(found.commitment.quorum_hash(), q.quorum_hash);
        assert_eq!(found.quorum.public_key().to_bytes(BlsSchemeKind::Basic).as_slice(), core_key);

        let requests = host.read(|rig| rig.engine.requests()).await;
        let gen = handle.status.borrow().gen;
        let recorder = Recorder::new(&handle);
        assert_eq!(bridge.lookup(&recorder, q, DEADLINE).await.expect("cached"), found);
        assert_eq!(
            recorder.take().await,
            vec![(
                true,
                Resolved::Answer {
                    lookup: Lookup::Found(found),
                    reasked: false
                }
            )]
        );
        assert_eq!(host.read(|rig| rig.engine.requests()).await, requests);
        assert_eq!(handle.status.borrow().gen, gen);
        assert_eq!(handle.status.borrow().lookup_waits, 0);
    }

    /// Each lock reply carries the generation after its own call: a check that moved the best
    /// ChainLock is stamped with the generation it published, and the same check again moves
    /// nothing.
    #[tokio::test(start_paused = true)]
    async fn a_lock_reply_carries_the_generation_after_its_own_call() {
        let rig = rig();
        let cl = rig.chainlock(1_574_516).expect("captured");
        let (handle, _host) = spawn(rig);
        let verify = || async {
            let (reply, answer) = oneshot::channel();
            handle
                .cmd
                .send(MnCommand::VerifyChainLock {
                    cl: cl.clone(),
                    peer: "127.0.0.1:19999".parse().expect("an address"),
                    reply,
                })
                .await
                .expect("the owner runs");
            answer.await.expect("a reply")
        };
        let before = handle.status.borrow().gen;
        let (outcome, gen) = verify().await;
        let v = outcome.expect("Core's ChainLock verifies");
        assert_eq!((v.height, v.block_hash), (cl.height(), cl.block_hash()));
        assert!(gen > before, "the best ChainLock moved");
        assert_eq!(gen, handle.status.borrow().gen, "stamped after its own call");
        let (again, same) = verify().await;
        assert!(again.is_ok());
        assert_eq!(same, gen, "a repeated check moves nothing");

        let (reply, answer) = oneshot::channel();
        handle
            .cmd
            .send(MnCommand::ClearPendingChainLock {
                cl,
                reply,
            })
            .await
            .expect("runs");
        assert_eq!(answer.await.expect("a reply"), (false, gen), "no ChainLock is held");
    }

    fn asked(reasked: bool) -> Resolved {
        Resolved::Asked {
            reasked,
        }
    }

    fn answer(lookup: Lookup, reasked: bool) -> Resolved {
        Resolved::Answer {
            lookup,
            reasked,
        }
    }

    /// Five lookups wait, so a sixth reads `Busy`. A failed request that leaves the waits full
    /// moves `gen`, and the sixth asks again once and reads `Busy` again, which moves nothing; the
    /// failure that frees a slot lets the sixth register, on a chain that never moves.
    #[tokio::test(start_paused = true)]
    async fn a_failure_that_frees_a_slot_lets_a_busy_query_proceed() {
        let rig = rig();
        let waiting = [MINED, 1_574_480, 1_574_470, 1_574_460, 1_574_450].map(|h| query(&rig, h));
        let sixth = query(&rig, 1_574_440);
        let (handle, host) = spawn(rig);
        for q in waiting {
            let (resolved, _) = handle.resolve(q, false).await.expect("runs");
            assert_eq!(resolved, asked(false), "{}", q.height);
        }
        assert_eq!(handle.status.borrow().lookup_waits, 5);

        let bridge = QuorumLookupBridge::default();
        let recorder = Recorder::new(&handle);
        let lookup = bridge.lookup(&recorder, sixth, DEADLINE);
        tokio::pin!(lookup);
        let mut status = handle.status.clone();
        tokio::select! {
            _ = &mut lookup => panic!("a busy query waits"),
            _ = async {
                recorder.wait_calls(1).await;
                assert_eq!(recorder.take().await, vec![(true, Resolved::Busy)]);
                let gen = status.borrow_and_update().gen;
                host.run(Host::Fail(vec![key_to(&host, 1_574_432).await])).await;
                assert!(status.borrow().gen > gen, "the failure moved gen");
                assert_eq!(status.borrow().lookup_waits, 5, "and freed no slot");
                recorder.wait_calls(1).await;
                assert_eq!(recorder.take().await, vec![(true, Resolved::Busy)], "progress woke it");
                for _ in 0..10 {
                    yield_now().await;
                }
                assert!(recorder.calls.lock().await.is_empty(), "a Busy reply wakes nothing");
                host.run(Host::Fail(vec![key_to(&host, 1_574_470).await])).await;
                waits(&handle, 5).await;
                recorder.wait_calls(1).await;
            } => {}
        }
        assert_eq!(recorder.take().await, vec![(true, asked(false))]);
        let waiting = host
            .read(move |rig| {
                rig.engine.is_waiting(sixth.llmq_type, &sixth.quorum_hash, sixth.height)
            })
            .await;
        assert!(waiting, "the sixth query registered in the freed slot");
    }

    /// The key of the outstanding `getmnlistd` to `height`.
    async fn key_to(host: &RigHost, height: u32) -> RequestKey {
        host.read(move |rig| {
            rig.engine
                .requests()
                .into_iter()
                .find(|r| matches!(r, MnRequest::MnListDiff { target_height, .. } if *target_height == height))
                .expect("asked")
                .key()
        })
        .await
    }

    /// The payload that answers a query lands right after the owner replied `Asked` and before
    /// the bridge waits: the publication is already there when it waits, so it is not stranded.
    #[tokio::test(start_paused = true)]
    async fn a_publication_between_the_reply_and_the_wait_is_not_missed() {
        let rig = rig();
        let q = query(&rig, MINED);
        let (handle, host) = spawn(rig);
        host.after_next_command(Host::Pump);
        let recorder = Recorder::new(&handle);
        let found = QuorumLookupBridge::default().lookup(&recorder, q, DEADLINE).await;
        assert_eq!(found.expect("found").commitment.quorum_hash(), q.quorum_hash);
        let calls = recorder.take().await;
        assert_eq!(calls[0], (true, asked(false)));
        assert_eq!(calls.len(), 2, "one wake, then the answer");
    }

    /// An answer already present when `Resolve` runs comes back as `Answer`, not `Asked`, stamped
    /// with the generation that published it.
    #[tokio::test(start_paused = true)]
    async fn a_payload_before_resolve_is_answered_not_asked() {
        let rig = rig();
        let q = query(&rig, MINED);
        let (handle, host) = spawn(rig);
        let (first, _) = handle.resolve(q, false).await.expect("runs");
        assert_eq!(first, asked(false));
        host.run(Host::Pump).await;
        let published = handle.status.borrow().gen;
        let (resolved, gen) = handle.resolve(q, false).await.expect("runs");
        assert!(
            matches!(&resolved, Resolved::Answer { lookup: Lookup::Found(f), reasked: false } if f.commitment.quorum_hash() == q.quorum_hash)
        );
        assert_eq!(gen, published);
    }

    /// `Resolve` first, then the payload, while another query's wait already asked for it: the
    /// reply is `Asked`, and the payload's publication wakes it to `Found`.
    #[tokio::test(start_paused = true)]
    async fn resolve_then_payload_is_asked_then_found() {
        let rig = rig();
        let q = query(&rig, MINED);
        let (handle, host) = spawn(rig);
        let (first, _) = handle.resolve(q, false).await.expect("runs");
        assert_eq!(first, asked(false));
        host.after_next_command(Host::Pump);
        let (second, gen) = handle.resolve(q, false).await.expect("runs");
        assert_eq!(second, asked(false));
        let woke = timeout(DEADLINE, handle.status.clone().wait_for(|s| s.gen > gen || s.stopped))
            .await
            .expect("the payload's publication wakes it")
            .expect("the owner runs")
            .gen;
        let (third, at) = handle.resolve(q, false).await.expect("runs");
        assert!(matches!(
            third,
            Resolved::Answer {
                lookup: Lookup::Found(_),
                reasked: false
            }
        ));
        assert_eq!(at, woke);
    }

    /// A query whose fetch fails is woken, re-asks once and then answers `unavailable`; a second
    /// query on the same tip may not re-ask. A tip-only change, with no payload, gives the quorum
    /// a new re-ask.
    #[tokio::test(start_paused = true)]
    async fn a_tip_only_change_resets_the_reask_budget() {
        let rig = rig();
        let q = query(&rig, 1_574_470);
        let (handle, host) = spawn(rig);
        let bridge = QuorumLookupBridge::default();
        let recorder = Recorder::new(&handle);

        let (first, ()) = tokio::join!(bridge.lookup(&recorder, q, DEADLINE), async {
            waits(&handle, 1).await;
            fail_targets(&host, &[1_574_432, 1_574_470]).await;
            recorder.wait_calls(2).await;
            waits(&handle, 1).await;
            fail_targets(&host, &[1_574_432, 1_574_470]).await;
        });
        assert!(
            matches!(first, Err(SpvError::QuorumLookupError(m)) if m.starts_with("unavailable"))
        );
        assert_eq!(
            recorder.take().await,
            vec![
                (true, asked(false)),
                (true, asked(true)),
                (false, answer(Lookup::Unavailable, false)),
            ],
            "the first fetch spends no re-ask; woken by its failure, it re-asks once, and woken by \
             that failure it answers"
        );

        let second = bridge.lookup(&recorder, q, DEADLINE).await;
        assert!(
            matches!(second, Err(SpvError::QuorumLookupError(m)) if m.starts_with("unavailable"))
        );
        assert_eq!(recorder.take().await, vec![(false, answer(Lookup::Unavailable, false))]);

        host.run(Host::SetTip(MINED + 1)).await;
        assert_eq!(handle.status.borrow().lookup_waits, 0);
        let (third, ()) = tokio::join!(bridge.lookup(&recorder, q, DEADLINE), async {
            waits(&handle, 1).await;
            fail_targets(&host, &[1_574_432, 1_574_470]).await;
        });
        assert!(
            matches!(third, Err(SpvError::QuorumLookupError(m)) if m.starts_with("unavailable"))
        );
        assert_eq!(
            recorder.take().await,
            vec![(true, asked(true)), (false, answer(Lookup::Unavailable, false))],
            "the new tip gives the quorum one re-ask"
        );
    }

    /// A stopped owner answers at once: a waiting query, and any query after it.
    #[tokio::test(start_paused = true)]
    async fn a_stopped_owner_answers_at_once() {
        let rig = rig();
        let q = query(&rig, MINED);
        let (handle, _host) = spawn(rig);
        let bridge = QuorumLookupBridge::default();
        let start = Instant::now();
        let (waiting, ()) = tokio::join!(bridge.lookup(&handle, q, DEADLINE), async {
            waits(&handle, 1).await;
            let (ack, done) = oneshot::channel();
            handle.cmd.send(MnCommand::Stop(ack)).await.expect("runs");
            done.await.expect("acked");
        });
        assert!(
            matches!(waiting, Err(SpvError::QuorumLookupError(m)) if m.starts_with("unavailable"))
        );
        assert!(handle.status.borrow().stopped);
        let after = bridge.lookup(&handle, q, DEADLINE).await;
        assert!(
            matches!(after, Err(SpvError::QuorumLookupError(m)) if m.starts_with("unavailable"))
        );
        assert_eq!(start.elapsed(), Duration::ZERO, "no deadline was reached");
    }
}
