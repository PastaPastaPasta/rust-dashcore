//! Per-block differential checks.
//!
//! Every block is decoded independently by `dashcore` and by `dash_primitives`, and each
//! result is compared with the input bytes, with the other library and, when available, with
//! an independent reference: the oracle's `getblock <hash> 1` view, the chain linkage from
//! blk files, Core's txid rule ([`core_txid`]), or the oracle's `getblock <hash> 2` JSON for
//! sampled blocks.

use dash_pkc::bls::BlsCtx;
use dash_primitives::{
    Block as BsBlock, BlockHeader as BsHeader, BlockInvalid, SpecialPayload, Transaction as BsTx,
};
use dash_types::codec::BaseCodec;
use dash_types::{Checkable, Hashable, Numeric};
use dashcore::Transaction as DcTx;
use dashcore::block::{Block as DcBlock, Header as DcHeader};
use dashcore::consensus::{deserialize, deserialize_partial, serialize};
use dashcore::hashes::{Hash, sha256d};
use dashcore::transaction::special_transaction::TransactionPayload as DcPayload;
use hex_conservative::DisplayHex;
use serde_json::Value;

use crate::consensus;
use crate::payload::{self, FieldDiff, bs_bytes, dc_bytes};
use crate::report::{MAX_REPRO_TX_LEN, Mismatch, Side, Stats};
use crate::split::{self, HEADER_LEN, TxSpan};
use crate::verbose::{self, PayloadDecode, PayloadStatus};

/// A 32-byte hash in internal (little-endian, storage) byte order.
pub type H256 = [u8; 32];

/// Display hex (reversed), as dashd prints hashes.
pub fn rev_hex(h: &H256) -> String {
    let mut r = *h;
    r.reverse();
    r.to_lower_hex_string()
}

/// Parses display (reversed) hex into internal byte order.
pub fn parse_rev_hex(s: &str) -> Option<H256> {
    let mut out: H256 = hex_conservative::decode_to_array(s).ok()?;
    out.reverse();
    Some(out)
}

/// The fields of `getblock <hash> 1` the checker uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleBlock {
    pub hash: H256,
    pub height: u32,
    pub version: i32,
    pub merkle_root: H256,
    pub time: u32,
    pub nonce: u32,
    pub bits: u32,
    pub prev: Option<H256>,
    pub txids: Vec<H256>,
}

impl OracleBlock {
    pub fn from_json(v: &Value) -> Option<Self> {
        let hash_field = |k: &str| v.get(k).and_then(Value::as_str).and_then(parse_rev_hex);
        Some(Self {
            hash: hash_field("hash")?,
            height: u32::try_from(v.get("height")?.as_u64()?).ok()?,
            version: i32::try_from(v.get("version")?.as_i64()?).ok()?,
            merkle_root: hash_field("merkleroot")?,
            time: u32::try_from(v.get("time")?.as_u64()?).ok()?,
            nonce: u32::try_from(v.get("nonce")?.as_u64()?).ok()?,
            bits: u32::from_str_radix(v.get("bits")?.as_str()?, 16).ok()?,
            prev: hash_field("previousblockhash"),
            txids: v
                .get("tx")?
                .as_array()?
                .iter()
                .map(|t| t.as_str().and_then(parse_rev_hex))
                .collect::<Option<Vec<_>>>()?,
        })
    }
}

/// Where the block came from, for reports.
#[derive(Debug, Clone)]
pub struct Context {
    pub net: String,
    pub source: &'static str,
}

/// One block to check.
#[derive(Debug, Clone, Default)]
pub struct BlockJob<'a> {
    pub bytes: &'a [u8],
    pub height: Option<u32>,
    /// The block is not on the selected best chain (blk-file mode only).
    pub stale: bool,
    /// Independent reference for the block hash (oracle, or the next block's `prev` link).
    pub expected_hash: Option<H256>,
    pub oracle: Option<&'a OracleBlock>,
    /// `getblock <hash> 2` for sampled blocks.
    pub verbose: Option<&'a Value>,
}

/// Collects mismatches for one block.
pub struct Sink<'a> {
    ctx: &'a Context,
    height: Option<u32>,
    block_hash: Option<String>,
    pub out: Vec<Mismatch>,
}

/// Transaction context attached to a mismatch.
#[derive(Clone, Copy)]
pub struct TxRef<'a> {
    pub index: usize,
    pub txid: Option<&'a H256>,
    pub raw: Option<&'a [u8]>,
}

impl Sink<'_> {
    /// The network name of the blocks being checked (`Context::net`).
    pub(crate) fn net(&self) -> &str {
        &self.ctx.net
    }

    /// The block hash the checks established (the reference, else a library's).
    pub(crate) fn block_hash(&self) -> Option<&str> {
        self.block_hash.as_deref()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        check: &str,
        side: Side,
        tx: Option<TxRef<'_>>,
        field: Option<String>,
        expected: Option<String>,
        actual: Option<String>,
        detail: impl Into<String>,
    ) {
        self.out.push(Mismatch {
            net: self.ctx.net.clone(),
            source: self.ctx.source.to_string(),
            height: self.height,
            block_hash: self.block_hash.clone(),
            tx_index: tx.map(|t| t.index),
            txid: tx.and_then(|t| t.txid).map(rev_hex),
            check: check.to_string(),
            side,
            field,
            expected,
            actual,
            detail: detail.into(),
            tx_hex: tx
                .and_then(|t| t.raw)
                .filter(|r| r.len() <= MAX_REPRO_TX_LEN)
                .map(|r| r.to_lower_hex_string()),
            unit: None,
            run: None,
        });
    }

    /// A mismatch without a field or values, such as a decode failure.
    pub(crate) fn fail(
        &mut self,
        check: &str,
        side: Side,
        tx: Option<TxRef<'_>>,
        detail: impl Into<String>,
    ) {
        self.push(check, side, tx, None, None, None, detail);
    }

    /// The decoded value, or `None` after reporting the decode error against `side`.
    fn decoded<T>(
        &mut self,
        check: &str,
        side: Side,
        tx: Option<TxRef<'_>>,
        result: Result<T, String>,
    ) -> Option<T> {
        result.map_err(|e| self.fail(check, side, tx, e)).ok()
    }

    /// Field differences between the libraries, with nothing independent to decide them.
    fn lib_diffs(&mut self, check: &str, tx: TxRef<'_>, diffs: Vec<FieldDiff>) {
        for d in diffs {
            self.push(
                check,
                Side::Undecided,
                Some(tx),
                Some(d.field),
                Some(d.dashcore),
                Some(d.base_sdk),
                "expected=dashcore, actual=base-sdk",
            );
        }
    }

    /// Compares each library's value with a reference, or the libraries with each other when
    /// there is no reference, and reports who is wrong.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn judge<T: PartialEq>(
        &mut self,
        check: &str,
        tx: Option<TxRef<'_>>,
        field: Option<&str>,
        dc: Option<&T>,
        bs: Option<&T>,
        reference: Option<&T>,
        show: impl Fn(&T) -> String,
    ) {
        let field = field.map(str::to_string);
        match reference {
            Some(r) => {
                let dc_bad = dc.filter(|v| *v != r);
                let bs_bad = bs.filter(|v| *v != r);
                match (dc_bad, bs_bad) {
                    (Some(a), Some(b)) if a == b => {
                        self.push(check, Side::Both, tx, field, Some(show(r)), Some(show(a)), "")
                    }
                    (a, b) => {
                        if let Some(a) = a {
                            let (e, a) = (Some(show(r)), Some(show(a)));
                            self.push(check, Side::Dashcore, tx, field.clone(), e, a, "");
                        }
                        if let Some(b) = b {
                            let (e, b) = (Some(show(r)), Some(show(b)));
                            self.push(check, Side::BaseSdk, tx, field, e, b, "");
                        }
                    }
                }
            }
            None => {
                if let (Some(a), Some(b)) = (dc, bs)
                    && a != b
                {
                    let detail = "libraries disagree; expected=dashcore, actual=base-sdk";
                    self.push(
                        check,
                        Side::Undecided,
                        tx,
                        field,
                        Some(show(a)),
                        Some(show(b)),
                        detail,
                    );
                }
            }
        }
    }
}

fn bs_decode_exact<T: BaseCodec>(bytes: &[u8]) -> Result<T, String> {
    let mut cur = bytes;
    let v = T::decode(&mut cur).map_err(|e| e.to_string())?;
    if !cur.is_empty() {
        return Err(format!("{} trailing bytes not consumed", cur.len()));
    }
    Ok(v)
}

fn dc_decode_exact<T: dashcore::consensus::Decodable>(bytes: &[u8]) -> Result<T, String> {
    match deserialize_partial::<T>(bytes) {
        Ok((v, n)) if n == bytes.len() => Ok(v),
        Ok((_, n)) => Err(format!("{} trailing bytes not consumed", bytes.len() - n)),
        Err(e) => Err(e.to_string()),
    }
}

/// Dash Core's txid of one transaction (`CTransaction::ComputeHash`, v24), computed from its
/// raw bytes and the reference split: sha256d of the serialization, except for an asset unlock
/// whose payload has the expected size and version >= 2 (`IsAssetUnlockWithStableTxid`): its
/// trailing quorum signing info (`ASSET_UNLOCK_QUORUM_INFO_SIZE`) is zeroed first. The payload
/// is the last field serialized, so that info is the transaction's last 132 bytes.
pub fn core_txid(raw: &[u8], span: &TxSpan) -> H256 {
    const QUORUM_INFO_SIZE: usize = 4 + 32 + 96;
    const PAYLOAD_SIZE: usize = 1 + 8 + 4 + QUORUM_INFO_SIZE;
    const TRANSACTION_ASSET_UNLOCK: u16 = 9;
    const STABLE_TXID_VERSION: u8 = 2;
    let stable = span.version >= 3
        && span.tx_type == TRANSACTION_ASSET_UNLOCK
        && span.payload.as_ref().is_some_and(|p| p.len() == PAYLOAD_SIZE)
        && raw.len() >= PAYLOAD_SIZE
        && raw[raw.len() - PAYLOAD_SIZE] >= STABLE_TXID_VERSION;
    if !stable {
        return sha256d::Hash::hash(raw).to_byte_array();
    }
    let mut zeroed = raw.to_vec();
    let n = zeroed.len();
    zeroed[n - QUORUM_INFO_SIZE..].fill(0);
    sha256d::Hash::hash(&zeroed).to_byte_array()
}

/// Runs every check on one block.
pub fn check_block(ctx: &Context, job: &BlockJob<'_>, stats: &mut Stats) -> Vec<Mismatch> {
    stats.blocks += 1;
    stats.bytes += job.bytes.len() as u64;
    if job.stale {
        stats.stale_blocks += 1;
    } else if let Some(h) = job.height {
        stats.saw_height(h);
    }

    let mut sink = Sink {
        ctx,
        height: job.height,
        block_hash: job.expected_hash.as_ref().or(job.oracle.map(|o| &o.hash)).map(rev_hex),
        out: Vec::new(),
    };
    if job.bytes.len() < HEADER_LEN {
        sink.fail("block.short", Side::Harness, None, "fewer than 80 bytes");
    } else {
        check_block_bytes(&mut sink, job, stats);
    }
    stats.count(&sink.out);
    sink.out
}

/// The checks of [`check_block`] for a block of at least [`HEADER_LEN`] bytes.
fn check_block_bytes(sink: &mut Sink<'_>, job: &BlockJob<'_>, stats: &mut Stats) {
    let bytes = job.bytes;
    let header = &bytes[..HEADER_LEN];
    check_header(sink, job, stats, header);

    // Reference transaction boundaries.
    let spans = match split::split_block(bytes) {
        Ok(s) => Some(s.txs),
        Err(e) => {
            sink.fail("split", Side::Harness, None, e.to_string());
            None
        }
    };

    // Whole-block decode and re-encode.
    let dc_block =
        sink.decoded("block.decode", Side::Dashcore, None, dc_decode_exact::<DcBlock>(bytes));
    if dc_block.as_ref().is_some_and(|b| serialize(b) != bytes) {
        sink.fail("block.reencode", Side::Dashcore, None, "");
    }
    let bs_block =
        sink.decoded("block.decode", Side::BaseSdk, None, bs_decode_exact::<BsBlock>(bytes));
    if bs_block.as_ref().is_some_and(|b| bs_bytes(b) != bytes) {
        sink.fail("block.reencode", Side::BaseSdk, None, "");
    }

    // Merkle root: each library's computation against the oracle, else the raw header field.
    let header_root: H256 = header[36..68].try_into().expect("80-byte header");
    let dc_root =
        dc_block.as_ref().and_then(|b| b.compute_merkle_root()).map(|m| m.to_byte_array());
    // The harness checks base-sdk's own merkle helper, which node code may not use (root
    // clippy.toml, A1 §1.3).
    #[expect(
        clippy::disallowed_methods,
        reason = "conformance checks base-sdk's Block::merkle itself"
    )]
    let bs_root = bs_block.as_ref().map(|b| b.merkle().0.to_lendian());
    let root_ref = job.oracle.map_or(header_root, |o| o.merkle_root);
    sink.judge(
        "block.merkle_root",
        None,
        None,
        dc_root.as_ref(),
        bs_root.as_ref(),
        Some(&root_ref),
        rev_hex,
    );

    // Per-transaction checks.
    let n_txs = spans
        .as_ref()
        .map(Vec::len)
        .or(dc_block.as_ref().map(|b| b.txdata.len()))
        .or(bs_block.as_ref().map(|b| b.transactions.len()))
        .unwrap_or(0);
    stats.txs += n_txs as u64;
    if let Some(o) = job.oracle
        && o.txids.len() != n_txs
    {
        let (e, a) = (Some(o.txids.len().to_string()), Some(n_txs.to_string()));
        sink.push("block.tx_count", Side::Harness, None, None, e, a, "oracle vs split");
    }
    for (lib_len, side) in [
        (dc_block.as_ref().map(|b| b.txdata.len()), Side::Dashcore),
        (bs_block.as_ref().map(|b| b.transactions.len()), Side::BaseSdk),
    ] {
        if let Some(l) = lib_len
            && l != n_txs
        {
            let (e, a) = (Some(n_txs.to_string()), Some(l.to_string()));
            sink.push("block.tx_count", side, None, None, e, a, "");
        }
    }

    let raw_tx = |i: usize| spans.as_ref().map(|s| &bytes[s[i].range.clone()]);
    let ref_txids: Vec<Option<H256>> = (0..n_txs)
        .map(|i| {
            job.oracle
                .and_then(|o| o.txids.get(i).copied())
                .or_else(|| Some(core_txid(raw_tx(i)?, &spans.as_ref()?[i])))
        })
        .collect();
    let txrefs: Vec<TxRef<'_>> = (0..n_txs)
        .map(|i| TxRef {
            index: i,
            txid: ref_txids[i].as_ref(),
            raw: raw_tx(i),
        })
        .collect();

    // Transactions come from the decoded block when it decoded, else one by one from the
    // reference split, so a single bad transaction does not hide the rest of the block.
    let dc_owned = match &dc_block {
        Some(_) => Vec::new(),
        None => decode_each(sink, Side::Dashcore, &txrefs, dc_decode_exact::<DcTx>),
    };
    let bs_owned = match &bs_block {
        Some(_) => Vec::new(),
        None => decode_each(sink, Side::BaseSdk, &txrefs, bs_decode_exact::<BsTx>),
    };
    let dc_txs = lib_txs(dc_block.as_ref().map(|b| b.txdata.as_slice()), &dc_owned, n_txs);
    let bs_txs = lib_txs(bs_block.as_ref().map(|b| b.transactions.as_slice()), &bs_owned, n_txs);

    let mut payload_status = Vec::with_capacity(n_txs);
    for (i, &txref) in txrefs.iter().enumerate() {
        let span = spans.as_ref().map(|s| &s[i]);
        if let Some(s) = span
            && s.payload.is_some()
        {
            *stats.special_txs.entry(s.tx_type).or_default() += 1;
        }
        let oracle_txid = job.oracle.and_then(|o| o.txids.get(i));
        payload_status.push(check_tx(sink, txref, span, oracle_txid, dc_txs[i], bs_txs[i]));
    }

    consensus::check(
        sink,
        bytes,
        &consensus::Refs {
            hash: job.expected_hash.as_ref().or(job.oracle.map(|o| &o.hash)),
            merkle_root: &root_ref,
            txids: &ref_txids,
            txs: &txrefs,
            spans: spans.as_deref(),
        },
        stats,
    );

    #[cfg(feature = "bsdk-judges")]
    crate::bsdk_judges::check(sink, bytes, spans.as_deref(), &txrefs, stats);

    // base-sdk's stateless validation (PoW against nBits, merkle root, coinbase rules,
    // CheckTransaction, payload checks). dashd accepted every best-chain block, so a rejection
    // is a base-sdk bug. Blocks off the best chain may be invalid and are not validated.
    if !job.stale
        && let Some(e) = bs_block.as_ref().and_then(Checkable::check)
    {
        let tx = match &e {
            BlockInvalid::Transaction {
                index,
                ..
            } => txrefs.get(*index).copied(),
            _ => None,
        };
        sink.fail("block.check", Side::BaseSdk, tx, e.to_string());
    }

    if let (Some(v), Some(spans)) = (job.verbose, spans.as_ref()) {
        stats.verbose_blocks += 1;
        verbose::check(sink, v, bytes, spans, &dc_txs, &bs_txs, &payload_status);
    }
}

/// Header decode, block hash (X11) and header fields, independently of transaction decoding.
fn check_header(sink: &mut Sink<'_>, job: &BlockJob<'_>, stats: &mut Stats, header: &[u8]) {
    let dc_header: Option<DcHeader> = sink.decoded(
        "header.decode",
        Side::Dashcore,
        None,
        deserialize(header).map_err(|e| e.to_string()),
    );
    let bs_header: Option<BsHeader> =
        sink.decoded("header.decode", Side::BaseSdk, None, bs_decode_exact(header));
    // dashcore's proof-of-work check against the header's own nBits (best chain only).
    if !job.stale
        && let Some(h) = &dc_header
        && let Err(e) = h.validate_pow(h.target())
    {
        sink.fail("header.pow", Side::Dashcore, None, e.to_string());
    }
    let dc_hash = dc_header.map(|h| h.block_hash().to_byte_array());
    let bs_hash = bs_header.map(|h| h.hash().to_lendian());
    if sink.block_hash.is_none() {
        sink.block_hash = dc_hash.or(bs_hash).as_ref().map(rev_hex);
    }
    let reference_hash = job.expected_hash.or(job.oracle.map(|o| o.hash));
    if reference_hash.is_some() {
        stats.oracle_hash_checks += 1;
    }
    sink.judge(
        "block.hash",
        None,
        None,
        dc_hash.as_ref(),
        bs_hash.as_ref(),
        reference_hash.as_ref(),
        rev_hex,
    );
    if let (Some(oracle), Some(expected)) = (job.oracle, job.expected_hash)
        && oracle.hash != expected
    {
        let (e, a) = (Some(rev_hex(&expected)), Some(rev_hex(&oracle.hash)));
        sink.push("oracle.hash", Side::Harness, None, None, e, a, "oracle vs chain index");
    }
    check_header_fields(sink, job.oracle, dc_header.as_ref(), bs_header.as_ref());
}

/// Decodes every transaction on its own from its raw bytes (when the split found them).
fn decode_each<T>(
    sink: &mut Sink<'_>,
    side: Side,
    txrefs: &[TxRef<'_>],
    decode: impl Fn(&[u8]) -> Result<T, String>,
) -> Vec<Option<T>> {
    txrefs.iter().map(|&tx| sink.decoded("tx.decode", side, Some(tx), decode(tx.raw?))).collect()
}

/// One library's transactions: from its decoded block, else from `owned` (see [`decode_each`]).
fn lib_txs<'a, T>(block: Option<&'a [T]>, owned: &'a [Option<T>], n: usize) -> Vec<Option<&'a T>> {
    (0..n)
        .map(|i| match block {
            Some(b) => b.get(i),
            None => owned.get(i).and_then(Option::as_ref),
        })
        .collect()
}

/// Re-encoding, txid and field checks for one transaction. Returns how each library's payload
/// decode went (as reported here).
fn check_tx(
    sink: &mut Sink<'_>,
    txref: TxRef<'_>,
    span: Option<&TxSpan>,
    oracle_txid: Option<&H256>,
    dc_tx: Option<&DcTx>,
    bs_tx: Option<&BsTx>,
) -> PayloadStatus {
    if let Some(raw) = txref.raw {
        for (side, encoded) in
            [(Side::Dashcore, dc_tx.map(serialize)), (Side::BaseSdk, bs_tx.map(bs_bytes))]
        {
            if let Some(enc) = encoded
                && enc != raw
            {
                let a = Some(enc.to_lower_hex_string());
                sink.push("tx.reencode", side, Some(txref), None, None, a, "");
            }
        }
    }

    let dc_txid = dc_tx.map(|t| t.txid().to_byte_array());
    let bs_txid = bs_tx.map(|t| t.hash().to_lendian());
    sink.judge(
        "tx.txid",
        Some(txref),
        None,
        dc_txid.as_ref(),
        bs_txid.as_ref(),
        txref.txid,
        rev_hex,
    );
    // The reference used without an oracle (blk mode) must agree with dashd's txid.
    if let (Some(o), Some(r), Some(s)) = (oracle_txid, txref.raw, span) {
        let reference = core_txid(r, s);
        if *o != reference {
            let (e, a) = (Some(rev_hex(o)), Some(rev_hex(&reference)));
            let detail = "the harness's Core txid rule disagrees with the oracle";
            sink.push("tx.txid_ref", Side::Harness, Some(txref), None, e, a, detail);
        }
    }

    // When only one library decoded the transaction, still check its payload on its own.
    let failed = |ok: bool| {
        if ok {
            PayloadDecode::Ok
        } else {
            PayloadDecode::Failed
        }
    };
    match (dc_tx, bs_tx, span) {
        (Some(a), Some(b), _) => compare_tx_fields(sink, txref, span, a, b),
        (None, Some(b), Some(s)) => PayloadStatus {
            dashcore: PayloadDecode::Ok,
            base_sdk: failed(s.payload.is_none() || bs_payload(sink, txref, s, b).is_some()),
        },
        (Some(a), None, Some(s)) => PayloadStatus {
            dashcore: failed(s.payload.is_none() || dc_payload(sink, txref, s, a).is_some()),
            base_sdk: PayloadDecode::Ok,
        },
        _ => PayloadStatus::default(),
    }
}

/// Header fields per library, against the oracle when there is one (else lib vs lib).
fn check_header_fields(
    sink: &mut Sink<'_>,
    o: Option<&OracleBlock>,
    dc: Option<&DcHeader>,
    bs: Option<&BsHeader>,
) {
    let dc_fields = dc.map(|h| {
        [
            h.version.to_consensus().to_string(),
            rev_hex(&h.prev_blockhash.to_byte_array()),
            rev_hex(&h.merkle_root.to_byte_array()),
            h.time.to_string(),
            format!("{:08x}", h.bits.to_consensus()),
            h.nonce.to_string(),
        ]
    });
    let bs_fields = bs.map(|h| {
        [
            h.version.to_string(),
            rev_hex(&h.prev_hash.to_lendian()),
            rev_hex(&h.merkle_root.to_lendian()),
            h.time.to_string(),
            format!("{:08x}", h.bits),
            h.nonce.to_string(),
        ]
    });
    let oracle = o.map(|o| {
        [
            o.version.to_string(),
            rev_hex(&o.prev.unwrap_or([0; 32])),
            rev_hex(&o.merkle_root),
            o.time.to_string(),
            format!("{:08x}", o.bits),
            o.nonce.to_string(),
        ]
    });
    let names = ["version", "prev_hash", "merkle_root", "time", "bits", "nonce"];
    for (i, name) in names.iter().enumerate() {
        sink.judge(
            "header.field",
            None,
            Some(name),
            dc_fields.as_ref().map(|f| &f[i]),
            bs_fields.as_ref().map(|f| &f[i]),
            oracle.as_ref().map(|f| &f[i]),
            String::clone,
        );
    }
}

fn compare_tx_fields(
    sink: &mut Sink<'_>,
    txref: TxRef<'_>,
    span: Option<&TxSpan>,
    a: &DcTx,
    b: &BsTx,
) -> PayloadStatus {
    let mut c = payload::Cmp::new("tx");
    let dc_type = a.special_transaction_payload.as_ref().map_or(0, |p| p.get_type().to_u16());
    let dc_raw_version = (u32::from(dc_type) << 16) | u32::from(a.version);
    c.int("raw_version", dc_raw_version.into(), (b.raw_version() as u32).into());
    c.int("inputs.len", a.input.len() as i128, b.inputs.len() as i128);
    for (i, (x, y)) in a.input.iter().zip(&b.inputs).enumerate() {
        c.bytes(
            &format!("inputs[{i}].prevout"),
            &dc_bytes(&x.previous_output),
            &bs_bytes(&y.prevout),
        );
        c.bytes(
            &format!("inputs[{i}].script_sig"),
            x.script_sig.as_bytes(),
            y.script_sig.as_bytes(),
        );
        c.int(&format!("inputs[{i}].sequence"), x.sequence.into(), y.sequence.into());
    }
    c.int("outputs.len", a.output.len() as i128, b.outputs.len() as i128);
    for (i, (x, y)) in a.output.iter().zip(&b.outputs).enumerate() {
        c.int(&format!("outputs[{i}].value"), x.value.into(), y.value.into());
        c.bytes(
            &format!("outputs[{i}].script_pubkey"),
            x.script_pubkey.as_bytes(),
            y.script_pubkey.as_bytes(),
        );
    }
    c.int("lock_time", a.lock_time.into(), b.lock_time.into());

    // Raw payload bytes, against the reference split when available.
    let dc_raw_payload = match &a.special_transaction_payload {
        Some(p) if span.is_none_or(|s| s.payload.is_some()) => Some(dc_bytes(p)),
        _ => None,
    };
    let bs_raw_payload = (!b.extra_payload.is_empty() || span.is_some_and(|s| s.payload.is_some()))
        .then(|| b.extra_payload.clone());
    c.opt_bytes("extra_payload", dc_raw_payload, bs_raw_payload);
    sink.lib_diffs("tx.field", txref, c.finish());

    // Decoded payload fields. Each library's payload is checked on its own first, so a
    // payload neither library interprets is reported against both.
    let Some(span) = span else {
        return PayloadStatus::default();
    };
    let dc_payload = dc_payload(sink, txref, span, a);
    let bs_payload = bs_payload(sink, txref, span, b);
    let status = |ok: bool| {
        if ok {
            PayloadDecode::Ok
        } else {
            PayloadDecode::Failed
        }
    };
    let (Some(dc_payload), Some(bs_payload)) = (dc_payload, &bs_payload) else {
        let carried = span.payload.is_some();
        return PayloadStatus {
            dashcore: status(!carried || dc_payload.is_some()),
            base_sdk: status(!carried || bs_payload.is_some()),
        };
    };
    match payload::compare(dc_payload, bs_payload) {
        Some(diffs) => {
            sink.lib_diffs("payload.field", txref, diffs);
            PayloadStatus::default()
        }
        None => {
            let (e, a) = (
                Some(payload::dc_variant(dc_payload).to_string()),
                Some(payload::bs_variant(bs_payload).to_string()),
            );
            sink.push("payload.variant", Side::Undecided, Some(txref), None, e, a, "");
            PayloadStatus {
                dashcore: PayloadDecode::Variant(payload::dc_variant(dc_payload)),
                base_sdk: PayloadDecode::Variant(payload::bs_variant(bs_payload)),
            }
        }
    }
}

/// dashcore's decoded payload for a transaction that carries one (per the reference split).
/// A payload dashcore keeps as raw bytes (`UnknownPayloadType`: a type it has no decoder for,
/// or a payload its decoder does not consume exactly) is reported against dashcore: the
/// transaction is in a valid chain, so dashd interprets the payload.
fn dc_payload<'a>(
    sink: &mut Sink<'_>,
    txref: TxRef<'_>,
    span: &TxSpan,
    a: &'a DcTx,
) -> Option<&'a DcPayload> {
    span.payload.as_ref()?;
    match &a.special_transaction_payload {
        None | Some(DcPayload::ClassicalWithNonStandardVersionTypeBytesPayloadType(_)) => {
            let detail = "dashcore decoded no payload but Core reads vExtraPayload";
            sink.fail("payload.missing", Side::Dashcore, Some(txref), detail);
            None
        }
        Some(DcPayload::UnknownPayloadType(tx_type, _)) => {
            let detail = format!(
                "special tx type {tx_type} kept as raw bytes: no decoder, or the payload does not \
                 decode exactly"
            );
            sink.fail("payload.unsupported", Side::Dashcore, Some(txref), detail);
            None
        }
        Some(p) => Some(p),
    }
}

/// Core's BLS flag when the oracle prints `getblock 2`: its tip is past V19 on every chain the
/// harness reads, so the basic scheme. Core decodes a payload for RPC under that flag, whatever
/// the block's height. base-sdk decodes payloads without the flag; the harness resolves the
/// coinbase and asset-unlock signatures under it.
pub(crate) const ORACLE_BLS: BlsCtx = BlsCtx::BASIC;

/// base-sdk's decoded payload for a transaction that carries one (per the reference split).
/// Decode failures and types base-sdk cannot interpret are reported against base-sdk: the
/// transaction is in a valid chain, so dashd interprets the payload.
fn bs_payload(
    sink: &mut Sink<'_>,
    txref: TxRef<'_>,
    span: &TxSpan,
    b: &BsTx,
) -> Option<SpecialPayload> {
    span.payload.as_ref()?;
    let Some(decoded) = b.decode_payload() else {
        let detail = "base-sdk carries no payload but Core reads vExtraPayload";
        sink.fail("payload.missing", Side::BaseSdk, Some(txref), detail);
        return None;
    };
    match decoded {
        // Core accepted these bytes, so a payload base-sdk does not read to the end is
        // base-sdk misreading it (Core's GetTxPayload rule).
        Ok(d) if !d.is_complete() => {
            let detail =
                format!("{} payload: trailing bytes: {} remaining", b.tx_type, d.len - d.consumed);
            sink.fail("payload.decode", Side::BaseSdk, Some(txref), detail);
            None
        }
        Ok(dash_primitives::PayloadDecode {
            payload:
                SpecialPayload::Unknown {
                    tx_type,
                    ..
                },
            ..
        }) => {
            let detail = format!("no decoder for special tx type {}", tx_type.to_base());
            sink.fail("payload.unsupported", Side::BaseSdk, Some(txref), detail);
            None
        }
        Ok(d) => Some(d.payload),
        Err(e) => {
            sink.fail("payload.decode", Side::BaseSdk, Some(txref), e.to_string());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rev_hex_roundtrip() {
        let mut h = [0u8; 32];
        h[0] = 0xab;
        h[31] = 0x01;
        let s = rev_hex(&h);
        assert!(s.starts_with("01") && s.ends_with("ab"));
        assert_eq!(parse_rev_hex(&s), Some(h));
    }
}
