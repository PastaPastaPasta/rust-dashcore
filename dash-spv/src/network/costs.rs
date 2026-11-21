use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use dashcore::consensus::encode;
use dashcore::hash_types::{ChainLockHash, InstantSendLockHash};
use dashcore::network::message::NetworkMessage;
use dashcore::network::message_blockdata::Inventory;
use dashcore::{BlockHash, Txid};
use dashcore_hashes::sha256d;
use parking_lot::Mutex as RecordMutex;
use tokio::sync::Mutex;

use crate::network::ledger::{ledger_keys, LedgerKey};

const HEADERS_RESP_BYTES: u64 = 2000 * 81;
/// Dash Core sends up to 8000 compressed headers of about 43 bytes each.
const HEADERS2_RESP_BYTES: u64 = 8000 * 43;
const CFHEADERS_RESP_BYTES: u64 = 2000 * 32 + 65;
const RESP_BYTES_ALPHA: f64 = 0.3;
const REPLY_KINDS: usize = 10;
const RESP_BYTES_BOOTSTRAP: [u64; REPLY_KINDS] = [
    HEADERS_RESP_BYTES,
    HEADERS2_RESP_BYTES,
    CFHEADERS_RESP_BYTES,
    256 << 10,
    32 << 10,
    1 << 10,
    1 << 10,
    1 << 10,
    64 << 10,
    1 << 20,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reply {
    Headers,
    Headers2,
    CfHeaders(BlockHash),
    CFilters(BlockHash),
    Block(BlockHash),
    Tx(Txid),
    InstantLock(InstantSendLockHash),
    ChainLock(ChainLockHash),
    MnListDiff(BlockHash),
    QrInfo,
}

impl Reply {
    fn kind(&self) -> usize {
        match self {
            Reply::Headers => 0,
            Reply::Headers2 => 1,
            Reply::CfHeaders(_) => 2,
            Reply::CFilters(_) => 3,
            Reply::Block(_) => 4,
            Reply::Tx(_) => 5,
            Reply::InstantLock(_) => 6,
            Reply::ChainLock(_) => 7,
            Reply::MnListDiff(_) => 8,
            Reply::QrInfo => 9,
        }
    }

    fn awaited(msg: &NetworkMessage) -> Vec<Reply> {
        match msg {
            NetworkMessage::GetHeaders(_) => vec![Reply::Headers],
            NetworkMessage::GetHeaders2(_) => vec![Reply::Headers2],
            NetworkMessage::GetCFHeaders(m) => vec![Reply::CfHeaders(m.stop_hash)],
            NetworkMessage::GetCFilters(m) => vec![Reply::CFilters(m.stop_hash)],
            NetworkMessage::GetData(inv) => inv.iter().filter_map(Reply::of_item).collect(),
            NetworkMessage::GetMnListD(m) => vec![Reply::MnListDiff(m.block_hash)],
            NetworkMessage::GetQRInfo(_) => vec![Reply::QrInfo],
            _ => Vec::new(),
        }
    }

    fn of_item(item: &Inventory) -> Option<Reply> {
        match item {
            Inventory::Block(hash) => Some(Reply::Block(*hash)),
            Inventory::Transaction(txid) => Some(Reply::Tx(*txid)),
            Inventory::InstantSendLock(hash) => Some(Reply::InstantLock(*hash)),
            Inventory::ChainLock(hash) => Some(Reply::ChainLock(*hash)),
            _ => None,
        }
    }

    fn of(msg: &NetworkMessage) -> Option<Reply> {
        Some(match msg {
            NetworkMessage::Headers(_) => Reply::Headers,
            NetworkMessage::Headers2(_) => Reply::Headers2,
            NetworkMessage::CFHeaders(m) => Reply::CfHeaders(m.stop_hash),
            NetworkMessage::Block(block) => Reply::Block(block.block_hash()),
            NetworkMessage::Tx(tx) => Reply::Tx(tx.txid()),
            NetworkMessage::ISLock(lock) => Reply::InstantLock(lock.hash()),
            NetworkMessage::CLSig(lock) => Reply::ChainLock(ChainLockHash::from_raw_hash(
                sha256d::Hash::hash(&encode::serialize(lock)),
            )),
            NetworkMessage::MnListDiff(m) => Reply::MnListDiff(m.block_hash),
            NetworkMessage::QRInfo(_) => Reply::QrInfo,
            _ => return None,
        })
    }
}

/// Returns `msg` as a peer receives it. A peer that serves compressed headers gets a
/// `getheaders` as `getheaders2`.
pub(super) fn as_sent(msg: &NetworkMessage, headers2: bool) -> Cow<'_, NetworkMessage> {
    match msg {
        NetworkMessage::GetHeaders(m) if headers2 => {
            Cow::Owned(NetworkMessage::GetHeaders2(m.clone()))
        }
        _ => Cow::Borrowed(msg),
    }
}

pub struct ResponseCosts {
    expected: [AtomicU64; REPLY_KINDS],
}

impl Default for ResponseCosts {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponseCosts {
    pub fn new() -> Self {
        Self {
            expected: RESP_BYTES_BOOTSTRAP.map(AtomicU64::new),
        }
    }

    /// What the router budgets for `msg`, priced for a peer that serves compressed headers.
    /// The router prices a request before it picks a peer, and nearly every peer does.
    pub fn cost(&self, msg: &NetworkMessage) -> u64 {
        Reply::awaited(&as_sent(msg, true)).iter().map(|reply| self.expected(reply)).sum()
    }

    pub fn expected_kb(&self) -> [u64; REPLY_KINDS] {
        self.expected.each_ref().map(|e| e.load(Ordering::Relaxed) >> 10)
    }

    fn expected(&self, reply: &Reply) -> u64 {
        self.expected[reply.kind()].load(Ordering::Relaxed)
    }

    fn record(&self, reply: &Reply, bytes: u64) {
        if matches!(reply, Reply::Headers | Reply::Headers2 | Reply::CfHeaders(_)) {
            return;
        }
        let expected = &self.expected[reply.kind()];
        let cur = expected.load(Ordering::Relaxed) as f64;
        let next = RESP_BYTES_ALPHA * bytes as f64 + (1.0 - RESP_BYTES_ALPHA) * cur;
        expected.store(next as u64, Ordering::Relaxed);
    }
}

struct Awaited {
    reply: Reply,
    cost: u64,
    sent: Instant,
    bytes: u64,
}

/// Identifies one expected reply on one connection. Never reused on that connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChargeId(u64);

struct Record {
    id: ChargeId,
    tracked: bool,
    awaited: Awaited,
}

#[derive(Default)]
struct Records {
    next: u64,
    list: Vec<Record>,
}

pub struct Ledger {
    costs: Arc<ResponseCosts>,
    awaited: Mutex<Vec<Awaited>>,
    records: RecordMutex<Records>,
    in_flight_bytes: AtomicU64,
    settled: AtomicU64,
    latency_ns: AtomicU64,
    latency_count: AtomicU64,
    latency_max_ns: AtomicU64,
}

impl Ledger {
    pub fn new(costs: Arc<ResponseCosts>) -> Self {
        Self {
            costs,
            awaited: Mutex::new(Vec::new()),
            records: RecordMutex::new(Records::default()),
            in_flight_bytes: AtomicU64::new(0),
            settled: AtomicU64::new(0),
            latency_ns: AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
            latency_max_ns: AtomicU64::new(0),
        }
    }

    pub fn in_flight_bytes(&self) -> u64 {
        self.in_flight_bytes.load(Ordering::Relaxed)
    }

    /// Returns the number of replies settled so far, including those a `notfound` answered.
    pub fn settled(&self) -> u64 {
        self.settled.load(Ordering::Relaxed)
    }

    pub fn latency_stats(&self) -> (u64, f64, f64) {
        let count = self.latency_count.load(Ordering::Relaxed);
        let avg_ms = self.latency_ns.load(Ordering::Relaxed) as f64 / count.max(1) as f64 / 1e6;
        (count, avg_ms, self.latency_max_ns.load(Ordering::Relaxed) as f64 / 1e6)
    }

    pub async fn charge(&self, msg: &NetworkMessage) {
        let mut awaited = self.awaited.lock().await;
        for reply in Reply::awaited(msg) {
            let cost = self.costs.expected(&reply);
            self.in_flight_bytes.fetch_add(cost, Ordering::Relaxed);
            awaited.push(Awaited {
                reply,
                cost,
                sent: Instant::now(),
                bytes: 0,
            });
        }
    }

    pub async fn settle(&self, msg: &NetworkMessage, size: u64) {
        let mut awaited = self.awaited.lock().await;
        let i = match msg {
            NetworkMessage::NotFound(inv) => {
                for reply in inv.iter().filter_map(Reply::of_item) {
                    if let Some(i) = awaited.iter().position(|a| a.reply == reply) {
                        self.release(awaited.remove(i).cost);
                    }
                }
                return;
            }
            NetworkMessage::CFilter(filter) => {
                let Some(i) = awaited.iter().position(|a| matches!(a.reply, Reply::CFilters(_)))
                else {
                    return;
                };
                awaited[i].bytes += size;
                if awaited[i].reply != Reply::CFilters(filter.block_hash) {
                    return;
                }
                i
            }
            msg => {
                let Some(reply) = Reply::of(msg) else {
                    return;
                };
                let Some(i) = awaited.iter().position(|a| a.reply == reply) else {
                    return;
                };
                awaited[i].bytes += size;
                i
            }
        };
        let a = awaited.remove(i);
        self.release(a.cost);
        self.costs.record(&a.reply, a.bytes);
        let ns = a.sent.elapsed().as_nanos() as u64;
        self.latency_ns.fetch_add(ns, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);
        self.latency_max_ns.fetch_max(ns, Ordering::Relaxed);
    }

    /// Charges each reply a tracked request awaits and returns one record per member key.
    /// `msg` is the request as sent, so a `getheaders2` is charged as compressed headers.
    #[cfg_attr(not(test), expect(dead_code, reason = "the router charges tracked sends"))]
    pub fn charge_tracked(&self, msg: &NetworkMessage) -> Vec<(LedgerKey, ChargeId)> {
        let keys = ledger_keys(msg).unwrap_or_default();
        let replies = Reply::awaited(msg);
        keys.into_iter()
            .zip(replies)
            .map(|(key, reply)| (key, self.push_record(reply, true)))
            .collect()
    }

    /// Charges the replies an untracked request awaits.
    #[cfg_attr(not(test), expect(dead_code, reason = "the router charges untracked sends"))]
    pub fn charge_untracked(&self, msg: &NetworkMessage) {
        for reply in Reply::awaited(msg) {
            self.push_record(reply, false);
        }
    }

    fn push_record(&self, reply: Reply, tracked: bool) -> ChargeId {
        let mut records = self.records.lock();
        let id = ChargeId(records.next);
        records.next = records.next.wrapping_add(1);
        let cost = self.costs.expected(&reply);
        self.in_flight_bytes.fetch_add(cost, Ordering::Relaxed);
        records.list.push(Record {
            id,
            tracked,
            awaited: Awaited {
                reply,
                cost,
                sent: Instant::now(),
                bytes: 0,
            },
        });
        id
    }

    /// Adds the bytes of one filter of a stream to its record.
    #[cfg_attr(not(test), expect(dead_code, reason = "the router accrues stream bytes"))]
    pub fn add_bytes(&self, id: ChargeId, size: u64) {
        if let Some(record) = self.records.lock().list.iter_mut().find(|r| r.id == id) {
            record.awaited.bytes += size;
        }
    }

    /// Settles a record by its reply. An unknown id is a no-op.
    #[cfg_attr(not(test), expect(dead_code, reason = "the router settles tracked replies"))]
    pub fn settle_id(&self, id: ChargeId, size: u64) {
        let mut records = self.records.lock();
        let Some(i) = records.list.iter().position(|r| r.id == id) else {
            return;
        };
        let mut record = records.list.remove(i);
        drop(records);
        record.awaited.bytes += size;
        self.finish(record.awaited);
    }

    /// Releases records whose reply will not come. Unknown ids are no-ops.
    #[cfg_attr(not(test), expect(dead_code, reason = "the router releases tracked charges"))]
    pub fn release_ids(&self, ids: &[ChargeId]) {
        let mut records = self.records.lock();
        records.list.retain(|record| {
            if !ids.contains(&record.id) {
                return true;
            }
            self.release(record.awaited.cost);
            false
        });
    }

    /// Settles the oldest untracked record `msg` answers. Tracked records are never touched.
    #[cfg_attr(not(test), expect(dead_code, reason = "the router settles untracked replies"))]
    pub fn settle_untracked(&self, msg: &NetworkMessage, size: u64) {
        let mut records = self.records.lock();
        if let NetworkMessage::NotFound(inv) = msg {
            for reply in inv.iter().filter_map(Reply::of_item) {
                if let Some(i) =
                    records.list.iter().position(|r| !r.tracked && r.awaited.reply == reply)
                {
                    let record = records.list.remove(i);
                    self.release(record.awaited.cost);
                }
            }
            return;
        }
        let Some(reply) = Reply::of(msg) else {
            return;
        };
        let Some(i) = records.list.iter().position(|r| !r.tracked && r.awaited.reply == reply)
        else {
            return;
        };
        let mut record = records.list.remove(i);
        drop(records);
        record.awaited.bytes += size;
        self.finish(record.awaited);
    }

    fn finish(&self, a: Awaited) {
        self.release(a.cost);
        self.costs.record(&a.reply, a.bytes);
        let ns = a.sent.elapsed().as_nanos() as u64;
        self.latency_ns.fetch_add(ns, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);
        self.latency_max_ns.fetch_max(ns, Ordering::Relaxed);
    }

    fn release(&self, cost: u64) {
        self.settled.fetch_add(1, Ordering::Relaxed);
        let _ = self
            .in_flight_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_sub(cost)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashcore::network::message_blockdata::GetHeadersMessage;
    use dashcore::network::message_headers2::Headers2Message;
    use test_case::test_case;

    fn locator() -> GetHeadersMessage {
        GetHeadersMessage::new(vec![BlockHash::dummy(1)], BlockHash::dummy(0))
    }

    fn headers2() -> NetworkMessage {
        NetworkMessage::Headers2(Headers2Message {
            headers: Vec::new(),
        })
    }

    /// The router costs a request before it picks the peer that may upgrade it.
    #[test_case(NetworkMessage::GetHeaders(locator()); "plain headers")]
    #[test_case(NetworkMessage::GetHeaders2(locator()); "compressed headers")]
    fn a_header_request_is_routed_at_the_compressed_reply_cost(request: NetworkMessage) {
        assert_eq!(ResponseCosts::new().cost(&request), 8000 * 43);
    }

    #[test_case(NetworkMessage::GetHeaders(locator()), true => NetworkMessage::GetHeaders2(locator()); "upgraded for a headers2 peer")]
    #[test_case(NetworkMessage::GetHeaders(locator()), false => NetworkMessage::GetHeaders(locator()); "kept for a plain peer")]
    #[test_case(NetworkMessage::GetHeaders2(locator()), false => NetworkMessage::GetHeaders2(locator()); "a getheaders2 is kept")]
    fn a_peer_receives_getheaders_as_it_serves_headers(
        request: NetworkMessage,
        headers2: bool,
    ) -> NetworkMessage {
        as_sent(&request, headers2).into_owned()
    }

    /// Charges `request` on a peer's ledger and settles it with `reply`.
    #[test_case(NetworkMessage::GetHeaders2(locator()), headers2(), 8000 * 43, 0; "a headers2 reply settles a getheaders2")]
    #[test_case(NetworkMessage::GetHeaders2(locator()), NetworkMessage::Headers(Vec::new()), 8000 * 43, 8000 * 43; "a plain headers announcement does not")]
    #[test_case(NetworkMessage::GetHeaders(locator()), NetworkMessage::Headers(Vec::new()), 2000 * 81, 0; "a peer without headers2 owes a plain reply")]
    #[tokio::test]
    async fn a_header_request_is_charged_and_settled_by_its_reply(
        request: NetworkMessage,
        reply: NetworkMessage,
        charged: u64,
        in_flight: u64,
    ) {
        let ledger = Ledger::new(Arc::new(ResponseCosts::new()));
        ledger.charge(&request).await;
        assert_eq!(ledger.in_flight_bytes(), charged);

        ledger.settle(&reply, charged).await;
        assert_eq!(ledger.in_flight_bytes(), in_flight);
    }
}
