//! The BLS checks of mined quorum commitments on Core's check queue (LQ2 D11, DEC-325):
//! `ProcessBlock` under `fBLSChecks` queues every non-null commitment's checks on `m_bls_queue`,
//! whose workers run them while the connecting thread queues the rest, and reports one verdict
//! at `Wait` (`llmq/blockprocessor.cpp` v24:267-284). [`BlsCheckQueue`] is that queue behind
//! dash-evo's [`BlsCheckRunner`] seam.

use std::sync::atomic::{AtomicBool, Ordering};

use dash_evo::llmq::{BlsCheck, BlsCheckRunner, BlsVerdict};

use super::{Check, CheckQueue};

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

/// One queued [`BlsCheck`]: it fails the queue when Core's check returns false, and records a
/// check with no BLS verdict without failing it, so that a failure anywhere still decides.
struct Queued<'s> {
    check: BlsCheck,
    unsupported: &'s AtomicBool,
}

impl Check for Queued<'_> {
    const BATCH_SIZE: usize = BLS_BATCH_SIZE;

    fn check(self) -> bool {
        match self.check.verdict() {
            BlsVerdict::Valid => true,
            BlsVerdict::Invalid => false,
            BlsVerdict::Unsupported => {
                self.unsupported.store(true, Ordering::Relaxed);
                true
            }
        }
    }
}

/// Core's `m_bls_queue`: the BLS check threads ([`bls_check_threads`]), or none for serial
/// checks.
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
    // Core: llmq/blockprocessor.cpp:267-284 (CCheckQueueControl, Add, Wait)
    fn run(&self, checks: Vec<BlsCheck>) -> BlsVerdict {
        let unsupported = AtomicBool::new(false);
        let unsupported = &unsupported;
        let ((), ok) = self.queue.control(|control| {
            control.add(
                checks
                    .into_iter()
                    .map(|check| Queued {
                        check,
                        unsupported,
                    })
                    .collect(),
            );
        });
        if !ok {
            BlsVerdict::Invalid
        } else if unsupported.load(Ordering::Relaxed) {
            BlsVerdict::Unsupported
        } else {
            BlsVerdict::Valid
        }
    }
}

#[cfg(test)]
mod tests {
    use dash_evo::bls::BlsSchemeKind;
    use dash_evo::llmq::{BlsPkValue, BlsSigValue, CommitmentReject, SerialBlsChecks};

    use super::*;

    /// A check with no key, which fails (`utils.cpp` v24:561-579): `Invalid`, or `Unsupported`
    /// when `unsure`.
    fn failing(unsure: bool) -> BlsCheck {
        let sig = BlsSigValue::invalid();
        BlsCheck::new(
            sig,
            vec![],
            [0; 32],
            BlsSchemeKind::Basic,
            CommitmentReject::QuorumSignature,
            unsure,
        )
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
        use dash_pkc::bls::{BlsScIetf, BlsSecretKey, BlsSigId};
        let hash = [7; 32];
        let sk = BlsSecretKey::<BlsScIetf>::from_ikm(&[1; 32]).expect("a key");
        let sig = BlsSigValue::from_canonical(
            &sk.sign_with(&hash, BlsSigId::Basic).to_bytes(),
            BlsSchemeKind::Basic,
        )
        .expect("canonical");
        let pk = BlsPkValue::from_canonical(&sk.public_key().to_bytes(), BlsSchemeKind::Basic)
            .expect("canonical");
        BlsCheck::new(
            sig,
            vec![pk],
            hash,
            BlsSchemeKind::Basic,
            CommitmentReject::QuorumSignature,
            false,
        )
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
