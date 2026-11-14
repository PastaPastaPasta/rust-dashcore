//! Actor replies (CH1-node §2.2, §8.3): a one-shot [`ReplySender`]/[`ActorReply`] pair that a
//! caller can block on or await.
//!
//! LO-9's no-wait marker: a task V may wait on (an F1 pop/ack task, the `ValidationSignals`
//! thread, W, a shared EV-gate holder) never awaits an actor reply. [`ActorReply`] reads the
//! marker on its first poll or blocking wait and refuses at once with
//! [`WaitError::WaitOnVFromNoWaitTask`]; the command still runs and its reply is dropped, which
//! is posting.
//!
//! The marker is thread-local ([`NoWaitGuard`]: the callback thread, W's thread, V's helper
//! threads, the EV gate's shared guard for its lifetime) or task-local ([`Marked`]: a future
//! that carries it into every poll, whichever thread polls it). [`spawn_thread`] and [`inherit`] pass
//! the spawner's marker on, and [`NodeJoinHandle::join`] refuses a marked joiner like a reply.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

/// Why a reply never arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WaitError {
    /// V (or the actor) dropped the sender without replying: it shut down or aborted.
    #[error("the actor dropped the reply")]
    Disconnected,
    /// LO-9: the waiting task carries the no-wait marker.
    #[error("a no-wait task awaited an actor reply (LO-9)")]
    WaitOnVFromNoWaitTask,
}

thread_local! {
    /// The live [`NoWaitGuard`]s on this thread: marked while any is.
    static NO_WAIT: Cell<u32> = const { Cell::new(0) };
}

/// A parked reply: a [`Closing`] whose drop closes it, so no path that drops it can leave its
/// caller waiting (D55, Opus m-1).
type Parked = Box<dyn Any>;

/// Closes its reply, `Disconnected`, when dropped.
struct Closing<T>(Arc<Shared<T>>);

impl<T> Drop for Closing<T> {
    fn drop(&mut self) {
        close(&self.0, None);
    }
}

thread_local! {
    /// The replies dropped unsent on this thread while a [`ParkDrops`] is live (V's thread as it
    /// serves, D55): each closes, `Disconnected`, when [`release_parked`] runs.
    static PARKED: RefCell<Option<Vec<Parked>>> = const { RefCell::new(None) };
}

/// D55: while the guard lives, a [`ReplySender`] dropped unsent on this thread is parked rather
/// than closed, so V can disconnect a failed command's replies only after its `AbortNode` has
/// set the node's flag. Dropping the guard releases what is parked and ends the parking. Not
/// `Send`: it drops on the thread it parks.
pub(crate) struct ParkDrops {
    _thread: PhantomData<*const ()>,
}

impl ParkDrops {
    pub(crate) fn start() -> ParkDrops {
        PARKED.with(|p| {
            let mut p = p.borrow_mut();
            assert!(p.is_none(), "one parking scope per thread");
            *p = Some(Vec::new());
        });
        ParkDrops {
            _thread: PhantomData,
        }
    }
}

impl Drop for ParkDrops {
    fn drop(&mut self) {
        // Parking ends first, so a reply dropped as the parked ones close closes at once.
        let parked = PARKED.with(|p| p.borrow_mut().take());
        drop(parked);
    }
}

/// Releases this thread's parked replies when dropped: after what is declared after it, and as a
/// panic unwinds. `V::stop_serving` holds one so that its replies follow the node's flag whatever
/// `V::run` does after it; `run`'s [`ParkDrops`] would release them later anyway (Opus n-1).
pub(crate) struct ReleaseParked;

impl Drop for ReleaseParked {
    fn drop(&mut self) {
        release_parked();
    }
}

/// Closes, `Disconnected`, every reply this thread has parked, in the order they dropped.
pub(crate) fn release_parked() {
    // A closed reply runs no code of V's but its waker; a reply dropped meanwhile parks anew. If
    // a waker panics, dropping the batch still closes the replies after it (a second panic
    // aborts).
    while let Some(parked) = PARKED
        .with(|p| p.borrow_mut().as_mut().map(std::mem::take))
        .filter(|parked| !parked.is_empty())
    {
        drop(parked);
    }
}

/// Sets LO-9's no-wait marker on the current thread for the guard's lifetime (the callback
/// thread, W's thread, V-spawned helper threads, the EV gate's shared guard). Guards count, so
/// they nest and may drop in any order: the thread stays marked until its last guard drops.
/// Not `Send`: the guard drops on the thread whose count it raised.
pub struct NoWaitGuard {
    _thread: PhantomData<*const ()>,
}

impl NoWaitGuard {
    pub fn set() -> NoWaitGuard {
        NO_WAIT.with(|m| m.set(m.get().checked_add(1).expect("no-wait guards nest below 2^32")));
        NoWaitGuard {
            _thread: PhantomData,
        }
    }
}

impl Drop for NoWaitGuard {
    fn drop(&mut self) {
        NO_WAIT.with(|m| m.set(m.get() - 1));
    }
}

/// Whether the current thread carries the no-wait marker.
pub fn no_wait_marked() -> bool {
    NO_WAIT.with(Cell::get) > 0
}

/// A future that carries a no-wait marker into each poll: task-local marking for an async
/// runtime (ND1's), whichever thread polls it. It only adds the marker: a marked thread that
/// polls an unmarked task (a `block_on` on the callback thread) stays marked.
pub struct Marked<F> {
    marked: bool,
    inner: Pin<Box<F>>,
}

/// `f` as a marked task (an F1 pop/ack task, a listener's task).
pub fn no_wait_task<F: Future>(f: F) -> Marked<F> {
    Marked {
        marked: true,
        inner: Box::pin(f),
    }
}

/// `f` carrying the spawner's marker: the node runtime's `spawn` wraps every task in this.
pub fn inherit<F: Future>(f: F) -> Marked<F> {
    Marked {
        marked: no_wait_marked(),
        inner: Box::pin(f),
    }
}

impl<F: Future> Future for Marked<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let _scope = self.marked.then(NoWaitGuard::set);
        self.inner.as_mut().poll(cx)
    }
}

/// A thread started by [`spawn_thread`].
pub struct NodeJoinHandle<T> {
    #[expect(clippy::disallowed_types, reason = "the handle LO-9's join check wraps")]
    inner: std::thread::JoinHandle<T>,
}

impl<T> NodeJoinHandle<T> {
    /// Waits for the thread, unless the caller is marked: then it refuses at once and the
    /// thread runs on detached (posting). `Ok(Err(_))` is the thread's panic.
    pub fn join(self) -> Result<std::thread::Result<T>, WaitError> {
        if no_wait_marked() {
            return Err(WaitError::WaitOnVFromNoWaitTask);
        }
        #[expect(clippy::disallowed_methods, reason = "the join LO-9's check guards")]
        let joined = self.inner.join();
        Ok(joined)
    }

    pub fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }

    /// The thread's id, for tests that read the lines it logged.
    #[cfg(test)]
    pub(crate) fn thread_id(&self) -> std::thread::ThreadId {
        self.inner.thread().id()
    }
}

/// The node's thread `spawn`: the new thread starts with the spawner's marker.
pub fn spawn_thread<T, F>(name: &str, f: F) -> std::io::Result<NodeJoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let marked = no_wait_marked();
    #[expect(clippy::disallowed_methods, reason = "the spawn that passes the marker on")]
    let inner = std::thread::Builder::new().name(name.into()).spawn(move || {
        let _marked = marked.then(NoWaitGuard::set);
        f()
    })?;
    Ok(NodeJoinHandle {
        inner,
    })
}

/// A panic payload's message, for logs.
pub(crate) fn panic_message(p: &(dyn Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

struct Slot<T> {
    value: Option<T>,
    /// The sender is gone: replied or dropped.
    closed: bool,
    waker: Option<Waker>,
}

impl<T> Slot<T> {
    /// The reply once the sender is gone: its value, or `Disconnected`.
    fn take(&mut self) -> Option<Result<T, WaitError>> {
        self.closed.then(|| self.value.take().ok_or(WaitError::Disconnected))
    }
}

struct Shared<T> {
    slot: Mutex<Slot<T>>,
    ready: Condvar,
}

impl<T> Shared<T> {
    fn slot(&self) -> MutexGuard<'_, Slot<T>> {
        self.slot.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// V's half of a reply. Dropped unsent, it disconnects; on V's thread, only once V releases it
/// (D55).
pub struct ReplySender<T: 'static> {
    shared: Option<Arc<Shared<T>>>,
}

/// The caller's half of a reply: [`ActorReply::recv`] blocks, `.await` suspends.
pub struct ActorReply<T> {
    shared: Arc<Shared<T>>,
}

/// A new reply pair.
pub fn reply<T: 'static>() -> (ReplySender<T>, ActorReply<T>) {
    let shared = Arc::new(Shared {
        slot: Mutex::new(Slot {
            value: None,
            closed: false,
            waker: None,
        }),
        ready: Condvar::new(),
    });
    (
        ReplySender {
            shared: Some(shared.clone()),
        },
        ActorReply {
            shared,
        },
    )
}

impl<T: 'static> ReplySender<T> {
    /// Sends the reply; a receiver that is gone drops it.
    pub fn send(mut self, value: T) {
        if let Some(shared) = self.shared.take() {
            close(&shared, Some(value));
        }
    }
}

/// Closes a reply with `value`, or `Disconnected` for `None`, and wakes its waiter.
fn close<T>(shared: &Shared<T>, value: Option<T>) {
    let waker = {
        let mut slot = shared.slot();
        slot.value = value;
        slot.closed = true;
        slot.waker.take()
    };
    shared.ready.notify_all();
    if let Some(w) = waker {
        w.wake();
    }
}

impl<T: 'static> Drop for ReplySender<T> {
    fn drop(&mut self) {
        let Some(shared) = self.shared.take() else {
            return;
        };
        let mut closing = Some(Closing(shared));
        // A thread whose `PARKED` is already destroyed (a sender dropped by another thread-local's
        // destructor) is never V mid-step: the reply closes at once (Opus m-2).
        let _ = PARKED.try_with(|p| {
            if let Some(parked) = p.borrow_mut().as_mut() {
                parked.extend(closing.take().map(|c| Box::new(c) as Parked));
            }
        });
        drop(closing);
    }
}

impl<T> ActorReply<T> {
    /// Blocks until the reply arrives.
    pub fn recv(self) -> Result<T, WaitError> {
        if no_wait_marked() {
            return Err(WaitError::WaitOnVFromNoWaitTask);
        }
        let mut slot = self.shared.slot();
        loop {
            if let Some(r) = slot.take() {
                return r;
            }
            slot = self.shared.ready.wait(slot).unwrap_or_else(|p| p.into_inner());
        }
    }

    /// [`ActorReply::recv`] for at most `timeout`; `None` if the reply has not arrived by then.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Result<T, WaitError>> {
        if no_wait_marked() {
            return Some(Err(WaitError::WaitOnVFromNoWaitTask));
        }
        let (mut slot, _) = self
            .shared
            .ready
            .wait_timeout_while(self.shared.slot(), timeout, |s| !s.closed)
            .unwrap_or_else(|p| p.into_inner());
        slot.take()
    }

    /// The reply if it has arrived, without waiting (a poster that checks later).
    pub fn try_recv(&self) -> Option<Result<T, WaitError>> {
        self.shared.slot().take()
    }
}

impl<T> Future for ActorReply<T> {
    type Output = Result<T, WaitError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if no_wait_marked() {
            return Poll::Ready(Err(WaitError::WaitOnVFromNoWaitTask));
        }
        let mut slot = self.shared.slot();
        if let Some(r) = slot.take() {
            return Poll::Ready(r);
        }
        slot.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(clippy::disallowed_methods, reason = "a plain test thread")]
    fn a_reply_arrives_across_threads() {
        let (tx, rx) = reply();
        let t = std::thread::spawn(move || tx.send(7));
        assert_eq!(rx.recv(), Ok(7));
        t.join().expect("sender");
    }

    /// D55: under a `ParkDrops`, a reply dropped unsent stays open until `release_parked`, and
    /// a sent one is not delayed; the guard's drop releases the rest and ends the parking.
    #[test]
    fn parked_drops_disconnect_when_released() {
        let parking = ParkDrops::start();
        let (dropped, waiting) = reply::<u8>();
        let (sent, answered) = reply::<u8>();
        drop(dropped);
        sent.send(7);
        assert_eq!(answered.recv_timeout(Duration::ZERO), Some(Ok(7)));
        assert_eq!(waiting.recv_timeout(Duration::ZERO), None, "parked");
        release_parked();
        assert_eq!(waiting.recv_timeout(Duration::ZERO), Some(Err(WaitError::Disconnected)));
        let (later, left) = reply::<u8>();
        drop(later);
        drop(parking);
        assert_eq!(left.recv_timeout(Duration::ZERO), Some(Err(WaitError::Disconnected)));
        let (unparked, closed) = reply::<u8>();
        drop(unparked);
        assert_eq!(closed.recv_timeout(Duration::ZERO), Some(Err(WaitError::Disconnected)));
    }

    /// Opus m-1: a waker that panics as its parked reply closes does not strand the replies
    /// parked after it, and a second parking scope on a thread is refused rather than dropping
    /// the first one's replies.
    #[test]
    fn a_panicking_waker_leaves_the_other_parked_replies_closing() {
        struct Panics;
        impl std::task::Wake for Panics {
            fn wake(self: Arc<Self>) {
                panic!("the waker failed");
            }
        }
        let parking = ParkDrops::start();
        let (first, mut woken) = reply::<u8>();
        let (second, waiting) = reply::<u8>();
        let waker = Waker::from(Arc::new(Panics));
        assert!(Pin::new(&mut woken).poll(&mut Context::from_waker(&waker)).is_pending());
        drop(first);
        drop(second);
        assert!(std::panic::catch_unwind(ParkDrops::start).is_err(), "one scope per thread");
        assert!(std::panic::catch_unwind(release_parked).is_err(), "the waker's panic");
        assert_eq!(waiting.recv_timeout(Duration::ZERO), Some(Err(WaitError::Disconnected)));
        drop(parking);
    }

    /// Opus m-2: a sender dropped by another thread-local's destructor, after this thread's
    /// parking slot is gone, closes its reply rather than aborting the process.
    #[test]
    #[expect(clippy::disallowed_methods, reason = "a plain test thread")]
    fn a_sender_dropped_as_its_thread_tears_down_closes() {
        thread_local! {
            static HELD: RefCell<Option<ReplySender<u8>>> = const { RefCell::new(None) };
        }
        let (tx, rx) = reply::<u8>();
        std::thread::spawn(move || {
            // Thread-locals are destroyed last-touched first: `PARKED` before `HELD`.
            HELD.with(|h| *h.borrow_mut() = Some(tx));
            release_parked();
        })
        .join()
        .expect("the thread exits");
        assert_eq!(rx.recv_timeout(Duration::ZERO), Some(Err(WaitError::Disconnected)));
    }

    #[test]
    fn a_dropped_sender_disconnects() {
        let (tx, rx) = reply::<u32>();
        drop(tx);
        assert_eq!(rx.recv(), Err(WaitError::Disconnected));
    }

    /// LO-9: a marked thread is refused at once, and the send still completes (posting).
    #[test]
    fn a_marked_thread_never_parks() {
        let (tx, rx) = reply::<u32>();
        {
            let _marked = NoWaitGuard::set();
            assert_eq!(rx.try_recv(), None);
            let (tx2, rx2) = reply::<u32>();
            assert_eq!(rx2.recv(), Err(WaitError::WaitOnVFromNoWaitTask));
            drop(tx2);
        }
        assert!(!no_wait_marked(), "the guard restores the marker");
        tx.send(1);
        assert_eq!(rx.recv(), Ok(1));
    }

    /// Sol cp1 #1: nested guards dropped out of order keep the thread marked until the last one
    /// drops; a prev-restoring guard cleared the marker under a live one.
    #[test]
    fn nested_guards_dropped_out_of_order_keep_the_marker() {
        let outer = NoWaitGuard::set();
        let inner = NoWaitGuard::set();
        drop(outer);
        assert!(no_wait_marked(), "the inner guard still holds it");
        let (tx, rx) = reply::<u32>();
        assert_eq!(rx.recv(), Err(WaitError::WaitOnVFromNoWaitTask));
        drop(tx);
        drop(inner);
        assert!(!no_wait_marked());
    }

    /// Sol cp1 #1: a guard cannot move to another thread (which would drop it there and leave
    /// its own thread marked). This compiles only while `NoWaitGuard: !Send`: with `Send`, both
    /// impls apply and the call is ambiguous (`static_assertions`' `assert_not_impl_any`).
    #[test]
    fn the_guard_is_not_send() {
        trait AmbiguousIfSend<A> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfSend<()> for T {}
        impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
        <NoWaitGuard as AmbiguousIfSend<_>>::check();
    }

    /// Polls `f` once on this thread with a no-op waker.
    fn poll_once<F: Future + Unpin>(f: &mut F) -> Poll<F::Output> {
        Pin::new(f).poll(&mut Context::from_waker(Waker::noop()))
    }

    /// A [`Marked`] task carries its marker into each poll, on any thread; an unmarked one
    /// never clears a marked poller's; the poller's own marker is back after the poll.
    #[test]
    fn a_task_carries_its_marker_into_each_poll() {
        let mut marked = no_wait_task(std::future::poll_fn(|_| Poll::Ready(no_wait_marked())));
        assert_eq!(poll_once(&mut marked), Poll::Ready(true));
        assert!(!no_wait_marked());

        let plain = || inherit(std::future::poll_fn(|_| Poll::Ready(no_wait_marked())));
        let (mut a, mut b) = (plain(), plain());
        assert_eq!(poll_once(&mut a), Poll::Ready(false), "inherited from an unmarked spawner");
        let _g = NoWaitGuard::set();
        assert_eq!(poll_once(&mut b), Poll::Ready(true), "a marked poller stays marked");
        assert!(no_wait_marked());

        // A marked task awaiting a reply is refused, whichever thread polls it.
        drop(_g);
        let (tx, rx) = reply::<u32>();
        let mut task = no_wait_task(rx);
        assert_eq!(poll_once(&mut task), Poll::Ready(Err(WaitError::WaitOnVFromNoWaitTask)));
        drop(tx);
    }

    /// `spawn_thread` passes the spawner's marker on; a marked joiner is refused at once and the
    /// thread runs on detached.
    #[test]
    fn spawned_threads_inherit_the_marker_and_marked_joins_are_refused() {
        let plain = spawn_thread("plain", no_wait_marked).expect("spawn");
        assert!(!plain.join().expect("unmarked joiner").expect("no panic"));

        let _g = NoWaitGuard::set();
        let (tx, rx) = reply::<()>();
        let child = spawn_thread("marked", move || {
            let marked = no_wait_marked();
            tx.send(());
            marked
        })
        .expect("spawn");
        assert_eq!(child.join().err(), Some(WaitError::WaitOnVFromNoWaitTask));
        drop(_g);
        rx.recv().expect("the detached thread ran");

        let _g = NoWaitGuard::set();
        let child = spawn_thread("marked", no_wait_marked).expect("spawn");
        drop(_g);
        assert!(child.join().expect("unmarked joiner").expect("no panic"), "inherited");
    }
}
