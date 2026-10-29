//! The engine (SB0 §5.3): lists and quorum sets authenticated by §5.2's steps 1–4, each used
//! commitment verified by step 5, and the scans the lock checks select from (step 6).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::ops::RangeInclusive;
use std::sync::Arc;

use dash_chain::{AncestryError, BlockRef, StoreError};
use dash_consensus::BlockId;
use dash_consensus::ctx::{BlsCtx, bls_ctx_for_block};
use dash_consensus::primitives::Tx;
use dash_evo::cbtx::{
    CBTX_VERSION_MERKLE_ROOT_QUORUMS, CoinbaseFacts, check_mn_list_root, check_quorums_root,
    verify_cbtx_proof,
};
use dash_evo::effect::Effect;
use dash_evo::llmq::{
    CommitmentError, LlmqCtx, MemberError, QuorumSnapshot, SIGN_HEIGHT_OFFSET, SigPath,
    UncachedMembers, VerifiedQuorum, WORK_DIFF_DEPTH, cycle_base_height,
    is_quorum_rotation_enabled, members_uncached, scan_store_block, verify_quorum,
};
use dash_evo::locks::{
    IslockSchemes, LockError, LockReject, LockSchemes, QuorumScan, QuorumScanView, QuorumStatus,
    ScanLookup, ScannedQuorum, VerifiedChainLock, VerifiedIsLock, verify_chainlock, verify_islock,
};
use dash_evo::sml::{SmlList, merkle_root_quorums_from_hashes};
use dash_evo::types::ProTxHash;
use dash_p2p_core::{ClSig, IsdLock, MnListDiff, QrInfo};
use dash_params::{BuriedDeployment, ChainParams, LlmqParams};
use dash_primitives::{
    BlockHash, CoinbaseCommitment, Commitment, LlmqType, QuorumHash, SpecialPayload, TxType,
};
use dash_types::Numeric;

use crate::chain::MnChain;
use crate::config::{LlmqConfig, interval};
use crate::convert::{commitment_hash, proof_tree, sml_entry, snapshot};
use crate::error::{FetchLookupError, MnReject, MnViewError};
use crate::view::{ChainView, Heights, HeldRow, RowKey, Rows, Source, ancestor_at};

mod store;

pub use store::{MnCfs, MnWrites};

/// Who runs the engine (SB0 §5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// dash-spv.
    Client,
    /// The node's mnxcheck task (SB19).
    Shadow,
}

/// A request's identity: its target (requests are deduplicated by it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RequestKey {
    /// A `getmnlistd` to this block.
    MnListDiff(BlockHash),
    /// A `getqrinfo` for the cycle starting at this block.
    QrInfo(BlockHash),
}

/// A payload the engine needs. The host sends it and feeds the reply to
/// [`MasternodeListEngine::apply_diff`] or [`MasternodeListEngine::apply_qrinfo`], or reports
/// [`MasternodeListEngine::request_failed`] (D1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MnRequest {
    /// `getmnlistd(base, target)`.
    MnListDiff {
        /// The newest held list below the target on its chain; `None` asks for the whole list
        /// (a null base).
        base: Option<BlockHash>,
        /// The block whose list is needed.
        target: BlockHash,
        /// Its height.
        target_height: u32,
    },
    /// `getqrinfo(bases, request, extraShare = false)` for a rotated commitment's cycle: the reply
    /// carries the work-block lists of the cycle and the three before it, and those three's rows
    /// (EV addendum §2.3 rev6, O4).
    QrInfo {
        /// The cycle's first block.
        request: BlockHash,
        /// Its height.
        request_height: u32,
        /// Held lists on the request's chain Core may diff from, oldest first.
        bases: Vec<BlockHash>,
    },
}

impl MnRequest {
    /// The request's identity.
    pub fn key(&self) -> RequestKey {
        match self {
            MnRequest::MnListDiff {
                target,
                ..
            } => RequestKey::MnListDiff(*target),
            MnRequest::QrInfo {
                request,
                ..
            } => RequestKey::QrInfo(*request),
        }
    }

    /// The block the request names, and its height.
    fn block(&self) -> (BlockHash, u32) {
        match self {
            MnRequest::MnListDiff {
                target,
                target_height,
                ..
            } => (*target, *target_height),
            MnRequest::QrInfo {
                request,
                request_height,
                ..
            } => (*request, *request_height),
        }
    }
}

/// How many held lists a `getqrinfo` offers as bases. Core accepts up to `MAX_BASE_BLOCK_HASHES`
/// (4096, `llmq/snapshot.h` rc.3:100) and diffs each list from the newest at or below it; the
/// newest few cover them (D14).
const QRINFO_BASES: usize = 4;

/// How many distinct rows the engine keeps for a cycle while none is confirmed (D20).
pub(crate) const ROW_CANDIDATES: usize = 3;

/// How many times a cycle's `qrinfo` is asked again after step 5 failed over unconfirmed rows
/// (D20).
pub(crate) const ROW_RETRIES: u8 = 2;

/// Why a commitment cannot be verified (D7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// An input lies below the anchored chain's floor: never fetched (SB0 §5.2).
    BelowFloor {
        /// The height read.
        height: u32,
    },
    /// The request for an input failed ([`MasternodeListEngine::request_failed`]).
    FetchFailed,
    /// The request was answered and the input is still not held.
    NotServed,
    /// The quorum's base block is not an accepted header (it lies below the floor).
    UnknownBase,
    /// Step 5 failed under every choice of the rows peers sent for its cycles, none of them
    /// confirmed, and the retries are spent (D20). Not `Unverified`, which rests on Core's rows
    /// only; a new row reopens it.
    RowsDisputed,
    /// No authenticated set says whether the block that mined it is below V19: V19 activates
    /// below A_q (D12).
    MiningScheme,
    /// An input is a block off the active chain, which Core serves no payload for: nothing is
    /// asked, and step 5 runs again on each applied payload (D12).
    OffChain,
}

/// A held commitment's status (SB0 §5.1, §5.2 steps 4–5; D7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuorumState {
    /// Step 5 passed: its key verifies locks.
    Verified(VerifiedQuorum),
    /// Step 5 ran and rejected it, or its signature check is outside BS12's domain.
    Unverified,
    /// An input is requested: not verified until it arrives (EV addendum §2.3).
    Pending(RequestKey),
    /// An input cannot be had.
    Unavailable(Unavailable),
    /// A type SPV does not use: authenticated by step 4 only (SB0 §5.2 step 5).
    Skipped,
    /// No held set containing it is authenticated: each lies below A_q, where the CbTx commits
    /// to no quorum set (SB0 §5.2.1).
    Unauthenticated,
}

impl QuorumState {
    /// The status a lock check reads.
    fn scan_status(&self) -> QuorumStatus {
        match self {
            QuorumState::Verified(q) => QuorumStatus::Verified(q.clone()),
            QuorumState::Unverified => QuorumStatus::Unverified,
            _ => QuorumStatus::Unavailable,
        }
    }
}

/// Platform's answer for a quorum at a core height (SB0 §5.3 Platform's lookup).
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "returned and matched at once: a box would allocate per lookup"
)]
pub enum Lookup {
    /// `Verified`, mined at or before h and not replaced at or before h − W_P: its key.
    Found(VerifiedQuorum),
    /// Not decided yet: step 5 waits on [`MasternodeListEngine::requests`], or, for the served
    /// type, [`MasternodeListEngine::fetch_lookup`] asks for its targets (D37).
    Fetch,
    /// Decided: no commitment of that type and hash is active at h by the predicate, in what is
    /// held and authenticated, or step 5 rejected it (`Unverified`). For the served type, both
    /// ends of the window are held authenticated; other types keep DEC-116's not found after
    /// pruning (D50). Never another quorum's key.
    NotFound,
    /// Not decided and nothing asked decides it (DEC-444, D50): h is above the header tip, a
    /// commitment active at h is `Unavailable` or `Skipped`, the marks are not read on the
    /// active chain, or, for the served type, an end of the window lies below the anchor or A_q,
    /// or its fetch failed or left it undecided. Not "absent": for the served type
    /// [`MasternodeListEngine::fetch_lookup`] asks again.
    Unavailable,
}

/// A list held whole, as the bridge and the shadow read it (D39): its block, its entries, and its
/// quorum set's commitments with their states (`None` before step 5 first runs), by type and
/// quorum hash.
#[derive(Clone, Debug, PartialEq)]
pub struct ListView {
    pub height: u32,
    pub block_hash: BlockHash,
    pub sml: Arc<SmlList>,
    pub quorums: Vec<(Arc<Commitment>, Option<QuorumState>)>,
}

/// What the engine published for readers off its task (SB0 §5.3 `snapshot()`, D39): the tip and
/// list its last retention left, one consistent state, never a half-applied payload; and the
/// best ChainLock as step 6 verified it, which may be newer than that retention.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MnSnapshot {
    /// The active tip the retention ran on.
    pub tip: Option<BlockRef>,
    /// The newest list held whole on the active chain there: the tip's, once its diff is
    /// applied (`latest_masternode_list()`, SB0 §5.3).
    pub list: Option<ListView>,
    /// The newest ChainLock step 6 verified: its height and block. Published when verified, so it
    /// may be above `tip`, on a header the chain has yet to accept.
    pub best_chainlock: Option<(u32, BlockHash)>,
}

/// A served type's lookup: its type, quorum hash and core height (D37).
type LookupKey = (u8, [u8; 32], u32);

/// The served type's lookup of `quorum_hash` at `h`, as kept (D37).
fn lookup_key(llmq_type: LlmqType, quorum_hash: &QuorumHash, h: u32) -> LookupKey {
    (llmq_type.to_base(), *quorum_hash.as_bytes(), h)
}

/// How many fetched lookups' answers are kept (D37).
const LOOKUP_CACHE: usize = 64;

/// How many served lookups may wait on their fetch by default (D44): each asks for at most two
/// diffs, so 5 take half of dash-spv's 20 concurrent `mnlistdiff` requests
/// (`MAX_CONCURRENT_MNLISTDIFF`), leaving the rest to the engine's own, and pin at most 10 whole
/// lists, about 8.5 MB on mainnet (D33).
pub const DEFAULT_LOOKUP_WAITS: usize = 5;

/// A served type's lookup waiting on its fetch (D37): the active chain's blocks it reads and the
/// request asked for each not held. A target waits while its request is out, whoever asked it
/// again (Opus s6 r1 m3: a mark per failed target left the wait while another lookup's request
/// for it was out), and the requests asked again for its holders' failed step-5 inputs (D51).
#[derive(Clone, Debug)]
struct LookupWait {
    llmq_type: LlmqType,
    quorum_hash: QuorumHash,
    h: u32,
    targets: Vec<(u32, BlockId)>,
    asked: Vec<(BlockId, RequestKey)>,
    reasked: Vec<RequestKey>,
}

impl LookupWait {
    fn key(&self) -> LookupKey {
        lookup_key(self.llmq_type, &self.quorum_hash, self.h)
    }

    /// Whether the request for the target `id` is still out.
    fn outstanding(&self, id: BlockId, requests: &BTreeMap<RequestKey, MnRequest>) -> bool {
        self.asked.iter().any(|(asked, key)| *asked == id && requests.contains_key(key))
    }

    /// The requests it asked, for its targets or again for its holders.
    fn keys(&self) -> impl Iterator<Item = RequestKey> + '_ {
        self.asked.iter().map(|(_, key)| *key).chain(self.reasked.iter().copied())
    }
}

/// A fetched lookup's answer, on the chain through its targets (D37): the key, or none. One not
/// `decided` (a fetch failed, or step 5 left the commitment undecided) reads
/// [`Lookup::Unavailable`] (DEC-444) until
/// [`MasternodeListEngine::fetch_lookup`] asks again (validator D37 r1 M1, r2 m2).
#[derive(Clone, Debug)]
struct LookedUp {
    key: LookupKey,
    targets: Vec<(u32, BlockId)>,
    answer: Option<VerifiedQuorum>,
    decided: bool,
}

/// What a waiting lookup's targets say once a call has run step 5 on what they hold (D37).
enum Resolved {
    Wait,
    Drop,
    Answer(Option<Box<VerifiedQuorum>>, bool),
}

/// Why a lock check gave no verified lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockOutcome {
    /// SB8's verdict: Core's rejection, or none for want of a verified quorum
    /// ([`LockReject::QuorumUnavailable`], [`LockReject::OutsideHorizon`]).
    Rejected(LockReject),
    /// The signature check is outside BS12's BLS domain.
    BlsUnsupported,
    /// The lock's LLMQ type is not configured.
    UnknownType(u8),
    /// The ChainLock's selection start block is above the header tip: held until headers reach
    /// it ([`MasternodeListEngine::take_pending_chainlock`], SB0 §5.2 step 7).
    Pending,
    /// A store read failed; its latch has tripped. Stop.
    Stopped(StoreError),
}

/// What a payload added.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MnChange {
    /// The blocks whose lists are now held, in application order; none for a diff to a list held
    /// already, which is applied all the same (D34).
    pub targets: Vec<BlockId>,
    /// The commitments first held: (type, quorum hash).
    pub new_quorums: Vec<(LlmqType, QuorumHash)>,
    /// What the host should surface, in order (D26).
    pub events: Vec<MnEvent>,
}

/// An operator-visible diagnostic (SB0 §4.3.1 step 4, §10.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MnEvent {
    /// A later coverage miss (D26): a used commitment in an authenticated set within the
    /// horizon needs history below the floor. It is `Unavailable` (`BelowFloor` or
    /// `UnknownBase`) and its locks report unavailable; the anchor and every table stay.
    CoverageMiss {
        /// The commitment's type.
        llmq_type: LlmqType,
        /// Its quorum hash.
        quorum_hash: QuorumHash,
        /// The chain's anchor, the lowest height that covers ([`crate::MnChain::anchor`]).
        anchor: u32,
        /// The missing height; `None` when the base itself is unknown.
        missing: Option<u32>,
    },
}

/// Where the authenticated set at a block comes from (SB0 §5.2.1, D24).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetAt {
    /// Held.
    Held,
    /// Not held: `getmnlistd(base, target)` from the nearest authenticated held list below the
    /// target on its chain, or a null base when none is held.
    Fetch {
        /// The base; `None` asks for the whole list.
        base: Option<BlockHash>,
        /// The block.
        target: BlockHash,
        /// Its height.
        target_height: u32,
    },
    /// Below `DIP0003Height`: no commitment can be mined, so the active set is canonically
    /// empty (a lock gets `NoQuorum`).
    NoQuorum,
    /// In [`DIP0003Height`, A_q): commitments can exist, but no CbTx root authenticates them.
    /// Never fetched.
    NoRoot,
    /// Below tip − H_keep, or off the active header chain. Never fetched.
    OutsideHorizon,
}

/// SB0 §4.3.1's coverage of the authenticated domain [max(tip − H_keep, A_q), tip − 8] (D25).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// Every element of required(T) for both ends' sets is an accepted entry at or above the
    /// floor, or the domain is empty.
    Covered,
    /// An element lies below the floor: at `lowest`, or at an unknown height (`None`) when
    /// some base hash is not an accepted header.
    Below {
        /// The lowest uncovered height, when every uncovered element's height is known.
        lowest: Option<u32>,
    },
    /// An end's set is not held: its request is listed ([`MasternodeListEngine::requests`]).
    Pending,
}

/// A quorum set: (type, quorum hash) → the commitment's hash, its leaf in `merkleRootQuorums`.
pub(crate) type QuorumSet = BTreeMap<(u8, [u8; 32]), [u8; 32]>;

/// What the engine holds after a block (D28): its quorum set, and its list where D29 keeps it.
#[derive(Clone)]
pub(crate) struct HeldList {
    pub height: u32,
    pub hash: BlockHash,
    /// `None` for a set only: no step after 4 reads its SML or coinbase.
    pub full: Option<Full>,
    pub quorums: Arc<QuorumSet>,
    /// Whether step 4 authenticated `quorums`: not below A_q, where the CbTx carries no
    /// `merkleRootQuorums` (SB0 §5.2.1).
    pub authenticated: bool,
}

/// A held list's SML and the block's coinbase.
#[derive(Clone)]
pub(crate) struct Full {
    pub sml: Arc<SmlList>,
    pub coinbase: CoinbaseFacts,
}

/// A diff that passed steps 1–4, not yet held.
struct Staged {
    target: BlockId,
    list: HeldList,
    commitments: Vec<([u8; 32], Arc<Commitment>)>,
}

/// A held commitment, keyed by its hash.
#[derive(Clone, Debug)]
struct Record {
    commitment: Arc<Commitment>,
    /// The lowest height of an authenticated held set containing it, on any branch (`u32::MAX`
    /// while none is): step 5's order. The scheme it is checked under reads the active chain's
    /// (`first_sight`, D12).
    first_seen: u32,
    /// Whether an authenticated set contains it; only those run step 5.
    authenticated: bool,
    /// `None` until step 5 first runs.
    state: Option<QuorumState>,
    /// The set whose branch decided the mining scheme `state` was reached under (D12).
    decider: Option<SetMark>,
    /// The lowest height of an authenticated active-chain set above the newest one holding it
    /// that does not hold it, while held sets show it; kept once they are pruned (D31).
    replaced: Option<u32>,
    /// The lowest authenticated active-chain set holding it, as D31 marks `replaced`: the mining
    /// height the lookup reads once its sets are pruned, while that set is on its chain (D36).
    mined: Option<SetMark>,
    /// The highest authenticated active-chain set holding it, as D31 marks `replaced`: active
    /// from `mined` through it. `replaced` bounds the replacement from above only (sets need
    /// not be consecutive), so the lookup reads this (D36).
    held_to: Option<u32>,
    /// Whether `held_to` is the newest authenticated active-chain held list's holding it, as
    /// `load` recomputes it, so its row stores no height while it follows the tip; retention
    /// sets it (BL-380, D48; Opus s5 r1 m1).
    held_by_sets: bool,
}

/// A block a quorum row's marks name, with its hash for the store (D38): a commitment's decider,
/// the held set whose branch decided its mining scheme (D12: the lowest holding it, below V19, or
/// the set at V19 − 1 lacking it; a state reached under it holds on the chains through it only),
/// and its mining set (D36).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SetMark {
    pub id: BlockId,
    pub height: u32,
    pub hash: BlockHash,
}

impl Record {
    /// The status a lock check reads; `Unavailable` until step 5 first runs.
    fn scan_status(&self) -> QuorumStatus {
        self.state.as_ref().map_or(QuorumStatus::Unavailable, QuorumState::scan_status)
    }

    /// Whether its state waits on a request: step 5 runs again for it on each payload (D12).
    fn waits(&self) -> bool {
        matches!(
            self.state,
            Some(
                QuorumState::Pending(_)
                    | QuorumState::Unavailable(Unavailable::FetchFailed | Unavailable::OffChain)
            )
        )
    }
}

/// A `qrinfo` part's place in Core's construction.
#[derive(Clone, Copy)]
enum Part {
    /// A `mnListDiffList` entry.
    Extra,
    /// The target cycle H − kC.
    Cycle(u32),
    /// H's work block.
    H,
}

/// One step-5 attempt's result.
enum Attempt {
    Done(QuorumState),
    Need(MnRequest),
    /// Rejected under every choice of the unconfirmed rows read: the cycle's `qrinfo` (D20).
    Disputed(MnRequest),
}

/// A row LQ1 computed for a cycle reading Core's rows only (D20).
type CoreRow = (RowKey, QuorumSnapshot);

/// One run of Core's `Verify` over a choice of candidate rows.
struct Run {
    result: Result<VerifiedQuorum, CommitmentError<MnViewError>>,
    /// The keys whose candidate was read, and how many candidates each has.
    read: BTreeMap<RowKey, usize>,
    /// The row LQ1 computed for the commitment's cycle.
    stored: Option<CoreRow>,
}

#[cfg(test)]
thread_local! {
    /// How many held lists the newest-first reads visited on this thread.
    pub(crate) static LIST_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// SPV's masternode-list engine (SB0 §5.3).
pub struct MasternodeListEngine {
    mode: Mode,
    lists: BTreeMap<BlockId, HeldList>,
    /// `lists`' keys by height, for the reads that want the newest list matching something.
    by_height: BTreeSet<(u32, BlockId)>,
    records: HashMap<[u8; 32], Record>,
    rows: HashMap<RowKey, HeldRow>,
    requests: BTreeMap<RequestKey, MnRequest>,
    /// How often each `qrinfo` was asked again over disputed rows (D20).
    retries: BTreeMap<RequestKey, u8>,
    /// Requests whose reply was applied: a later need for one means the reply lacked it.
    answered: BTreeSet<RequestKey>,
    /// Requests the host gave up on, with their block's height.
    failed: BTreeMap<RequestKey, u32>,
    /// Requests a branch change dropped: asking one again is no retry (D12, D20).
    dropped: BTreeSet<RequestKey>,
    pending_cl: Option<ClSig>,
    /// The height and block of the newest ChainLock step 6 verified: the reorg base's (D29,
    /// DEC-387).
    best_cl: Option<(u32, BlockHash)>,
    /// The newest verified ChainLock whose block was an accepted header while it was `best_cl`
    /// (at its verification, the next one's, or a retention): the reorg base's while `best_cl`'s
    /// block is yet to come (a lock up to 8 above the tip, D18; DEC-412). A lock replaced before
    /// its header came is skipped.
    cl_base: Option<(u32, BlockHash)>,
    /// The last `mnlistdiff`'s target: the host's next diff bases on it (D29).
    last_diff: Option<BlockId>,
    /// The active tip at the last retention, which D31's marks were read on: rows answer a
    /// lookup only on a chain through it (D36).
    retained: Option<BlockRef>,
    /// The served type's lookups waiting on their fetch (D37), at most `max_lookup_waits` (D44).
    lookups: Vec<LookupWait>,
    max_lookup_waits: usize,
    /// The fetched lookups' answers, newest first, at most [`LOOKUP_CACHE`] (D37).
    looked_up: VecDeque<LookedUp>,
    /// Lists a test keeps whole, as an outstanding request would its bases.
    #[cfg(test)]
    pinned: Vec<BlockId>,
    /// Whether a test keeps every list whole: one reading lists long after their checks.
    #[cfg(test)]
    keep_every_list: bool,
    /// The commitments whose coverage miss was raised, while they stay missed and held within
    /// the horizon (D26, DEC-377).
    reported: HashSet<[u8; 32]>,
    /// What the store holds, once the engine was loaded or wrote its rows (D38).
    stored: Option<store::Stored>,
    /// What the last retention published (D39).
    published: Arc<MnSnapshot>,
}

impl MasternodeListEngine {
    /// An empty engine.
    pub fn new(mode: Mode) -> Self {
        MasternodeListEngine {
            mode,
            lists: BTreeMap::new(),
            by_height: BTreeSet::new(),
            records: HashMap::new(),
            rows: HashMap::new(),
            requests: BTreeMap::new(),
            retries: BTreeMap::new(),
            answered: BTreeSet::new(),
            failed: BTreeMap::new(),
            dropped: BTreeSet::new(),
            pending_cl: None,
            best_cl: None,
            cl_base: None,
            last_diff: None,
            retained: None,
            lookups: Vec::new(),
            max_lookup_waits: DEFAULT_LOOKUP_WAITS,
            looked_up: VecDeque::new(),
            #[cfg(test)]
            pinned: Vec::new(),
            #[cfg(test)]
            keep_every_list: false,
            reported: HashSet::new(),
            stored: None,
            published: Arc::default(),
        }
    }

    /// The engine with at most `max` served lookups waiting on their fetch (D44); past it,
    /// [`Self::fetch_lookup`] answers [`FetchLookupError::Busy`]. The default is
    /// [`DEFAULT_LOOKUP_WAITS`], sized for dash-spv's concurrent `mnlistdiff` requests: a host
    /// that serves more lookups at once sizes it to its own request concurrency. Neither `new`
    /// nor [`load`](Self::load) keeps a bound set before: apply it to each
    /// (`load(..)?.with_lookup_waits(max)`). It is at most 64, the answers kept: one call may
    /// resolve every wait, and an answer evicted before the host reads it would be fetched
    /// again (validator s6 m2).
    pub fn with_lookup_waits(mut self, max: NonZeroUsize) -> Self {
        self.max_lookup_waits = max.get().min(LOOKUP_CACHE);
        self
    }

    /// How many served lookups wait on their fetch (D37, D49). A new wait is refused
    /// ([`FetchLookupError::Busy`]) only while it is at or above [`Self::max_lookup_waits`], so
    /// a host bridging Platform's queries (SB15) can hold a new one rather than meet `Busy`. It
    /// rises only in [`Self::fetch_lookup`] and falls only where waits resolve: `apply_diff`,
    /// `apply_qrinfo` and `tip_changed`, and [`Self::request_failed`] for a wait that asked the
    /// failed key once none of its targets is held or still asked. Read it again after each. A
    /// wait may go unanswered (a target left the chain or was dropped): its lookup reads
    /// [`Lookup::Fetch`] again, and the host fetches it again.
    pub fn lookup_waits(&self) -> usize {
        self.lookups.len()
    }

    /// The bound on waiting served lookups (D44): [`Self::with_lookup_waits`]'s, else
    /// [`DEFAULT_LOOKUP_WAITS`].
    pub fn max_lookup_waits(&self) -> usize {
        self.max_lookup_waits
    }

    /// Who runs the engine.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// What the last retention published (SB0 §5.3, D39): a reader on another thread holds it
    /// while the engine goes on.
    pub fn snapshot(&self) -> Arc<MnSnapshot> {
        self.published.clone()
    }

    /// The newest list held whole on the active chain at the last retention (SB0 §5.3
    /// `latest_masternode_list()`, D39): what the bridge fills dashcore's `MasternodeList` from.
    pub fn latest_masternode_list(&self) -> Option<ListView> {
        self.published.list.clone()
    }

    /// The held list at `id` with its set's states, if held whole.
    fn list_view(&self, id: BlockId) -> Option<ListView> {
        let l = self.lists.get(&id)?;
        let full = l.full.as_ref()?;
        let quorums = l
            .quorums
            .values()
            .map(|leaf| {
                let r = &self.records[leaf];
                (r.commitment.clone(), r.state.clone())
            })
            .collect();
        Some(ListView {
            height: l.height,
            block_hash: l.hash,
            sml: full.sml.clone(),
            quorums,
        })
    }

    /// Publishes the state at `tip` (D39).
    fn publish(&mut self, chain: &dyn MnChain, tip: BlockRef) -> Result<(), AncestryError> {
        let mut list = None;
        for (height, id) in self.by_height.iter().rev() {
            if self.lists[id].full.is_some() && through(chain, tip.id, *id, *height)? {
                list = self.list_view(*id);
                break;
            }
        }
        self.published = Arc::new(MnSnapshot {
            tip: Some(tip),
            list,
            best_chainlock: self.best_cl,
        });
        Ok(())
    }

    /// The SML after `at`, if held in full (D28: a recent height holds its quorum set only, so a
    /// target just applied reads `None` once retention leaves it out of D29's lists).
    pub fn list(&self, at: BlockId) -> Option<&Arc<SmlList>> {
        self.lists.get(&at)?.full.as_ref().map(|f| &f.sml)
    }

    /// Whether the quorum set after `at` is held and authenticated by step 4.
    pub fn has_authenticated_set(&self, at: BlockId) -> bool {
        self.lists.get(&at).is_some_and(|l| l.authenticated)
    }

    /// Platform's lookup of the quorum `quorum_hash` of `llmq_type` at core height `h` (SB0 §5.3,
    /// D36): `Found` when its commitment is `Verified` (its scheme decided on the active chain)
    /// and active at some height in h − W_P ..= h on the active chain, so mined by h and not
    /// replaced by h − W_P. An authenticated active-chain set held there says so; below the held
    /// sets, a row says so by D31's marks, read from the active chain's sets at the last
    /// retention, on a chain through its tip and its mining set: `replaced` > h − W_P and
    /// `mined` ≤ h. `Fetch` while step 5 has not decided, on the active chain, a commitment such
    /// a set or row holds; `Unavailable` above the header tip, while its input cannot be had
    /// (or `Skipped`), or while the marks are not yet read on the active chain and a row may
    /// answer; else `NotFound` (DEC-444, D50). The horizon does not apply: a lookup by hash is no
    /// selection (§5.2.1 rule 6). For the served type, what nothing held decides (the active
    /// chain's sets at h − W_P and h not both held) a fetched answer does, `Unavailable` if the
    /// fetch failed or left it undecided (from the failure on: a spent wait, D53), or if an end
    /// lies below the anchor or A_q, else `Fetch`: [`Self::fetch_lookup`] asks for them (D37).
    pub fn lookup_quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        h: u32,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<Lookup, MnReject> {
        self.lookup_answer(llmq_type, quorum_hash, h, chain, cfg, true)
    }

    /// [`Self::lookup_quorum`], reading a fetched answer not decided only if `undecided`.
    fn lookup_answer(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        h: u32,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        undecided: bool,
    ) -> Result<Lookup, MnReject> {
        let answer = self.held_lookup(llmq_type, quorum_hash, h, chain, cfg)?;
        let tip = chain.active_tip()?;
        // A held `Unavailable` does not stop a served fetch (D37); other types keep DEC-116's not
        // found (D50).
        let open = matches!(answer, Lookup::NotFound | Lookup::Unavailable);
        if !open || !cfg.roles(llmq_type).platform || h > tip.height {
            return Ok(answer);
        }
        // Absent only where both ends of the window are held authenticated (D37, DEC-444): an end
        // below the anchor or A_q leaves it undecided (Opus s6 r2 m1).
        let targets = self.lookup_targets(h, chain, cfg, tip.id)?;
        let both = targets.len() == 2;
        if targets.iter().all(|(_, id)| self.has_authenticated_set(*id)) {
            return Ok(if both {
                answer
            } else {
                Lookup::Unavailable
            });
        }
        let key = lookup_key(llmq_type, quorum_hash, h);
        let cached =
            |a: &&LookedUp| a.key == key && a.targets == targets && (a.decided || undecided);
        // A wait on these targets with nothing left out (every target not held asked by it and
        // failed or answered without a set; what it asked again for a holder leaves the holder
        // `Pending`, read above) is what `resolve` answers undecided at the next call: read so
        // now, or each call on `Fetch` asks a failed target again until a block (Opus s7 r1
        // MAJOR-1, D53).
        let spent = |w: &LookupWait| {
            w.key() == key
                && w.targets == targets
                && w.targets.iter().all(|(_, id)| {
                    self.has_authenticated_set(*id)
                        || (w.asked.iter().any(|(asked, _)| asked == id)
                            && !w.outstanding(*id, &self.requests))
                })
        };
        Ok(match self.looked_up.iter().find(cached) {
            Some(a) => match &a.answer {
                Some(v) => Lookup::Found(v.clone()),
                None if a.decided && both && answer == Lookup::NotFound => Lookup::NotFound,
                None => Lookup::Unavailable,
            },
            None if self.lookups.iter().any(spent) => Lookup::Unavailable,
            None => Lookup::Fetch,
        })
    }

    /// The served type's fetch for a lookup answering `Fetch` with a target not held (SB0 §5.3,
    /// DEC-115 R6-M3, DEC-116, D37): the one fetch path's diffs to the active chain's blocks at
    /// h − W_P and h, each asked once however many lookups read it. A target below the anchor or
    /// A_q is not asked: the diff to h alone may find it, but not decide its absence, which reads
    /// `Unavailable` (DEC-444, Opus s6 r2 m1). Once held, step 5 decides what they hold and the
    /// answer is kept for later lookups. A lookup read `Unavailable` (a fetch failed, or step 5
    /// left the commitment undecided) is asked again: the host retries by calling this on
    /// `Unavailable` (DEC-444), once its wait is answered at the next block (a spent wait asks
    /// nothing, D49 (3), D53). A holder whose own step-5 input failed (`Unavailable(FetchFailed)`)
    /// is asked again too, by a call the lookup does not already wait on, and the wait owns it: the
    /// targets not held are asked with it, and its failure answers the lookup as theirs does
    /// (BL-390, D51). Nothing is asked again while a held set answers the lookup, nor for a holder
    /// whose input was answered without it (`NotServed`) or whose rows are disputed. A new wait
    /// past the engine's bound is refused, changing nothing, and none waiting is dropped (D44).
    pub fn fetch_lookup(
        &mut self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        h: u32,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<(), FetchLookupError> {
        let key = lookup_key(llmq_type, quorum_hash, h);
        let platform = cfg.roles(llmq_type).platform;
        let targets = self.lookup_targets(h, chain, cfg, chain.active_tip()?.id)?;
        // Asked again once per wait: a call while it waits (the bridge's on `Fetch`) asks no more
        // (D49 (2), D51).
        let waiting = self.lookups.iter().any(|w| w.key() == key && w.targets == targets);
        let failed = if platform && !waiting {
            self.failed_holders(llmq_type, quorum_hash, h, chain, cfg)?
        } else {
            Vec::new()
        };
        let fetch = platform
            && (!failed.is_empty()
                || self.lookup_answer(llmq_type, quorum_hash, h, chain, cfg, false)?
                    == Lookup::Fetch);
        if fetch
            && self.lookups.len() >= self.max_lookup_waits
            && self.lookups.iter().all(|w| w.key() != key)
        {
            return Err(FetchLookupError::Busy {
                max: self.max_lookup_waits,
            });
        }
        self.looked_up.retain(|a| a.key != key || a.decided);
        if !fetch {
            return Ok(());
        }
        // A commitment step 5 has not decided in the window decides alone and asks nothing; the
        // wait names it, so step 5 runs on it in a set only lookups fetched (validator D37 r1
        // m3, r2 m1). Read before a failed holder is asked again: its targets are asked with it,
        // as they were while it failed (validator s7 M1).
        let pending = self.held_lookup(llmq_type, quorum_hash, h, chain, cfg)? == Lookup::Fetch;
        let mut reasked = Vec::new();
        for leaf in failed {
            reasked.extend(self.reask(leaf, chain, cfg)?);
        }
        let mut asked = Vec::new();
        for (_, id) in &targets {
            if !pending && !self.has_authenticated_set(*id) {
                let request = self.diff_request(*id, chain)?;
                asked.push((*id, request.key()));
                self.requests.entry(request.key()).or_insert(request);
            }
        }
        let wait = LookupWait {
            llmq_type,
            quorum_hash: *quorum_hash,
            h,
            targets,
            asked,
            reasked,
        };
        // One for the same lookup on another chain is replaced (validator D37 r1 m2); one on this
        // chain waits again on what is asked again, which replaces what was asked for its targets
        // (Sol s6 r1 M1: a wait made while step 5 was pending asked nothing, so the failures of
        // what it asked later were not its own, and it never went). A call while it waits asks
        // nothing again for a holder, so only a new wait has `reasked`.
        self.lookups.retain(|w| w.key() != key || w.targets == wait.targets);
        match self.lookups.iter_mut().find(|w| w.key() == key) {
            Some(w) => {
                w.asked.retain(|(id, _)| wait.asked.iter().all(|(asked, _)| asked != id));
                w.asked.extend(wait.asked);
            }
            None => self.lookups.push(wait),
        }
        Ok(())
    }

    /// The records holding the lookup's commitment on the active chain whose step 5 gave up on a
    /// failed request (`Unavailable(FetchFailed)`, its scheme decided there): what a call asks
    /// again (BL-390, D51). None once one is `Verified`: it answers.
    fn failed_holders(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        h: u32,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<Vec<[u8; 32]>, MnReject> {
        let mut holders = Vec::new();
        if let Lookup::Found(_) =
            self.held_holders(llmq_type, quorum_hash, h, chain, cfg, &mut holders)?
        {
            return Ok(Vec::new());
        }
        let tip = chain.active_tip()?.id;
        let mut failed = Vec::new();
        for leaf in holders {
            let r = &self.records[&leaf];
            if r.state == Some(QuorumState::Unavailable(Unavailable::FetchFailed))
                && decided_on(chain, tip, r.decider)?
                && !failed.contains(&leaf)
            {
                failed.push(leaf);
            }
        }
        Ok(failed)
    }

    /// Runs step 5 on `leaf`'s record again, its failed request forgotten, so what it needs is
    /// asked again (BL-390, D51): `Pending` on the key returned, if `need` asked it, until the
    /// reply or the next failure, which the host may report as before. One that no longer needs an
    /// input takes step 5's verdict; one whose input lies off the active chain, or whose rows are
    /// disputed, is left as it was, for the next step 5.
    fn reask(
        &mut self,
        leaf: [u8; 32],
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<Option<RequestKey>, MnReject> {
        let tip = chain.active_tip()?.id;
        let (attempt, decider, core_row) = self.attempt(&self.records[&leaf], chain, cfg)?;
        if let Some((key, row)) = core_row {
            self.confirm(key, row);
        }
        let (state, asked) = match attempt {
            Attempt::Done(state) => (state, None),
            Attempt::Need(request) if on_active_chain(chain, tip, request.block())? => {
                let key = request.key();
                self.failed.remove(&key);
                let state = self.need(request);
                let asked = (state == QuorumState::Pending(key)).then_some(key);
                (state, asked)
            }
            Attempt::Need(_) | Attempt::Disputed(_) => return Ok(None),
        };
        let record = self.records.get_mut(&leaf).expect("held");
        record.state = Some(state);
        record.decider = decider;
        Ok(asked)
    }

    /// The blocks a served type's lookup at `h` reads (D37): the active chain's at h − W_P and h,
    /// those at or above the anchor and A_q (no set below either is authenticated). Fewer than two
    /// leave absence undecided (D50).
    fn lookup_targets(
        &self,
        h: u32,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        tip: BlockId,
    ) -> Result<Vec<(u32, BlockId)>, MnReject> {
        let lowest = chain.anchor().max(cfg.a_q());
        let mut targets = Vec::new();
        for height in [h.saturating_sub(cfg.w_p()), h] {
            if height >= lowest
                && let Some(id) = chain.ancestry().ancestor(tip, height)?
            {
                targets.push((height, id));
            }
        }
        Ok(targets)
    }

    /// The commitments only lookup targets below `horizon` hold, none a wait names: step 5 does
    /// not run for them, as no lock reads them (validator D37 r1 m1). Retention drops them with
    /// their targets.
    fn lookup_only(&self, horizon: u32) -> HashSet<[u8; 32]> {
        if self.lookups.is_empty() {
            return HashSet::new();
        }
        let mut only = HashSet::new();
        let named: HashSet<(u8, [u8; 32])> = self
            .lookups
            .iter()
            .map(|w| (w.llmq_type.to_base(), *w.quorum_hash.as_bytes()))
            .collect();
        let targets: HashSet<BlockId> =
            self.lookups.iter().flat_map(|w| w.targets.iter().map(|(_, id)| *id)).collect();
        let fetched = |id: &BlockId, l: &HeldList| targets.contains(id) && l.height < horizon;
        for (_, l) in self.lists.iter().filter(|(id, l)| fetched(id, l)) {
            only.extend(
                l.quorums.iter().filter(|(key, _)| !named.contains(*key)).map(|(_, leaf)| *leaf),
            );
        }
        for (_, l) in self.lists.iter().filter(|(id, l)| !fetched(id, l)) {
            for leaf in l.quorums.values() {
                only.remove(leaf);
            }
        }
        only
    }

    /// Each served lookup waiting on its fetch, answered from what is now held (D37). A wait
    /// whose targets left the active chain goes, as does one with a target held when asked that
    /// retention has since dropped: the host asks again. Read before any change: a fault loses no
    /// wait.
    fn resolve_lookups(&mut self, chain: &dyn MnChain, cfg: &LlmqConfig) -> Result<(), MnReject> {
        let tip = chain.active_tip()?.id;
        let mut resolved = Vec::with_capacity(self.lookups.len());
        for wait in &self.lookups {
            let mut on_chain = true;
            for (height, id) in &wait.targets {
                on_chain &= through(chain, tip, *id, *height)?;
            }
            resolved.push(if on_chain {
                self.resolve(wait, chain, cfg)?
            } else {
                Resolved::Drop
            });
        }
        for (wait, resolved) in std::mem::take(&mut self.lookups).into_iter().zip(resolved) {
            match resolved {
                Resolved::Wait => self.lookups.push(wait),
                Resolved::Drop => {}
                Resolved::Answer(answer, decided) => {
                    self.keep_answer(wait, answer.map(|v| *v), decided)
                }
            }
        }
        Ok(())
    }

    /// One waiting lookup's answer from its targets (D37): found from what is held; waiting while
    /// a target is asked for or step 5 runs on what they hold; once every target is held or
    /// failed, none, decided (not found) only when nothing held reads it `Unavailable` and each
    /// target's set lacks the commitment or holds it `Unverified` on the active chain, else
    /// undecided (`Unavailable`, DEC-444).
    fn resolve(
        &self,
        wait: &LookupWait,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<Resolved, MnReject> {
        let held = self.held_lookup(wait.llmq_type, &wait.quorum_hash, wait.h, chain, cfg)?;
        match held {
            Lookup::Found(v) => return Ok(Resolved::Answer(Some(Box::new(v)), true)),
            Lookup::Fetch => return Ok(Resolved::Wait),
            Lookup::NotFound | Lookup::Unavailable => {}
        }
        let (mut failed, mut gone) = (false, false);
        for (_, id) in &wait.targets {
            if self.has_authenticated_set(*id) {
                continue;
            }
            if wait.outstanding(*id, &self.requests) {
                return Ok(Resolved::Wait);
            }
            // Failed, or its reply held no authenticated set: none can be had.
            if wait.asked.iter().any(|(asked, _)| asked == id) {
                failed = true;
            } else {
                gone = true;
            }
        }
        if gone {
            return Ok(Resolved::Drop);
        }
        let tip = chain.active_tip()?.id;
        let key = (wait.llmq_type.to_base(), *wait.quorum_hash.as_bytes());
        let mut decided = !failed && held == Lookup::NotFound;
        for (_, id) in &wait.targets {
            if let Some(leaf) = self.lists.get(id).and_then(|l| l.quorums.get(&key)) {
                let r = &self.records[leaf];
                decided &=
                    r.state == Some(QuorumState::Unverified) && decided_on(chain, tip, r.decider)?;
            }
        }
        Ok(Resolved::Answer(None, decided))
    }

    /// Keeps a fetched lookup's answer, evicting the oldest past [`LOOKUP_CACHE`] (D37).
    fn keep_answer(&mut self, wait: LookupWait, answer: Option<VerifiedQuorum>, decided: bool) {
        let key = wait.key();
        self.looked_up.retain(|a| a.key != key);
        self.looked_up.push_front(LookedUp {
            key,
            targets: wait.targets,
            answer,
            decided,
        });
        self.looked_up.truncate(LOOKUP_CACHE);
    }

    /// [`Self::lookup_quorum`] from what is held (D36).
    fn held_lookup(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        h: u32,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<Lookup, MnReject> {
        self.held_holders(llmq_type, quorum_hash, h, chain, cfg, &mut Vec::new())
    }

    /// [`Self::held_lookup`], with the records of the active-chain sets and rows it read pushed
    /// to `holders` (D51).
    fn held_holders(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        h: u32,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        holders: &mut Vec<[u8; 32]>,
    ) -> Result<Lookup, MnReject> {
        let tip = chain.active_tip()?;
        // Headers have not reached h: nothing is decided there yet (DEC-444, Opus s6 r2 M1).
        if h > tip.height {
            return Ok(Lookup::Unavailable);
        }
        let low = h.saturating_sub(cfg.w_p());
        let key = (llmq_type.to_base(), *quorum_hash.as_bytes());
        let mut answer = Lookup::NotFound;
        for (height, id) in self.by_height.range((low, BlockId(0))..=(h, BlockId(u32::MAX))) {
            let l = &self.lists[id];
            let Some(leaf) = l.quorums.get(&key) else {
                continue;
            };
            if !l.authenticated || !through(chain, tip.id, *id, *height)? {
                continue;
            }
            holders.push(*leaf);
            if let Some(v) = fold_holder(&mut answer, &self.records[leaf], chain, tip.id)? {
                return Ok(Lookup::Found(v));
            }
        }
        // Marks read on a chain the active one left answer nothing until retention reads them
        // again (validator r7 M1″), as no verdict is read before step 5 runs again. A row that
        // may answer then leaves the lookup undecided, never not found (DEC-444).
        let row_mark = |r: &Record| {
            let survivor = r.authenticated
                && r.commitment.llmq_type == llmq_type
                && r.commitment.quorum_hash == *quorum_hash
                && r.held_to.is_some_and(|at| at >= low);
            r.mined.filter(|m| survivor && m.height <= h)
        };
        let readable = match self.retained {
            Some(retained) => through(chain, tip.id, retained.id, retained.height)?,
            // Retention faulted before it read the marks (Opus s6 r2 n4).
            None => false,
        };
        if !readable {
            let row =
                answer == Lookup::NotFound && self.records.values().any(|r| row_mark(r).is_some());
            return Ok(if row {
                Lookup::Unavailable
            } else {
                answer
            });
        }
        // A row active at h by its marks answers as a held set does (DEC-444 for the undecided).
        for (leaf, r) in &self.records {
            let Some(mined) = row_mark(r) else {
                continue;
            };
            if !through(chain, tip.id, mined.id, mined.height)? {
                continue;
            }
            holders.push(*leaf);
            if let Some(v) = fold_holder(&mut answer, r, chain, tip.id)? {
                return Ok(Lookup::Found(v));
            }
        }
        Ok(answer)
    }

    /// The held commitment of `llmq_type` for `quorum_hash`, and its status, from the newest
    /// held set holding it on any branch: as step 5 last left it, which a lock check reads only
    /// when its mining scheme was decided on the active chain (D12).
    pub fn quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Option<(Arc<Commitment>, QuorumState)> {
        let record = self.record(llmq_type, quorum_hash)?;
        let state = match (&record.state, record.authenticated) {
            (Some(state), true) => state.clone(),
            _ => QuorumState::Unauthenticated,
        };
        Some((record.commitment.clone(), state))
    }

    /// The commitment for the quorum in the newest authenticated held set holding one, else in
    /// the newest held set, on any branch: what is held. A lock check reads the active chain's
    /// sets only (`ScanView::quorum`).
    fn record(&self, llmq_type: LlmqType, quorum_hash: &QuorumHash) -> Option<&Record> {
        let key = (llmq_type.to_base(), *quorum_hash.as_bytes());
        let mut newest = None;
        for (_, l) in self.newest_first(u32::MAX) {
            if let Some(hash) = l.quorums.get(&key) {
                if l.authenticated {
                    return self.records.get(hash);
                }
                newest.get_or_insert(hash);
            }
        }
        self.records.get(newest?)
    }

    /// The held lists at or below `height`, newest first.
    fn newest_first(&self, height: u32) -> impl Iterator<Item = (BlockId, &HeldList)> + '_ {
        self.by_height.range(..=(height, BlockId(u32::MAX))).rev().map(|(_, id)| {
            #[cfg(test)]
            LIST_READS.with(|r| r.set(r.get() + 1));
            (*id, &self.lists[id])
        })
    }

    fn insert_list(&mut self, at: BlockId, list: HeldList) {
        self.by_height.insert((list.height, at));
        self.lists.insert(at, list);
    }

    /// The requests outstanding, each until its reply is applied, [`Self::request_failed`]
    /// reports it, or a branch change leaves its block off the active chain (D12).
    pub fn requests(&self) -> Vec<MnRequest> {
        self.requests.values().cloned().collect()
    }

    /// The host gave up on `key` (every allowed peer failed or timed out; SB0 §5.2.1 rule 2): the
    /// commitments waiting on it are `Unavailable(FetchFailed)`, or `RowsDisputed` when it asked
    /// again for rows they failed over, so a new row reopens them (D20).
    /// A key not outstanding (answered, or dropped by a branch change) is ignored.
    pub fn request_failed(&mut self, key: RequestKey) {
        let Some(request) = self.requests.remove(&key) else {
            return;
        };
        self.failed.insert(key, request.block().1);
        let why = if self.retries.contains_key(&key) {
            Unavailable::RowsDisputed
        } else {
            Unavailable::FetchFailed
        };
        for record in self.records.values_mut() {
            if record.state == Some(QuorumState::Pending(key)) {
                record.state = Some(QuorumState::Unavailable(why));
            }
        }
        // A wait that asked `key` is undecided (`Unavailable`; SB0 §5.3's not-found for a failed
        // fetch, DEC-444, D37) once no target is held or asked for; a target held decides at the
        // next call (validator D37 r1 M1). As in `resolve`, a target neither held nor asked went
        // since: the wait goes, and the host asks again. Waits that did not ask `key` are
        // untouched (validator s6 M1). One asked again for a holder is the wait's as a target's
        // is, and open while out (D51, validator s7 M1).
        for wait in std::mem::take(&mut self.lookups) {
            if wait.keys().all(|asked| asked != key) {
                self.lookups.push(wait);
                continue;
            }
            let mut open = wait.reasked.iter().any(|k| self.requests.contains_key(k));
            let mut gone = false;
            for (_, id) in &wait.targets {
                if self.has_authenticated_set(*id) || wait.outstanding(*id, &self.requests) {
                    open = true;
                } else if wait.asked.iter().all(|(asked, _)| asked != id) {
                    gone = true;
                }
            }
            if open {
                self.lookups.push(wait);
            } else if !gone {
                self.keep_answer(wait, None, false);
            }
        }
    }

    /// Step 5 after the active tip moved with no payload to apply: a branch change may move what
    /// a commitment waits on, and the mining scheme a verdict was reached under (D12). Until it
    /// runs, a lock check reads no verdict reached on another branch.
    pub fn tip_changed(
        &mut self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<MnChange, MnReject> {
        self.finish(MnChange::default(), chain, cfg)
    }

    // ---- steps 1–5 ---------------------------------------------------------------------------

    /// Applies an `mnlistdiff`: steps 1–4 authenticate the list and quorum set at its target,
    /// then step 5 runs for each authenticated commitment not yet decided. A refused diff changes
    /// nothing; [`MnViewError::Storage`] from step 5 comes after the diff is held: stop. Its base
    /// must be held whole (D29): the host's last tip diff's target, a request's base, or null; a
    /// tip diff bases on the last one the host applied. A diff to a target held whole holds
    /// nothing new when it carries the held coinbase, and step 5 runs as for
    /// [`Self::tip_changed`]: the second reply for one block (the host's tip diff and a fetch at
    /// the header tip, D27) need not find its base (D34).
    pub fn apply_diff(
        &mut self,
        d: &MnListDiff,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<MnChange, MnReject> {
        if let Some(id) = chain.lookup(&d.block_hash)?
            && let Some(l) = self.lists.get(&id)
            && let Some(Full {
                coinbase: CoinbaseFacts::First(tx),
                ..
            }) = l.full.as_ref().filter(|_| l.authenticated)
            && tx.txid() == Tx::new(d.cb_tx.clone()).txid()
        {
            // One of the two replies was the host's: its base, unless the base held is newer on
            // the active chain (a fetch's reply for an older block).
            let height = l.height;
            let tip = chain.active_tip()?.id;
            let newer = match self.last_diff.and_then(|b| self.lists.get(&b).map(|l| (b, l))) {
                Some((b, base)) => base.height > height && through(chain, tip, b, base.height)?,
                None => false,
            };
            if !newer {
                self.last_diff = Some(id);
            }
            return self.finish(MnChange::default(), chain, cfg);
        }
        let staged = self.authenticate(d, &BTreeMap::new(), chain, cfg)?;
        let mut change = MnChange::default();
        // The host's tip diff answered no request, or one at the header tip (a lock's first
        // pass, D27); a fetch's reply below it, late or not, is no base the host names (D34).
        let key = RequestKey::MnListDiff(staged.list.hash);
        let fetched = self.requests.contains_key(&key)
            || self.dropped.contains(&key)
            || self.failed.contains_key(&key);
        if !fetched || staged.target == chain.active_tip()?.id {
            self.last_diff = Some(staged.target);
        }
        self.commit(staged, &mut change);
        self.finish(change, chain, cfg)
    }

    /// Applies a `qrinfo`, all or nothing: its diffs through steps 1–4 in Core's construction
    /// order (`llmq/snapshot.cpp` rc.3:204-255: the extra cycles, the target cycles oldest first,
    /// H's work block, the tip), each at its place in that construction, then its rows, each
    /// under the cycle whose work block its diff targets; then step 5 as for a diff. Rows are
    /// held only from a reply to an outstanding request (D17), as candidates (D20), and the
    /// extra ones only when they are Core's set (D21); another reply adds its lists. The host
    /// passes a `qrinfo` only from the peer it sent that `getqrinfo` to (D1).
    ///
    /// `lastCommitmentPerIndex` is not read: no root commits to it (D11). The diff to Core's tip
    /// is skipped while its target is not an accepted header: no member computation reads it.
    pub fn apply_qrinfo(
        &mut self,
        q: &QrInfo,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<MnChange, MnReject> {
        if q.quorum_snapshot_list.len() != q.mn_list_diff_list.len() {
            return Err(MnReject::QrInfo("the row and diff lists differ in length"));
        }
        let params = cfg.params();
        let rotated = params.consensus.llmq_type_dip0024_instant_send;
        let interval = params
            .consensus
            .llmq(rotated)
            .map(interval)
            .ok_or(MnReject::QrInfo("no rotated type is configured"))?;

        let mut parts: Vec<(&MnListDiff, Option<&dash_p2p_core::QuorumSnapshot>, Part)> = q
            .mn_list_diff_list
            .iter()
            .zip(&q.quorum_snapshot_list)
            .map(|(d, row)| (d, Some(row), Part::Extra))
            .collect();
        if let Some(c) = &q.cycle_h_4c {
            parts.push((&c.diff, Some(&c.snapshot), Part::Cycle(4)));
        }
        parts.push((&q.diff_h_3c, Some(&q.snapshot_h_3c), Part::Cycle(3)));
        parts.push((&q.diff_h_2c, Some(&q.snapshot_h_2c), Part::Cycle(2)));
        parts.push((&q.diff_h_c, Some(&q.snapshot_h_c), Part::Cycle(1)));
        parts.push((&q.diff_h, None, Part::H));

        let mut staged: BTreeMap<BlockId, HeldList> = BTreeMap::new();
        let mut order = Vec::new();
        for (d, ..) in &parts {
            let s = self.authenticate(d, &staged, chain, cfg)?;
            staged.insert(s.target, s.list.clone());
            order.push(s);
        }
        if chain.lookup(&q.diff_tip.block_hash)?.is_some() {
            let s = self.authenticate(&q.diff_tip, &staged, chain, cfg)?;
            staged.insert(s.target, s.list.clone());
            order.push(s);
        }

        // H's cycle starts WORK_DIFF_DEPTH above its work block; each row's cycle, as far above
        // its diff's target, is an ancestor of H's work block (`ConstructCycle`, rc.3:57-75).
        let h_work = order[parts.len() - 1].target;
        let h_work_height = order[parts.len() - 1].list.height;
        let h_cycle_height = h_work_height + WORK_DIFF_DEPTH;
        if !h_cycle_height.is_multiple_of(interval) {
            return Err(MnReject::QrInfo("H's diff does not target a cycle's work block"));
        }
        // The target cycles are H − kC (`GetCycles`); the extra ones are Core's
        // `snapshotHeightsNeeded`, a set: ascending, distinct, none a target cycle
        // (`llmq/snapshot.cpp` rc.3:173-210), and exactly that set (below).
        let targets: Vec<u32> = parts
            .iter()
            .filter_map(|(.., part)| match part {
                Part::Cycle(k) => h_cycle_height.checked_sub(k * interval),
                _ => None,
            })
            .collect();
        let mut extras: Vec<u32> = Vec::new();
        let mut rows = Vec::new();
        for ((_, row, part), s) in parts.iter().zip(&order) {
            let Some(row) = row else {
                continue;
            };
            let cycle_height = s.list.height + WORK_DIFF_DEPTH;
            if let Part::Cycle(k) = part {
                if h_cycle_height.checked_sub(k * interval) != Some(cycle_height) {
                    return Err(MnReject::QrInfo("a target cycle's diff is not at its place"));
                }
            } else {
                if !cycle_height.is_multiple_of(interval) || cycle_height > h_work_height {
                    return Err(MnReject::QrInfo(
                        "a row's diff does not target an earlier cycle's work block",
                    ));
                }
                if extras.last().is_some_and(|l| *l >= cycle_height)
                    || targets.contains(&cycle_height)
                {
                    return Err(MnReject::QrInfo("the extra cycles are not Core's ascending set"));
                }
                extras.push(cycle_height);
            }
            let cycle = ancestor_at(chain, h_work, cycle_height)?;
            if ancestor_at(chain, cycle, s.list.height)? != s.target {
                return Err(MnReject::QrInfo("a row's cycle is not on H's chain"));
            }
            let hash = chain.ancestry().header(cycle)?.hash;
            // The extra share's H − 4C is not asked for (D14): its row is held as an extra one.
            let extra = matches!(part, Part::Extra | Part::Cycle(4));
            rows.push(((rotated.to_base(), *hash.as_bytes()), snapshot(row), extra));
        }

        // The requests this answers: those for H's cycle on H's chain.
        let mut answered = Vec::new();
        for (key, r) in &self.requests {
            if let MnRequest::QrInfo {
                request,
                request_height,
                ..
            } = r
                && *request_height == h_cycle_height
                && let Some(id) = chain.lookup(request)?
                && ancestor_at(chain, id, h_work_height)? == h_work
            {
                answered.push((*key, id));
            }
        }
        // The extra rows are held when they are Core's at the request block; a reply naming
        // others is refused, and while that set is unknown none is held (D21).
        let (mut hold_extras, mut unknown) = (false, false);
        for &(_, request) in &answered {
            match self.extra_cycles(request, &targets, interval, rotated, &staged, chain)? {
                Some(needed) => {
                    hold_extras |= needed.iter().copied().eq(extras.iter().map(|h| i64::from(*h)));
                }
                None => unknown = true,
            }
        }
        if !answered.is_empty() && !hold_extras && !unknown {
            return Err(MnReject::QrInfo("the extra cycles are not Core's set"));
        }

        let mut change = MnChange::default();
        for s in order {
            self.commit(s, &mut change);
        }
        let mut added = false;
        if !answered.is_empty() {
            for (key, row, extra) in rows {
                if !extra || hold_extras {
                    added |= self.add_candidate(key, row);
                }
            }
        }
        for (key, _) in answered {
            self.answer(key);
        }
        if added {
            self.reopen_disputed();
        }
        self.finish(change, chain, cfg)
    }

    /// Core's `snapshotHeightsNeeded` for a `getqrinfo` at `request`, ascending
    /// (`llmq/snapshot.cpp` rc.3:194-210): for each index's last mined rotated commitment at the
    /// request block, the three cycles before its own, less the target cycles. Those commitments
    /// are the rotated ones of the authenticated set at the request block (D10), surviving older
    /// indexes included. `None` while that set is not held, or a commitment's base is not an
    /// accepted header (D21).
    fn extra_cycles(
        &self,
        request: BlockId,
        targets: &[u32],
        interval: u32,
        rotated: LlmqType,
        staged: &BTreeMap<BlockId, HeldList>,
        chain: &dyn MnChain,
    ) -> Result<Option<BTreeSet<i64>>, MnReject> {
        let Some(set) =
            staged.get(&request).or_else(|| self.lists.get(&request)).filter(|l| l.authenticated)
        else {
            return Ok(None);
        };
        let mut needed = BTreeSet::new();
        for (t, quorum_hash) in set.quorums.keys() {
            if *t != rotated.to_base() {
                continue;
            }
            let Some(base) = chain.lookup(&BlockHash::from(*quorum_hash))? else {
                return Ok(None);
            };
            let height = chain.ancestry().header(base)?.height;
            let start = i64::from(height - height % interval);
            needed.extend((1..=3).map(|k| start - k * i64::from(interval)));
        }
        for t in targets {
            needed.remove(&i64::from(*t));
        }
        Ok(Some(needed))
    }

    /// Holds a reply's row for its cycle as a candidate, not once Core's is held: the newest
    /// `ROW_CANDIDATES` distinct ones, the oldest giving way; a repeated row counts as newest
    /// again (D20). Whether it is new.
    fn add_candidate(&mut self, key: RowKey, row: QuorumSnapshot) -> bool {
        match self.rows.entry(key).or_insert_with(|| HeldRow::Candidates(Vec::new())) {
            HeldRow::Core(_) => false,
            HeldRow::Candidates(rows) => {
                if let Some(i) = rows.iter().position(|r| *r == row) {
                    let repeated = rows.remove(i);
                    rows.push(repeated);
                    return false;
                }
                if rows.len() == ROW_CANDIDATES {
                    rows.remove(0);
                }
                rows.push(row);
                true
            }
        }
    }

    /// Holds Core's row for a cycle, its candidates dropped (D20).
    fn confirm(&mut self, key: RowKey, row: QuorumSnapshot) {
        if !matches!(self.rows.get(&key), Some(HeldRow::Core(_))) {
            self.rows.insert(key, HeldRow::Core(row));
            self.reopen_disputed();
        }
    }

    /// Step 5 again for the commitments whose rows were disputed: a row changed (D20).
    fn reopen_disputed(&mut self) {
        for r in self.records.values_mut() {
            if r.state == Some(QuorumState::Unavailable(Unavailable::RowsDisputed)) {
                r.state = None;
            }
        }
    }

    /// Steps 1–4 for one diff, over the held (or `staged`) list at its base.
    fn authenticate(
        &self,
        d: &MnListDiff,
        staged: &BTreeMap<BlockId, HeldList>,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<Staged, MnReject> {
        // Step 1: the target is an accepted header.
        let target = chain.lookup(&d.block_hash)?.ok_or(MnReject::UnknownTarget(d.block_hash))?;
        let height = chain.ancestry().header(target)?.height;

        // The base: a null or genesis base is the empty list (`evo/smldiff.cpp` rc.3:166-176).
        let genesis = BlockHash::from(*cfg.params().consensus.hash_genesis_block.as_bytes());
        let empty;
        let base = if d.base_block_hash.as_bytes() == &[0; 32] || d.base_block_hash == genesis {
            empty = HeldList {
                height: 0,
                hash: genesis,
                full: Some(Full {
                    sml: Arc::new(SmlList::new()),
                    coinbase: CoinbaseFacts::NoTransactions,
                }),
                quorums: Arc::new(QuorumSet::new()),
                authenticated: true,
            };
            &empty
        } else {
            let id = chain
                .lookup(&d.base_block_hash)?
                .ok_or(MnReject::UnknownBase(d.base_block_hash))?;
            // A set held without its list is no base (D28).
            let base = staged
                .get(&id)
                .or_else(|| self.lists.get(&id))
                .filter(|l| l.full.is_some())
                .ok_or(MnReject::UnknownBase(d.base_block_hash))?;
            if base.height > height || ancestor_at(chain, target, base.height)? != id {
                return Err(MnReject::UnknownBase(d.base_block_hash));
            }
            base
        };

        // Step 2: the coinbase is a CbTx for the target's height, proven in its merkle root.
        let tx = Arc::new(Tx::new(d.cb_tx.clone()));
        let cbtx = cbtx_payload(&tx)?;
        let root = chain.merkle_root(target)?.ok_or(MnReject::UnknownTarget(d.block_hash))?;
        verify_cbtx_proof(&proof_tree(&d.cb_tx_merkle_tree), &root, &tx.txid())
            .map_err(MnReject::CbTxProof)?;
        if i64::from(cbtx.height) != i64::from(height) {
            return Err(MnReject::BadCoinbase("its height is not the target's"));
        }

        // Step 3: the base list and the diff hash to `merkleRootMNList`.
        let mut sml = (*base.full.as_ref().expect("a base is held in full").sml).clone();
        for pro_tx in &d.deleted_mns {
            sml.remove(&ProTxHash::from(*pro_tx.as_bytes()));
        }
        for entry in &d.mn_list {
            sml.upsert(sml_entry(entry));
        }
        let mn_root = sml.merkle_root().map_err(|_| MnReject::MnListRoot)?;
        check_mn_list_root(&cbtx, &mn_root).map_err(|_| MnReject::MnListRoot)?;

        // Step 4: the base set and the diff hash to `merkleRootQuorums`, from CbTx version 2. A
        // diff changing no quorum shares its base's set (D28).
        let mut quorums = base.quorums.clone();
        let mut commitments = Vec::with_capacity(d.new_quorums.len());
        if !d.deleted_quorums.is_empty() || !d.new_quorums.is_empty() {
            let set = Arc::make_mut(&mut quorums);
            for q in &d.deleted_quorums {
                set.remove(&(q.llmq_type.to_base(), *q.hash.as_bytes()));
            }
            for qc in &d.new_quorums {
                let hash = commitment_hash(qc);
                set.insert((qc.llmq_type.to_base(), *qc.quorum_hash.as_bytes()), hash);
                commitments.push((hash, Arc::new(qc.clone())));
            }
        }
        check_quorums_root(&cbtx, || {
            merkle_root_quorums_from_hashes(quorums.values().copied().collect())
        })
        .map_err(|_| MnReject::QuorumsRoot)?;
        // A version-2 root commits to the whole resulting set, whatever the base's CbTx carried
        // (`evo/specialtxman.cpp` rc.3:943-952): the first rooted target is authenticated even
        // from a version-1 base (D19).
        let authenticated = cbtx.version >= CBTX_VERSION_MERKLE_ROOT_QUORUMS;

        Ok(Staged {
            target,
            list: HeldList {
                height,
                hash: d.block_hash,
                full: Some(Full {
                    sml: Arc::new(sml),
                    coinbase: CoinbaseFacts::First(tx),
                }),
                quorums,
                authenticated,
            },
            commitments,
        })
    }

    /// Holds a staged list and its commitments.
    fn commit(&mut self, s: Staged, change: &mut MnChange) {
        let mut fresh: HashMap<[u8; 32], Arc<Commitment>> = s.commitments.into_iter().collect();
        for hash in s.list.quorums.values() {
            match self.records.get_mut(hash) {
                Some(r) => {
                    if s.list.authenticated {
                        r.first_seen = r.first_seen.min(s.list.height);
                    }
                    r.authenticated |= s.list.authenticated;
                }
                None => {
                    let commitment = fresh.remove(hash).expect("step 4 hashed it from the diff");
                    change.new_quorums.push((commitment.llmq_type, commitment.quorum_hash));
                    self.records.insert(
                        *hash,
                        Record {
                            commitment,
                            first_seen: if s.list.authenticated {
                                s.list.height
                            } else {
                                u32::MAX
                            },
                            authenticated: s.list.authenticated,
                            state: None,
                            decider: None,
                            replaced: None,
                            mined: None,
                            held_to: None,
                            held_by_sets: false,
                        },
                    );
                }
            }
        }
        self.answer(RequestKey::MnListDiff(s.list.hash));
        change.targets.push(s.target);
        self.insert_list(s.target, s.list);
    }

    fn answer(&mut self, key: RequestKey) {
        if self.requests.remove(&key).is_some() {
            self.answered.insert(key);
        }
    }

    /// Step 5, then the diagnostics it raised joined to `change`, then retention (D30).
    fn finish(
        &mut self,
        mut change: MnChange,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<MnChange, MnReject> {
        self.verify_open(chain, cfg)?;
        self.resolve_lookups(chain, cfg)?;
        let (events, reported) = self.raise_misses(chain, cfg)?;
        self.retain(chain, cfg)?;
        // Kept only once the call cannot stop: a retry raises each miss once (D26).
        self.reported = reported;
        change.events = events;
        Ok(change)
    }

    // ---- retention (SB0 §5.3, D28–D31) -------------------------------------------------------

    /// Retention after a call's checks (D30): the lists D29 keeps stay whole; every other set
    /// within tip − H_keep, on any branch, stays without its list, and the rest goes. Then each
    /// row (`Record`) no held set holds goes, unless it was replaced at or above tip − R (D31).
    fn retain(&mut self, chain: &dyn MnChain, cfg: &LlmqConfig) -> Result<(), MnReject> {
        let tip = chain.active_tip()?;
        // The host's base is a list held whole on the active chain, else none: Core serves no base
        // on a branch the chain left, and a list no longer whole is no base (D34, Sol s3 r1 M1).
        if let Some(id) = self.last_diff {
            let live = match self.lists.get(&id) {
                Some(l) if l.full.is_some() => through(chain, tip.id, id, l.height)?,
                _ => false,
            };
            if !live {
                self.last_diff = None;
            }
        }
        self.note_cl_base(chain)?;
        let keep = self.kept_lists(chain, cfg, tip.id)?;
        let horizon = tip.height.saturating_sub(cfg.h_keep());
        #[cfg(test)]
        let horizon = if self.keep_every_list {
            0
        } else {
            horizon
        };
        // Before the drops: a gap across the horizon starts at a set they remove (D31). Marks a
        // fault leaves half written answer nothing (validator r8 nit).
        self.retained = None;
        self.mark_replaced(chain, tip.id, horizon)?;
        self.retained = Some(tip);
        let mut gone = Vec::new();
        let mut released = false;
        for (id, l) in &mut self.lists {
            if l.full.is_some() && keep.contains(id) {
                continue;
            }
            if l.height < horizon {
                gone.push((l.height, *id));
            } else if l.full.take().is_some() {
                // Its list was a reply: one needed again is asked again.
                self.answered.remove(&RequestKey::MnListDiff(l.hash));
                released = true;
            }
        }
        for (height, id) in gone {
            let l = self.lists.remove(&id).expect("held");
            self.by_height.remove(&(height, id));
            self.answered.remove(&RequestKey::MnListDiff(l.hash));
            released |= l.full.is_some();
        }
        // So may a list a `qrinfo` reply held, whose key names its cycle, not the list: a
        // commitment step 5 first reads later (one a branch switched to mined, or a reorg
        // reopened) asks again rather than reading `NotServed`. One already decided is not read
        // again, so this asks only for what a later step 5 needs (validator r2 finding 1). A list
        // goes on nearly every call, so a mark lasts the call that applied its reply: a later
        // commitment needing what the reply lacked asks once more, then reads `NotServed`.
        if released {
            self.answered.retain(|k| !matches!(k, RequestKey::QrInfo(_)));
        }

        let mut sets = HashSet::new();
        let mut held = HashSet::new();
        for l in self.lists.values() {
            if sets.insert(Arc::as_ptr(&l.quorums)) {
                held.extend(l.quorums.values().copied());
            }
        }
        // A reported commitment is in a set within the horizon, so held.
        let r = cfg.r();
        // One step 5 never ran on answers nothing once no set holds it (validator D37 r1 m1).
        self.records.retain(|hash, record| {
            held.contains(hash)
                || (record.state.is_some()
                    && record.replaced.is_some_and(|at| at.saturating_add(r) >= tip.height))
        });
        let mut bases = Vec::with_capacity(self.records.len());
        for r in self.records.values() {
            if let Some(base) = known_height(chain, *r.commitment.quorum_hash.as_bytes())? {
                bases.push((r.commitment.llmq_type.to_base(), base));
            }
        }
        self.prune_rows(chain, cfg, &bases)?;
        let oldest = bases.iter().map(|(_, base)| *base).min();
        self.prune_bookkeeping(chain, bookkeeping_floor(cfg, tip.height, oldest))?;
        // A record's row names the sets for its `held_to` only where `load` would read the same
        // from the lists it stores (D48): retention keeps a dropped list's height, or none for a
        // record only a list kept below the horizon holds (Opus s5 r1 m1).
        let newest = newest_holders(chain, tip.id, &self.lists)?;
        for (hash, r) in &mut self.records {
            r.held_by_sets = r.replaced.is_none() && r.held_to == newest.get(hash).copied();
        }
        self.publish(chain, tip)?;
        Ok(())
    }

    /// Drops the rotation rows no step 5 reads (BL-379, D45): kept are, for each held commitment
    /// of a rotated type (`bases`, each held commitment's type and base height), its cycle c's
    /// rows at c − C, c − 2C and c − 3C (the quarters LQ1 reads) and at c (the next cycle's
    /// reads, with c − C and c − 2C), and for each kept row still a
    /// candidate, its own three (D20's confirmation reads them). A row on a block the chain does
    /// not know goes. Rows go with the `qrinfo` marks, as lists do: a commitment step 5 reads
    /// later asks again rather than reading `NotServed`.
    fn prune_rows(
        &mut self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        bases: &[(u8, u32)],
    ) -> Result<(), AncestryError> {
        #[cfg(test)]
        if self.keep_every_list {
            return Ok(());
        }
        if self.rows.is_empty() {
            return Ok(());
        }
        let params = cfg.params();
        let rotation = |t: u8| {
            params.consensus.llmq(LlmqType::from_base(t)).filter(|lp| lp.use_rotation).map(interval)
        };
        let quarters =
            |t: u8, cycle: u32, c: u32, ks: RangeInclusive<u32>, keep: &mut HashSet<_>| {
                keep.extend(ks.filter_map(|k| cycle.checked_sub(k * c)).map(|h| (t, h)));
            };
        let mut keep = HashSet::new();
        for (t, base) in bases {
            if let Some(c) = rotation(*t) {
                quarters(*t, base - base % c, c, 0..=3, &mut keep);
            }
        }
        let heights = self
            .rows
            .keys()
            .map(|key| Ok((*key, known_height(chain, key.1)?)))
            .collect::<Result<HashMap<_, _>, AncestryError>>()?;
        let mut candidates = HashSet::new();
        for (key, row) in &self.rows {
            if let (HeldRow::Candidates(_), Some(h)) = (row, heights[key])
                && keep.contains(&(key.0, h))
                && let Some(c) = rotation(key.0)
            {
                quarters(key.0, h, c, 1..=3, &mut candidates);
            }
        }
        keep.extend(candidates);
        let before = self.rows.len();
        self.rows.retain(|key, _| heights[key].is_some_and(|h| keep.contains(&(key.0, h))));
        if self.rows.len() < before {
            self.answered.retain(|k| !matches!(k, RequestKey::QrInfo(_)));
        }
        Ok(())
    }

    /// Forgets the request bookkeeping (`answered`, `failed`, `dropped`, `retries`) for blocks
    /// nothing held asks for again (BL-382, D47): one the chain does not know, or below `floor`
    /// ([`bookkeeping_floor`]), unless outstanding or asked by a waiting lookup. A need below it
    /// (a lookup's fetch of an old set) asks again: one more request, never a wrong state.
    fn prune_bookkeeping(&mut self, chain: &dyn MnChain, floor: u32) -> Result<(), AncestryError> {
        let asked: HashSet<RequestKey> = self.lookups.iter().flat_map(LookupWait::keys).collect();
        let mut stale = HashSet::new();
        let keys = self.answered.iter().chain(self.failed.keys()).chain(&self.dropped);
        for key in keys.chain(self.retries.keys()) {
            if self.requests.contains_key(key) || asked.contains(key) || stale.contains(key) {
                continue;
            }
            let (RequestKey::MnListDiff(hash) | RequestKey::QrInfo(hash)) = key;
            let live = known_height(chain, *hash)?.is_some_and(|height| height >= floor);
            if !live {
                stale.insert(*key);
            }
        }
        if !stale.is_empty() {
            self.answered.retain(|k| !stale.contains(k));
            self.failed.retain(|k, _| !stale.contains(k));
            self.dropped.retain(|k| !stale.contains(k));
            self.retries.retain(|k, _| !stale.contains(k));
        }
        Ok(())
    }

    /// `cl_base` becomes `best_cl` once its block is an accepted header at the lock's height, as
    /// Core's `bestChainLockWithKnownBlock` (`chainlock.cpp` rc.3:117-127, 156-171; DEC-387 Q4,
    /// DEC-412).
    fn note_cl_base(&mut self, chain: &dyn MnChain) -> Result<(), AncestryError> {
        let Some((height, hash)) =
            self.best_cl.filter(|(height, _)| self.cl_base.is_none_or(|(h, _)| *height > h))
        else {
            return Ok(());
        };
        let Some(id) = chain.lookup(&hash)? else {
            return Ok(());
        };
        let at = match chain.ancestry().header(id) {
            Ok(header) => header.height,
            Err(AncestryError::BelowFloor {
                ..
            }) => return Ok(()),
            Err(e) => return Err(e),
        };
        // Else Core logs "height of CLSIG … does not match" and keeps the base (Opus s3 r2 n2).
        if at == height {
            self.cl_base = self.best_cl;
        }
        Ok(())
    }

    /// The lists D29 keeps whole: the newest one held whole on the active chain, and the host's
    /// last diff's target, the next tip diff's base; the work blocks step 5 reads for each used
    /// type's cycles whose mining window is open at the newest, and the newest rotated cycle's
    /// k = 0…2, which the next cycle's members read with its own (DEC-387); those an open
    /// commitment reads; the bases of the outstanding requests; and the newest held whole at or
    /// below the newest verified ChainLock whose block is a header, on that block's chain, the
    /// reorg base (DEC-387, DEC-412).
    fn kept_lists(
        &self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        tip: BlockId,
    ) -> Result<HashSet<BlockId>, MnReject> {
        // `retain` cleared a base off the active chain (D34).
        let mut keep: HashSet<BlockId> = self.last_diff.into_iter().collect();
        #[cfg(test)]
        {
            keep.extend(&self.pinned);
            if self.keep_every_list {
                keep.extend(self.lists.keys());
            }
        }
        // The newest list held whole at or below `height` on `end`'s chain: authenticated from
        // A_q, where ChainLocks start (DIP8), and before it the list a tip diff bases on.
        let newest = |end: BlockId, height: u32| -> Result<_, AncestryError> {
            for (h, id) in self.by_height.range(..=(height, BlockId(u32::MAX))).rev() {
                if self.lists[id].full.is_some() && through(chain, end, *id, *h)? {
                    return Ok(Some((*id, *h)));
                }
            }
            Ok(None)
        };
        let top = newest(tip, u32::MAX)?;
        keep.extend(top.map(|(id, _)| id));
        // The ChainLock's block is on every chain the host follows (step 7).
        if let Some((height, hash)) = self.cl_base
            && let Some(cl) = chain.lookup(&hash)?
        {
            keep.extend(newest(cl, height)?.map(|(id, _)| id));
        }

        if let Some((_, top)) = top {
            keep_ancestors(chain, tip, window_lists(cfg, top), &mut keep)?;
        }

        let params = cfg.params();
        // Not yet authenticated (below A_q) counts: step 5 runs once a rooted set holds it. So
        // does a disputed one: a new row reopens it, and reads them without a fetch (D20).
        let open = |r: &&Record| {
            r.state.is_none()
                || r.waits()
                || r.state == Some(QuorumState::Unavailable(Unavailable::RowsDisputed))
        };
        for r in self.records.values().filter(open) {
            let qc = &r.commitment;
            let Some(lp) =
                params.consensus.llmq(qc.llmq_type).filter(|_| cfg.is_used(qc.llmq_type))
            else {
                continue;
            };
            let Some(base) = chain.lookup(&BlockHash::from(*qc.quorum_hash.as_bytes()))? else {
                continue;
            };
            let height = match chain.ancestry().header(base) {
                Ok(header) => header.height,
                Err(AncestryError::BelowFloor {
                    ..
                }) => continue,
                Err(e) => return Err(e.into()),
            };
            keep_ancestors(chain, base, member_lists(lp, params, height), &mut keep)?;
        }

        // A lookup's fetched targets, while step 5 decides what they hold (D37).
        for wait in &self.lookups {
            keep.extend(wait.targets.iter().map(|(_, id)| *id));
        }
        for request in self.requests.values() {
            let bases = match request {
                MnRequest::MnListDiff {
                    base,
                    ..
                } => base.as_slice(),
                MnRequest::QrInfo {
                    bases,
                    ..
                } => bases.as_slice(),
            };
            for base in bases {
                if let Some(id) = chain.lookup(base)? {
                    keep.insert(id);
                }
            }
        }
        Ok(keep)
    }

    /// Each row's `replaced` from the authenticated sets held on the active chain at or above
    /// `horizon`, oldest first (D31): a commitment one set holds and the next does not was
    /// replaced at the next's height; one the newest holds is not. A row no such set holds keeps
    /// its value: the lists kept below the horizon are far apart, and would set a gap's top. But
    /// one never marked, which the newest such set below holds (read before retention drops it)
    /// and the first set above does not (a gap across the horizon), was replaced by that set's
    /// height at the latest: the gap's top, set once (validator r2 finding 2, r3). Each row's
    /// `mined` is the lowest of those sets holding it, or the lower mark before when the lowest
    /// of them holds it and that mark's set is on the active chain. A row held only by sets off
    /// the active chain at or above `horizon` is unmarked: no branch the chain left answers the
    /// lookup, and retention drops it with its sets (validator r6 M1′, M2′).
    fn mark_replaced(
        &mut self,
        chain: &dyn MnChain,
        tip: BlockId,
        horizon: u32,
    ) -> Result<(), MnReject> {
        let mut below = None;
        for (height, id) in self.by_height.range(..(horizon, BlockId(0))).rev() {
            let l = &self.lists[id];
            if l.authenticated && through(chain, tip, *id, *height)? {
                below = Some((*height, *id, l.hash, &l.quorums));
                break;
            }
        }
        let mut replaced: HashMap<[u8; 32], Option<u32>> = HashMap::new();
        let mut mined: HashMap<[u8; 32], SetMark> = HashMap::new();
        let mut held_to: HashMap<[u8; 32], u32> = HashMap::new();
        let mut off_chain = HashSet::new();
        let mut first = None;
        let mut prev: Option<&Arc<QuorumSet>> = None;
        if let Some((height, id, block, set)) = below {
            let mark = SetMark {
                id,
                height,
                hash: block,
            };
            mined.extend(set.values().map(|hash| (*hash, mark)));
            held_to.extend(set.values().map(|hash| (*hash, height)));
        }
        for (height, id) in self.by_height.range((horizon, BlockId(0))..) {
            let l = &self.lists[id];
            if !l.authenticated {
                continue;
            }
            if !through(chain, tip, *id, *height)? {
                off_chain.extend(l.quorums.values().copied());
                continue;
            }
            let mark = SetMark {
                id: *id,
                height: *height,
                hash: l.hash,
            };
            for hash in l.quorums.values() {
                mined.entry(*hash).or_insert(mark);
                held_to.insert(*hash, *height);
            }
            match prev {
                None => {
                    first = Some((*height, &l.quorums));
                    replaced.extend(l.quorums.values().map(|hash| (*hash, None)));
                }
                Some(p) if !Arc::ptr_eq(p, &l.quorums) => {
                    for (key, hash) in p.iter() {
                        if l.quorums.get(key) != Some(hash) {
                            replaced.insert(*hash, Some(*height));
                        }
                    }
                    for (key, hash) in l.quorums.iter() {
                        if p.get(key) != Some(hash) {
                            replaced.insert(*hash, None);
                        }
                    }
                }
                Some(_) => {}
            }
            prev = Some(&l.quorums);
        }
        let bottom = below.map(|(height, ..)| height).or(first.map(|(height, _)| height));
        if let (Some((.., below)), Some((height, first))) = (below, first) {
            for (key, hash) in below.iter() {
                let unmarked = self.records.get(hash).is_some_and(|r| r.replaced.is_none());
                if first.get(key) != Some(hash) && unmarked && !replaced.contains_key(hash) {
                    replaced.insert(*hash, Some(height));
                }
            }
        }
        for (hash, at) in replaced {
            if let Some(record) = self.records.get_mut(&hash) {
                record.replaced = at;
            }
        }
        for hash in off_chain.iter().filter(|hash| !mined.contains_key(*hash)) {
            if let Some(record) = self.records.get_mut(hash) {
                record.replaced = None;
                record.mined = None;
                record.held_to = None;
            }
        }
        for (hash, at) in held_to {
            if let Some(record) = self.records.get_mut(&hash) {
                record.held_to = Some(at);
            }
        }
        for (hash, mark) in mined {
            let Some(record) = self.records.get_mut(&hash) else {
                continue;
            };
            // Held at the lowest set read: mined there, or at a lower mark on the active chain
            // (validator r7 m7).
            let lower = match record.mined {
                Some(m) if Some(mark.height) == bottom && m.height < mark.height => {
                    through(chain, tip, m.id, m.height)?.then_some(m)
                }
                _ => None,
            };
            record.mined = Some(lower.unwrap_or(mark));
        }
        Ok(())
    }

    /// Step 5 for every authenticated commitment not decided yet, oldest first, after confirming
    /// the rows LQ1 can compute (D20); again for any a newly confirmed row reopens. A commitment
    /// waiting on a request, or whose request failed, is tried again first: a branch change may
    /// have moved what it needs (D12), as is one whose mining scheme was decided off the active
    /// chain. A request whose block is off the active chain is dropped, and failures off it are
    /// forgotten.
    fn verify_open(&mut self, chain: &dyn MnChain, cfg: &LlmqConfig) -> Result<(), MnReject> {
        let tip = chain.active_tip()?;
        let skip = self.lookup_only(tip.height.saturating_sub(cfg.h_keep()));
        let tip = tip.id;
        self.forget_off_chain_failures(chain, tip)?;
        self.confirm_rows(chain, cfg)?;
        let mut first = true;
        loop {
            let mut open: Vec<(u32, [u8; 32])> = Vec::new();
            let records =
                self.records.iter().filter(|(hash, r)| r.authenticated && !skip.contains(*hash));
            for (hash, r) in records {
                let reopen = match r.state {
                    None => true,
                    Some(_) if r.waits() => first,
                    Some(_) => first && !decided_on(chain, tip, r.decider)?,
                };
                if reopen {
                    open.push((r.first_seen, *hash));
                }
            }
            if open.is_empty() {
                let mut drop = Vec::new();
                for (key, request) in &self.requests {
                    if !on_active_chain(chain, tip, request.block())? {
                        drop.push(*key);
                    }
                }
                for key in drop {
                    self.requests.remove(&key);
                    self.dropped.insert(key);
                }
                return Ok(());
            }
            first = false;
            open.sort_unstable();
            for (_, hash) in open {
                let (attempt, decider, core_row) =
                    self.attempt(&self.records[&hash], chain, cfg)?;
                if let Some((key, row)) = core_row {
                    self.confirm(key, row);
                }
                let state = match attempt {
                    Attempt::Done(state) => state,
                    Attempt::Need(request) | Attempt::Disputed(request)
                        if !on_active_chain(chain, tip, request.block())? =>
                    {
                        QuorumState::Unavailable(Unavailable::OffChain)
                    }
                    Attempt::Need(request) => self.need(request),
                    Attempt::Disputed(request) => self.dispute(request),
                };
                let record = self.records.get_mut(&hash).expect("listed");
                record.state = Some(state);
                record.decider = decider;
            }
        }
    }

    /// Forgets the failed requests whose block is off the active chain at this step 5: that
    /// branch's peers failing says nothing once it is active again, so a reorg back asks again
    /// (D12).
    fn forget_off_chain_failures(
        &mut self,
        chain: &dyn MnChain,
        tip: BlockId,
    ) -> Result<(), MnReject> {
        let mut off = Vec::new();
        for (key, height) in &self.failed {
            let (RequestKey::MnListDiff(hash) | RequestKey::QrInfo(hash)) = key;
            if !on_active_chain(chain, tip, (*hash, *height))? {
                off.push(*key);
            }
        }
        for key in off {
            self.failed.remove(&key);
        }
        Ok(())
    }

    /// Confirms each candidate row LQ1 can compute from the held lists and Core's rows, oldest
    /// cycle first, so one confirmation feeds the next (D20). A computation that cannot run (a
    /// list or row not held) leaves the candidates; nothing is requested for it.
    fn confirm_rows(&mut self, chain: &dyn MnChain, cfg: &LlmqConfig) -> Result<(), MnReject> {
        let mut open = Vec::new();
        for (key, row) in &self.rows {
            if matches!(row, HeldRow::Candidates(_))
                && let Some(id) = chain.lookup(&BlockHash::from(key.1))?
            {
                open.push((chain.ancestry().header(id)?.height, id, *key));
            }
        }
        open.sort_unstable();
        for (height, cycle, key) in open {
            if let Some(row) =
                self.core_row(LlmqType::from_base(key.0), cycle, height, chain, cfg)?
            {
                self.confirm(key, row);
            }
        }
        Ok(())
    }

    /// The row of the rotated cycle at `cycle` LQ1 computes from the held lists and Core's rows
    /// (`GetAllQuorumMembers` stores it), if it can.
    fn core_row(
        &self,
        llmq_type: LlmqType,
        cycle: BlockId,
        height: u32,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<Option<QuorumSnapshot>, MnReject> {
        let params = cfg.params();
        let view = ChainView {
            chain,
        };
        let heights = Heights::new(params, chain);
        let ctx = LlmqCtx::new(params, &view, &heights, bls_ctx_for_block(params, height))
            .expect("LlmqConfig::new checked the domain");
        let src = Source {
            lists: &self.lists,
        };
        let none = HashMap::new();
        let mut rows = Rows::new(&self.rows, &none, false, ctx, chain);
        let result = members_uncached(&ctx, &src, &mut rows, llmq_type, cycle);
        match (heights.take_error(), result) {
            (Some(MnViewError::Storage(s)), _)
            | (_, Err(MemberError::View(MnViewError::Storage(s)))) => {
                Err(MnReject::View(MnViewError::Storage(s)))
            }
            (None, Ok(_)) => Ok(rows.stored.map(|(_, row)| row)),
            _ => Ok(None),
        }
    }

    /// The state of a commitment waiting on `request`: the request registered, or why not.
    fn need(&mut self, request: MnRequest) -> QuorumState {
        let key = request.key();
        if self.failed.contains_key(&key) {
            QuorumState::Unavailable(Unavailable::FetchFailed)
        } else if self.answered.contains(&key) {
            QuorumState::Unavailable(Unavailable::NotServed)
        } else {
            self.dropped.remove(&key);
            self.requests.entry(key).or_insert(request);
            QuorumState::Pending(key)
        }
    }

    /// The state of a commitment step 5 rejected over unconfirmed rows: its cycle's `qrinfo`
    /// asked again, for the host to send to another peer, up to `ROW_RETRIES` times (D20). One a
    /// branch change dropped is asked again without counting a retry: no peer answered it.
    fn dispute(&mut self, request: MnRequest) -> QuorumState {
        let key = request.key();
        if !self.requests.contains_key(&key) {
            if !self.dropped.remove(&key) {
                let tries = self.retries.entry(key).or_insert(0);
                if *tries == ROW_RETRIES {
                    return QuorumState::Unavailable(Unavailable::RowsDisputed);
                }
                *tries += 1;
            }
            self.answered.remove(&key);
            self.failed.remove(&key);
            self.requests.insert(key, request);
        }
        QuorumState::Pending(key)
    }

    /// Step 5: Core's `CFinalCommitment::Verify(..., checkSigs = true)` over SPV's views, the
    /// members computed from the held lists and rows (EV addendum §2.3), under each choice of
    /// the candidate rows it reads until one verifies (D20). With it, Core's row for the
    /// commitment's cycle when LQ1 computed that reading no candidate, and the set that decided
    /// the mining scheme. A final state reached under the same set stands: its inputs are the
    /// same.
    fn attempt(
        &self,
        record: &Record,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<(Attempt, Option<SetMark>, Option<CoreRow>), MnReject> {
        let qc = &record.commitment;
        let params = cfg.params();
        let Some(lp) = params.consensus.llmq(qc.llmq_type).filter(|_| cfg.is_used(qc.llmq_type))
        else {
            return Ok((Attempt::Done(QuorumState::Skipped), None, None));
        };
        let Some(base) = chain.lookup(&BlockHash::from(*qc.quorum_hash.as_bytes()))? else {
            let unknown = QuorumState::Unavailable(Unavailable::UnknownBase);
            return Ok((Attempt::Done(unknown), None, None));
        };
        let (bls, decider) = self.mining_bls(record, base, chain, params)?;
        let bls = match bls {
            Ok(bls) => bls,
            Err(attempt) => return Ok((attempt, decider, None)),
        };
        if let Some(state) = record.state.as_ref().filter(|_| !record.waits())
            && decider == record.decider
        {
            return Ok((Attempt::Done(state.clone()), decider, None));
        }
        let mut choice = HashMap::new();
        let mut run = self.run(record, base, bls, chain, cfg, &choice);
        let core_row = run.stored.take().filter(|_| run.read.is_empty());
        while matches!(
            run.result,
            Err(CommitmentError::Rejected(_) | CommitmentError::BlsUnsupported)
        ) && next_choice(&mut choice, &run.read)
        {
            run = self.run(record, base, bls, chain, cfg, &choice);
        }
        let attempt = match run.result {
            Ok(q) => Attempt::Done(QuorumState::Verified(q)),
            // Rejected over rows no root commits to: not Core's verdict (D20).
            Err(CommitmentError::Rejected(_) | CommitmentError::BlsUnsupported)
                if !run.read.is_empty() =>
            {
                match self.qrinfo_request(lp, base, chain)? {
                    Attempt::Need(request) => Attempt::Disputed(request),
                    attempt => attempt,
                }
            }
            Err(CommitmentError::Rejected(_) | CommitmentError::BlsUnsupported) => {
                Attempt::Done(QuorumState::Unverified)
            }
            Err(CommitmentError::Members(MemberError::Missing(at))) => {
                self.need_list(lp, base, at, chain)?
            }
            Err(CommitmentError::Members(MemberError::View(e))) => {
                self.view_failure(e, lp, base, chain)?
            }
            Err(CommitmentError::Members(MemberError::UnknownLlmqType(_))) => {
                Attempt::Done(QuorumState::Skipped)
            }
        };
        Ok((attempt, decider, core_row))
    }

    /// One run of Core's `Verify` for `record` under `bls`, reading the candidate rows `choice`
    /// names.
    fn run(
        &self,
        record: &Record,
        base: BlockId,
        bls: BlsCtx,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        choice: &HashMap<RowKey, usize>,
    ) -> Run {
        let params = cfg.params();
        let view = ChainView {
            chain,
        };
        let heights = Heights::new(params, chain);
        let ctx =
            LlmqCtx::new(params, &view, &heights, bls).expect("LlmqConfig::new checked the domain");
        let src = Source {
            lists: &self.lists,
        };
        let mut rows = Rows::new(&self.rows, choice, true, ctx, chain);
        let mut effects: Vec<Effect> = Vec::new();
        let result = {
            let members = UncachedMembers::new(ctx, &src, &mut rows);
            verify_quorum(&ctx, &members, &record.commitment, base, SigPath::Mined, &mut effects)
        };
        // A failed deployment read answered `false`: nothing of this run is Core's (`Heights`).
        if let Some(e) = heights.take_error() {
            return Run {
                result: Err(CommitmentError::Members(MemberError::View(e))),
                read: BTreeMap::new(),
                stored: None,
            };
        }
        Run {
            result,
            read: rows.read.into_inner(),
            stored: rows.stored,
        }
    }

    /// The BLS scheme of the block that mined `record`'s commitment (D12), and the set that
    /// decided it when a branch did: Core's `ProcessBlock` checks it under that block's. That
    /// block lies between the quorum's base and the lowest authenticated set holding it on the
    /// active chain (`first_sight`); where those straddle V19, the authenticated set at V19 − 1
    /// on that chain decides. It lacks the commitment (else that set would be the lowest), so the
    /// commitment was mined at or above V19 on every chain through it.
    fn mining_bls(
        &self,
        record: &Record,
        base: BlockId,
        chain: &dyn MnChain,
        params: &ChainParams,
    ) -> Result<(Result<BlsCtx, Attempt>, Option<SetMark>), MnReject> {
        let v19 = params.consensus.v19_height.to_u32();
        if chain.ancestry().header(base)?.height >= v19 {
            return Ok((Ok(bls_ctx_for_block(params, record.first_seen)), None));
        }
        let sight = self.first_sight(record, chain)?;
        let (first_seen, seen_at) = match sight {
            Some(d) => (d.height, d.id),
            None => (record.first_seen, chain.active_tip()?.id),
        };
        if first_seen < v19 {
            return Ok((Ok(bls_ctx_for_block(params, first_seen)), sight));
        }
        let at = match ancestor_at(chain, seen_at, v19 - 1) {
            Ok(at) => at,
            Err(AncestryError::BelowFloor {
                height,
            }) => {
                let below = Unavailable::BelowFloor {
                    height,
                };
                return Ok((Err(Attempt::Done(QuorumState::Unavailable(below))), sight));
            }
            Err(e) => return Err(e.into()),
        };
        let decider = Some(SetMark {
            id: at,
            height: v19 - 1,
            hash: chain.ancestry().header(at)?.hash,
        });
        let scheme = match self.lists.get(&at) {
            Some(l) if l.authenticated => Ok(bls_ctx_for_block(params, v19)),
            Some(_) => Err(Attempt::Done(QuorumState::Unavailable(Unavailable::MiningScheme))),
            None => Err(Attempt::Need(self.diff_request(at, chain)?)),
        };
        Ok((scheme, decider))
    }

    /// The lowest authenticated held set containing `record`'s commitment, on the active chain
    /// where one there does: the chain whose blocks Core serves (`evo/smldiff.cpp` rc.3:177-180),
    /// so a branch change moves the set at V19 − 1 asked for (D12).
    fn first_sight(
        &self,
        record: &Record,
        chain: &dyn MnChain,
    ) -> Result<Option<SetMark>, MnReject> {
        let qc = &record.commitment;
        let key = (qc.llmq_type.to_base(), *qc.quorum_hash.as_bytes());
        let hash = commitment_hash(qc);
        let tip = chain.active_tip()?.id;
        let mut best: Option<(bool, u32, BlockId, BlockHash)> = None;
        for (id, l) in &self.lists {
            if !l.authenticated || l.quorums.get(&key) != Some(&hash) {
                continue;
            }
            let off_active = match chain.ancestry().ancestor(tip, l.height) {
                Ok(a) => a != Some(*id),
                Err(AncestryError::BelowFloor {
                    ..
                }) => true,
                Err(e) => return Err(e.into()),
            };
            let rank = (off_active, l.height, *id, l.hash);
            if best.is_none_or(|b| rank < b) {
                best = Some(rank);
            }
        }
        Ok(best.map(|(_, height, id, hash)| SetMark {
            id,
            height,
            hash,
        }))
    }

    fn view_failure(
        &self,
        e: MnViewError,
        lp: &LlmqParams,
        base: BlockId,
        chain: &dyn MnChain,
    ) -> Result<Attempt, MnReject> {
        match e {
            MnViewError::BelowFloor {
                height,
            } => Ok(Attempt::Done(QuorumState::Unavailable(Unavailable::BelowFloor {
                height,
            }))),
            MnViewError::RowNotHeld {
                ..
            } => self.qrinfo_request(lp, base, chain),
            MnViewError::Storage(_) => Err(MnReject::View(e)),
        }
    }

    /// A list the member computation needs: a rotated commitment's come with its cycle's
    /// `qrinfo`, any other's with a diff to it.
    fn need_list(
        &self,
        lp: &LlmqParams,
        base: BlockId,
        at: BlockId,
        chain: &dyn MnChain,
    ) -> Result<Attempt, MnReject> {
        if lp.use_rotation {
            self.qrinfo_request(lp, base, chain)
        } else {
            Ok(Attempt::Need(self.diff_request(at, chain)?))
        }
    }

    /// `getqrinfo` for the cycle of the rotated quorum based at `base`; `Unavailable` when the
    /// deepest list Core's reply carries lies below the floor, as no backfill fetches it (SB0
    /// §5.2). That list is the oldest target cycle's work block, H − 3C − 8, or an extra cycle's
    /// below it: exact when the authenticated set at the request block is held, else a reply
    /// reaching below the floor is refused (D15).
    fn qrinfo_request(
        &self,
        lp: &LlmqParams,
        base: BlockId,
        chain: &dyn MnChain,
    ) -> Result<Attempt, MnReject> {
        let base_height = chain.ancestry().header(base)?.height;
        let request_height = cycle_base_height(lp, base_height);
        let cycle = interval(lp);
        let floor = chain.ancestry().floor();
        let below = |deepest: i64| {
            u32::try_from(deepest).ok().filter(|h| *h < floor).map(|height| {
                Attempt::Done(QuorumState::Unavailable(Unavailable::BelowFloor {
                    height,
                }))
            })
        };
        let oldest = i64::from(request_height) - 3 * i64::from(cycle);
        if let Some(unavailable) = below(oldest - i64::from(WORK_DIFF_DEPTH)) {
            return Ok(unavailable);
        }
        let request = ancestor_at(chain, base, request_height)?;
        // The targets are no deeper than `oldest`: the extras' depth is read without them.
        let extras =
            self.extra_cycles(request, &[], cycle, lp.llmq_type, &BTreeMap::new(), chain)?;
        if let Some(deepest) = extras.and_then(|e| e.first().copied())
            && let Some(unavailable) = below(deepest.min(oldest) - i64::from(WORK_DIFF_DEPTH))
        {
            return Ok(unavailable);
        }
        let mut bases = self.held_below(chain, request, request_height, QRINFO_BASES)?;
        bases.reverse();
        Ok(Attempt::Need(MnRequest::QrInfo {
            request: chain.ancestry().header(request)?.hash,
            request_height,
            bases,
        }))
    }

    /// `getmnlistd` to `at` from the newest held list below it on its chain.
    fn diff_request(&self, at: BlockId, chain: &dyn MnChain) -> Result<MnRequest, AncestryError> {
        let (base, target, target_height) = self.fetch(at, chain)?;
        Ok(MnRequest::MnListDiff {
            base,
            target,
            target_height,
        })
    }

    /// The one request path's (base, target, target height) for the set at `at`: the newest
    /// authenticated held list below it on its chain as the base, else none.
    fn fetch(
        &self,
        at: BlockId,
        chain: &dyn MnChain,
    ) -> Result<(Option<BlockHash>, BlockHash, u32), AncestryError> {
        // Every caller fetches a block holding no authenticated whole list, so none is its own
        // base (validator s6 n2).
        debug_assert!(!self.lists.get(&at).is_some_and(|l| l.authenticated && l.full.is_some()));
        let header = chain.ancestry().header(at)?;
        let base = match header.height.checked_sub(1) {
            Some(below) => self.held_below(chain, at, below, 1)?.pop(),
            None => None,
        };
        Ok((base, header.hash, header.height))
    }

    /// The hashes of up to `n` held authenticated lists at or below `height` on `at`'s chain,
    /// newest first. An unauthenticated set is no base: a peer may have changed it, and Core's
    /// delta to a rooted target would then fail its root (D19).
    fn held_below(
        &self,
        chain: &dyn MnChain,
        at: BlockId,
        height: u32,
        n: usize,
    ) -> Result<Vec<BlockHash>, AncestryError> {
        let mut out = Vec::new();
        for (id, l) in
            self.newest_first(height).filter(|(_, l)| l.authenticated && l.full.is_some())
        {
            if out.len() == n {
                break;
            }
            match chain.ancestry().ancestor(at, l.height) {
                Ok(Some(a)) if a == id => out.push(l.hash),
                Ok(_)
                | Err(AncestryError::BelowFloor {
                    ..
                }) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    // ---- the horizon and coverage (SB0 §4.3.1, §5.2.1) ---------------------------------------

    /// Where the authenticated set at `s` comes from (D24): classified without a fetch below
    /// A_q, below tip − H_keep and off the active chain; else held, or a fetch through the one
    /// request path (the nearest authenticated held list below as the base).
    pub fn set_at(
        &self,
        s: BlockId,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<SetAt, MnViewError> {
        let tip = chain.active_tip()?;
        let header = match chain.ancestry().header(s) {
            Ok(header) => header,
            Err(AncestryError::BelowFloor {
                height,
            }) => return Ok(classify(height, tip.height, cfg).unwrap_or(SetAt::OutsideHorizon)),
            Err(e) => return Err(e.into()),
        };
        if let Some(class) = classify(header.height, tip.height, cfg) {
            return Ok(class);
        }
        // Off the active chain, or above its tip.
        if !through(chain, tip.id, s, header.height)? {
            return Ok(SetAt::OutsideHorizon);
        }
        if self.has_authenticated_set(s) {
            return Ok(SetAt::Held);
        }
        let (base, target, target_height) = self.fetch(s, chain)?;
        Ok(SetAt::Fetch {
            base,
            target,
            target_height,
        })
    }

    /// SB0 §4.3.1's coverage (D25): required(T), for every used type, of the authenticated sets
    /// at the domain's two ends, max(tip − H_keep, A_q) and tip − 8. An element below the anchor
    /// is uncovered: context entries cover nothing. An empty domain (tip − 8 below A_q) is
    /// covered with no request. An end not held is asked for through [`Self::set_at`], its
    /// failure or unserved reply forgotten, as the host calls this when it retries and bounds its
    /// tries; the answer is `Pending` unless an element is already uncovered.
    pub fn coverage(
        &mut self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<Coverage, MnViewError> {
        let tip = chain.active_tip()?;
        let Some(newest) = tip.height.checked_sub(SIGN_HEIGHT_OFFSET).filter(|h| *h >= cfg.a_q())
        else {
            return Ok(Coverage::Covered);
        };
        let oldest = tip.height.saturating_sub(cfg.h_keep()).max(cfg.a_q());
        let mut below = Below {
            anchor: chain.anchor(),
            ..Below::default()
        };
        let mut pending = false;
        for height in BTreeSet::from([oldest, newest]) {
            if below.uncovered(height) {
                continue;
            }
            let at = chain.ancestry().ancestor(tip.id, height)?.expect("at or below the tip");
            match self.set_at(at, chain, cfg)? {
                SetAt::Held => {
                    for (t, quorum_hash) in self.lists[&at].quorums.keys() {
                        self.required(
                            LlmqType::from_base(*t),
                            quorum_hash,
                            chain,
                            cfg,
                            &mut below,
                        )?;
                    }
                }
                SetAt::Fetch {
                    base,
                    target,
                    target_height,
                } => {
                    let request = MnRequest::MnListDiff {
                        base,
                        target,
                        target_height,
                    };
                    let key = request.key();
                    self.failed.remove(&key);
                    self.answered.remove(&key);
                    self.need(request);
                    pending = true;
                }
                // At or above A_q (so DIP3), at or above tip − H_keep, on the active chain.
                other => unreachable!("an end of the domain is {other:?}"),
            }
        }
        Ok(match below.coverage() {
            Coverage::Covered if pending => Coverage::Pending,
            coverage => coverage,
        })
    }

    /// Resolves required(T)'s elements for one commitment of a set (SB0 §4.3.1): its base by
    /// hash, and its work blocks by height below it. A `Skipped` type adds none.
    fn required(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &[u8; 32],
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        below: &mut Below,
    ) -> Result<(), MnViewError> {
        let params = cfg.params();
        let Some(lp) = params.consensus.llmq(llmq_type).filter(|_| cfg.is_used(llmq_type)) else {
            return Ok(());
        };
        let Some(base) = chain.lookup(&BlockHash::from(*quorum_hash))? else {
            below.unknown = true;
            return Ok(());
        };
        let height = match chain.ancestry().header(base) {
            Ok(header) => header.height,
            Err(AncestryError::BelowFloor {
                height,
            }) => height,
            Err(e) => return Err(e.into()),
        };
        below.uncovered(height);
        // Each is an ancestor of the base, so at or above the anchor it is an accepted entry;
        // below a base below the anchor it names a deeper height.
        for h in member_lists(lp, params, height) {
            below.uncovered(h);
        }
        Ok(())
    }

    /// Raises a later coverage miss (D26) for each used commitment step 5 left `BelowFloor` or
    /// `UnknownBase` that an authenticated set within the horizon on the active chain holds, once
    /// while that lasts: a commitment that leaves the miss state, or that no such set holds any
    /// more, may raise again (DEC-377). Returns the events and the commitments to keep as
    /// reported, which `finish` keeps only once the call can no longer stop: a store fault raises
    /// nothing twice.
    fn raise_misses(
        &self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<(Vec<MnEvent>, HashSet<[u8; 32]>), MnReject> {
        // Held in an authenticated set, so authenticated.
        let mut missed: Vec<_> = self
            .records
            .iter()
            .filter_map(|(hash, r)| {
                let missing = match r.state {
                    Some(QuorumState::Unavailable(Unavailable::BelowFloor {
                        height,
                    })) => Some(height),
                    Some(QuorumState::Unavailable(Unavailable::UnknownBase)) => None,
                    _ => return None,
                };
                Some((r.first_seen, *hash, missing))
            })
            .collect();
        missed.sort_unstable();
        let tip = chain.active_tip()?;
        let horizon = tip.height.saturating_sub(cfg.h_keep());
        let (mut events, mut reported) = (Vec::new(), HashSet::new());
        for (_, hash, missing) in missed {
            let qc = &self.records[&hash].commitment;
            let (llmq_type, quorum_hash) = (qc.llmq_type, qc.quorum_hash);
            let key = (llmq_type.to_base(), *quorum_hash.as_bytes());
            let mut held = false;
            for (id, l) in self.newest_first(tip.height) {
                if l.height < horizon {
                    break;
                }
                if l.authenticated
                    && l.quorums.get(&key) == Some(&hash)
                    && through(chain, tip.id, id, l.height)?
                {
                    held = true;
                    break;
                }
            }
            if !held {
                continue;
            }
            if !self.reported.contains(&hash) {
                events.push(MnEvent::CoverageMiss {
                    llmq_type,
                    quorum_hash,
                    anchor: chain.anchor(),
                    missing,
                });
            }
            reported.insert(hash);
        }
        Ok((events, reported))
    }

    // ---- step 6 ------------------------------------------------------------------------------

    /// Step 6 for a `clsig`: SB8's `verify_chainlock` over the held sets. A lock whose selection
    /// start block is above the header tip is held for [`Self::take_pending_chainlock`]; one
    /// whose set is not held requests it. Step 7 (`HeaderChain::apply_chainlock`) is the host's.
    pub fn verify_chainlock(
        &mut self,
        clsig: &ClSig,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<VerifiedChainLock, LockOutcome> {
        let tip = chain.active_tip().map_err(stopped)?;
        if start_above_tip(clsig.height, tip.height) {
            // The lowest waits (D18): a forged lock far above the tip never holds the slot.
            if self.pending_cl.as_ref().is_none_or(|p| clsig.height < p.height) {
                self.pending_cl = Some(clsig.clone());
            }
            return Err(LockOutcome::Pending);
        }
        let params = cfg.params();
        let schemes = LockSchemes::both(bls_ctx_for_block(params, tip.height + 1));
        let verified =
            self.with_scan(chain, cfg, tip, |view| verify_chainlock(params, view, schemes, clsig))?;
        if let Ok(height) = u32::try_from(clsig.height)
            && self.best_cl.is_none_or(|(h, _)| height > h)
        {
            // The lock it replaces is the reorg base while this one's block is yet to come, once
            // its own came; retention notes this one when its block comes (DEC-412).
            self.note_cl_base(chain).map_err(stopped)?;
            self.best_cl = Some((height, clsig.block_hash));
            // Readers see the newest lock now, not at the next retention (D39).
            Arc::make_mut(&mut self.published).best_chainlock = self.best_cl;
        }
        Ok(verified)
    }

    /// Step 6 for an `isdlock`: SB8's `verify_islock` over the held sets.
    pub fn verify_islock(
        &mut self,
        islock: &IsdLock,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<VerifiedIsLock, LockOutcome> {
        let tip = chain.active_tip().map_err(stopped)?;
        let params = cfg.params();
        let schemes = IslockSchemes::both(bls_ctx_for_block(params, tip.height + 1));
        self.with_scan(chain, cfg, tip, |view| verify_islock(params, view, schemes, islock))
    }

    /// The held ChainLock, once headers reach its selection start block.
    pub fn take_pending_chainlock(
        &mut self,
        chain: &dyn MnChain,
    ) -> Result<Option<ClSig>, LockOutcome> {
        let tip = chain.active_tip().map_err(stopped)?;
        let ready =
            self.pending_cl.as_ref().is_some_and(|p| !start_above_tip(p.height, tip.height));
        Ok(if ready {
            self.pending_cl.take()
        } else {
            None
        })
    }

    fn with_scan<T>(
        &mut self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        tip: BlockRef,
        check: impl FnOnce(&ScanView<'_>) -> Result<T, LockError<MnViewError>>,
    ) -> Result<T, LockOutcome> {
        let view = ScanView::new(self, chain, cfg, tip);
        let result = check(&view);
        let needs = view.needs.into_inner();
        for at in needs {
            let request = self.diff_request(at, chain).map_err(|e| match e {
                AncestryError::Storage(s) => LockOutcome::Stopped(s),
                AncestryError::BelowFloor {
                    ..
                } => LockOutcome::Rejected(LockReject::QuorumUnavailable),
            })?;
            self.need(request);
        }
        result.map_err(|e| match e {
            LockError::Rejected(r) => LockOutcome::Rejected(r),
            LockError::BlsUnsupported => LockOutcome::BlsUnsupported,
            LockError::UnknownLlmqType(t) => LockOutcome::UnknownType(t),
            LockError::View(MnViewError::Storage(s)) => LockOutcome::Stopped(s),
            LockError::View(MnViewError::BelowFloor {
                ..
            }) => LockOutcome::Rejected(LockReject::OutsideHorizon),
            LockError::View(_) => LockOutcome::Rejected(LockReject::QuorumUnavailable),
        })
    }
}

/// The next choice of candidate rows over the keys `read`, counting through them in key order;
/// `false` after the last.
fn next_choice(choice: &mut HashMap<RowKey, usize>, read: &BTreeMap<RowKey, usize>) -> bool {
    for (key, count) in read {
        let c = choice.entry(*key).or_insert(0);
        if *c + 1 < *count {
            *c += 1;
            return true;
        }
        *c = 0;
    }
    false
}

/// Whether a ChainLock's selection start block, `SIGN_HEIGHT_OFFSET` below its height, is above the
/// header tip.
fn start_above_tip(cl_height: i32, tip_height: u32) -> bool {
    i64::from(cl_height) - i64::from(SIGN_HEIGHT_OFFSET) > i64::from(tip_height)
}

fn stopped(e: AncestryError) -> LockOutcome {
    match e {
        AncestryError::Storage(s) => LockOutcome::Stopped(s),
        AncestryError::BelowFloor {
            ..
        } => LockOutcome::Rejected(LockReject::OutsideHorizon),
    }
}

/// The held sets as SB8's [`QuorumScanView`] (SB0 §5.2 step 6): Core's `ScanQuorums` at a start
/// block is the authenticated set at its cycle-adjusted store block (D13), in Core's order.
struct ScanView<'a> {
    engine: &'a MasternodeListEngine,
    chain: &'a dyn MnChain,
    cfg: &'a LlmqConfig,
    tip: BlockRef,
    /// Store blocks whose set a scan needed and the engine does not hold.
    needs: RefCell<Vec<BlockId>>,
}

impl<'a> ScanView<'a> {
    fn new(
        engine: &'a MasternodeListEngine,
        chain: &'a dyn MnChain,
        cfg: &'a LlmqConfig,
        tip: BlockRef,
    ) -> Self {
        ScanView {
            engine,
            chain,
            cfg,
            tip,
            needs: RefCell::new(Vec::new()),
        }
    }

    /// The status a lock check reads for `record`: none for a state whose mining scheme was
    /// decided off the active chain, until step 5 runs again (D12).
    fn status(&self, record: &Record) -> Result<QuorumStatus, MnViewError> {
        Ok(if decided_on(self.chain, self.tip.id, record.decider)? {
            record.scan_status()
        } else {
            QuorumStatus::Unavailable
        })
    }

    /// Core's scan from the set, or `None` when the set cannot give Core's.
    fn entries(
        &self,
        lp: &LlmqParams,
        set: &QuorumSet,
        count: usize,
    ) -> Result<Option<Vec<ScannedQuorum>>, MnViewError> {
        let mut found = Vec::new();
        for ((t, quorum_hash), hash) in set {
            if *t != lp.llmq_type.to_base() {
                continue;
            }
            let Some(base) = self.chain.lookup(&BlockHash::from(*quorum_hash))? else {
                return Ok(None);
            };
            let height = self.chain.ancestry().header(base)?.height;
            let Some(record) = self.engine.records.get(hash) else {
                return Ok(None);
            };
            let quorum_hash = QuorumHash::from(*quorum_hash);
            let index = record.commitment.quorum_index.unwrap_or(0);
            let entry = match self.status(record)? {
                QuorumStatus::Verified(q) => ScannedQuorum::verified(base, q),
                QuorumStatus::Unverified => ScannedQuorum::unverified(base, quorum_hash, index),
                QuorumStatus::Unavailable => ScannedQuorum::unavailable(base, quorum_hash, index),
            };
            found.push((height, index, entry));
        }
        if lp.use_rotation {
            // Core's rotated scan takes each index's newest mined commitment in index order, then
            // each index's second newest (`GetMinedCommitmentsIndexedUntilBlock`,
            // `llmq/blockprocessor.cpp` rc.3:730-816); the set is the first round. They agree when
            // every index has a commitment, and when none has (Core's first round is empty and it
            // stops); not when some lack one (D10).
            found.sort_by_key(|(_, index, _)| *index);
            if !found.is_empty() && found.len() != count
                || found
                    .iter()
                    .enumerate()
                    .any(|(i, (_, index, _))| usize::try_from(*index) != Ok(i))
            {
                return Ok(None);
            }
        } else {
            // Newest mined first (`GetMinedCommitmentsUntilBlock`); a type mines in base order.
            found.sort_by_key(|f| std::cmp::Reverse(f.0));
            found.truncate(count);
        }
        Ok(Some(found.into_iter().map(|(.., e)| e).collect()))
    }
}

impl QuorumScanView for ScanView<'_> {
    type Error = MnViewError;

    fn tip_height(&self) -> u32 {
        self.tip.height
    }

    fn scan(&self, llmq_type: LlmqType, start_height: u32) -> Result<ScanLookup, MnViewError> {
        // SB0 §5.2.1, before any set is read (D24): no request for a start outside the domain.
        match classify(start_height, self.tip.height, self.cfg) {
            Some(SetAt::NoQuorum) => return Ok(ScanLookup::Held(QuorumScan::default())),
            Some(SetAt::NoRoot) => return Ok(ScanLookup::Unavailable),
            Some(_) => return Ok(ScanLookup::OutsideHorizon),
            None => {}
        }
        let start = match self.chain.ancestry().ancestor(self.tip.id, start_height) {
            Ok(Some(start)) => start,
            Ok(None)
            | Err(AncestryError::BelowFloor {
                ..
            }) => return Ok(ScanLookup::OutsideHorizon),
            Err(e) => return Err(e.into()),
        };
        let params = self.cfg.params();
        let Some(lp) = params.consensus.llmq(llmq_type) else {
            return Ok(ScanLookup::Unavailable);
        };
        let view = ChainView {
            chain: self.chain,
        };
        let heights = Heights::new(params, self.chain);
        let bls = bls_ctx_for_block(params, start_height);
        let ctx =
            LlmqCtx::new(params, &view, &heights, bls).expect("LlmqConfig::new checked the domain");
        let count = usize::try_from(lp.signing_active_quorum_count).unwrap_or(0);
        let store = scan_store_block(&ctx, llmq_type, start, count);
        let rotation = is_quorum_rotation_enabled(&ctx, lp, start);
        if let Some(e) = heights.take_error() {
            return member_failure(MemberError::View(e));
        }
        let (store, rotation_enabled) = match (store, rotation) {
            (Ok(store), Ok(rotation)) => (store, rotation),
            (Err(e), _) | (_, Err(e)) => return member_failure(e),
        };
        let empty = QuorumScan {
            rotation_enabled,
            entries: Vec::new(),
        };
        let Some(store) = store else {
            return Ok(ScanLookup::Held(empty));
        };
        let store_height = self.chain.ancestry().header(store)?.height;
        // Below DIP3 the type's active set is canonically empty.
        if store_height < self.cfg.dip3_height() {
            return Ok(ScanLookup::Held(empty));
        }
        if rotation_enabled != lp.use_rotation {
            return Ok(ScanLookup::Unavailable);
        }
        // No commitment of the type is mined in (store, start], so the type's entries are the
        // same in every set there (D13): any held authenticated one at or above A_q gives them.
        let lowest = store_height.max(self.cfg.a_q());
        let mut held = None;
        for (id, l) in self.engine.newest_first(start_height) {
            if l.height < lowest {
                break;
            }
            if l.authenticated && through(self.chain, start, id, l.height)? {
                held = Some(l);
                break;
            }
        }
        let Some(list) = held else {
            let target = if store_height == lowest {
                store
            } else {
                self.chain.ancestry().ancestor(start, lowest)?.expect("at or below the start")
            };
            // A held set that is not authenticated is no base (D19); a refetch would not root it.
            if !self.engine.lists.contains_key(&target) {
                self.needs.borrow_mut().push(target);
            }
            return Ok(ScanLookup::Unavailable);
        };
        Ok(match self.entries(lp, &list.quorums, count)? {
            Some(entries) => ScanLookup::Held(QuorumScan {
                rotation_enabled,
                entries,
            }),
            None => ScanLookup::Unavailable,
        })
    }

    fn scan_at_tip(&self, _: LlmqType, _: usize) -> Result<ScanLookup, MnViewError> {
        Ok(ScanLookup::Unavailable)
    }

    fn block_height(&self, hash: &BlockHash) -> Result<Option<u32>, MnViewError> {
        match self.chain.lookup(hash)? {
            Some(id) => Ok(Some(self.chain.ancestry().header(id)?.height)),
            None => Ok(None),
        }
    }

    fn quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<QuorumStatus>, MnViewError> {
        if self.chain.lookup(&BlockHash::from(*quorum_hash.as_bytes()))?.is_none() {
            return Ok(None);
        }
        // Core's `GetQuorum` reads the active chain: the commitment in the newest authenticated
        // held set on it.
        let key = (llmq_type.to_base(), *quorum_hash.as_bytes());
        let mut best = None;
        for (id, l) in self.engine.newest_first(u32::MAX) {
            let Some(hash) = l.quorums.get(&key).filter(|_| l.authenticated) else {
                continue;
            };
            match self.chain.ancestry().ancestor(self.tip.id, l.height) {
                Ok(Some(a)) if a == id => {
                    best = Some(hash);
                    break;
                }
                Ok(_)
                | Err(AncestryError::BelowFloor {
                    ..
                }) => {}
                Err(e) => return Err(e.into()),
            }
        }
        // A record an authenticated set holds is authenticated (`commit`).
        match best.and_then(|hash| self.engine.records.get(hash)) {
            Some(record) => self.status(record).map(Some),
            None => Ok(Some(QuorumStatus::Unavailable)),
        }
    }
}

/// A scan's member-layer failure: no scan, or a stop.
fn member_failure(e: MemberError<MnViewError>) -> Result<ScanLookup, MnViewError> {
    match e {
        MemberError::View(MnViewError::BelowFloor {
            ..
        }) => Ok(ScanLookup::OutsideHorizon),
        MemberError::View(MnViewError::Storage(s)) => Err(MnViewError::Storage(s)),
        _ => Ok(ScanLookup::Unavailable),
    }
}

/// mnxcheck's surface (SB0 §5.4, D9, D40): what the node compares with its own projection at the
/// same block. Node crates only; dash-spv never enables `shadow`, so SPV exposes no member list.
#[cfg(feature = "shadow")]
impl MasternodeListEngine {
    /// The list held whole at `at`, with its set's states.
    pub fn shadow_list(&self, at: BlockId) -> Option<ListView> {
        self.list_view(at)
    }

    /// Core's `GetAllQuorumMembers(llmq_type, base)` over the held lists and Core's rows: the
    /// members LQ1 reconstructs for the quorum based at `base`, in Core's order. A cycle whose
    /// row only a peer's unconfirmed candidate holds is [`MnViewError::RowNotHeld`]: Core reads
    /// no candidate (Sol s3 r1 m1).
    pub fn shadow_members(
        &self,
        llmq_type: LlmqType,
        base: BlockId,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<Vec<dash_evo::sml::SmlEntry>, MemberError<MnViewError>> {
        self.members(chain, cfg, llmq_type, base, false)
    }
}

#[cfg(any(test, feature = "shadow"))]
impl MasternodeListEngine {
    /// Core's `GetAllQuorumMembers` over the held lists and Core's rows, and with `candidates`
    /// a cycle's chosen candidate where no Core row is held, as step 5 reads them.
    pub(crate) fn members(
        &self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        llmq_type: LlmqType,
        base: BlockId,
        candidates: bool,
    ) -> Result<Vec<dash_evo::sml::SmlEntry>, MemberError<MnViewError>> {
        let height = chain.ancestry().header(base).map_err(|e| MemberError::View(e.into()))?.height;
        let params = cfg.params();
        let view = ChainView {
            chain,
        };
        let heights = Heights::new(params, chain);
        let ctx = LlmqCtx::new(params, &view, &heights, bls_ctx_for_block(params, height))
            .expect("in the domain");
        let src = Source {
            lists: &self.lists,
        };
        let none = HashMap::new();
        let mut rows = Rows::new(&self.rows, &none, candidates, ctx, chain);
        let members = members_uncached(&ctx, &src, &mut rows, llmq_type, base);
        // A failed deployment read answered `false`: these are not Core's members (`Heights`;
        // Opus s3 r1 m1, DEC-412).
        if let Some(e) = heights.take_error() {
            return Err(MemberError::View(e));
        }
        Ok(members?.iter().map(|m| m.entry().clone()).collect())
    }
}

#[cfg(test)]
impl MasternodeListEngine {
    /// Holds a list and coinbase as an authenticated diff would.
    pub(crate) fn hold(
        &mut self,
        at: BlockId,
        height: u32,
        hash: BlockHash,
        sml: SmlList,
        coinbase: CoinbaseFacts,
    ) {
        let list = HeldList {
            height,
            hash,
            full: Some(Full {
                sml: Arc::new(sml),
                coinbase,
            }),
            quorums: Arc::default(),
            authenticated: true,
        };
        self.insert_list(at, list);
    }

    /// Holds an authenticated set of `commitments` at `at` with an empty list, as coverage reads
    /// it: the set's keys and each commitment's quorum index.
    pub(crate) fn hold_set(
        &mut self,
        at: BlockId,
        height: u32,
        hash: BlockHash,
        commitments: &[Commitment],
    ) {
        let mut quorums = QuorumSet::new();
        for qc in commitments {
            let leaf = commitment_hash(qc);
            quorums.insert((qc.llmq_type.to_base(), *qc.quorum_hash.as_bytes()), leaf);
            self.records.entry(leaf).or_insert_with(|| Record {
                commitment: Arc::new(qc.clone()),
                first_seen: height,
                authenticated: true,
                state: None,
                decider: None,
                replaced: None,
                mined: None,
                held_to: None,
                held_by_sets: false,
            });
        }
        let list = HeldList {
            height,
            hash,
            full: Some(Full {
                sml: Arc::new(SmlList::new()),
                coinbase: CoinbaseFacts::NoTransactions,
            }),
            quorums: Arc::new(quorums),
            authenticated: true,
        };
        self.insert_list(at, list);
    }

    /// The height of the host's last diff's target, the next tip diff's base (D34).
    pub(crate) fn host_base(&self) -> Option<u32> {
        self.last_diff.map(|id| self.lists[&id].height)
    }

    /// The height of the newest verified ChainLock whose block is a header, the reorg base (D29).
    pub(crate) fn reorg_base(&self) -> Option<u32> {
        self.cl_base.map(|(h, _)| h)
    }

    /// The heights of the served lookups waiting on their fetch, in the order they came (D37).
    pub(crate) fn waiting_lookups(&self) -> Vec<u32> {
        self.lookups.iter().map(|w| w.h).collect()
    }

    /// The heights of the lists held whole, ascending.
    pub(crate) fn whole_lists(&self) -> Vec<u32> {
        self.by_height
            .iter()
            .filter(|(_, id)| self.lists[id].full.is_some())
            .map(|(h, _)| *h)
            .collect()
    }

    /// The heights of the sets held, ascending.
    #[cfg(feature = "testnet-replay")]
    pub(crate) fn held_heights(&self) -> Vec<u32> {
        self.by_height.iter().map(|(h, _)| *h).collect()
    }

    /// Where `qc`'s row was replaced, as retention last saw it (D31).
    pub(crate) fn replaced_at(&self, qc: &Commitment) -> Option<u32> {
        self.records[&commitment_hash(qc)].replaced
    }

    /// `qc`'s mining height as D31 marked it.
    pub(crate) fn mined_at(&self, qc: &Commitment) -> Option<u32> {
        self.records[&commitment_hash(qc)].mined.map(|m| m.height)
    }

    /// Registers `request` as step 6 does for a lock whose scan needs its set (D27).
    pub(crate) fn ask(&mut self, request: MnRequest) {
        self.requests.insert(request.key(), request);
    }

    /// Records that the set at `at` (height `height`) decided `qc`'s mining scheme (D12).
    pub(crate) fn set_decider(
        &mut self,
        qc: &Commitment,
        at: BlockId,
        height: u32,
        hash: BlockHash,
    ) {
        let record = self.records.get_mut(&commitment_hash(qc)).expect("held");
        record.decider = Some(SetMark {
            id: at,
            height,
            hash,
        });
    }

    /// Marks `key` answered, as applying its reply does.
    pub(crate) fn mark_answered(&mut self, key: RequestKey) {
        self.answered.insert(key);
    }

    /// Answers the outstanding request `key`, as applying its reply does.
    pub(crate) fn answer_request(&mut self, key: RequestKey) {
        self.answer(key);
    }

    /// Whether `key` is marked answered: step 5 takes an input it needs as `NotServed`.
    pub(crate) fn is_answered(&self, key: RequestKey) -> bool {
        self.answered.contains(&key)
    }

    /// Marks `key` answered, failed (at `height`), dropped and retried once (D47).
    pub(crate) fn mark_every_bookkeeping(&mut self, key: RequestKey, height: u32) {
        self.answered.insert(key);
        self.failed.insert(key, height);
        self.dropped.insert(key);
        self.retries.insert(key, 1);
    }

    /// [`Self::prune_bookkeeping`] at `floor`.
    pub(crate) fn forget_bookkeeping_below(&mut self, chain: &dyn MnChain, floor: u32) {
        self.prune_bookkeeping(chain, floor).expect("the fixture's headers");
    }

    /// Whether `key` is marked answered, failed, dropped and retried (D47).
    pub(crate) fn bookkeeping_of(&self, key: RequestKey) -> [bool; 4] {
        [
            self.answered.contains(&key),
            self.failed.contains_key(&key),
            self.dropped.contains(&key),
            self.retries.contains_key(&key),
        ]
    }

    /// Sets `qc`'s status as step 5 left it.
    pub(crate) fn set_state(&mut self, qc: &Commitment, state: QuorumState) {
        self.records.get_mut(&commitment_hash(qc)).expect("held").state = Some(state);
    }

    /// As `load` leaves a verdict it resets (D38): not yet run.
    pub(crate) fn reset_state(&mut self, qc: &Commitment) {
        self.records.get_mut(&commitment_hash(qc)).expect("held").state = None;
    }

    /// Holds a ChainLock at `height` on block `hash` as step 6 verified it.
    pub(crate) fn hold_chainlock(&mut self, height: u32, hash: BlockHash) {
        self.best_cl = Some((height, hash));
    }

    /// The height of the newest ChainLock step 6 verified, the reorg base's (DEC-387).
    pub(crate) fn best_chainlock(&self) -> Option<u32> {
        self.best_cl.map(|(h, _)| h)
    }

    /// The rotation rows held: (cycles, snapshots counting each candidate, `activeQuorumMembers`
    /// bits over them), and the bookkeeping sizes (answered, failed, dropped, retries) (BL-382).
    #[cfg(feature = "testnet-replay")]
    pub(crate) fn rotation_rows_and_bookkeeping(&self) -> ([usize; 3], [usize; 4]) {
        let mut rows = [self.rows.len(), 0, 0];
        for row in self.rows.values() {
            let snapshots = match row {
                HeldRow::Core(s) => std::slice::from_ref(s),
                HeldRow::Candidates(c) => c.as_slice(),
            };
            rows[1] += snapshots.len();
            rows[2] += snapshots.iter().map(|s| s.active_quorum_members.len()).sum::<usize>();
        }
        let books =
            [self.answered.len(), self.failed.len(), self.dropped.len(), self.retries.len()];
        (rows, books)
    }

    /// How many rows (`Record`s) are held (D31).
    #[cfg(feature = "testnet-replay")]
    pub(crate) fn row_count(&self) -> usize {
        self.records.len()
    }

    /// How many distinct sets the held heights share (D28).
    #[cfg(feature = "testnet-replay")]
    pub(crate) fn distinct_sets(&self) -> usize {
        self.lists.values().map(|l| Arc::as_ptr(&l.quorums)).collect::<HashSet<_>>().len()
    }

    /// Whether the sets held at `a` and `b` are one shared set (D28).
    pub(crate) fn shares_set(&self, a: BlockId, b: BlockId) -> bool {
        Arc::ptr_eq(&self.lists[&a].quorums, &self.lists[&b].quorums)
    }

    /// Each record's `held_to` (D36), by commitment hash.
    pub(crate) fn held_tos(&self) -> BTreeMap<[u8; 32], Option<u32>> {
        self.records.iter().map(|(hash, r)| (*hash, r.held_to)).collect()
    }

    /// The records whose rows store the sets' `held_to` (tag 2, D48), by commitment hash.
    pub(crate) fn held_by_sets(&self) -> BTreeMap<[u8; 32], bool> {
        self.records.iter().map(|(hash, r)| (*hash, r.held_by_sets)).collect()
    }

    /// Whether `qc`'s row (its `Record`) is held (D31).
    pub(crate) fn holds_row(&self, qc: &Commitment) -> bool {
        self.records.contains_key(&commitment_hash(qc))
    }

    /// Keeps every list whole, and every set: retention prunes rows only.
    pub(crate) fn keep_every_list(&mut self) {
        self.keep_every_list = true;
    }

    /// Keeps the list at `at` whole, as an outstanding request with it as a base would.
    pub(crate) fn pin_list(&mut self, at: BlockId) {
        self.pinned.push(at);
    }

    /// Releases every list [`Self::pin_list`] kept.
    #[cfg(feature = "testnet-replay")]
    pub(crate) fn unpin_lists(&mut self) {
        self.pinned.clear();
    }

    /// Drops the list at `at`, as retention would.
    pub(crate) fn forget_list(&mut self, at: BlockId) {
        if let Some(list) = self.lists.remove(&at) {
            self.by_height.remove(&(list.height, at));
        }
    }

    /// As a fault mid-retention leaves it: the marks are not read (Opus s6 r2 n4).
    pub(crate) fn forget_retained(&mut self) {
        self.retained = None;
    }

    /// Marks the list at `at` and the commitments of its set as below A_q: unauthenticated, their
    /// states kept.
    pub(crate) fn forget_authentication(&mut self, at: BlockId) {
        let list = self.lists.get_mut(&at).expect("held");
        list.authenticated = false;
        for hash in list.quorums.values() {
            self.records.get_mut(hash).expect("held").authenticated = false;
        }
    }

    /// Points `quorum`'s entry in the set held at `at` at the commitment `to` has there, or
    /// removes it.
    pub(crate) fn point(
        &mut self,
        at: BlockId,
        llmq_type: LlmqType,
        quorum: &QuorumHash,
        to: Option<&QuorumHash>,
    ) {
        let key = |q: &QuorumHash| (llmq_type.to_base(), *q.as_bytes());
        let set = Arc::make_mut(&mut self.lists.get_mut(&at).expect("held").quorums);
        let to = to.map(|q| set[&key(q)]);
        match to {
            Some(hash) => set.insert(key(quorum), hash),
            None => set.remove(&key(quorum)),
        }
        .expect("in the set");
    }

    /// `ScanView`'s scan of the set held at `at`, `drop`'s commitment removed (D10): its
    /// entries' indexes in order, or `None` where the set cannot give Core's scan.
    pub(crate) fn scan_indexes(
        &self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        llmq_type: LlmqType,
        at: BlockId,
        drop: Option<&QuorumHash>,
    ) -> Option<Vec<i16>> {
        use dash_evo::llmq::ScanEntry;
        let lp = cfg.params().consensus.llmq(llmq_type).expect("configured");
        let mut set = (*self.lists[&at].quorums).clone();
        if let Some(q) = drop {
            set.remove(&(llmq_type.to_base(), *q.as_bytes())).expect("in the set");
        }
        let count = usize::try_from(lp.signing_active_quorum_count).expect("small");
        let entries = self.scan_view(chain, cfg).entries(lp, &set, count).expect("no stop")?;
        Some(entries.iter().map(ScanEntry::quorum_index).collect())
    }

    /// `ScanView::quorum`: the status a lock check reads for a quorum named by hash.
    pub(crate) fn scan_quorum(
        &self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Option<QuorumStatus> {
        self.scan_view(chain, cfg).quorum(llmq_type, quorum_hash).expect("no stop")
    }

    /// `ScanView::scan` at `start_height`: each entry's quorum and status, or `None` where it
    /// holds no scan.
    pub(crate) fn scan_statuses(
        &self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        llmq_type: LlmqType,
        start_height: u32,
    ) -> Option<Vec<(QuorumHash, QuorumStatus)>> {
        use dash_evo::llmq::ScanEntry;
        match self.scan_view(chain, cfg).scan(llmq_type, start_height).expect("no stop") {
            ScanLookup::Held(scan) => {
                Some(scan.entries.iter().map(|e| (e.quorum_hash(), e.status().clone())).collect())
            }
            _ => None,
        }
    }

    fn scan_view<'a>(&'a self, chain: &'a dyn MnChain, cfg: &'a LlmqConfig) -> ScanView<'a> {
        ScanView::new(self, chain, cfg, chain.active_tip().expect("a tip"))
    }

    /// Holds Core's row for a cycle.
    pub(crate) fn hold_row(&mut self, llmq_type: LlmqType, cycle: BlockHash, row: QuorumSnapshot) {
        self.rows.insert((llmq_type.to_base(), *cycle.as_bytes()), HeldRow::Core(row));
    }

    /// Drops the row at `cycle`.
    pub(crate) fn forget_row(&mut self, llmq_type: LlmqType, cycle: BlockHash) {
        self.rows.remove(&(llmq_type.to_base(), *cycle.as_bytes()));
    }

    /// Holds a row as a reply to a `qrinfo` request would: a candidate.
    pub(crate) fn hold_candidate(
        &mut self,
        llmq_type: LlmqType,
        cycle: BlockHash,
        row: QuorumSnapshot,
    ) {
        self.add_candidate((llmq_type.to_base(), *cycle.as_bytes()), row);
    }

    /// Holds a cycle's row as these candidates, in place of whatever it held.
    pub(crate) fn hold_candidates(
        &mut self,
        llmq_type: LlmqType,
        cycle: BlockHash,
        rows: Vec<QuorumSnapshot>,
    ) {
        self.rows.insert((llmq_type.to_base(), *cycle.as_bytes()), HeldRow::Candidates(rows));
    }

    /// `pro`'s operator key set to `key` in every held list, as no root would let a list carry.
    pub(crate) fn set_operator_key(&mut self, pro: &ProTxHash, key: [u8; 48]) {
        for full in self.lists.values_mut().filter_map(|l| l.full.as_mut()) {
            if let Some(entry) = full.sml.get(pro) {
                let entry = dash_evo::sml::SmlEntry {
                    operator_key: key,
                    ..entry.clone()
                };
                Arc::make_mut(&mut full.sml).upsert(entry);
            }
        }
    }

    /// A row for a cycle as another cycle's requested `qrinfo` would carry it, then step 5.
    pub(crate) fn offer_row(
        &mut self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
        llmq_type: LlmqType,
        cycle: BlockHash,
        row: QuorumSnapshot,
    ) {
        if self.add_candidate((llmq_type.to_base(), *cycle.as_bytes()), row) {
            self.reopen_disputed();
        }
        self.verify_open(chain, cfg).expect("no stop");
    }

    /// The row held for a cycle.
    pub(crate) fn held_row(&self, llmq_type: LlmqType, cycle: BlockHash) -> Option<&HeldRow> {
        self.rows.get(&(llmq_type.to_base(), *cycle.as_bytes()))
    }

    /// [`Self::confirm_rows`].
    pub(crate) fn confirm_held_rows(
        &mut self,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> Result<(), MnReject> {
        self.confirm_rows(chain, cfg)
    }

    /// Step 5 for an authenticated commitment mined at `mined_height`.
    pub(crate) fn step5(
        &self,
        qc: Commitment,
        mined_height: u32,
        chain: &dyn MnChain,
        cfg: &LlmqConfig,
    ) -> QuorumState {
        let record = Record {
            commitment: Arc::new(qc),
            first_seen: mined_height,
            authenticated: true,
            state: None,
            decider: None,
            replaced: None,
            mined: None,
            held_to: None,
            held_by_sets: false,
        };
        match self.attempt(&record, chain, cfg).expect("no stop").0 {
            Attempt::Done(state) => state,
            Attempt::Need(request) | Attempt::Disputed(request) => {
                QuorumState::Pending(request.key())
            }
        }
    }
}

/// `tx`'s CbTx payload, when it is a coinbase special transaction whose payload decodes whole.
pub(crate) fn cbtx_payload(tx: &Tx) -> Result<CoinbaseCommitment, MnReject> {
    if !tx.is_coinbase() || tx.tx_type() != TxType::CoinbaseCommitment {
        return Err(MnReject::BadCoinbase("not a coinbase special transaction"));
    }
    match tx.payload() {
        Some(Ok(decoded)) if decoded.is_complete() => match &decoded.payload {
            SpecialPayload::CoinbaseCommitment(cbtx) => Ok(cbtx.clone()),
            _ => Err(MnReject::BadCoinbase("its payload is not a CbTx")),
        },
        _ => Err(MnReject::BadCoinbase("its payload does not decode")),
    }
}

/// Whether `block` (a hash and its height) is on the active chain ending at `tip`; a height
/// below the floor counts as on it.
fn on_active_chain(
    chain: &dyn MnChain,
    tip: BlockId,
    (hash, height): (BlockHash, u32),
) -> Result<bool, MnReject> {
    match chain.lookup(&hash)? {
        Some(id) => Ok(through(chain, tip, id, height)?),
        None => Ok(false),
    }
}

/// One active-chain holder of a looked-up commitment (D36: a held set, or a row by its marks)
/// folded into `answer`, ranked `Found` > `Fetch` > `Unavailable` > `NotFound`: its key when
/// `Verified`; `Fetch` while step 5 has not decided it on the active chain (it runs it again,
/// D12); `Unavailable` while its input cannot be had, never not found (DEC-444, D50).
fn fold_holder(
    answer: &mut Lookup,
    r: &Record,
    chain: &dyn MnChain,
    tip: BlockId,
) -> Result<Option<VerifiedQuorum>, AncestryError> {
    match &r.state {
        None | Some(QuorumState::Pending(_)) => *answer = Lookup::Fetch,
        Some(_) if !decided_on(chain, tip, r.decider)? => *answer = Lookup::Fetch,
        Some(QuorumState::Verified(v)) => return Ok(Some(v.clone())),
        // `Skipped`: held, but no key is had for a type SPV does not use (Opus s6 r2 m2).
        Some(QuorumState::Unavailable(_) | QuorumState::Skipped) if *answer == Lookup::NotFound => {
            *answer = Lookup::Unavailable
        }
        _ => {}
    }
    Ok(None)
}

/// Whether a state's scheme was decided on the active chain ending at `tip`: by no branch, or by
/// a set on it (D12).
fn decided_on(
    chain: &dyn MnChain,
    tip: BlockId,
    decider: Option<SetMark>,
) -> Result<bool, AncestryError> {
    decider.map_or(Ok(true), |d| through(chain, tip, d.id, d.height))
}

/// SB0 §5.2.1's classification of a selection start at `height` with the header tip at `tip`,
/// before any set is read (D24); `None` inside the authenticated domain's horizon.
fn classify(height: u32, tip: u32, cfg: &LlmqConfig) -> Option<SetAt> {
    if height < cfg.dip3_height() {
        Some(SetAt::NoQuorum)
    } else if height < cfg.a_q() {
        Some(SetAt::NoRoot)
    } else if height.saturating_add(cfg.h_keep()) < tip {
        Some(SetAt::OutsideHorizon)
    } else {
        None
    }
}

/// The lists D29 (2) keeps for each used type given the newest list `top`: from the newest cycle
/// whose work block is at or below `top` (from V20; before it a non-rotated cycle's list is its
/// base, perhaps above `top`, not yet held), older ones while their window is open, which closes
/// once `top` reaches its last block; a rotated type's newest closed cycle keeps k = 0…2.
fn window_lists(cfg: &LlmqConfig, top: u32) -> Vec<u32> {
    let params = cfg.params();
    let c = &params.consensus;
    let mut used =
        vec![c.llmq_type_chain_locks, c.llmq_type_dip0024_instant_send, c.llmq_type_platform];
    used.sort_by_key(|t| t.to_base());
    used.dedup();
    let mut heights = Vec::new();
    for lp in used.into_iter().filter_map(|t| c.llmq(t)) {
        let (step, end) = (interval(lp), lp.dkg_mining_window_end.unsigned_abs());
        let mut newest_cycle = true;
        let mut next = Some(top.saturating_add(WORK_DIFF_DEPTH) / step * step);
        while let Some(cycle) = next {
            next = cycle.checked_sub(step);
            let mut blocks = member_lists(lp, params, cycle);
            if cycle.saturating_add(end) <= top {
                if !(newest_cycle && lp.use_rotation) {
                    break;
                }
                blocks.truncate(3);
            }
            newest_cycle = false;
            heights.extend(blocks);
        }
    }
    heights
}

/// The lowest block whose request bookkeeping is kept (D47): L_est below the oldest held
/// commitment's base, and at most tip − (R + 2·L_est). What a commitment's step reads lies within
/// its type's reach L ≤ L_est below its base (5C + W + 16 rotated, a cycle's quarters and work
/// blocks). A commitment stays held while its index has no successor, however old (a DKG that
/// keeps failing, `llmq/blockprocessor.cpp` rc.3:779-794, 818-832), so the floor follows the
/// oldest, keeping its `retries` (ROW_RETRIES, D20) and `failed` marks while it is held.
pub(crate) fn bookkeeping_floor(cfg: &LlmqConfig, tip: u32, oldest_base: Option<u32>) -> u32 {
    let by_tip = tip.saturating_sub(cfg.r().saturating_add(cfg.l_est().saturating_mul(2)));
    oldest_base.map_or(by_tip, |base| by_tip.min(base.saturating_sub(cfg.l_est())))
}

/// The height of `hash`, `None` when the chain does not know the block or holds its header no
/// more (below the floor).
fn known_height(
    chain: &dyn MnChain,
    hash: impl Into<BlockHash>,
) -> Result<Option<u32>, AncestryError> {
    let Some(id) = chain.lookup(&hash.into())? else {
        return Ok(None);
    };
    match chain.ancestry().header(id) {
        Ok(header) => Ok(Some(header.height)),
        Err(AncestryError::BelowFloor {
            ..
        }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Adds to `keep` the blocks of `from`'s chain at `heights`, those the headers hold (the floor
/// and a height above `from` have none).
fn keep_ancestors(
    chain: &dyn MnChain,
    from: BlockId,
    heights: impl IntoIterator<Item = u32>,
    keep: &mut HashSet<BlockId>,
) -> Result<(), AncestryError> {
    for h in heights {
        match chain.ancestry().ancestor(from, h) {
            Ok(id) => keep.extend(id),
            Err(AncestryError::BelowFloor {
                ..
            }) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The heights of the lists Core's member computation reads for a commitment based at `height`
/// (`llmq/utils.cpp` rc.3:491, :700-722), newest first: rotated, the cycle start (the base less
/// its quorumIndex, base height mod C) less k·C + 8 for k = 0…3; non-rotated, the base less 8
/// once V20 is active after it, else the base. One at a negative height does not exist (clipped
/// at genesis).
fn member_lists(lp: &LlmqParams, params: &ChainParams, height: u32) -> Vec<u32> {
    if lp.use_rotation {
        let c = interval(lp);
        let cycle = height - height % c;
        (0..4).filter_map(|k| cycle.checked_sub(k * c)?.checked_sub(WORK_DIFF_DEPTH)).collect()
    } else if height.saturating_add(1) >= deployment(params, BuriedDeployment::V20) {
        height.checked_sub(WORK_DIFF_DEPTH).into_iter().collect()
    } else {
        vec![height]
    }
}

/// A buried deployment's height.
fn deployment(params: &ChainParams, d: BuriedDeployment) -> u32 {
    params.consensus.deployment_height(d).to_u32()
}

/// The uncovered elements coverage met (D25).
#[derive(Default)]
struct Below {
    /// The lowest height that covers: an element below it is uncovered.
    anchor: u32,
    lowest: Option<u32>,
    /// A base hash that is not an accepted header: below the floor, height unknown.
    unknown: bool,
}

impl Below {
    /// Whether an element at `height` is uncovered, noting it if so.
    fn uncovered(&mut self, height: u32) -> bool {
        let below = height < self.anchor;
        if below {
            self.lowest = Some(self.lowest.map_or(height, |l| l.min(height)));
        }
        below
    }

    /// A known lowest height is the better fallback hint: `None` only when no height is known.
    fn coverage(&self) -> Coverage {
        if self.lowest.is_none() && !self.unknown {
            Coverage::Covered
        } else {
            Coverage::Below {
                lowest: self.lowest,
            }
        }
    }
}

/// Each commitment's newest authenticated list among `lists` on `tip`'s chain holding it (D48).
pub(crate) fn newest_holders(
    chain: &dyn MnChain,
    tip: BlockId,
    lists: &BTreeMap<BlockId, HeldList>,
) -> Result<HashMap<[u8; 32], u32>, AncestryError> {
    // Lists share their sets (D28): each set's newest holder once, then its leaves.
    let mut sets: HashMap<*const QuorumSet, (&QuorumSet, u32)> = HashMap::new();
    for (id, l) in lists {
        if l.authenticated && through(chain, tip, *id, l.height)? {
            let newest = sets.entry(Arc::as_ptr(&l.quorums)).or_insert((&l.quorums, l.height));
            newest.1 = newest.1.max(l.height);
        }
    }
    let mut holders: HashMap<[u8; 32], u32> = HashMap::new();
    for (set, height) in sets.into_values() {
        for leaf in set.values() {
            let newest = holders.entry(*leaf).or_insert(height);
            *newest = (*newest).max(height);
        }
    }
    Ok(holders)
}

/// Whether the chain ending at `tip` runs through block `id` at `height`; a height below the
/// floor counts as on it.
fn through(
    chain: &dyn MnChain,
    tip: BlockId,
    id: BlockId,
    height: u32,
) -> Result<bool, AncestryError> {
    match chain.ancestry().ancestor(tip, height) {
        Ok(a) => Ok(a == Some(id)),
        Err(AncestryError::BelowFloor {
            ..
        }) => Ok(true),
        Err(e) => Err(e),
    }
}
