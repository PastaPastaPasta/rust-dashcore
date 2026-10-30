//! BS4b: base-sdk's decode domain against Dash Core's, for `Transaction`, `Block`, `BlockHeader`
//! and every special-transaction payload (types 1 to 12).
//!
//! The judges here take Core's verdict as data ([`CoreDecode`], [`CorePayload`]) so that the
//! same comparison runs live against the oracle (the `diff_bsdk_*` and `diff_payload_decode`
//! targets) and, from `tests/data/bsdk_decode_recorded.json`, without it. This module needs no
//! oracle and compiles without the `oracle` feature (the replay test includes it by path).
//!
//! What is compared is decoding, not re-encoding (the canonical re-serialization is CS0's
//! `core_serialize`): whether the bytes decode, how many are read, and the decoded field values.
//!
//! - **Transactions, blocks, headers.** Each field of base-sdk's value is written on its own
//!   (a hash, a script with its length, an amount) and compared, in wire order, with the same
//!   field read out of Core's re-serialization of the object it decoded. Neither side compares
//!   a whole-object encoding, so the node's serializer is not part of the check.
//! - **Payloads.** A payload's layout depends on its type and version, and Core exposes no field
//!   list, only the payload serialized again. The decoded value is written with base-sdk's wire
//!   codec and compared with Core's re-serialization, under each BLS scheme flag. Three BLS
//!   field kinds are read out as Core holds them rather than as the codec writes them: a
//!   lazy field (a ProTx operator key) keeps its raw bytes in Core until used, and a field under
//!   the process-wide flag (the CbTx and asset-unlock signatures) is serialized by Core under
//!   the flag, zeros when invalid in that scheme ([`patch`]).
//!
//! A divergence is reported as the name of the field only (CS0 §9.5).

use dash_consensus::primitives::canonical;
use dash_pkc::bls::{BlsCtx, BlsGlobal, BlsLazy, BlsSchemeKind, BlsSchemeSource, BlsWirePoint};
use dash_primitives::{
    Block, BlockHeader, CoinbaseCommitment, SpecialPayload, Transaction, TxType,
};
use dash_types::codec::BaseCodec;
use dash_types::{CompactSize, Numeric};

/// Core agrees with base-sdk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agreement {
    /// Both decode, and every compared field is equal.
    BothAccept,
    /// Both reject the bytes.
    BothReject,
}

/// The field in which base-sdk and Core disagree (a fixed name, never a value).
pub type Judged = Result<Agreement, &'static str>;

/// What Core's deserializer did with a transaction, block or header: how many bytes it read, and
/// the object serialized again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreDecode {
    pub consumed: usize,
    pub reencoded: Vec<u8>,
}

/// What `GetTxPayload` did with a payload under one BLS scheme flag: whether the deserializer
/// read every byte, and the payload serialized again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorePayload {
    pub consumed_all: bool,
    pub reencoded: Vec<u8>,
}

/// A decoded field: its name and the bytes it is written as.
type Chunk = (&'static str, Vec<u8>);

fn encoded<T: BaseCodec>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

fn compact(n: usize) -> Vec<u8> {
    encoded(&CompactSize::from(n as u64))
}

/// The first field that differs, by name.
fn first_difference(core: &[Chunk], ours: &[Chunk]) -> Option<&'static str> {
    for (c, o) in core.iter().zip(ours) {
        if c != o {
            return Some(o.0);
        }
    }
    (core.len() != ours.len()).then_some("field count")
}

// ---------------------------------------------------------------------------------------------
// Core's re-serialization, read field by field.

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }

    fn chunk(&mut self, out: &mut Vec<Chunk>, name: &'static str, n: usize) -> Option<()> {
        out.push((name, self.take(n)?.to_vec()));
        Some(())
    }

    /// A CompactSize, as its value and its bytes.
    fn compact(&mut self) -> Option<(u64, Vec<u8>)> {
        let first = *self.0.first()?;
        let width = match first {
            0xfd => 3,
            0xfe => 5,
            0xff => 9,
            _ => 1,
        };
        let raw = self.take(width)?.to_vec();
        let value = match width {
            1 => u64::from(first),
            3 => u64::from(u16::from_le_bytes(raw[1..3].try_into().ok()?)),
            5 => u64::from(u32::from_le_bytes(raw[1..5].try_into().ok()?)),
            _ => u64::from_le_bytes(raw[1..9].try_into().ok()?),
        };
        Some((value, raw))
    }

    /// A CompactSize count, kept as a field.
    fn count(&mut self, out: &mut Vec<Chunk>, name: &'static str) -> Option<u64> {
        let (n, raw) = self.compact()?;
        out.push((name, raw));
        Some(n)
    }

    /// A length-prefixed byte string, prefix included, kept as a field.
    fn bytes(&mut self, out: &mut Vec<Chunk>, name: &'static str) -> Option<()> {
        let (n, mut raw) = self.compact()?;
        raw.extend_from_slice(self.take(usize::try_from(n).ok()?)?);
        out.push((name, raw));
        Some(())
    }

    fn header(&mut self, out: &mut Vec<Chunk>) -> Option<()> {
        self.chunk(out, "header version", 4)?;
        self.chunk(out, "header prev hash", 32)?;
        self.chunk(out, "header merkle root", 32)?;
        self.chunk(out, "header time", 4)?;
        self.chunk(out, "header bits", 4)?;
        self.chunk(out, "header nonce", 4)
    }

    fn tx(&mut self, out: &mut Vec<Chunk>) -> Option<()> {
        let version = i16::from_le_bytes(self.0.get(..2)?.try_into().ok()?);
        let tx_type = u16::from_le_bytes(self.0.get(2..4)?.try_into().ok()?);
        self.chunk(out, "tx version and type", 4)?;
        for _ in 0..self.count(out, "input count")? {
            self.chunk(out, "input prevout hash", 32)?;
            self.chunk(out, "input prevout index", 4)?;
            self.bytes(out, "input script")?;
            self.chunk(out, "input sequence", 4)?;
        }
        for _ in 0..self.count(out, "output count")? {
            self.chunk(out, "output value", 8)?;
            self.bytes(out, "output script")?;
        }
        self.chunk(out, "lock time", 4)?;
        if version >= 3 && tx_type != 0 {
            self.bytes(out, "extra payload")?;
        }
        Some(())
    }
}

// ---------------------------------------------------------------------------------------------
// base-sdk's decoded values, written field by field.

fn header_fields(header: &BlockHeader, out: &mut Vec<Chunk>) {
    out.push(("header version", encoded(&header.version)));
    out.push(("header prev hash", encoded(&header.prev_hash)));
    out.push(("header merkle root", encoded(&header.merkle_root)));
    out.push(("header time", encoded(&header.time)));
    out.push(("header bits", encoded(&header.bits)));
    out.push(("header nonce", encoded(&header.nonce)));
}

fn tx_fields(tx: &Transaction, out: &mut Vec<Chunk>) {
    // Core's version/type word, from dash-consensus' canonical module.
    let mut word = Vec::new();
    canonical::write_version_and_type(tx, &mut word);
    out.push(("tx version and type", word));
    out.push(("input count", compact(tx.inputs.len())));
    for input in &tx.inputs {
        out.push(("input prevout hash", encoded(&input.prevout.hash)));
        out.push(("input prevout index", encoded(&input.prevout.index)));
        out.push(("input script", encoded(&input.script_sig)));
        out.push(("input sequence", encoded(&input.sequence)));
    }
    out.push(("output count", compact(tx.outputs.len())));
    for output in &tx.outputs {
        out.push(("output value", encoded(&output.value)));
        out.push(("output script", encoded(&output.script_pubkey)));
    }
    out.push(("lock time", encoded(&tx.lock_time)));
    // The field exists on the wire exactly when Core's `HasExtraPayloadField` holds.
    if tx.version >= 3 && tx.tx_type != TxType::Spend {
        out.push(("extra payload", encoded(&tx.extra_payload)));
    }
}

/// Decodes `bytes` as `T` from the front: the value and the bytes read.
fn decode_front<T: BaseCodec>(bytes: &[u8]) -> Option<(T, usize)> {
    let mut cursor = bytes;
    let value = T::decode(&mut cursor).ok()?;
    Some((value, bytes.len() - cursor.len()))
}

/// Header, transaction or block against Core: the verdict, the length read, then the fields.
fn judge_object<T: BaseCodec>(
    bytes: &[u8],
    core: Option<&CoreDecode>,
    core_fields: impl FnOnce(&mut Reader<'_>, &mut Vec<Chunk>) -> Option<()>,
    ours_fields: impl FnOnce(&T, &mut Vec<Chunk>),
) -> Judged {
    match (core, decode_front::<T>(bytes)) {
        (None, None) => Ok(Agreement::BothReject),
        (Some(core), Some((value, consumed))) => {
            if consumed != core.consumed {
                return Err("consumed");
            }
            let (mut theirs, mut ours) = (Vec::new(), Vec::new());
            let mut reader = Reader(&core.reencoded);
            if core_fields(&mut reader, &mut theirs).is_none() || !reader.0.is_empty() {
                return Err("core serialization");
            }
            ours_fields(&value, &mut ours);
            match first_difference(&theirs, &ours) {
                Some(field) => Err(field),
                None => Ok(Agreement::BothAccept),
            }
        }
        _ => Err("decode verdict"),
    }
}

/// `diff_bsdk_header_decode`: one 80-byte header.
pub fn judge_header(bytes: &[u8], core: Option<&CoreDecode>) -> Judged {
    judge_object::<BlockHeader>(bytes, core, |r, out| r.header(out), header_fields)
}

/// `diff_bsdk_tx_decode`: one transaction read from the front of `bytes`.
pub fn judge_tx(bytes: &[u8], core: Option<&CoreDecode>) -> Judged {
    judge_object::<Transaction>(bytes, core, |r, out| r.tx(out), tx_fields)
}

/// `diff_bsdk_block_decode`: one block read from the front of `bytes`.
pub fn judge_block(bytes: &[u8], core: Option<&CoreDecode>) -> Judged {
    judge_object::<Block>(
        bytes,
        core,
        |r, out| {
            r.header(out)?;
            for _ in 0..r.count(out, "transaction count")? {
                r.tx(out)?;
            }
            Some(())
        },
        |block: &Block, out| {
            header_fields(&block.header, out);
            out.push(("transaction count", compact(block.transactions.len())));
            for tx in &block.transactions {
                tx_fields(tx, out);
            }
        },
    )
}

// ---------------------------------------------------------------------------------------------
// Payloads.

/// The special-transaction types `diff_payload_decode` covers: 1 to 12, the v24 set (the shared-
/// masternode types 10 to 12 do not exist on v23.1, whose payload layouts also differ).
pub const PAYLOAD_TYPES: u16 = 12;

/// The `diff_payload_decode` input: a selector byte, then the payload. The selector picks the
/// type (`1 + selector % types`), so every input names a type the oracle decodes.
pub fn split_payload_input(bytes: &[u8], types: u16) -> Option<(u16, &[u8])> {
    let (&selector, payload) = bytes.split_first()?;
    Some((1 + u16::from(selector) % types, payload))
}

/// A BLS field of a payload that Core serializes differently from base-sdk's codec, at `at` in
/// the codec's bytes (`None` when the field cannot be placed, which leaves the bytes to differ).
enum Slot {
    /// A lazy field, `valid` when its bytes are an object in its scheme. Core serializes the raw
    /// bytes it read (nothing used the field); the codec writes zeros for an invalid object.
    Lazy {
        raw: Vec<u8>,
        valid: bool,
        at: Option<usize>,
    },
    /// A field under the process-wide flag. The codec writes its wire bytes; Core writes the
    /// object under the flag, zeros when the bytes are no object in that scheme.
    Global {
        core: Vec<u8>,
        at: usize,
    },
}

/// A G1 point (the generator, compressed, basic scheme): a lazy key the codec writes as it is.
const VALID_KEY: [u8; 48] = [
    0x97, 0xf1, 0xd3, 0xa7, 0x31, 0x97, 0xd7, 0x94, 0x26, 0x95, 0x63, 0x8c, 0x4f, 0xa9, 0xac, 0x0f,
    0xc3, 0x68, 0x8c, 0x4f, 0x97, 0x74, 0xb9, 0x05, 0xa1, 0x4e, 0x3a, 0x3f, 0x17, 0x1b, 0xac, 0x58,
    0x6c, 0x55, 0xe8, 0x3f, 0xf9, 0x7a, 0x1a, 0xef, 0xfb, 0x3a, 0xf0, 0x0a, 0xdb, 0x22, 0xc6, 0xbb,
];

/// Where the codec writes the lazy key `set` replaces: the first byte that differs between the
/// payload with the key unset (zeros) and with [`VALID_KEY`]. Not an offset in the input: the
/// codec re-encodes other fields (a net-info list) at other lengths.
fn locate_key<T: BaseCodec + Clone, K: BlsWirePoint<48>>(
    payload: &T,
    set: impl Fn(&mut T, BlsLazy<K, 48>),
) -> Option<usize> {
    let with = |key: BlsLazy<K, 48>| {
        let mut copy = payload.clone();
        set(&mut copy, key);
        encoded(&copy)
    };
    let unset = with(BlsLazy::new(BlsSchemeKind::Basic));
    let set = with(BlsLazy::from_bytes(&VALID_KEY, BlsSchemeSource::AlwaysBasic));
    if unset.len() != set.len() {
        return None;
    }
    unset.iter().zip(&set).position(|(a, b)| a != b)
}

fn lazy<K: BlsWirePoint<N>, const N: usize>(field: &BlsLazy<K, N>, at: Option<usize>) -> Slot {
    // The raw bytes first: `get` decodes the field, which serializes zeros once invalid. The
    // copy keeps the payload's field as it was decoded.
    let raw = field.to_bytes().to_vec();
    let valid = field.clone().get().is_valid();
    Slot::Lazy {
        raw,
        valid,
        at,
    }
}

/// `tail` is what the codec writes after the field.
fn global<K: BlsWirePoint<N>, const N: usize>(
    field: &BlsGlobal<K, N>,
    ctx: BlsCtx,
    written: usize,
    tail: usize,
) -> Option<Slot> {
    Some(Slot::Global {
        core: field.get(ctx).to_bytes(ctx.scheme()).to_vec(),
        at: written.checked_sub(N + tail)?,
    })
}

fn slots(payload: &SpecialPayload, ctx: BlsCtx, written: usize) -> Vec<Slot> {
    macro_rules! operator_key {
        ($p:expr) => {
            vec![lazy(
                &$p.pub_key_operator,
                locate_key($p, |copy, key| copy.pub_key_operator = key),
            )]
        };
    }
    match payload {
        SpecialPayload::ProviderRegister(p) => operator_key!(p),
        SpecialPayload::ProviderUpdateRegistrar(p) => operator_key!(p),
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => operator_key!(p),
        // The signature is followed by the credit pool balance (an i64) and, from version 4, the
        // asset unlocks root (32 bytes).
        SpecialPayload::CoinbaseCommitment(CoinbaseCommitment {
            version,
            best_cl_signature: Some(sig),
            ..
        }) => global(
            sig,
            ctx,
            written,
            if *version >= 4 {
                8 + 32
            } else {
                8
            },
        )
        .into_iter()
        .collect(),
        // The signature ends the payload.
        SpecialPayload::AssetUnlock(unlock) => {
            global(&unlock.quorum_sig, ctx, written, 0).into_iter().collect()
        }
        _ => Vec::new(),
    }
}

/// The payload as base-sdk's codec writes it.
fn payload_bytes(payload: &SpecialPayload) -> Vec<u8> {
    match payload {
        SpecialPayload::ProviderRegister(p) => encoded(p),
        SpecialPayload::ProviderUpdateService(p) => encoded(p),
        SpecialPayload::ProviderUpdateRegistrar(p) => encoded(p),
        SpecialPayload::ProviderUpdateRevoke(p) => encoded(p),
        SpecialPayload::UnknownProTxVersion {
            version,
            ..
        } => version.to_le_bytes().to_vec(),
        SpecialPayload::CoinbaseCommitment(p) => encoded(p),
        SpecialPayload::QuorumCommitment(p) => encoded(p),
        SpecialPayload::MnhfSignal(p) => encoded(p),
        SpecialPayload::AssetLock(p) => encoded(p),
        SpecialPayload::AssetUnlock(p) => encoded(p),
        SpecialPayload::ProviderDissolve(p) => encoded(p),
        SpecialPayload::ProviderUpdateShare(p) => encoded(p),
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => encoded(p),
        SpecialPayload::Unknown {
            data,
            ..
        } => data.clone(),
    }
}

/// What Core serializes for `payload` as base-sdk decoded it, under the BLS scheme flag `legacy`:
/// the codec's bytes with the BLS fields of [`Slot`] as Core holds them. Every other byte is the
/// codec's, so a difference anywhere else is a divergence.
fn expected_payload_bytes(payload: &SpecialPayload, legacy: bool) -> Vec<u8> {
    let mut expected = payload_bytes(payload);
    for slot in slots(payload, BlsCtx::from_legacy(legacy), expected.len()) {
        let (at, core) = match slot {
            Slot::Lazy {
                raw,
                valid: false,
                at: Some(at),
            } => (at, raw),
            Slot::Global {
                core,
                at,
                ..
            } => (at, core),
            Slot::Lazy {
                ..
            } => continue,
        };
        if let Some(field) = expected.get_mut(at..at + core.len()) {
            field.copy_from_slice(&core);
        }
    }
    expected
}

/// `diff_payload_decode` under one BLS scheme flag: what `GetTxPayload<T>` does with `payload`.
/// Core's side is its deserializer: a throw is a rejection; otherwise it reports whether it read
/// every byte (`GetTxPayload` returns nothing unless it did).
pub fn judge_payload(
    tx_type: u16,
    legacy: bool,
    payload: &[u8],
    core: Option<&CorePayload>,
) -> Judged {
    let mut cursor = payload;
    let ours = SpecialPayload::decode(TxType::from_base(tx_type), &mut cursor);
    let (core, ours) = match (core, ours) {
        (None, Err(_)) => return Ok(Agreement::BothReject),
        (Some(core), Ok(ours)) => (core, ours),
        _ => return Err("decode verdict"),
    };
    if cursor.is_empty() != core.consumed_all {
        return Err("consumed");
    }
    if expected_payload_bytes(&ours, legacy) != core.reencoded {
        return Err("payload fields");
    }
    Ok(Agreement::BothAccept)
}
