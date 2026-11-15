//! The BLS checks of mined quorum commitments on Core's check queue (LQ2 D11, DEC-325):
//! `ProcessBlock` under `fBLSChecks` queues every non-null commitment's checks on `m_bls_queue`,
//! whose workers run them while the connecting thread queues the rest, and reports one verdict
//! at `Wait` (`llmq/blockprocessor.cpp` v24:267-284). [`BlsCheckQueue`] is that queue behind
//! dash-evo's [`BlsCheckRunner`] seam.

use std::sync::atomic::{AtomicBool, Ordering};

use dash_evo::llmq::{BlsCheck, BlsCheckRunner, BlsVerdict};

use super::{Check, CheckQueue, Shared};

/// `MAX_BLSCHECK_THREADS`: the most additional BLS check threads `-parbls` can ask for.
// Core: llmq/options.h:42 MAX_BLSCHECK_THREADS
pub const MAX_BLSCHECK_THREADS: usize = 33;

/// `DEFAULT_BLSCHECK_THREADS`: `-parbls`'s default, 0 (auto).
// Core: llmq/options.h:38 DEFAULT_BLSCHECK_THREADS
pub const DEFAULT_BLSCHECK_THREADS: i64 = 0;

/// The `nBatchSize` Core builds `m_bls_queue` with.
// Core: llmq/blockprocessor.h:60 m_bls_queue{4}
pub const BLS_BATCH_SIZE: usize = 4;

/// Core's `-parbls` arithmetic: the number of **additional** BLS check threads for
/// `-parbls=parbls` on a host with `num_cores` cores (Core's `GetNumCores()`, as for
/// [`super::script_check_threads`]). `-parbls=0` (the default) means one fewer than the cores,
/// `-parbls=-n` leaves `n` cores free, and the result is clamped to `[0, 33]`: the connecting
/// thread counts as one, as it joins the workers at `Wait`.
// Core: init.cpp:2061-2071 at v24.0.0 (options.bls_threads); llmq/blockprocessor.cpp:81-90
// StartWorkerThreads
pub fn bls_check_threads(parbls: i64, num_cores: usize) -> usize {
    let mut threads = parbls;
    if threads <= 0 {
        threads = threads.saturating_add(i64::try_from(num_cores).unwrap_or(i64::MAX));
    }
    let max = i64::try_from(MAX_BLSCHECK_THREADS).unwrap_or(i64::MAX);
    usize::try_from(threads.clamp(1, max + 1) - 1).unwrap_or(0)
}

/// One queued [`BlsCheck`]: it fails the queue when Core's check returns false, logging Core's
/// line for it under `-debug=llmq`, and records a check with no BLS verdict without failing it,
/// so that a failure anywhere still decides.
struct Queued<'s> {
    check: BlsCheck,
    unsupported: &'s AtomicBool,
}

impl Check for Queued<'_> {
    const BATCH_SIZE: usize = BLS_BATCH_SIZE;

    fn check(self) -> bool {
        match self.check.verdict() {
            BlsVerdict::Valid => true,
            BlsVerdict::Invalid => {
                // Core: llmq/utils.cpp:561-579 BlsCheck::operator() (LogPrint(BCLog::LLMQ))
                tracing::debug!(target: "llmq", "{}", self.check.failure_line());
                false
            }
            BlsVerdict::Unsupported => {
                self.unsupported.store(true, Ordering::Relaxed);
                true
            }
        }
    }
}

/// Core's `m_bls_queue`: the BLS check threads ([`bls_check_threads`]), or none: the caller runs
/// Core's `Loop` alone at `Wait`.
#[derive(Debug)]
pub struct BlsCheckQueue {
    queue: CheckQueue,
}

impl BlsCheckQueue {
    /// A queue with `threads` additional threads (see [`bls_check_threads`]).
    ///
    /// # Errors
    ///
    /// When the threads cannot be started.
    // Core: llmq/blockprocessor.cpp:90 m_bls_queue.StartWorkerThreads
    pub fn new(threads: usize) -> Result<Self, rayon::ThreadPoolBuildError> {
        Ok(Self {
            queue: CheckQueue::new(threads)?,
        })
    }

    /// The additional threads.
    pub fn threads(&self) -> usize {
        self.queue.threads()
    }
}

impl BlsCheckRunner for BlsCheckQueue {
    /// Core builds the control on `m_bls_queue` with or without worker threads, so without them
    /// the caller runs Core's `Loop` alone at `Wait`: batches off the top of the stack, which
    /// orders the checks, and so which failure Core logs, differently from push order.
    // Core: llmq/blockprocessor.cpp:267-284 (CCheckQueueControl, Add, Wait)
    fn run(&self, checks: Vec<BlsCheck>) -> BlsVerdict {
        let unsupported = AtomicBool::new(false);
        let unsupported = &unsupported;
        let queued: Vec<Queued<'_>> = checks
            .into_iter()
            .map(|check| Queued {
                check,
                unsupported,
            })
            .collect();
        let ok = if self.queue.has_threads() {
            self.queue.control(|control| control.add(queued)).1
        } else {
            let shared = Shared::new(0);
            let helpers = shared.add(queued);
            debug_assert_eq!(helpers, 0, "no worker to start");
            shared.drain(false);
            shared.all_ok()
        };
        if !ok {
            BlsVerdict::Invalid
        } else if unsupported.load(Ordering::Relaxed) {
            BlsVerdict::Unsupported
        } else {
            BlsVerdict::Valid
        }
    }
}

// Not under loom: the tests read `chain`'s log capture, which loom's build leaves out.
#[cfg(all(test, not(loom)))]
mod tests {
    use dash_evo::bls::BlsSchemeKind;
    use dash_evo::llmq::{BlsPkValue, BlsSigValue, CommitmentReject, SerialBlsChecks};
    use dash_primitives::QuorumHash;

    use super::*;

    /// A check with no key, which fails (`utils.cpp` v24:561-579): `Invalid`, or `Unsupported`
    /// when `unsure`.
    fn failing(unsure: bool) -> BlsCheck {
        no_key(QuorumHash::from([0; 32]), CommitmentReject::QuorumSignature, unsure)
    }

    /// [`failing`]'s check of quorum `q`, failing with `reject`.
    fn no_key(q: QuorumHash, reject: CommitmentReject, unsure: bool) -> BlsCheck {
        let sig = BlsSigValue::invalid();
        BlsCheck::new(sig, vec![], [0; 32], BlsSchemeKind::Basic, q, reject, unsure)
    }

    /// Over every helper count and batch boundary, the queue gives what the serial checks give:
    /// a failure anywhere is `Invalid`, even among checks with no verdict, which alone are
    /// `Unsupported`; no checks are `Valid`.
    #[test]
    fn the_queue_gives_the_serial_verdict() {
        let (invalid, unsupported) = (|| failing(false), || failing(true));
        let lists: Vec<(Vec<BlsCheck>, BlsVerdict)> = vec![
            (vec![], BlsVerdict::Valid),
            (vec![invalid()], BlsVerdict::Invalid),
            (vec![unsupported()], BlsVerdict::Unsupported),
            (vec![unsupported(), invalid()], BlsVerdict::Invalid),
            (vec![invalid(), unsupported()], BlsVerdict::Invalid),
            ((0..50).map(|_| unsupported()).collect(), BlsVerdict::Unsupported),
            ((0..50).map(|i| failing(i != 49)).collect(), BlsVerdict::Invalid),
            ((0..50).map(|i| failing(i != 0)).collect(), BlsVerdict::Invalid),
        ];
        for threads in [0, 1, 3, 8] {
            let queue = BlsCheckQueue::new(threads).expect("the threads");
            assert_eq!(queue.threads(), threads);
            for (checks, verdict) in &lists {
                assert_eq!(
                    SerialBlsChecks.run(checks.clone()),
                    *verdict,
                    "serial, {}",
                    checks.len()
                );
                assert_eq!(
                    queue.run(checks.clone()),
                    *verdict,
                    "{threads} threads, {}",
                    checks.len()
                );
            }
        }
    }

    /// A valid check: one basic-scheme key's signature of `[7; 32]`, verified insecure.
    fn passing() -> BlsCheck {
        signed(QuorumHash::from([0; 32]), CommitmentReject::QuorumSignature, true)
    }

    /// A check of quorum `q` failing with `reject`: one basic-scheme key's signature of
    /// `[7; 32]`, verified insecure against that key when `valid`, else against another.
    fn signed(q: QuorumHash, reject: CommitmentReject, valid: bool) -> BlsCheck {
        use dash_pkc::bls::{BlsScIetf, BlsSecretKey, BlsSigId};
        static SIGNED: std::sync::OnceLock<(BlsSigValue, [BlsPkValue; 2])> =
            std::sync::OnceLock::new();
        let hash = [7; 32];
        let (sig, keys) = SIGNED.get_or_init(|| {
            let sk = |seed| BlsSecretKey::<BlsScIetf>::from_ikm(&[seed; 32]).expect("a key");
            let pk = |seed| {
                let key = sk(seed).public_key().to_bytes();
                BlsPkValue::from_canonical(&key, BlsSchemeKind::Basic).expect("canonical")
            };
            let sig = sk(1).sign_with(&hash, BlsSigId::Basic).to_bytes();
            let sig = BlsSigValue::from_canonical(&sig, BlsSchemeKind::Basic).expect("canonical");
            (sig, [pk(1), pk(2)])
        });
        let key = keys[usize::from(!valid)].clone();
        BlsCheck::new(sig.clone(), vec![key], hash, BlsSchemeKind::Basic, q, reject, false)
    }

    /// A quorum hash only `test`'s `run` uses, `index` telling its checks apart: Core's line
    /// for it starts with [`run_tag`].
    fn quorum(test: u8, run: u32, index: u8) -> QuorumHash {
        let mut bytes = [0; 32];
        bytes[0] = index;
        bytes[25..29].copy_from_slice(&run.to_le_bytes());
        bytes[29] = test;
        bytes[30] = 0x43;
        bytes[31] = 0xb8;
        QuorumHash::from(bytes)
    }

    /// The start of the lines of [`quorum`]`(test, run, _)`, which no other run logs.
    fn run_tag(test: u8, run: u32) -> String {
        format!("CFinalCommitment -- q[b843{test:02x}{run:08x}")
    }

    /// The lines `test`'s `run` logged.
    fn run_lines(test: u8, run: u32) -> Vec<String> {
        crate::chain::tests::support::lines_with(&run_tag(test, run))
    }

    /// Core's order of `n` checks at `-parbls=1` (BL-843 §2.1): the master alone at `Wait`
    /// (`checkqueue.h` v24:69-126, `nTotal` 1, `nIdle` 0) takes the top
    /// `max(1, min(4, len / 2))` of the stack each time and runs that batch in push order.
    fn core_order(n: usize) -> Vec<usize> {
        let (mut left, mut order) = (n, Vec::new());
        while left > 0 {
            let now = (left / 2).clamp(1, BLS_BATCH_SIZE);
            order.extend(left - now..left);
            left -= now;
        }
        order
    }

    #[test]
    fn core_order_is_bl843s() {
        assert_eq!(core_order(2), [1, 0]);
        assert_eq!(core_order(4), [2, 3, 1, 0]);
        assert_eq!(core_order(6), [3, 4, 5, 2, 1, 0]);
        assert_eq!(core_order(12), [8, 9, 10, 11, 4, 5, 6, 7, 2, 3, 1, 0]);
    }

    /// The failure patterns of `n` checks BL-843's T2 runs: each check alone, each pair, all.
    fn patterns(n: usize) -> Vec<Vec<usize>> {
        let mut out: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
        for i in 0..n {
            out.extend((i + 1..n).map(|j| vec![i, j]));
        }
        if n > 2 {
            out.push((0..n).collect());
        }
        out
    }

    /// BL-843 T2: without threads the queue logs exactly Core's one line at `-parbls=1`: the
    /// first failing check's in Core's order ([`core_order`]), not in push order. The checks
    /// that do not fail have no verdict (an unsure check with no key), which the queue counts
    /// as passing and which log nothing.
    #[test]
    fn without_threads_the_queue_logs_cores_line() {
        crate::chain::tests::support::install_accept_log();
        let queue = BlsCheckQueue::new(0).expect("no threads");
        let mut run = 0;
        for n in (1..=12).chain([64]) {
            let order = core_order(n);
            for failing_at in patterns(n) {
                run += 1;
                let checks: Vec<BlsCheck> = (0..n)
                    .map(|i| {
                        let q = quorum(2, run, u8::try_from(i).expect("small"));
                        if failing_at.contains(&i) {
                            signed(q, CommitmentReject::QuorumSignature, false)
                        } else {
                            no_key(q, CommitmentReject::QuorumSignature, true)
                        }
                    })
                    .collect();
                let first = order.iter().find(|i| failing_at.contains(i)).expect("one fails");
                let expected = vec![checks[*first].failure_line()];
                assert_eq!(queue.run(checks), BlsVerdict::Invalid, "{n} checks, {failing_at:?}");
                assert_eq!(run_lines(2, run), expected, "{n} checks, {failing_at:?}");
            }
        }
    }

    /// BL-843 T3: with threads, which failing checks run depends on the schedule, so the lines
    /// are held to BL-843 §2.2's rule: those of failing checks only, as many as fail at most,
    /// one per thread (the caller is one) at most, at least one, and every failing check's when
    /// one fails. Checks with the same quorum and kind log the same line, so this counts lines.
    #[test]
    fn with_threads_the_lines_follow_cores_rule() {
        crate::chain::tests::support::install_accept_log();
        let mut run = 0;
        let kinds = [CommitmentReject::MembersSignature, CommitmentReject::QuorumSignature];
        for threads in [1, 3, 8] {
            let queue = BlsCheckQueue::new(threads).expect("the threads");
            for n in [1, 2, 4, 6, 7, 9, 16, 30] {
                let mut cases: Vec<Vec<usize>> = patterns(n.min(7));
                cases.push((0..n).collect());
                cases.push((0..n).filter(|i| i % 3 != 1).collect());
                cases.push((n / 2..n).collect());
                for failing_at in cases {
                    for repeat in 0..4 {
                        run += 1;
                        // Pairs of checks share a quorum (and, by `repeat`, a kind).
                        let check = |i: usize| {
                            let q = quorum(3, run, u8::try_from(i / 2).expect("small"));
                            let kind = kinds[(i / 2 + i * (repeat % 2)) % 2];
                            if !failing_at.contains(&i) {
                                signed(q, kind, true)
                            } else if i % 4 == 3 {
                                no_key(q, kind, false)
                            } else {
                                signed(q, kind, false)
                            }
                        };
                        let checks: Vec<BlsCheck> = (0..n).map(check).collect();
                        let failed: Vec<String> =
                            failing_at.iter().map(|&i| checks[i].failure_line()).collect();
                        let case = format!("{threads} threads, {n} checks, {failing_at:?}");
                        assert_eq!(queue.run(checks), BlsVerdict::Invalid, "{case}");
                        crate::chain::tests::support::assert_line_rule(
                            &case,
                            &run_lines(3, run),
                            &failed,
                            threads + 1,
                        );
                    }
                }
            }
        }
    }

    /// A script check that passes when its flag is set.
    struct Flag(bool);

    impl Check for Flag {
        fn check(self) -> bool {
            self.0
        }
    }

    /// BL-647 §5.2: Core opens `m_bls_queue`'s control inside `ConnectBlock`'s script control
    /// (`ProcessSpecialTxsInBlock` at `validation.cpp` v24:2674, after the control at :2660 and
    /// before the transaction loop's checks from :2709). Both queues threaded, or either serial,
    /// the BLS verdict is serial's and the script verdict the AND of its checks, added before and
    /// after the BLS run, with no deadlock.
    #[test]
    fn a_bls_control_inside_a_script_control() {
        // A deadlock fails the test after 10 minutes rather than hanging the binary.
        let (done, finished) = crate::chain::reply();
        let body = crate::chain::spawn_thread("nested-controls", move || {
            nested_controls();
            done.send(());
        })
        .expect("the body's thread");
        if finished.recv_timeout(std::time::Duration::from_secs(600)).is_none() {
            panic!("the nested controls did not finish in 10 minutes");
        }
        if let Err(panic) = body.join().expect("the test thread may wait") {
            std::panic::resume_unwind(panic);
        }
    }

    fn nested_controls() {
        let lists: Vec<(Vec<BlsCheck>, BlsVerdict)> = vec![
            (vec![passing()], BlsVerdict::Valid),
            ((0..9).map(|_| passing()).collect(), BlsVerdict::Valid),
            (
                (0..9)
                    .map(|i| {
                        if i == 4 {
                            failing(false)
                        } else {
                            passing()
                        }
                    })
                    .collect(),
                BlsVerdict::Invalid,
            ),
            (vec![passing(), failing(true)], BlsVerdict::Unsupported),
            (vec![failing(true), passing(), failing(false)], BlsVerdict::Invalid),
        ];
        for (checks, verdict) in &lists {
            assert_eq!(SerialBlsChecks.run(checks.clone()), *verdict);
        }
        for (script_threads, bls_threads) in [(3, 3), (1, 8), (8, 1), (0, 3), (3, 0), (15, 31)] {
            let script = CheckQueue::new(script_threads).expect("the script threads");
            let bls = BlsCheckQueue::new(bls_threads).expect("the BLS threads");
            for (checks, verdict) in &lists {
                for script_ok in [true, false] {
                    let flags = |from: usize| -> Vec<Flag> {
                        (from..from + 40).map(|i| Flag(script_ok || i != 57)).collect()
                    };
                    let (got, ok) = script.control(|control| {
                        control.add(flags(0));
                        let got = bls.run(checks.clone());
                        control.add(flags(40));
                        got
                    });
                    let case = format!(
                        "-par {script_threads}, -parbls {bls_threads}, {} checks",
                        checks.len()
                    );
                    assert_eq!(got, *verdict, "{case}");
                    assert_eq!(ok, script_ok, "{case}");
                }
            }
        }
    }

    /// `init.cpp:2061-2071` at v24.0.0: 0 and negative values count from the cores, the connecting
    /// thread is one of them, and the result is clamped to `[0, 33]`.
    #[test]
    fn bls_check_threads_follow_parbls() {
        assert_eq!(bls_check_threads(0, 32), 31);
        assert_eq!(bls_check_threads(0, 1), 0);
        assert_eq!(bls_check_threads(-2, 32), 29);
        assert_eq!(bls_check_threads(-40, 32), 0);
        assert_eq!(bls_check_threads(1, 32), 0);
        assert_eq!(bls_check_threads(4, 32), 3);
        assert_eq!(bls_check_threads(34, 2), 33);
        assert_eq!(bls_check_threads(1000, 2), 33);
        assert_eq!(bls_check_threads(0, 64), 33);
        assert_eq!(bls_check_threads(i64::MIN, 8), 0);
    }
}
