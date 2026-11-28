//! The regtest share seam behind `-testquorumshares=<file>` (feature `testkit-seams`): a node's
//! own quorum key share and the quorum's verification vector, recorded from a twin, installed
//! the way v24.0.0 installs a `qdata` answer (src/llmq/net_quorum.cpp:243-269): the checked
//! `SetVerificationVector`, the checked `SetSecretKeyShare`, then `WriteContributions`.
//!
//! The file is JSON:
//! `{"v":1,"items":[{"llmq_type":<n>,"quorum_hash":<hex>,"pro_tx":<hex>,"vvec":[<hex>…],
//! "sk_share":<hex>,"provenance":{"source_slot":<str>,"source_exe_sha256":<str>,"method":<str>,
//! "tip_height":<n>,"captured_seq":<n>}}]}`. `quorum_hash` and `pro_tx` are 64 hex digits in the
//! order Core's RPC prints a `uint256` (byte-reversed); `vvec` keys (48 bytes) and `sk_share`
//! (32 bytes) are hex of their serialized bytes, as `quorum info` prints them.

use std::sync::Arc;

use dash_evo::types::ProTxHash;
use dash_llmq::quorum::{QuorumKeys, QuorumView, quorum_db_key, to_store};
use dash_params::{LlmqType, Network};
use dash_pkc::bls::{BlsPkBytes, BlsSkBytes};
use dash_primitives::QuorumHash;
use dash_types::Numeric;
use serde_json::{Map, Value};
use tracing::debug;

use super::QuorumManager;
use crate::traits::{LocalMasternode, QuorumKeyStore, ShareSeamView, WarmExecutor};

/// The only file version this loader reads.
pub const SHARE_FILE_VERSION: u64 = 1;

/// Why a whole file was refused. Nothing was read from the chain view or written.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LoadError {
    /// The option is regtest-only.
    #[error("-testquorumshares is regtest-only; this chain is {0}")]
    NotRegtest(String),
    /// Bad JSON, an unknown `v`, or a missing or ill-typed field.
    #[error("malformed quorum share file: {0}")]
    Malformed(String),
}

/// Why one item was rejected. A rejected item changes neither a cached quorum's keys nor the
/// store.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Rejection {
    /// No [`ShareSeamView`] is installed.
    #[error("no chain view")]
    NoView,
    /// `llmq_type` is not configured on this chain.
    #[error("unknown LLMQ type")]
    UnknownLlmqType,
    /// The view failed to read the quorum.
    #[error("chain view error: {0}")]
    View(String),
    /// No commitment for `(llmq_type, quorum_hash)` is mined in this node's chain.
    #[error("quorum not mined")]
    NotMined,
    /// The cached quorum object's commitment is not the mined commitment.
    #[error("cached commitment is not the mined one")]
    CachedCommitmentMismatch,
    /// The vector's length is not the quorum's `threshold` (net_quorum.cpp:248-253).
    #[error("verification vector size {got}, expected {expected}")]
    WrongVvecLength {
        /// The quorum's `threshold`.
        expected: usize,
        /// The item's length.
        got: usize,
    },
    /// Element `index` is not hex of 48 bytes or does not decode under either scheme.
    #[error("undecodable verification vector element {index}")]
    UndecodableVvecElement {
        /// The element's position.
        index: usize,
    },
    /// The vector's hash is not the mined `quorumVvecHash` (quorums.cpp:117-127).
    #[error("verification vector hash mismatch")]
    VvecHashMismatch,
    /// `pro_tx` is not this node's own proTxHash.
    #[error("not this node's proTxHash")]
    NotOwnProTx,
    /// `pro_tx` is not a valid member of the quorum.
    #[error("not a valid member")]
    NotValidMember,
    /// `sk_share` is not hex of 32 bytes or not a canonical scalar.
    #[error("undecodable secret key share")]
    UndecodableShare,
    /// The share's public key is not the member's public key share (quorums.cpp:129-137).
    #[error("secret key share mismatch")]
    ShareMismatch,
    /// The store refused the contributions row.
    #[error("store write: {0}")]
    StoreWrite(String),
}

/// One item's identity and outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemReport {
    /// The numeric Core LLMQ type.
    pub llmq_type: u8,
    pub quorum_hash: QuorumHash,
    pub pro_tx: ProTxHash,
    pub outcome: Result<(), Rejection>,
}

/// The per-item outcomes of one load, in file order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LoadReport {
    pub items: Vec<ItemReport>,
}

impl LoadReport {
    /// The number of accepted items.
    pub fn accepted(&self) -> usize {
        self.items.iter().filter(|i| i.outcome.is_ok()).count()
    }
}

/// Where an item came from, logged with its outcome.
#[derive(Clone, Debug)]
struct Provenance {
    source_slot: String,
    source_exe_sha256: String,
    method: String,
    tip_height: u64,
    captured_seq: u64,
}

/// One parsed item. Hex that is not of the right length decodes to `None`, which rejects the
/// item, not the file.
struct Item {
    llmq_type: u8,
    quorum_hash: QuorumHash,
    pro_tx: ProTxHash,
    vvec: Vec<Option<BlsPkBytes>>,
    sk_share: Option<BlsSkBytes>,
    provenance: Provenance,
}

impl<S, C, M, W> QuorumManager<S, C, M, W>
where
    S: QuorumKeyStore + Send + Sync + 'static,
    S::Error: Send + Sync + 'static,
    M: LocalMasternode,
    W: WarmExecutor + Send + Sync + 'static,
{
    /// Installs the view the share seam reads quorums through.
    pub fn with_share_seam_view(mut self, view: Arc<dyn ShareSeamView>) -> Self {
        self.share_seam_view = Some(view);
        self
    }

    /// Loads a `-testquorumshares` file (the module docs give its format). Callers run it at
    /// init, before the node builds any quorum: an item for a quorum that is not cached writes
    /// only the contributions row, which the quorum's later build reads; an item for a cached
    /// quorum also replaces its keys whole with the validated ones, under their mutex, and queues
    /// its warming.
    ///
    /// Each item is checked on scratch keys first: the quorum must be mined in this node's
    /// chain, the vector must have the quorum's `threshold` keys decodable under this manager's
    /// scheme and hash to the mined `quorumVvecHash`, and `pro_tx` must be this node's own
    /// proTxHash, a valid member whose public key share the secret share matches. One debug line
    /// per item, at the `llmq` target, gives its identity, outcome, reason and provenance, never
    /// key bytes. A rejected item touches neither the cached keys nor the store. The row is
    /// written before a cached quorum's keys are replaced, so a store write that fails rejects the
    /// item and leaves the cached keys as they were.
    ///
    /// # Errors
    ///
    /// [`LoadError::NotRegtest`] on any other chain and [`LoadError::Malformed`] for a file that
    /// does not parse; either way nothing is read from the view or written. Item rejections are
    /// in the report.
    // Core: v24.0.0 src/llmq/net_quorum.cpp:243-269 NetQuorum::ProcessMessage (QDATA)
    pub fn load_test_quorum_shares(&self, json: &str) -> Result<LoadReport, LoadError> {
        if self.params.network != Network::Regtest {
            return Err(LoadError::NotRegtest(format!("{:?}", self.params.network)));
        }
        let items = parse(json)?;
        let items = items
            .into_iter()
            .map(|item| {
                let outcome = self.load_item(&item);
                log_item(&item, &outcome);
                ItemReport {
                    llmq_type: item.llmq_type,
                    quorum_hash: item.quorum_hash,
                    pro_tx: item.pro_tx,
                    outcome,
                }
            })
            .collect();
        Ok(LoadReport {
            items,
        })
    }

    fn load_item(&self, item: &Item) -> Result<(), Rejection> {
        let view = self.share_seam_view.as_ref().ok_or(Rejection::NoView)?;
        let llmq_type = LlmqType::from_base(item.llmq_type);
        if self.params.consensus.llmq(llmq_type).is_none() {
            return Err(Rejection::UnknownLlmqType);
        }
        let mined = view
            .mined_quorum(llmq_type, &item.quorum_hash)
            .map_err(|e| Rejection::View(e.to_string()))?
            .ok_or(Rejection::NotMined)?;
        // :236 GetCachedMutableQuorum
        let cached = view.cached_quorum(llmq_type, &item.quorum_hash);
        let scheme = mined.scheme;
        let (commitment, members) = match &cached {
            Some(q) if q.commitment != mined.commitment => {
                return Err(Rejection::CachedCommitmentMismatch);
            }
            Some(q) => (q.commitment.clone(), q.members.clone()),
            None => (mined.commitment, mined.members),
        };
        let member_view =
            self.member_view(llmq_type, &members).ok_or(Rejection::UnknownLlmqType)?;
        let (params, pro_txs, operator_keys) = &member_view;
        let quorum = QuorumView {
            llmq: params,
            commitment: &commitment,
            fields: commitment.bls(),
            members: pro_txs,
            operator_keys,
        };

        // :248-253, the size before any element is decoded
        let expected = usize::from(params.threshold);
        if item.vvec.len() != expected {
            return Err(Rejection::WrongVvecLength {
                expected,
                got: item.vvec.len(),
            });
        }
        let vvec = item
            .vvec
            .iter()
            .enumerate()
            .map(|(index, key)| {
                key.as_ref().and_then(|key| Self::decode_vvec_key(key, scheme)).ok_or(
                    Rejection::UndecodableVvecElement {
                        index,
                    },
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut scratch = QuorumKeys::default();
        // :256
        if !scratch.set_verification_vector(&quorum, vvec, scheme) {
            return Err(Rejection::VvecHashMismatch);
        }
        if self.local_masternode.pro_tx_hash() != Some(item.pro_tx) {
            return Err(Rejection::NotOwnProTx);
        }
        if !quorum.is_valid_member(&item.pro_tx) {
            return Err(Rejection::NotValidMember);
        }
        let sk = item
            .sk_share
            .as_ref()
            .and_then(|sk| Self::decode_sk_share(sk, scheme))
            .ok_or(Rejection::UndecodableShare)?;
        // ProcessContribQDATA's SetSecretKeyShare (:348)
        if !scratch.set_secret_key_share(&quorum, sk, &item.pro_tx) {
            return Err(Rejection::ShareMismatch);
        }

        // :268 WriteContributions, before the live keys change, so a failed write leaves them
        let key = quorum_db_key(llmq_type, &commitment.quorum_hash(), pro_txs);
        self.store
            .write(&key, &to_store(&scratch))
            .map_err(|e| Rejection::StoreWrite(e.to_string()))?;
        if let Some(q) = cached {
            // The validated keys whole: a cached object's per-member public key shares from an
            // earlier vector must not outlive it.
            *q.keys.lock().unwrap_or_else(|p| p.into_inner()) = scratch;
            // :257 QueueQuorumForWarming
            self.queue_warm(q.0.clone(), member_view);
        }
        Ok(())
    }
}

fn log_item(item: &Item, outcome: &Result<(), Rejection>) {
    let (status, reason) = match outcome {
        Ok(()) => ("accepted", String::new()),
        Err(e) => ("rejected", e.to_string()),
    };
    let p = &item.provenance;
    debug!(target: "llmq", "QuorumManager::LoadTestQuorumShares -- llmqType[{}] quorumHash[{}] proTxHash[{}] {} reason[{}] source_slot[{:?}] source_exe_sha256[{:?}] method[{:?}] tip_height[{}] captured_seq[{}]", item.llmq_type, item.quorum_hash, item.pro_tx, status, reason, p.source_slot, p.source_exe_sha256, p.method, p.tip_height, p.captured_seq);
}

/// The loader's own parse of a `-testquorumshares` file, without its network check.
pub fn check_file(json: &str) -> Result<(), LoadError> {
    parse(json).map(|_| ())
}

fn malformed(why: impl Into<String>) -> LoadError {
    LoadError::Malformed(why.into())
}

fn parse(json: &str) -> Result<Vec<Item>, LoadError> {
    let root: Value = serde_json::from_str(json).map_err(|e| malformed(format!("JSON: {e}")))?;
    let root = root.as_object().ok_or_else(|| malformed("the file is not an object"))?;
    let v = u64_field(root, "v", "the file")?;
    if v != SHARE_FILE_VERSION {
        return Err(malformed(format!("unknown version {v}")));
    }
    let items = field(root, "items", "the file")?
        .as_array()
        .ok_or_else(|| malformed("items is not an array"))?;
    items.iter().enumerate().map(|(i, item)| parse_item(i, item)).collect()
}

fn parse_item(index: usize, item: &Value) -> Result<Item, LoadError> {
    let at = format!("item {index}");
    let item = item.as_object().ok_or_else(|| malformed(format!("{at} is not an object")))?;
    let llmq_type = u8::try_from(u64_field(item, "llmq_type", &at)?)
        .map_err(|_| malformed(format!("{at}: llmq_type is out of range")))?;
    let quorum_hash = QuorumHash::from(uint256(item, "quorum_hash", &at)?);
    let pro_tx = ProTxHash::from(uint256(item, "pro_tx", &at)?);
    let vvec = field(item, "vvec", &at)?
        .as_array()
        .ok_or_else(|| malformed(format!("{at}: vvec is not an array")))?
        .iter()
        .map(|key| {
            key.as_str()
                .map(|key| hex_array::<48>(key).map(BlsPkBytes::from_bytes))
                .ok_or_else(|| malformed(format!("{at}: a vvec element is not a string")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let sk_share = hex_array::<32>(str_field(item, "sk_share", &at)?).map(BlsSkBytes::from);
    let provenance = field(item, "provenance", &at)?
        .as_object()
        .ok_or_else(|| malformed(format!("{at}: provenance is not an object")))?;
    let at = format!("{at} provenance");
    let provenance = Provenance {
        source_slot: str_field(provenance, "source_slot", &at)?.to_owned(),
        source_exe_sha256: str_field(provenance, "source_exe_sha256", &at)?.to_owned(),
        method: str_field(provenance, "method", &at)?.to_owned(),
        tip_height: u64_field(provenance, "tip_height", &at)?,
        captured_seq: u64_field(provenance, "captured_seq", &at)?,
    };
    Ok(Item {
        llmq_type,
        quorum_hash,
        pro_tx,
        vvec,
        sk_share,
        provenance,
    })
}

fn field<'v>(obj: &'v Map<String, Value>, name: &str, at: &str) -> Result<&'v Value, LoadError> {
    obj.get(name).ok_or_else(|| malformed(format!("{at}: missing {name}")))
}

fn u64_field(obj: &Map<String, Value>, name: &str, at: &str) -> Result<u64, LoadError> {
    field(obj, name, at)?
        .as_u64()
        .ok_or_else(|| malformed(format!("{at}: {name} is not a non-negative integer")))
}

fn str_field<'v>(obj: &'v Map<String, Value>, name: &str, at: &str) -> Result<&'v str, LoadError> {
    field(obj, name, at)?.as_str().ok_or_else(|| malformed(format!("{at}: {name} is not a string")))
}

/// A `uint256` as Core's RPC prints it, `GetHex`'s byte-reversed hex, as its internal bytes.
fn uint256(obj: &Map<String, Value>, name: &str, at: &str) -> Result<[u8; 32], LoadError> {
    let mut bytes = hex_array::<32>(str_field(obj, name, at)?)
        .ok_or_else(|| malformed(format!("{at}: {name} is not 64 hex digits")))?;
    bytes.reverse();
    Ok(bytes)
}

/// Exactly `2 * N` hex digits, in byte order.
fn hex_array<const N: usize>(hex: &str) -> Option<[u8; N]> {
    if hex.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (byte, [hi, lo]) in out.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
        let digit = |d: u8| char::from(d).to_digit(16);
        *byte = u8::try_from(digit(*hi)? << 4 | digit(*lo)?).ok()?;
    }
    Some(out)
}
