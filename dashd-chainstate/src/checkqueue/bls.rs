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
// Core: init.cpp:2071-2081 (options.bls_threads); llmq/blockprocessor.cpp:81-90 StartWorkerThreads
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
    use dash_evo::llmq::{BlsSigValue, CommitmentReject, SerialBlsChecks};

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

    /// `init.cpp` v24:2071-2081: 0 and negative values count from the cores, the connecting
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
