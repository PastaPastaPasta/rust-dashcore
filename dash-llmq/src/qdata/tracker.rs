//! Core's quorum data request map (`mapQuorumDataRequests`) with its inbound caps: the requests
//! we sent and the `qgetdata` requests peers sent us, each kept until it expires.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use dash_consensus::primitives::BlockHash;
use dash_evo::types::ProTxHash;
use dash_params::LlmqType;
use dash_types::Numeric;

/// Seconds after which a request expires.
// Core: v24.0.0 src/llmq/quorums.h:104 CQuorumDataRequest::EXPIRATION_TIMEOUT
pub const EXPIRATION_TIMEOUT: i64 = 300;
/// Seconds added to [`EXPIRATION_TIMEOUT`] when the bias applies.
// Core: v24.0.0 src/llmq/quorums.h:105 CQuorumDataRequest::EXPIRATION_BIAS
pub const EXPIRATION_BIAS: i64 = 60;
/// Inbound requests tracked at once for one requester identity.
// Core: v24.0.0 src/llmq/quorumsman.h:52 MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER
pub const MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER: usize = 64;
/// Inbound requests tracked at once in total.
// Core: v24.0.0 src/llmq/quorumsman.h:53 MAX_INBOUND_DATA_REQUESTS
pub const MAX_INBOUND_DATA_REQUESTS: usize = 4096;

/// A tracked request's identity: the requester (or, for ours, the member asked), the direction,
/// and the quorum.
// Core: v24.0.0 src/llmq/quorums.h:52-70 CQuorumDataRequestKey
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub pro_reg_tx: ProTxHash,
    pub we_requested: bool,
    pub quorum_hash: BlockHash,
    pub llmq_type: LlmqType,
}

impl Key {
    fn order(&self) -> (&ProTxHash, bool, &BlockHash, u8) {
        (&self.pro_reg_tx, self.we_requested, &self.quorum_hash, self.llmq_type.to_base())
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        self.order().cmp(&other.order())
    }
}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A `qgetdata` request or a `qdata` response header.
///
/// Equality compares the type, quorum hash, data mask and pro_tx hash; the error code is not
/// part of it.
// Core: v24.0.0 src/llmq/quorums.h:75-158 CQuorumDataRequest
#[derive(Clone, Copy, Debug, Eq)]
pub struct Request {
    pub llmq_type: LlmqType,
    pub quorum_hash: BlockHash,
    pub data_mask: u16,
    pub pro_tx: ProTxHash,
    pub error: u8,
}

impl Request {
    pub const QUORUM_VERIFICATION_VECTOR: u16 = 0x0001;
    pub const ENCRYPTED_CONTRIBUTIONS: u16 = 0x0002;

    pub const ERROR_NONE: u8 = 0x00;
    pub const ERROR_QUORUM_TYPE_INVALID: u8 = 0x01;
    pub const ERROR_QUORUM_BLOCK_NOT_FOUND: u8 = 0x02;
    pub const ERROR_QUORUM_NOT_FOUND: u8 = 0x03;
    pub const ERROR_MASTERNODE_IS_NO_MEMBER: u8 = 0x04;
    pub const ERROR_QUORUM_VERIFICATION_VECTOR_MISSING: u8 = 0x05;
    pub const ERROR_ENCRYPTED_CONTRIBUTIONS_MISSING: u8 = 0x06;
    pub const ERROR_UNDEFINED: u8 = 0xFF;
}

// Core: v24.0.0 src/llmq/quorums.h:147-153 CQuorumDataRequest::operator==
impl PartialEq for Request {
    fn eq(&self, other: &Self) -> bool {
        self.llmq_type == other.llmq_type
            && self.quorum_hash == other.quorum_hash
            && self.data_mask == other.data_mask
            && self.pro_tx == other.pro_tx
    }
}

/// A tracked request with the time it was registered and whether its response arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub req: Request,
    pub time: i64,
    pub processed: bool,
}

impl Entry {
    // Core: v24.0.0 src/llmq/quorums.h:143 CQuorumDataRequest::IsExpired
    pub fn is_expired(&self, now: i64, add_bias: bool) -> bool {
        let bias = if add_bias {
            EXPIRATION_BIAS
        } else {
            0
        };
        now - self.time >= EXPIRATION_TIMEOUT + bias
    }
}

/// [`Tracker::register`]'s outcome.
// Core: v24.0.0 src/llmq/quorumsman.h:55-60 DataRequestRegistration
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registration {
    Accepted,
    RateLimited,
    RequesterLimitExceeded,
    CapacityExhausted,
}

/// [`Tracker::validate_response`]'s outcome.
// Core: v24.0.0 src/llmq/quorumsman.h:168 CQuorumManager::DataResponseValidation
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseCheck {
    Ok,
    NotRequested,
    AlreadyReceived,
    Mismatch,
}

/// [`Tracker::status`]'s outcome. `Requested` is never returned, as in Core.
// Core: v24.0.0 src/llmq/quorums.h:36-41 DataRequestStatus
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    NotFound,
    Requested,
    Pending,
    Processed,
}

/// The data request map and its inbound counters.
#[derive(Clone, Debug, Default)]
pub struct Tracker {
    requests: BTreeMap<Key, Entry>,
    inbound_counts: BTreeMap<ProTxHash, usize>,
    inbound_count: usize,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `req` under `key` at `now`. An existing unexpired entry rate-limits it and an
    /// expired one is replaced, before any cap applies; only inbound keys count against the caps.
    // Core: v24.0.0 src/llmq/quorumsman.cpp:567-598 CQuorumManager::RegisterDataRequest
    pub fn register(&mut self, key: Key, req: Request, now: i64, add_bias: bool) -> Registration {
        if let Some(entry) = self.requests.get_mut(&key) {
            if !entry.is_expired(now, add_bias) {
                return Registration::RateLimited;
            }
            *entry = Entry {
                req,
                time: now,
                processed: false,
            };
            return Registration::Accepted;
        }

        if !key.we_requested {
            if self.inbound_count >= MAX_INBOUND_DATA_REQUESTS {
                return Registration::CapacityExhausted;
            }
            if self.inbound_counts.get(&key.pro_reg_tx).copied().unwrap_or(0)
                >= MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER
            {
                // Every unauthenticated peer shares the null identity, so its exhaustion is not
                // the last arrival's fault.
                return if key.pro_reg_tx == ProTxHash::from([0; 32]) {
                    Registration::CapacityExhausted
                } else {
                    Registration::RequesterLimitExceeded
                };
            }
        }

        self.requests.insert(
            key,
            Entry {
                req,
                time: now,
                processed: false,
            },
        );
        if !key.we_requested {
            *self.inbound_counts.entry(key.pro_reg_tx).or_insert(0) += 1;
            self.inbound_count += 1;
        }
        Registration::Accepted
    }

    /// Checks a `qdata` header against the request registered under `key` and marks it
    /// processed on a match. It never expires an entry.
    // Core: v24.0.0 src/llmq/quorumsman.cpp:600-616 CQuorumManager::ValidateDataResponse
    pub fn validate_response(&mut self, key: &Key, response: &Request) -> ResponseCheck {
        let Some(entry) = self.requests.get_mut(key) else {
            return ResponseCheck::NotRequested;
        };
        if entry.processed {
            return ResponseCheck::AlreadyReceived;
        }
        if *response != entry.req {
            return ResponseCheck::Mismatch;
        }
        entry.processed = true;
        ResponseCheck::Ok
    }

    /// Whether `key` has an entry that has not expired, with the bias.
    // Core: v24.0.0 src/llmq/quorumsman.cpp:436-443 CQuorumManager::IsDataRequestPending
    pub fn is_pending(&self, key: &Key, now: i64) -> bool {
        self.requests.get(key).is_some_and(|e| !e.is_expired(now, true))
    }

    /// The entry's state, expired or not.
    // Core: v24.0.0 src/llmq/quorumsman.cpp:445-458 CQuorumManager::GetDataRequestStatus
    pub fn status(&self, key: &Key) -> Status {
        match self.requests.get(key) {
            None => Status::NotFound,
            Some(e) if e.processed => Status::Processed,
            Some(_) => Status::Pending,
        }
    }

    /// Drops every entry expired at `now`, with the bias, and releases its inbound counts.
    // Core: v24.0.0 src/llmq/quorumsman.cpp:460-483 CQuorumManager::CleanupExpiredDataRequests
    pub fn cleanup(&mut self, now: i64) {
        let Self {
            requests,
            inbound_counts,
            inbound_count,
        } = self;
        requests.retain(|key, entry| {
            if !entry.is_expired(now, true) {
                return true;
            }
            if !key.we_requested {
                // On a counter desync the decrement is skipped (Core's `Assume`): over-counting
                // fails toward stricter limits, an underflow would disable the caps.
                if let Some(count) = inbound_counts.get_mut(&key.pro_reg_tx).filter(|c| **c > 0) {
                    *count -= 1;
                    if *count == 0 {
                        inbound_counts.remove(&key.pro_reg_tx);
                    }
                }
                if *inbound_count > 0 {
                    *inbound_count -= 1;
                }
            }
            false
        });
    }

    /// The entry under `key`.
    pub fn entry(&self, key: &Key) -> Option<&Entry> {
        self.requests.get(key)
    }

    /// The number of inbound entries counted in total.
    pub fn inbound_count(&self) -> usize {
        self.inbound_count
    }

    /// The number of inbound entries counted for `requester`.
    pub fn inbound_count_for(&self, requester: &ProTxHash) -> usize {
        self.inbound_counts.get(requester).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 100_000;

    fn id(n: u16) -> ProTxHash {
        let mut b = [0; 32];
        b[..2].copy_from_slice(&n.to_be_bytes());
        ProTxHash::from(b)
    }

    fn hash(n: u16) -> BlockHash {
        let mut b = [0; 32];
        b[..2].copy_from_slice(&n.to_be_bytes());
        BlockHash::from(b)
    }

    fn null() -> ProTxHash {
        ProTxHash::from([0; 32])
    }

    fn key(requester: ProTxHash, we_requested: bool, q: u16) -> Key {
        Key {
            pro_reg_tx: requester,
            we_requested,
            quorum_hash: hash(q),
            llmq_type: LlmqType::LlmqTest,
        }
    }

    fn req(q: u16, mask: u16) -> Request {
        Request {
            llmq_type: LlmqType::LlmqTest,
            quorum_hash: hash(q),
            data_mask: mask,
            pro_tx: null(),
            error: Request::ERROR_UNDEFINED,
        }
    }

    fn vvec(q: u16) -> Request {
        req(q, Request::QUORUM_VERIFICATION_VECTOR)
    }

    fn inbound(t: &mut Tracker, requester: ProTxHash, q: u16, now: i64) -> Registration {
        t.register(key(requester, false, q), vvec(q), now, false)
    }

    #[test]
    fn expiry_boundaries_both_biases() {
        let e = Entry {
            req: vvec(1),
            time: T0,
            processed: false,
        };
        for (age, unbiased, biased) in
            [(299, false, false), (300, true, false), (359, true, false), (360, true, true)]
        {
            assert_eq!(e.is_expired(T0 + age, false), unbiased, "age {age} unbiased");
            assert_eq!(e.is_expired(T0 + age, true), biased, "age {age} biased");
        }

        for (bias, fresh, stale) in [(false, 299, 300), (true, 359, 360)] {
            let mut t = Tracker::new();
            let k = key(id(1), true, 1);
            assert_eq!(t.register(k, vvec(1), T0, bias), Registration::Accepted);
            assert_eq!(t.register(k, vvec(1), T0 + fresh, bias), Registration::RateLimited);
            assert_eq!(t.register(k, vvec(1), T0 + stale, bias), Registration::Accepted);
            assert_eq!(t.entry(&k).unwrap().time, T0 + stale);
        }

        let mut t = Tracker::new();
        let k = key(id(1), false, 1);
        inbound(&mut t, id(1), 1, T0);
        assert!(t.is_pending(&k, T0 + 359));
        assert!(!t.is_pending(&k, T0 + 360));
        t.cleanup(T0 + 359);
        assert_eq!(t.status(&k), Status::Pending);
        t.cleanup(T0 + 360);
        assert_eq!(t.status(&k), Status::NotFound);
    }

    #[test]
    fn expired_entry_is_replaced_and_counts_kept() {
        let mut t = Tracker::new();
        let k = key(id(1), false, 1);
        assert_eq!(inbound(&mut t, id(1), 1, T0), Registration::Accepted);
        assert_eq!(t.validate_response(&k, &vvec(1)), ResponseCheck::Ok);
        let replacement = req(1, Request::ENCRYPTED_CONTRIBUTIONS);
        assert_eq!(t.register(k, replacement, T0 + 300, false), Registration::Accepted);
        let e = t.entry(&k).unwrap();
        assert_eq!(
            (e.req.data_mask, e.time, e.processed),
            (Request::ENCRYPTED_CONTRIBUTIONS, T0 + 300, false)
        );
        assert_eq!(t.inbound_count(), 1);
        assert_eq!(t.inbound_count_for(&id(1)), 1);
    }

    #[test]
    fn existing_key_is_handled_before_the_caps() {
        let mut t = Tracker::new();
        for q in 0..MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER as u16 {
            assert_eq!(inbound(&mut t, id(1), q, T0), Registration::Accepted);
        }
        assert_eq!(inbound(&mut t, id(1), 1000, T0), Registration::RequesterLimitExceeded);
        assert_eq!(inbound(&mut t, id(1), 0, T0 + 299), Registration::RateLimited);
        assert_eq!(inbound(&mut t, id(1), 0, T0 + 300), Registration::Accepted);
        assert_eq!(t.inbound_count_for(&id(1)), MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER);

        let mut t = Tracker::new();
        for i in 0..MAX_INBOUND_DATA_REQUESTS {
            let who = id((i / MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER) as u16 + 1);
            assert_eq!(inbound(&mut t, who, i as u16, T0), Registration::Accepted);
        }
        assert_eq!(inbound(&mut t, id(1), 0, T0 + 1), Registration::RateLimited);
        assert_eq!(inbound(&mut t, id(1), 0, T0 + 300), Registration::Accepted);
        assert_eq!(t.inbound_count(), MAX_INBOUND_DATA_REQUESTS);
    }

    #[test]
    fn null_identity_overflow_is_capacity_exhausted() {
        let mut t = Tracker::new();
        for q in 0..MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER as u16 {
            assert_eq!(inbound(&mut t, null(), q, T0), Registration::Accepted);
        }
        assert_eq!(inbound(&mut t, null(), 1000, T0), Registration::CapacityExhausted);
        assert_eq!(t.status(&key(null(), false, 1000)), Status::NotFound);
    }

    #[test]
    fn requester_overflow_is_requester_limit_exceeded() {
        let mut t = Tracker::new();
        for q in 0..MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER as u16 {
            assert_eq!(inbound(&mut t, id(0x77), q, T0), Registration::Accepted);
        }
        assert_eq!(inbound(&mut t, id(0x77), 1000, T0), Registration::RequesterLimitExceeded);
        assert!(!t.is_pending(&key(id(0x77), false, 1000), T0));
        assert_eq!(inbound(&mut t, id(0x78), 1000, T0), Registration::Accepted);
    }

    // Core: v24.0.0 src/test/llmq_qgetdata_tests.cpp:213-238
    #[test]
    fn global_capacity_bounds_inbound_only() {
        let mut t = Tracker::new();
        for i in 0..MAX_INBOUND_DATA_REQUESTS {
            let who = id((i / MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER) as u16 + 1);
            assert_eq!(inbound(&mut t, who, i as u16, T0), Registration::Accepted);
        }
        assert_eq!(t.inbound_count(), MAX_INBOUND_DATA_REQUESTS);

        let fresh = id(0xbeef);
        assert_eq!(inbound(&mut t, fresh, 0xfeed, T0), Registration::CapacityExhausted);
        // A requester already at its own cap still gets the global answer first.
        assert_eq!(t.inbound_count_for(&id(1)), MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER);
        assert_eq!(inbound(&mut t, id(1), 0xfeee, T0), Registration::CapacityExhausted);
        assert!(!t.is_pending(&key(fresh, false, 0xfeed), T0));
        assert_eq!(
            t.register(key(fresh, true, 0xfeed), vvec(0xfeed), T0, false),
            Registration::Accepted
        );
        assert_eq!(t.inbound_count(), MAX_INBOUND_DATA_REQUESTS);
        assert_eq!(t.inbound_count_for(&fresh), 0);

        t.cleanup(T0 + 360);
        assert_eq!(t.inbound_count(), 0);
        assert_eq!(inbound(&mut t, fresh, 0xfeed, T0 + 360), Registration::Accepted);
    }

    #[test]
    fn cleanup_decrements_counts() {
        let mut t = Tracker::new();
        inbound(&mut t, id(1), 1, T0);
        inbound(&mut t, id(1), 2, T0 + 10);
        inbound(&mut t, id(2), 1, T0);
        t.register(key(id(3), true, 1), vvec(1), T0, true);
        assert_eq!(
            (t.inbound_count(), t.inbound_count_for(&id(1)), t.inbound_count_for(&id(2))),
            (3, 2, 1)
        );

        t.cleanup(T0 + 360);
        assert_eq!(
            (t.inbound_count(), t.inbound_count_for(&id(1)), t.inbound_count_for(&id(2))),
            (1, 1, 0)
        );
        assert!(!t.inbound_counts.contains_key(&id(2)));
        assert_eq!(t.status(&key(id(3), true, 1)), Status::NotFound);
        assert_eq!(t.status(&key(id(1), false, 2)), Status::Pending);

        t.cleanup(T0 + 370);
        assert_eq!(t.inbound_count(), 0);
        assert!(t.inbound_counts.is_empty());
        assert!(t.requests.is_empty());
    }

    #[test]
    fn cleanup_skips_a_desynced_decrement() {
        let mut t = Tracker::new();
        inbound(&mut t, id(1), 1, T0);
        inbound(&mut t, id(2), 1, T0 + 100);
        t.inbound_counts.remove(&id(1));
        t.inbound_count = 0;
        t.cleanup(T0 + 360);
        assert_eq!((t.inbound_count(), t.inbound_count_for(&id(2))), (0, 1));
        assert_eq!(t.status(&key(id(1), false, 1)), Status::NotFound);
    }

    #[test]
    fn validate_response_outcomes() {
        let mut t = Tracker::new();
        let k = key(id(1), true, 1);
        assert_eq!(t.validate_response(&k, &vvec(1)), ResponseCheck::NotRequested);

        t.register(k, vvec(1), T0, true);
        assert_eq!(
            t.validate_response(&k, &req(1, Request::ENCRYPTED_CONTRIBUTIONS)),
            ResponseCheck::Mismatch
        );
        assert_eq!(t.status(&k), Status::Pending);

        let answered = Request {
            error: Request::ERROR_NONE,
            ..vvec(1)
        };
        assert_eq!(t.validate_response(&k, &answered), ResponseCheck::Ok);
        assert_eq!(t.status(&k), Status::Processed);
        assert_eq!(t.validate_response(&k, &answered), ResponseCheck::AlreadyReceived);
        // Processed is checked before equality.
        assert_eq!(
            t.validate_response(&k, &req(1, Request::ENCRYPTED_CONTRIBUTIONS)),
            ResponseCheck::AlreadyReceived
        );
    }

    #[test]
    fn validate_and_status_do_not_expire() {
        let mut t = Tracker::new();
        let k = key(id(1), true, 1);
        t.register(k, vvec(1), T0, true);
        assert!(!t.is_pending(&k, T0 + 1000));
        assert_eq!(t.status(&k), Status::Pending);
        assert_eq!(t.validate_response(&k, &vvec(1)), ResponseCheck::Ok);
        assert_eq!(t.status(&k), Status::Processed);
    }
}
