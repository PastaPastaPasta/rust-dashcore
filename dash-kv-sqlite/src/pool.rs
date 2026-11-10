//! The capped pool of read connections (SB0 §9.4, DEC-104 m5): each connection carries a page
//! cache, so the pool never opens more than its cap, and a caller beyond it waits: for as long as
//! it takes when it holds none of them, for a bounded time when it holds one (SB10 Q8).

use std::cell::RefCell;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dash_kv::{ErrorKind, Result, StoreError};
use rusqlite::Connection;

use crate::error::ENGINE;

thread_local! {
    /// The pools this thread holds a connection of, once per connection.
    static HELD: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// Why [`Pool::take`] gave no connection.
pub(crate) enum TakeError {
    /// A bug of the caller (SB10 Q8): the thread holds every connection of a full pool, or it
    /// holds one and its wait for another outlasted the bound (threads that each hold a snapshot
    /// and each take again).
    Misuse(StoreError),
    /// Opening a connection failed.
    Open(StoreError),
}

impl From<TakeError> for StoreError {
    fn from(e: TakeError) -> Self {
        match e {
            TakeError::Misuse(e) | TakeError::Open(e) => e,
        }
    }
}

pub(crate) struct Pool {
    id: usize,
    cap: usize,
    /// How long a take by a thread that holds a connection waits for a returned one.
    wait: Duration,
    open: Box<dyn Fn() -> Result<Connection> + Send + Sync>,
    state: Mutex<State>,
    returned: Condvar,
}

struct State {
    idle: Vec<Connection>,
    /// Connections open, idle or taken.
    opened: usize,
}

impl Pool {
    /// A pool of at most `cap` connections, each opened by `open` when first needed; a take by a
    /// thread that holds one waits at most `wait` for another to come back.
    pub(crate) fn new(
        cap: usize,
        wait: Duration,
        open: impl Fn() -> Result<Connection> + Send + Sync + 'static,
    ) -> Self {
        assert!(cap >= 1, "a read pool needs a connection");
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            cap,
            wait,
            open: Box::new(open),
            state: Mutex::new(State {
                idle: Vec::new(),
                opened: 0,
            }),
            returned: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn held_here(&self) -> usize {
        HELD.with(|h| h.borrow().iter().filter(|&&id| id == self.id).count())
    }

    /// A connection: an idle one, a new one below the cap, or the next one returned.
    ///
    /// A thread that holds every open connection of a full pool would wait for itself for ever
    /// (on mobile, the cap is 1: a live read inside a snapshot). That is a bug of the caller: a
    /// debug build asserts, and a release build returns an error of kind [`ErrorKind::Other`]
    /// instead of hanging (Q8). Threads that each hold a snapshot and each take again would wait
    /// for each other: a thread that holds a connection waits at most the pool's bound, then the
    /// take fails with that kind too. A thread that holds none only waits behind other threads'
    /// reads, as Core's reads queue behind a compaction: it waits until a connection comes back.
    pub(crate) fn take(&self) -> Result<Pooled<'_>, TakeError> {
        self.take_checked(cfg!(debug_assertions))
    }

    /// [`take`](Self::take), with the debug assertion on or off (so a test sees both).
    pub(crate) fn take_checked(&self, assert_nested: bool) -> Result<Pooled<'_>, TakeError> {
        let held = self.held_here();
        // `None` past `Instant`'s range: then the wait is unbounded in effect.
        let deadline = Instant::now().checked_add(self.wait);
        let mut state = self.lock();
        loop {
            if let Some(conn) = state.idle.pop() {
                drop(state);
                return Ok(self.lend(conn));
            }
            if state.opened < self.cap {
                // Reserve the slot, open outside the lock, release the slot if the open fails.
                state.opened += 1;
                drop(state);
                return match (self.open)() {
                    Ok(conn) => Ok(self.lend(conn)),
                    Err(e) => {
                        self.lock().opened -= 1;
                        self.returned.notify_one();
                        Err(TakeError::Open(e))
                    }
                };
            }
            if held > 0 && held >= state.opened {
                let err = StoreError::engine(
                    ENGINE,
                    ErrorKind::Other,
                    format!(
                        "nested pool take: this thread holds all {held} read connections and \
                         would wait for itself (SB0 §9.4)"
                    ),
                );
                assert!(!assert_nested, "{err}");
                return Err(TakeError::Misuse(err));
            }
            if held == 0 {
                state = self.returned.wait(state).unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            let left = deadline.map_or(self.wait, |d| d.saturating_duration_since(Instant::now()));
            if left.is_zero() {
                return Err(TakeError::Misuse(StoreError::engine(
                    ENGINE,
                    ErrorKind::Other,
                    format!(
                        "no read connection came back within {:?}: are threads holding snapshots \
                         while reading live (SB10 Q8)?",
                        self.wait
                    ),
                )));
            }
            state =
                self.returned.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    fn lend(&self, conn: Connection) -> Pooled<'_> {
        HELD.with(|h| h.borrow_mut().push(self.id));
        Pooled {
            pool: self,
            conn: Some(conn),
            _here: PhantomData,
        }
    }

    /// Connections open now, idle or taken (for tests).
    #[cfg(test)]
    pub(crate) fn opened(&self) -> usize {
        self.lock().opened
    }
}

/// A connection taken from a [`Pool`]; dropping it ends any read transaction and returns it.
///
/// It is not `Send`: the nested-take guard counts the connections a thread holds in a
/// thread-local, which a connection moved to another thread would leave stale for ever.
pub(crate) struct Pooled<'a> {
    pool: &'a Pool,
    conn: Option<Connection>,
    _here: PhantomData<*const ()>,
}

impl Deref for Pooled<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("held until drop")
    }
}

impl Drop for Pooled<'_> {
    fn drop(&mut self) {
        HELD.with(|h| {
            let mut held = h.borrow_mut();
            if let Some(at) = held.iter().position(|&id| id == self.pool.id) {
                held.swap_remove(at);
            }
        });
        let conn = self.conn.take().expect("held until drop");
        if !conn.is_autocommit() {
            let _ = crate::exec_cached(&conn, "ROLLBACK");
        }
        // No pinned view or running statement may reach the next taker (SB5's read-after-commit
        // contract): a connection that cannot shed them is closed instead.
        let clean = conn.is_autocommit() && !conn.is_busy();
        debug_assert!(clean, "a pooled connection comes back in a transaction or mid-statement");
        let closed = {
            let mut state = self.pool.lock();
            if clean {
                state.idle.push(conn);
                None
            } else {
                state.opened -= 1;
                Some(conn)
            }
        };
        drop(closed);
        self.pool.returned.notify_one();
    }
}
