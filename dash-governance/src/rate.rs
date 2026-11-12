//! `CRateCheckBuffer` and `MasternodeRateCheck`: at most about one trigger per masternode per
//! superblock cycle (notes D14).

use crate::GovParams;

/// `RATE_BUFFER_SIZE`.
// Core: governance/governance.h:63
pub const RATE_BUFFER_SIZE: usize = 5;

/// `MAX_TIME_FUTURE_DEVIATION`: how far ahead of the adjusted clock an object may be.
// Core: governance/governance.cpp:36
pub const MAX_TIME_FUTURE_DEVIATION: i64 = 60 * 60;

/// `CRateCheckBuffer`: a ring of the last [`RATE_BUFFER_SIZE`] trigger timestamps, with Core's
/// fields (it is serialized in `governance.dat`). The fields are private, as in Core: both
/// positions stay inside the ring.
// Core: governance/governance.h:66-153
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateCheckBuffer {
    timestamps: [i64; RATE_BUFFER_SIZE],
    start: usize,
    end: usize,
    empty: bool,
}

impl Default for RateCheckBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl RateCheckBuffer {
    /// The constructor: empty, both positions 0.
    pub const fn new() -> Self {
        RateCheckBuffer {
            timestamps: [0; RATE_BUFFER_SIZE],
            start: 0,
            end: 0,
            empty: true,
        }
    }

    /// A buffer from its serialized fields (`vecTimestamps`, `nDataStart`, `nDataEnd`,
    /// `fBufferEmpty`), or `None` unless there are [`RATE_BUFFER_SIZE`] timestamps and both
    /// positions index them. Core indexes the vector with the positions unchecked.
    // Core: governance/governance.h:150-153
    pub fn from_parts(timestamps: &[i64], start: i32, end: i32, empty: bool) -> Option<Self> {
        let position = |p: i32| usize::try_from(p).ok().filter(|&p| p < RATE_BUFFER_SIZE);
        Some(RateCheckBuffer {
            timestamps: timestamps.try_into().ok()?,
            start: position(start)?,
            end: position(end)?,
            empty,
        })
    }

    /// `vecTimestamps`.
    pub fn timestamps(&self) -> &[i64; RATE_BUFFER_SIZE] {
        &self.timestamps
    }

    /// `nDataStart`: the oldest timestamp's slot.
    pub fn start(&self) -> usize {
        self.start
    }

    /// `nDataEnd`: the next slot written.
    pub fn end(&self) -> usize {
        self.end
    }

    /// `fBufferEmpty`.
    pub fn is_empty(&self) -> bool {
        self.empty
    }

    /// `AddTimestamp`: a full buffer drops its oldest entry.
    pub fn add(&mut self, timestamp: i64) {
        if self.end == self.start && !self.empty {
            self.start = (self.start + 1) % RATE_BUFFER_SIZE;
        }
        self.timestamps[self.end] = timestamp;
        self.end = (self.end + 1) % RATE_BUFFER_SIZE;
        self.empty = false;
    }

    /// The held timestamps, oldest first (the `do … while (nIndex != nDataEnd)` walk).
    fn held(&self) -> impl Iterator<Item = i64> + '_ {
        let count = self.count();
        (0..count).map(move |i| self.timestamps[(self.start + i) % RATE_BUFFER_SIZE])
    }

    /// `GetMinTimestamp`: `INT64_MAX` when empty.
    pub fn min(&self) -> i64 {
        self.held().fold(i64::MAX, i64::min)
    }

    /// `GetMaxTimestamp`: 0 when empty, and never below 0.
    pub fn max(&self) -> i64 {
        self.held().fold(0, i64::max)
    }

    /// `GetCount`.
    pub fn count(&self) -> usize {
        if self.empty {
            0
        } else if self.end > self.start {
            self.end - self.start
        } else {
            RATE_BUFFER_SIZE - self.start + self.end
        }
    }

    /// `GetRate`: 0 until full; 1e10 when all timestamps are equal.
    pub fn rate(&self) -> f64 {
        let count = self.count();
        if count < RATE_BUFFER_SIZE {
            return 0.0;
        }
        let (min, max) = (self.min(), self.max());
        if min == max {
            return 1.0e10;
        }
        count as f64 / (max.wrapping_sub(min)) as f64
    }
}

/// `last_object_rec`: a masternode's trigger history.
// Core: governance/governance.h:158-172
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastObjectRecord {
    pub buffer: RateCheckBuffer,
    pub status_ok: bool,
}

impl Default for LastObjectRecord {
    fn default() -> Self {
        LastObjectRecord {
            buffer: RateCheckBuffer::default(),
            status_ok: true,
        }
    }
}

/// `MasternodeRateCheck`'s answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateCheck {
    pub ok: bool,
    /// `fRateCheckBypassed`.
    pub bypassed: bool,
}

/// `MasternodeRateCheck(govobj, fUpdateFailStatus, fForce, fRateCheckBypassed)` past its gate
/// (synced, rate checks enabled, a trigger): `creation_time` is the object's, `record` the
/// masternode's entry in `mapLastMasternodeObject`, which a failure with `update_fail_status`
/// marks.
// Core: governance/governance.cpp:732-788
pub fn masternode_rate_check(
    creation_time: i64,
    adjusted_now: i64,
    record: Option<&mut LastObjectRecord>,
    force: bool,
    update_fail_status: bool,
    p: &GovParams,
) -> RateCheck {
    let pass = RateCheck {
        ok: true,
        bypassed: false,
    };
    let fail = RateCheck {
        ok: false,
        bypassed: false,
    };
    let cycle_seconds = i64::from(p.superblock_cycle()) * p.target_spacing();
    if creation_time < adjusted_now - 2 * cycle_seconds {
        return fail;
    }
    if creation_time > adjusted_now + MAX_TIME_FUTURE_DEVIATION {
        return fail;
    }
    let Some(record) = record else {
        return pass;
    };
    if record.status_ok && !force {
        return RateCheck {
            ok: true,
            bypassed: true,
        };
    }
    let max_rate = 2.0 * 1.1 / cycle_seconds as f64;
    let mut buffer = record.buffer.clone();
    buffer.add(creation_time);
    if buffer.rate() < max_rate {
        return pass;
    }
    if update_fail_status {
        record.status_ok = false;
    }
    fail
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::params;
    use dash_params::Network;

    #[test]
    fn buffer_ring() {
        let mut b = RateCheckBuffer::default();
        assert_eq!((b.count(), b.min(), b.max(), b.rate()), (0, i64::MAX, 0, 0.0));
        for t in 1..=4 {
            b.add(t * 10);
        }
        assert_eq!(b.rate(), 0.0);
        b.add(50);
        assert_eq!(b.count(), 5);
        assert_eq!(b.rate(), 5.0 / 40.0);
        b.add(60);
        assert_eq!((b.min(), b.max(), b.start(), b.end()), (20, 60, 1, 1));
        let mut same = RateCheckBuffer::default();
        for _ in 0..5 {
            same.add(7);
        }
        assert_eq!(same.rate(), 1.0e10);
        // Negative timestamps: the maximum starts at 0.
        let mut neg = RateCheckBuffer::default();
        neg.add(-5);
        assert_eq!(neg.max(), 0);
    }

    #[test]
    fn rate_check() {
        let p = params(Network::Main);
        let cycle = i64::from(p.superblock_cycle()) * p.target_spacing();
        let now = 10 * cycle;
        assert!(!masternode_rate_check(now - 2 * cycle - 1, now, None, false, false, &p).ok);
        assert!(
            !masternode_rate_check(
                now + MAX_TIME_FUTURE_DEVIATION + 1,
                now,
                None,
                false,
                false,
                &p
            )
            .ok
        );
        assert!(masternode_rate_check(now, now, None, false, false, &p).ok);
        let mut rec = LastObjectRecord::default();
        assert_eq!(
            masternode_rate_check(now, now, Some(&mut rec), false, false, &p),
            RateCheck {
                ok: true,
                bypassed: true
            }
        );
        for _ in 0..4 {
            rec.buffer.add(now);
        }
        assert!(!masternode_rate_check(now, now, Some(&mut rec), true, true, &p).ok);
        assert!(!rec.status_ok);
    }

    /// The bounds exactly, and the rate threshold `2 * 1.1 / cycle_seconds`
    /// (`governance.cpp:760-783`), with Core's constants written out.
    #[test]
    fn rate_check_bounds() {
        let p = params(Network::Main);
        assert_eq!(MAX_TIME_FUTURE_DEVIATION, 3600);
        // Mainnet: 16616 blocks of 150 s.
        let cycle = 2_492_400;
        assert_eq!(i64::from(p.superblock_cycle()) * p.target_spacing(), cycle);
        let now = 10 * cycle;
        let check = |t, rec: Option<&mut LastObjectRecord>| {
            masternode_rate_check(t, now, rec, true, false, &p).ok
        };
        assert!(check(now - 2 * cycle, None));
        assert!(!check(now - 2 * cycle - 1, None));
        assert!(check(now + 3600, None));
        assert!(!check(now + 3601, None));
        // Five timestamps over `span` seconds: a rate of 5 / span against 2.2 / cycle, so the
        // shortest span that passes is 5664546 s.
        let with_span = |span: i64| {
            let mut rec = LastObjectRecord {
                status_ok: false,
                ..LastObjectRecord::default()
            };
            for i in 0..4 {
                rec.buffer.add(now - span + i);
            }
            check(now, Some(&mut rec))
        };
        assert!(with_span(5_664_546));
        assert!(!with_span(5_664_545));
    }

    #[test]
    fn from_parts_validates() {
        let ts = [1, 2, 3, 4, 5];
        let b = RateCheckBuffer::from_parts(&ts, 4, 0, false).unwrap();
        assert_eq!((b.timestamps(), b.start(), b.end(), b.is_empty()), (&ts, 4, 0, false));
        assert_eq!((b.count(), b.min(), b.max()), (1, 5, 5));
        assert!(RateCheckBuffer::new().is_empty());
        assert_eq!(RateCheckBuffer::from_parts(&[0; 5], 0, 0, true), Some(RateCheckBuffer::new()));
        for (ts, start, end) in [
            (&ts[..4], 0, 0),
            (&[0; 6][..], 0, 0),
            (&ts[..], 5, 0),
            (&ts[..], 0, 5),
            (&ts[..], -1, 0),
            (&ts[..], 0, -1),
        ] {
            assert_eq!(
                RateCheckBuffer::from_parts(ts, start, end, false),
                None,
                "{ts:?} {start} {end}"
            );
        }
        // The round trip through the accessors.
        let mut b = RateCheckBuffer::new();
        for t in 1..=7 {
            b.add(t);
        }
        let back = RateCheckBuffer::from_parts(
            b.timestamps(),
            b.start() as i32,
            b.end() as i32,
            b.is_empty(),
        );
        assert_eq!(back, Some(b));
    }

    #[test]
    fn count_after_the_ring_wraps() {
        let mut b = RateCheckBuffer::default();
        for t in 1..=7 {
            b.add(t);
            assert_eq!(b.count(), (t as usize).min(RATE_BUFFER_SIZE));
        }
        assert_eq!((b.start(), b.end()), (2, 2));
        assert_eq!((b.min(), b.max()), (3, 7));
    }
}
