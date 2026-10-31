//! Core's RPC wrappers over rdashd's results (O2b §5.4, D11): the shim runs Core's handlers, so
//! `NodeSut`'s replies are shaped by this cited port of them. Each branch names the Core line
//! it ports (the oracle pin's Core, `rpc/mining.cpp`, `rpc/blockchain.cpp`, `core_read.cpp`,
//! `validation.cpp`).
//!
//! **Replaced by RP1a.** When RP1a's handlers merge, the ops call `dashd-rpc`'s handlers over
//! `NodeSut`'s V instead, so that the wrappers under test are production's (an O2b follow-up
//! row). Until then a reply difference whose cause is here is a harness bug, fixed in place.
//!
//! The handlers read the node through [`Node`]; the post-state renderers ([`chain_tips`],
//! [`deployment_info`]) take what the adapter read from V.

use dash_consensus::deployments::info::{Bip9Info, DeploymentInfo};
use dash_consensus::validation::ResultClass;
use dash_consensus::{Block, BlockValidationError, Header, decode_block, decode_header};
use dash_num::Hash256;
use dash_oracle_sys::{ChainHash, CheckResult, Checked, RpcError};
use dash_primitives::BlockHash;
use dash_statedigest::REJECT_CLASSES;
use dash_types::Numeric;
use dashd_chainstate::chain::{BlockNotFound, BlockState, NotTested};
use serde_json::{Map, Value, json};

/// A handler's reply: its result, or the error it throws.
pub type Outcome = Result<Value, RpcError>;

// Core: rpc/protocol.h:41-46
pub const RPC_INVALID_ADDRESS_OR_KEY: i64 = -5;
pub const RPC_DESERIALIZATION_ERROR: i64 = -22;
pub const RPC_VERIFY_ERROR: i64 = -25;

fn error(code: i64, message: impl Into<String>) -> Outcome {
    Err(RpcError {
        code: Some(code),
        message: message.into(),
    })
}

fn text(s: &str) -> Outcome {
    Ok(Value::String(s.to_owned()))
}

pub(crate) fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Core's `uint256::GetHex()`: the reversed internal bytes.
pub(crate) fn core_hex(hash: Hash256) -> String {
    hex(hash.to_bendian())
}

pub(crate) fn block_hash_hex(hash: BlockHash) -> String {
    core_hex(Hash256::from(hash))
}

/// What a handler reads of an index entry before its work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Known {
    /// `IsValid(BLOCK_VALID_SCRIPTS)`: valid to scripts and not failed.
    pub scripts: bool,
    /// `nStatus & BLOCK_FAILED_MASK`.
    pub failed: bool,
}

impl Known {
    /// The reply `submitblock` gives for a block the index already holds, if its status settles
    /// one.
    fn duplicate(self) -> Option<&'static str> {
        if self.scripts {
            Some("duplicate")
        } else if self.failed {
            Some("duplicate-invalid")
        } else {
            None
        }
    }
}

/// `ProcessNewBlock` under a `submitblock_StateCatcher` (`rpc/mining.cpp:989-1005`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Processed {
    /// `*new_block`.
    pub new_block: bool,
    /// `ProcessNewBlock`'s return.
    pub accepted: bool,
    /// The state of the last `BlockChecked` for the block's hash, if one fired (`found`).
    pub checked: Option<BlockState>,
}

/// The node the handlers run against, read and called as Core's handlers do.
pub trait Node {
    /// `m_blockman.LookupBlockIndex(hash)`.
    fn lookup(&mut self, hash: BlockHash) -> Option<Known>;
    /// `ProcessNewBlock(blockptr, /*force_processing=*/true, &new_block)`.
    fn process_block(&mut self, block: Block) -> Processed;
    /// `ProcessNewBlockHeaders({h}, state)`: the header's rejection, if any.
    fn process_header(&mut self, header: Header) -> Option<BlockValidationError>;
    /// The proposal's work under `getblocktemplate`'s one `cs_main` hold: the duplicate lookup,
    /// the parent check, then `TestBlockValidity(state, ..., pindexPrev = tip,
    /// fCheckPOW = false, fCheckMerkleRoot = true)`. The first two answer [`NotTested`].
    // Core: rpc/mining.cpp:676, :707-724
    fn propose(&mut self, block: Block) -> Result<BlockState, NotTested>;
}

/// `DecodeHexBlk`: one block, trailing bytes allowed; empty input is not hex.
// Core: core_read.cpp:144-159
fn decode(bytes: &[u8]) -> Option<Block> {
    if bytes.is_empty() {
        return None;
    }
    decode_block(&mut &bytes[..]).ok().map(|d| d.into_value())
}

/// `BIP22ValidationResult(state)`.
// Core: rpc/mining.cpp:532-548
pub fn bip22(state: &BlockState) -> Outcome {
    match state {
        BlockState::Valid => Ok(Value::Null),
        BlockState::Error(_) => error(RPC_VERIFY_ERROR, state.to_string()),
        BlockState::Invalid(e) if e.reject_reason().is_empty() => text("rejected"),
        BlockState::Invalid(e) => text(e.reject_reason()),
    }
}

/// `submitblock(hexdata)`.
// Core: rpc/mining.cpp:1025-1064
pub fn submit_block(bytes: &[u8], node: &mut impl Node) -> Outcome {
    // Core: rpc/mining.cpp:1029-1031
    let Some(block) = decode(bytes) else {
        return error(RPC_DESERIALIZATION_ERROR, "Block decode failed");
    };
    // Core: rpc/mining.cpp:1033-1035
    if !block.txs().first().is_some_and(|tx| tx.is_coinbase()) {
        return error(RPC_DESERIALIZATION_ERROR, "Block does not start with a coinbase");
    }
    // Core: rpc/mining.cpp:1041-1049
    if let Some(reply) = node.lookup(block.hash()).and_then(Known::duplicate) {
        return text(reply);
    }
    let processed = node.process_block(block);
    // Core: rpc/mining.cpp:1057-1059
    if !processed.new_block && processed.accepted {
        return text("duplicate");
    }
    // Core: rpc/mining.cpp:1060-1063
    match &processed.checked {
        None => text("inconclusive"),
        Some(state) => bip22(state),
    }
}

/// `submitheader(hexdata)`.
// Core: rpc/mining.cpp:1082-1103
pub fn submit_header(bytes: &[u8; 80], node: &mut impl Node) -> Outcome {
    // Core: rpc/mining.cpp:1084-1087, core_read.cpp:130-142 (unreachable here: the op holds
    // exactly 80 bytes, and Core's decode takes trailing bytes)
    let Ok(header) = decode_header(&mut &bytes[..]).map(|d| d.into_value()) else {
        return error(RPC_DESERIALIZATION_ERROR, "Block header decode failed");
    };
    // Core: rpc/mining.cpp:1089-1094
    let prev = header.prev_hash();
    if node.lookup(prev).is_none() {
        let message = format!("Must submit previous header ({}) first", block_hash_hex(prev));
        return error(RPC_VERIFY_ERROR, message);
    }
    // Core: rpc/mining.cpp:1096-1102 (a header's state is never an error)
    match node.process_header(header) {
        None => Ok(Value::Null),
        Some(e) => error(RPC_VERIFY_ERROR, e.reject_reason()),
    }
}

/// `getblocktemplate {"mode": "proposal", "data": hexdata}`.
// Core: rpc/mining.cpp:697-725
pub fn proposal(bytes: &[u8], node: &mut impl Node) -> Outcome {
    proposal_checked(bytes, node).0
}

/// [`proposal`], with the invalid state V's `TestBlockValidity` reached kept (its class, reason
/// and debug) for L3's class plane. `None` where it found none: a valid or errored state, and
/// the replies where it never ran (the decode error, `duplicate*`,
/// `inconclusive-not-best-prevblk`).
pub fn proposal_checked(
    bytes: &[u8],
    node: &mut impl Node,
) -> (Outcome, Option<BlockValidationError>) {
    // Core: rpc/mining.cpp:704-706
    let Some(block) = decode(bytes) else {
        return (error(RPC_DESERIALIZATION_ERROR, "Block decode failed"), None);
    };
    // Core: rpc/mining.cpp:708-724 (`NotTested` displays Core's strings)
    match node.propose(block) {
        Ok(state) => {
            let invalid = match &state {
                BlockState::Invalid(e) => Some(e.clone()),
                BlockState::Valid | BlockState::Error(_) => None,
            };
            (bip22(&state), invalid)
        }
        Err(not_tested) => (text(&not_tested.to_string()), None),
    }
}

/// `invalidateblock`, `reconsiderblock` and `preciousblock` once V replied: V's `Block not found`
/// is the handlers' lookup. Their `RPC_DATABASE_ERROR` on an invalid state is not modelled: V's
/// other failures follow an `AbortNode`, which stops V, so the reply disconnects instead.
// Core: rpc/blockchain.cpp:1971, :2010, :2056 (the lookup); :1978-1980, :2021-2023, :2065-2067
// (the database error); :1982, :2025, :2069 (the null result)
pub fn block_rpc(reply: Result<(), BlockNotFound>) -> Outcome {
    match reply {
        Ok(()) => Ok(Value::Null),
        Err(_) => error(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"),
    }
}

/// A `BlockChecked(block, state)` in the REPLY section's spelling (O8 `R`: `res`, `cls`, `why`).
pub fn checked(hash: BlockHash, state: &BlockState) -> Checked {
    let hash = ChainHash(Hash256::from(hash).to_lendian());
    match state {
        BlockState::Valid => Checked(hash, CheckResult::Valid, None, None),
        BlockState::Invalid(e) => Checked(
            hash,
            CheckResult::Invalid,
            Some(reject_class(e).into()),
            Some(e.reject_reason().into()),
        ),
        BlockState::Error(message) => {
            Checked(hash, CheckResult::Error, None, Some(message.clone()))
        }
    }
}

/// A reject's class in `REJECT_CLASSES`' spelling (`BlockChecked`'s `cls`).
pub fn reject_class(e: &BlockValidationError) -> &'static str {
    REJECT_CLASSES[e.result().as_i32() as usize]
}

/// `ConvertBitsToDouble(nBits)`, `GetDifficulty`'s value.
// Core: validation.cpp:1786-1804, rpc/blockchain.cpp:91-96
pub fn difficulty(bits: u32) -> f64 {
    let mut shift = (bits >> 24) & 0xff;
    let mut diff = f64::from(0x0000_ffff_u32) / f64::from(bits & 0x00ff_ffff);
    while shift < 29 {
        diff *= 256.0;
        shift += 1;
    }
    while shift > 29 {
        diff /= 256.0;
        shift -= 1;
    }
    diff
}

/// The JSON number Core's reply carries for `value`: UniValue writes doubles with 16 significant
/// digits (C's `%.16g`), so the reply's number is that text parsed, not `value` itself.
// Core: univalue/lib/univalue.cpp:75-82
pub fn univalue_double(value: f64) -> Value {
    let text = if value == 0.0 || !value.is_finite() {
        // Neither occurs here: bits' mantissa 0 is refused before a header is stored.
        format!("{value}")
    } else {
        // `{:.15e}`: 16 significant digits, rounded as printf's (both round the exact binary).
        let sci = format!("{value:.15e}");
        let (mantissa, exp) = sci.split_once('e').expect("{:e} has an exponent");
        let exp: i32 = exp.parse().expect("{:e}'s exponent is an integer");
        if (-4..16).contains(&exp) {
            let decimals = usize::try_from(15 - exp).expect("exp < 16");
            trim_fraction(format!("{value:.decimals$}"))
        } else {
            let sign = if exp < 0 {
                '-'
            } else {
                '+'
            };
            format!("{}e{sign}{:02}", trim_fraction(mantissa.to_owned()), exp.abs())
        }
    };
    serde_json::from_str(&text).expect("a finite %g is a JSON number")
}

/// `%g`'s trailing-zero removal.
fn trim_fraction(text: String) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

/// One `getchaintips` tip, as V's index has it.
#[derive(Clone, Debug, PartialEq)]
pub struct TipRow {
    pub height: u32,
    pub hash: BlockHash,
    pub bits: u32,
    /// `nChainWork`, big-endian.
    pub chain_work: [u8; 32],
    pub branchlen: u32,
    pub fork_point: BlockHash,
    /// `ChainTipStatus::name()`.
    pub status: &'static str,
}

/// `getchaintips`' rows, in the POST section's order: height descending, then hash.
// Core: rpc/blockchain.cpp:1898-1939 (the row)
pub fn chain_tips(rows: &[TipRow]) -> Vec<Value> {
    let mut out: Vec<(u32, String, Value)> = rows
        .iter()
        .map(|t| {
            let hash = block_hash_hex(t.hash);
            let row = json!({
                "height": t.height,
                "hash": hash,
                "difficulty": univalue_double(difficulty(t.bits)),
                "chainwork": hex(t.chain_work),
                "branchlen": t.branchlen,
                "forkpoint": block_hash_hex(t.fork_point),
                "status": t.status,
            });
            (t.height, hash, row)
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    out.into_iter().map(|(.., row)| row).collect()
}

fn insert_some<T: Into<Value>>(map: &mut Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(v) = value {
        map.insert(key.to_owned(), v.into());
    }
}

// Core: rpc/blockchain.cpp:1460-1537 SoftForkDescPushBack (DeploymentPos)
fn bip9_entry(b: &Bip9Info) -> Value {
    let mut bip9 = Map::new();
    insert_some(&mut bip9, "bit", b.bit);
    bip9.insert("start_time".into(), b.start_time.into());
    bip9.insert("timeout".into(), b.timeout.into());
    bip9.insert("ehf".into(), b.ehf.into());
    insert_some(&mut bip9, "ehf_height", b.ehf_height);
    insert_some(&mut bip9, "activation_height", b.activation_height);
    bip9.insert("min_activation_height".into(), b.min_activation_height.into());
    bip9.insert("status".into(), b.status.name().into());
    bip9.insert("since".into(), b.since.into());
    bip9.insert("status_next".into(), b.status_next.name().into());
    if let Some(s) = &b.statistics {
        let mut stats = Map::new();
        stats.insert("period".into(), s.period.into());
        stats.insert("elapsed".into(), s.elapsed.into());
        stats.insert("count".into(), s.count.into());
        insert_some(&mut stats, "threshold", s.threshold);
        insert_some(&mut stats, "possible", s.possible);
        bip9.insert("statistics".into(), stats.into());
    }
    insert_some(&mut bip9, "signalling", b.signalling.clone());
    let mut entry = Map::new();
    entry.insert("type".into(), "bip9".into());
    insert_some(&mut entry, "height", b.height);
    entry.insert("active".into(), b.active.into());
    entry.insert("bip9".into(), bip9.into());
    entry.into()
}

/// `getdeploymentinfo` at the tip: `hash`, `height` and `DeploymentInfo`'s entries by name.
// Core: rpc/blockchain.cpp:1736-1739 (the handler), :1664-1694 DeploymentInfo, :1445-1458
// SoftForkDescPushBack (BuriedDeployment)
pub fn deployment_info(hash: BlockHash, height: u32, infos: &[DeploymentInfo]) -> Value {
    let deployments: Map<String, Value> = infos
        .iter()
        .map(|info| {
            let entry = match info {
                DeploymentInfo::Buried(b) => {
                    json!({"type": "buried", "active": b.active, "height": b.height})
                }
                DeploymentInfo::Bip9(b) => bip9_entry(b),
            };
            (info.name().to_owned(), entry)
        })
        .collect();
    json!({"hash": block_hash_hex(hash), "height": height, "deployments": deployments})
}

#[cfg(test)]
mod tests;
