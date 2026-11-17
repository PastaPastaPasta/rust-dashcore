//! Time sources for the Rust Dash node and SPV (SB0 §2.3 class S; SB4 D1).
//!
//! - [`Clock`] is Core's `NodeClock` (mockable wall time, `util/time.h`) plus a steady clock:
//!   [`NodeTime`] at the clock's precision, [`NodeSeconds`] at whole seconds.
//! - [`SystemClock`] reads the system clock; [`MockClock`] is `SystemClock` with Core's
//!   `SetMockTime`.
//! - [`AdjustedTime`] is the seam for Core's `GetAdjustedTime` (system time plus the median peer
//!   offset, `timedata.cpp`). SB6 implements it; v24's header checks never read it (DEC-104 M1).

#![forbid(unsafe_code)]

mod format;

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub use format::{format_iso8601_date_time, push_iso8601};

/// Seconds since the Unix epoch, as Core's `NodeSeconds` (`std::chrono::time_point<NodeClock,
/// std::chrono::seconds>`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeSeconds(pub i64);

impl NodeSeconds {
    /// The count of seconds since the epoch (Core's `count_seconds(t.time_since_epoch())`).
    pub fn secs(self) -> i64 {
        self.0
    }
}

/// Core's `NodeClock::time_point` at the clock's own precision: `NodeClock::now()` compared
/// as is, as Core's `Tip()->Time() < NodeClock::now() - max_tip_age` does. Whole seconds under
/// mock time; the system clock's sub-second part otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeTime {
    /// Whole seconds since the epoch, rounded down.
    secs: i64,
    /// The nanoseconds past `secs`, below 10^9.
    nanos: u32,
}

impl NodeTime {
    const NANOS: u32 = 1_000_000_000;

    /// `secs` seconds and `nanos` nanoseconds past the epoch; `nanos` of a second or more
    /// carry into `secs` (saturating).
    pub fn new(secs: i64, nanos: u32) -> NodeTime {
        NodeTime {
            secs: secs.saturating_add(i64::from(nanos / Self::NANOS)),
            nanos: nanos % Self::NANOS,
        }
    }

    /// The whole seconds since the epoch, rounded down (Core's
    /// `std::chrono::floor<std::chrono::seconds>`).
    pub fn floor_secs(self) -> i64 {
        self.secs
    }

    /// The nanoseconds past [`NodeTime::floor_secs`].
    pub fn subsec_nanos(self) -> u32 {
        self.nanos
    }
}

impl From<NodeSeconds> for NodeTime {
    fn from(t: NodeSeconds) -> NodeTime {
        NodeTime::new(t.0, 0)
    }
}

/// Core's `NodeClock` and a steady clock.
pub trait Clock: Send + Sync {
    /// `NodeClock::now()` in whole seconds: the mock time when one is set, else the system
    /// time truncated.
    // Core: util/time.cpp NodeClock::now
    fn node_now(&self) -> NodeSeconds;

    /// `NodeClock::now()` at the clock's precision: the mock time when one is set, else the
    /// system time. The default is [`Clock::node_now`]'s whole second.
    // Core: util/time.cpp NodeClock::now
    fn node_time(&self) -> NodeTime {
        self.node_now().into()
    }

    /// `GetTime<std::chrono::microseconds>()`: [`Clock::node_time`] in microseconds, so the
    /// mock time × 10⁶ under mock time (Core's per-peer `m_last_send`, `m_last_recv` and ping
    /// times; SB6 D10). Rounded down; Core's `duration_cast` truncates toward zero, which differs
    /// only before 1970.
    // Core: util/time.h GetTime<T>
    fn node_now_micros(&self) -> i64 {
        let t = self.node_time();
        t.floor_secs().saturating_mul(1_000_000).saturating_add(i64::from(t.subsec_nanos() / 1_000))
    }

    /// A monotonic instant (Core's `SteadyClock`), for timeouts and intervals.
    fn steady(&self) -> Instant;
}

/// Core's `GetAdjustedTime()`: the node clock plus the median peer time offset (SB6).
pub trait AdjustedTime: Send + Sync {
    fn now(&self) -> NodeSeconds;
}

fn system_time() -> NodeTime {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => NodeTime::new(i64::try_from(d.as_secs()).unwrap_or(i64::MAX), d.subsec_nanos()),
        Err(e) => {
            // Before the epoch: −(s + n) is −(s + 1) + (10^9 − n) nanoseconds.
            let d = e.duration();
            let secs = -i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
            match d.subsec_nanos() {
                0 => NodeTime::new(secs, 0),
                n => NodeTime::new(secs - 1, NodeTime::NANOS - n),
            }
        }
    }
}

fn system_now() -> NodeSeconds {
    let secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
    };
    NodeSeconds(secs)
}

/// The system clock, with no mock time.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn node_now(&self) -> NodeSeconds {
        system_now()
    }

    fn node_time(&self) -> NodeTime {
        system_time()
    }

    fn steady(&self) -> Instant {
        Instant::now()
    }
}

/// The system clock with Core's mock time (`SetMockTime`, `-mocktime`, `setmocktime`).
///
/// A mock time of 0 means "not set", as in Core (`g_mock_time{0s}`, `util/time.cpp`).
#[derive(Debug, Default)]
pub struct MockClock {
    mock: AtomicI64,
}

impl MockClock {
    /// A clock with no mock time set.
    pub fn new() -> MockClock {
        MockClock::default()
    }

    /// A clock with the mock time set to `t`.
    pub fn at(t: i64) -> MockClock {
        let clock = MockClock::new();
        clock.set_mocktime(t);
        clock
    }

    /// Core's `SetMockTime(t)`; 0 clears it. Core asserts `t >= 0`.
    // Core: util/time.cpp SetMockTime
    pub fn set_mocktime(&self, t: i64) {
        assert!(t >= 0, "mock time cannot be negative");
        self.mock.store(t, Ordering::Relaxed);
    }

    /// Core's `GetMockTime()`: 0 when not set.
    pub fn mocktime(&self) -> i64 {
        self.mock.load(Ordering::Relaxed)
    }
}

impl Clock for MockClock {
    fn node_now(&self) -> NodeSeconds {
        match self.mocktime() {
            0 => system_now(),
            t => NodeSeconds(t),
        }
    }

    fn node_time(&self) -> NodeTime {
        match self.mocktime() {
            0 => system_time(),
            t => NodeTime::new(t, 0),
        }
    }

    fn steady(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_time_overrides_and_clears() {
        let clock = MockClock::at(1_700_000_000);
        assert_eq!(clock.node_now(), NodeSeconds(1_700_000_000));
        clock.set_mocktime(0);
        assert!(clock.node_now().secs() > 1_700_000_000);
    }

    #[test]
    fn node_time_orders_below_the_second() {
        let clock = MockClock::at(1_700_000_000);
        assert_eq!(clock.node_time(), NodeTime::from(NodeSeconds(1_700_000_000)));
        assert!(NodeTime::new(5, 1) > NodeTime::new(5, 0));
        assert!(NodeTime::new(5, 999_999_999) < NodeTime::new(6, 0));
        assert_eq!(NodeTime::new(5, 1_500_000_000), NodeTime::new(6, 500_000_000));
        let t = NodeTime::new(-3, 250);
        assert_eq!((t.floor_secs(), t.subsec_nanos()), (-3, 250));
        clock.set_mocktime(0);
        let (now, secs) = (clock.node_time(), clock.node_now());
        assert!(now.floor_secs() <= secs.secs() && now.floor_secs() > 1_700_000_000);
    }

    /// A clock reading a fixed sub-second time.
    struct Fixed(NodeTime);

    impl Clock for Fixed {
        fn node_now(&self) -> NodeSeconds {
            NodeSeconds(self.0.floor_secs())
        }

        fn node_time(&self) -> NodeTime {
            self.0
        }

        fn steady(&self) -> Instant {
            Instant::now()
        }
    }

    fn micros(t: NodeTime) -> i64 {
        t.floor_secs() * 1_000_000 + i64::from(t.subsec_nanos() / 1_000)
    }

    #[test]
    fn node_now_micros_is_node_time_in_micros() {
        let clock = MockClock::at(1_700_000_000);
        assert_eq!(clock.node_now_micros(), 1_700_000_000_000_000);
        assert_eq!(clock.node_now_micros(), micros(clock.node_time()));
        assert_eq!(Fixed(NodeTime::new(5, 123_456_789)).node_now_micros(), 5_123_456);
        // The real clock moves between reads, so its micros lie between two node_time readings
        // and keep their sub-second part.
        clock.set_mocktime(0);
        for c in [&SystemClock as &dyn Clock, &clock] {
            let before = micros(c.node_time());
            let now = c.node_now_micros();
            assert!(before <= now && now <= micros(c.node_time()), "{before} {now}");
        }
    }

    #[test]
    #[should_panic(expected = "mock time cannot be negative")]
    fn negative_mock_time_panics() {
        MockClock::new().set_mocktime(-1);
    }
}
