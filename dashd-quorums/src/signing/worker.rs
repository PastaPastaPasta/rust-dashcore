//! Core's "recsigs" thread, `NetSigning::WorkThreadSigning`, and its lifecycle.

use std::io;
use std::panic;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use dash_llmq::signing::api::SigningFault;

use super::SigningInterrupt;

/// The idle sleep between passes when a pass leaves no more work.
// Core: v24.0.2 src/llmq/net_signing.cpp:293-296
pub const IDLE_SLEEP: Duration = Duration::from_millis(100);

/// One loop's calls, as `WorkThreadSigning` makes them.
pub trait RecSigsWork: Send + Sync + 'static {
    /// `ProcessPendingRecoveredSigs`: Core's `more_work`.
    fn process_pending(&self) -> Result<bool, SigningFault>;
    /// `Cleanup` behind its five-second `CleanupThrottler`, on the mock-aware clock; whether a
    /// pass ran.
    fn cleanup(&self) -> Result<bool, SigningFault>;
}

/// Reports a `Storage` or `Lookup` fault that ended the loop: the node's `AbortNode`.
pub type RecSigsAbort = Box<dyn FnOnce(SigningFault) + Send>;

/// The thread body [`RecSigsWorker::start`] hands its spawner.
pub type RecSigsBody = Box<dyn FnOnce() + Send>;

/// The "recsigs" thread. It carries no no-wait marker: the recovered-signature listeners run on
/// it and may wait on V.
pub struct RecSigsWorker {
    interrupt: SigningInterrupt,
    thread: Option<JoinHandle<()>>,
}

impl RecSigsWorker {
    /// Starts the thread named "recsigs" through `spawn`. `interrupt` is NetSigning's
    /// `workInterrupt`, which the sig-share threads share.
    ///
    /// Production passes dashd-node's `host::trace_thread`, Core's `util::TraceThread`: a panic
    /// in the body, such as a listener's, is logged and ends the process. The worker itself
    /// catches nothing.
    // Core: v24.0.2 src/llmq/net_signing.cpp:152-160 NetSigning::Start; src/util/thread.cpp:16-26
    pub fn start<W: RecSigsWork>(
        work: Arc<W>,
        interrupt: SigningInterrupt,
        abort: RecSigsAbort,
        spawn: impl FnOnce(&str, RecSigsBody) -> io::Result<JoinHandle<()>>,
    ) -> io::Result<Self> {
        let thread_interrupt = interrupt.clone();
        let body: RecSigsBody = Box::new(move || {
            let sleep = |d| thread_interrupt.sleep_for(d);
            match run(&*work, &thread_interrupt, sleep) {
                Ok(()) | Err(SigningFault::Stopped) => {}
                Err(fault) => abort(fault),
            }
        });
        let thread = spawn("recsigs", body)?;
        Ok(Self {
            interrupt,
            thread: Some(thread),
        })
    }

    // Core: v24.0.2 src/llmq/net_signing.h:61-63 InterruptWorkerThread
    pub fn interrupt(&self) {
        self.interrupt.trigger();
    }

    /// Joins the thread. Call [`Self::interrupt`] first. A panic the spawner let through the
    /// join is resumed here.
    // Core: v24.0.2 src/llmq/net_signing.cpp:174-182 NetSigning::Stop
    pub fn stop(&mut self) {
        assert!(self.interrupt.is_set(), "recsigs stopped before its interrupt");
        if let Some(thread) = self.thread.take()
            && let Err(panic) = thread.join()
        {
            panic::resume_unwind(panic);
        }
    }
}

impl Drop for RecSigsWorker {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.interrupt();
            self.stop();
        }
    }
}

/// The loop, on the calling thread; `sleep` is the interrupt's `sleep_for`, false once
/// interrupted.
// Core: v24.0.2 src/llmq/net_signing.cpp:283-298 WorkThreadSigning
pub(crate) fn run(
    work: &impl RecSigsWork,
    interrupt: &SigningInterrupt,
    mut sleep: impl FnMut(Duration) -> bool,
) -> Result<(), SigningFault> {
    while !interrupt.is_set() {
        let more_work = work.process_pending()?;
        work.cleanup()?;
        if !more_work && !sleep(IDLE_SLEEP) {
            return Ok(());
        }
    }
    Ok(())
}

/// A plain named std thread, for tests.
#[cfg(test)]
pub(crate) fn std_spawn(name: &str, body: RecSigsBody) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name(name.into()).spawn(body)
}

#[cfg(test)]
mod tests;
