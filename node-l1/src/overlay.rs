//! The BS8 overlay messages (O2c, BL-55): base-sdk's reads (`dash-p2p-core`, with the node's BLS
//! rules) against Dash Core's handler reads (`oracle_p2p_decode`), one target per message type.
//! Ported from BS8's differential (`contrib/node/dde-probe/bs8`, `node/bs8-overlay` 4a4b1299),
//! whose separate judge process the in-process oracle replaces (dash-oracle-sys `tests/p2p.rs`
//! checks that it returns the judge's recorded reads). v24 only (DEC-82).
//!
//! The prediction of Core's read: base-sdk keeps BLS fields as wire bytes (DEC-98 Q-12). Core
//! reads some eagerly, under its process-wide scheme (clsig, mnauth, qdata's keys, the mnlistdiff
//! CL-sig map) or the basic scheme (platformban). An eager field Core cannot read throws, and one
//! it reads writes back canonical or as zeros; the prediction applies that with the fork's
//! `BlsEager`, and builds the CL-sig map Core's `std::map` holds: keys as re-serialized, the
//! first entry of a key kept, ordered by the key's double SHA-256. Lazy fields (isdlock, qsigrec,
//! the SML operator key) write back their wire bytes. Final commitments decode eagerly in base-sdk
//! already. A diff's coinbase transaction is written back as the node writes it (CS0's
//! `core_bytes`).
//!
//! Compared: the verdict, the bytes read, qdata's error, vector size and contribution count, and
//! the write-back. A flag byte Core reads as `true` that base-sdk refuses (SML `isValid`, the
//! getqrinfo and qrinfo `extraShare`; BSX A-12, kept strict by DEC-98) is
//! [`Outcome::StrictFlag`], counted apart from agreements.
//!
//! An input is a header and the payload. Byte 0: bits 0-4 select the stream version,
//! `70221 + bits % 22` (every common version a peer can have, `MIN_PEER_PROTO_VERSION` to
//! `PROTOCOL_VERSION`); bit 5 is the BLS scheme flag (legacy); bits 6-7 are unused. qdata adds
//! NetQuorum's limits, the quorum's threshold and its valid member count, each a little-endian
//! `u16` (bytes 1-2 and 3-4). Missing header bytes read as 0.

use std::collections::BTreeMap;

use bitcoin_hashes::sha256d;
use dash_consensus::decode_tx_exact;
// A read, by either side: the bytes read, the write-back and qdata's fields.
use dash_oracle_sys::{
    ErrorKind, Oracle, P2pDecoded as Read, P2pStream, P2pVerdict, Pin, QDataRead,
};
use dash_p2p_core::{
    CommandString, MnListDiff, MnListFormat, P2pMsg, ProtoCtx, ProtocolVersion, QData, QrInfo,
    QuorumClSig, QuorumDataError, QuorumDataRequest, QuorumSnapshot, SimplifiedMnListEntry,
};
use dash_pkc::bls::{
    BlsCtx, BlsEagerPk, BlsEagerSig, BlsIesBlobBytes, BlsPkBytes, BlsScIetf, BlsSchemeKind,
    BlsSchemeSource, BlsSigBytes,
};
use dash_primitives::Transaction;
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

use crate::{Compared, Outcome, divergence};

/// The lowest stream version: `MIN_PEER_PROTO_VERSION`.
pub const MIN_VERSION: i32 = 70221;
/// The number of stream versions an input selects from, up to `PROTOCOL_VERSION` (70242).
pub const VERSIONS: i32 = 22;

/// One input: a command's payload on a stream, with qdata's limits.
#[derive(Clone, PartialEq, Eq)]
pub struct Case {
    pub cmd: &'static str,
    pub version: i32,
    pub legacy: bool,
    /// qdata: the quorum's threshold and valid member count.
    pub max: [u16; 2],
    pub raw: Vec<u8>,
}

impl Case {
    /// The case an input of `cmd`'s target encodes (see the module docs).
    pub fn from_input(cmd: &'static str, input: &[u8]) -> Self {
        let byte = |i: usize| input.get(i).copied().unwrap_or(0);
        let le16 = |i: usize| u16::from_le_bytes([byte(i), byte(i + 1)]);
        let (header, max) = if cmd == "qdata" {
            (5, [le16(1), le16(3)])
        } else {
            (1, [0, 0])
        };
        Self {
            cmd,
            version: MIN_VERSION + i32::from(byte(0) & 0x1f) % VERSIONS,
            legacy: byte(0) & 0x20 != 0,
            max,
            raw: input.get(header..).unwrap_or_default().to_vec(),
        }
    }

    /// The input that encodes this case, or `None` for a version or limits it cannot encode.
    pub fn to_input(&self) -> Option<Vec<u8>> {
        let index =
            u8::try_from(self.version - MIN_VERSION).ok().filter(|&i| i32::from(i) < VERSIONS)?;
        let mut input = vec![index | (u8::from(self.legacy) << 5)];
        if self.cmd == "qdata" {
            input.extend(self.max.iter().flat_map(|m| m.to_le_bytes()));
        } else if self.max != [0, 0] {
            return None;
        }
        input.extend(&self.raw);
        Some(input)
    }

    fn stream(&self) -> P2pStream {
        P2pStream {
            version: self.version,
            bls_legacy: self.legacy,
            qdata_max_vvec: u32::from(self.max[0]),
            qdata_max_contrib: u32::from(self.max[1]),
        }
    }
}

/// A read with no qdata fields.
fn plain(consumed: usize, reencoded: Vec<u8>) -> Read {
    Read {
        consumed,
        reencoded,
        qdata: None,
    }
}

fn wire<T: BaseCodec>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

fn malleable<E>(_: E) -> String {
    "malleable BLS object".into()
}

/// An eager signature as Core reads and writes it under `scheme`.
fn sig(
    bytes: &BlsSigBytes<BlsScIetf>,
    scheme: BlsSchemeKind,
) -> Result<BlsSigBytes<BlsScIetf>, String> {
    let eager = BlsEagerSig::decode(bytes.as_bytes(), BlsSchemeSource::Version(scheme))
        .map_err(malleable)?;
    Ok(BlsSigBytes::from_bytes(eager.to_bytes(scheme)))
}

/// An eager public key as Core reads and writes it under `scheme`.
fn pk(
    bytes: &BlsPkBytes<BlsScIetf>,
    scheme: BlsSchemeKind,
) -> Result<BlsPkBytes<BlsScIetf>, String> {
    let eager = BlsEagerPk::decode(bytes.as_bytes(), BlsSchemeSource::Version(scheme))
        .map_err(malleable)?;
    Ok(BlsPkBytes::from_bytes(eager.to_bytes(scheme)))
}

/// Core's `std::map<CBLSSignature, std::set<uint16_t>>` of the wire entries.
fn cl_sig_map(
    wire_order: &[QuorumClSig],
    scheme: BlsSchemeKind,
) -> Result<Vec<QuorumClSig>, String> {
    let mut map = BTreeMap::new();
    for entry in wire_order {
        let key = sig(&entry.sig, scheme)?;
        let hash = sha256d::Hash::hash(key.as_bytes()).to_byte_array();
        map.entry(hash).or_insert_with(|| QuorumClSig {
            sig: key,
            index_set: entry.index_set.clone(),
        });
    }
    Ok(map.into_values().collect())
}

/// The coinbase transaction as the node writes it back: CS0's `core_bytes`, Core's
/// serialization of the transaction it read.
fn node_tx(tx: &Transaction) -> Result<Vec<u8>, String> {
    let bytes = wire(tx);
    let d = decode_tx_exact(&bytes).map_err(|_| "node tx".to_owned())?;
    Ok(d.core_bytes().as_slice().to_vec())
}

fn count(n: usize, out: &mut Vec<u8>) {
    dash_types::CompactSize::from(n).encode(out);
}

/// A diff as Core writes it back, in its own layout.
fn diff(d: &MnListDiff, scheme: BlsSchemeKind, out: &mut Vec<u8>) -> Result<(), String> {
    let f = d.format;
    if f >= MnListFormat::VersionFirst {
        d.version.encode(out);
    }
    d.base_block_hash.encode(out);
    d.block_hash.encode(out);
    d.cb_tx_merkle_tree.encode(out);
    out.extend(node_tx(&d.cb_tx)?);
    if matches!(f, MnListFormat::VersionAfterCbTx | MnListFormat::VersionedEntries) {
        d.version.encode(out);
    }
    d.deleted_mns.encode(out);
    count(d.mn_list.len(), out);
    for e in &d.mn_list {
        e.encode_as(out, f.has_entry_version());
    }
    d.deleted_quorums.encode(out);
    d.new_quorums.encode(out);
    if f >= MnListFormat::ClSigs {
        cl_sig_map(&d.quorum_cl_sigs, scheme)?.encode(out);
    }
    Ok(())
}

/// Rotation info as Core writes it back.
fn qrinfo(q: &QrInfo, scheme: BlsSchemeKind) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for s in [&q.snapshot_h_c, &q.snapshot_h_2c, &q.snapshot_h_3c] {
        s.encode(&mut out);
    }
    for d in [&q.diff_tip, &q.diff_h, &q.diff_h_c, &q.diff_h_2c, &q.diff_h_3c] {
        diff(d, scheme, &mut out)?;
    }
    out.push(u8::from(q.cycle_h_4c.is_some()));
    if let Some(c) = &q.cycle_h_4c {
        c.snapshot.encode(&mut out);
        diff(&c.diff, scheme, &mut out)?;
    }
    q.last_commitment_per_index.encode(&mut out);
    q.quorum_snapshot_list.encode(&mut out);
    count(q.mn_list_diff_list.len(), &mut out);
    for d in &q.mn_list_diff_list {
        diff(d, scheme, &mut out)?;
    }
    Ok(out)
}

/// qdata as the oracle models NetQuorum's read with the case's limits: the codec reads only
/// (dash-oracle-sys [`QDataRead`]). The counts are compared, but not the handler's exact-count
/// checks, its stop after a refused vector or its state gates, so an agreement is no peer-message
/// acceptance.
fn qdata(q: &QData, consumed_head: usize, c: &Case, scheme: BlsSchemeKind) -> Result<Read, String> {
    let mut read = plain(consumed_head, wire(&q.request));
    let mut fields = QDataRead {
        error: q.error.to_base(),
        ..QDataRead::default()
    };
    if q.error != QuorumDataError::Undefined {
        read.reencoded.push(fields.error);
    }
    if q.error != QuorumDataError::None {
        read.qdata = Some(fields);
        return Ok(read);
    }
    let data =
        q.read_data(usize::from(c.max[0]), usize::from(c.max[1])).map_err(|e| e.to_string())?;
    let mut body = Vec::new();
    if let Some(vvec) = &data.vvec {
        let keys = vvec.iter().map(|k| pk(k, scheme)).collect::<Result<Vec<_>, _>>()?;
        fields.vvec_size = Some(keys.len());
        keys.encode(&mut body);
    }
    if let Some(contributions) = &data.contributions {
        let blobs = contributions
            .iter()
            .map(|b| {
                Ok(BlsIesBlobBytes::new(
                    pk(b.ephemeral_pk(), scheme)?,
                    *b.iv_seed(),
                    b.data().to_vec(),
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        fields.contrib_count = Some(blobs.len());
        blobs.encode(&mut body);
    }
    // Every field has one width, so the bytes read are the bytes written.
    read.consumed += body.len();
    read.reencoded.extend(body);
    read.qdata = Some(fields);
    Ok(read)
}

/// base-sdk's read of `c`, with the node's BLS rules, or why it failed.
fn ours(c: &Case) -> Result<Read, String> {
    let global = BlsSchemeKind::from_legacy(c.legacy);
    match c.cmd {
        "smlentry" => {
            let mut data = &c.raw[..];
            let versioned = c.version >= ProtocolVersion::SMNLE_VERSIONED.to_base();
            let e = SimplifiedMnListEntry::decode_as(&mut data, versioned)
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            e.encode_as(&mut out, versioned);
            return Ok(plain(c.raw.len() - data.len(), out));
        }
        "qsnapshot" => {
            let mut data = &c.raw[..];
            let s = QuorumSnapshot::decode(&mut data).map_err(|e| e.to_string())?;
            return Ok(plain(c.raw.len() - data.len(), wire(&s)));
        }
        _ => {}
    }
    let ctx = ProtoCtx::new(ProtocolVersion::from_base(c.version), BlsCtx::from_legacy(c.legacy));
    let cmd = CommandString::from_static(c.cmd);
    let d = P2pMsg::decode_with_policy(&cmd, &c.raw, &ctx).map_err(|e| e.to_string())?;
    let mut msg = d.msg;
    match &mut msg {
        P2pMsg::ClSig(m) => m.sig = sig(&m.sig, global)?,
        P2pMsg::MnAuth(m) => m.sig = sig(&m.sig, global)?,
        P2pMsg::PlatformBan(m) => m.sig = sig(&m.sig, BlsSchemeKind::Basic)?,
        P2pMsg::MnListDiff(m) => {
            let mut out = Vec::new();
            diff(m, global, &mut out)?;
            return Ok(plain(d.consumed, out));
        }
        P2pMsg::QrInfo(m) => return Ok(plain(d.consumed, qrinfo(m, global)?)),
        P2pMsg::QData(q) => {
            // Core reads the error byte when there is one, 0xff included.
            let head = c.raw.len().min(QuorumDataRequest::SIZE + 1);
            return qdata(q, head, c, global);
        }
        _ => {}
    }
    let mut reenc = Vec::new();
    msg.encode_payload(&mut reenc);
    Ok(plain(d.consumed, reenc))
}

/// Core's read of `c`: `Err(true)` when Core refuses the message (its deserializer refuses the
/// bytes or a handler check refuses it), `Err(false)` for any other oracle failure, which is
/// no verdict (an allocation failure among them, BL-61).
fn core(c: &Case, pin: Pin) -> Result<Read, bool> {
    match Oracle::new(pin).p2p_decode(c.cmd, &c.raw, &c.stream()) {
        Ok(P2pVerdict::Read(r)) => Ok(r),
        Ok(P2pVerdict::Refused(_)) => Err(true),
        Err(e) => Err(e.kind == ErrorKind::Decode),
    }
}

/// Whether two reads agree, or the field that differs.
fn same(ours: &Read, core: &Read) -> Compared {
    if ours.consumed != core.consumed {
        Err(divergence("consumed"))
    } else if ours.qdata != core.qdata {
        Err(divergence("qdata fields"))
    } else if ours.reencoded != core.reencoded {
        Err(divergence("write-back"))
    } else {
        Ok(Outcome::BothAccept)
    }
}

/// Whether base-sdk refused only flag bytes Core read as `true` (BSX A-12, kept strict by
/// DEC-98): with each refused flag byte set to 1, base-sdk reads what Core read.
fn strict_flags(c: &Case, core: &Read) -> bool {
    let mut fixed = c.clone();
    loop {
        match ours(&fixed) {
            Ok(read) => return same(&read, core).is_ok(),
            Err(e) if refused_flag(&e) => match refused_flag_at(&fixed) {
                Some(at) if e.ends_with(&format!("got {}", fixed.raw[at])) => fixed.raw[at] = 1,
                _ => return false,
            },
            Err(_) => return false,
        }
    }
}

/// The commands whose flags are declared strict: SML `isValid` (in an entry, a diff or rotation
/// info) and the getqrinfo and qrinfo `extraShare`. A refused flag elsewhere is a divergence.
const STRICT_FLAG_COMMANDS: [&str; 4] = ["smlentry", "mnlistdiff", "qrinfo", "getqrinfo"];

/// base-sdk's refusal of a flag byte other than 0 or 1 (`DecodeError::NonCanonicalBool`).
fn refused_flag(e: &str) -> bool {
    e.contains("non-canonical bool: got ")
}

/// The flag byte base-sdk refuses: the last byte of the shortest prefix that, zero-filled to the
/// input's length, it refuses for a flag. A zero is a valid flag and the reads are sequential, so
/// a prefix that ends before the flag reads past it.
fn refused_flag_at(c: &Case) -> Option<usize> {
    let (mut lo, mut hi) = (0, c.raw.len());
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        let mut raw = c.raw[..mid].to_vec();
        raw.resize(c.raw.len(), 0);
        let prefix = Case {
            raw,
            ..*c
        };
        match ours(&prefix) {
            Err(e) if refused_flag(&e) => hi = mid,
            _ => lo = mid,
        }
    }
    hi.checked_sub(1)
}

/// Compares base-sdk's read of `c` with Core's on `pin`.
pub fn compare_case(c: &Case, pin: Pin) -> Compared {
    match (ours(c), core(c, pin)) {
        // Verdicts only: no Core logic observes the error class (DEC-98).
        (Err(_), Err(true)) => Ok(Outcome::BothReject),
        (_, Err(false)) => Err(divergence("oracle error")),
        (Ok(o), Ok(k)) => same(&o, &k),
        (Err(e), Ok(k))
            if STRICT_FLAG_COMMANDS.contains(&c.cmd) && refused_flag(&e) && strict_flags(c, &k) =>
        {
            Ok(Outcome::StrictFlag)
        }
        (Ok(_), Err(true)) | (Err(_), Ok(_)) => Err(divergence("decode verdict")),
    }
}

/// Defines each command's target comparison, `compare::<command>` (target `diff_p2p_<command>`).
macro_rules! compare_fns {
    ($($cmd:ident),* $(,)?) => {
        /// The targets' comparisons, by command.
        pub mod compare {
            use super::{Case, Compared, Pin, compare_case};

            $(
                #[doc = concat!("`diff_p2p_", stringify!($cmd), "`: one `", stringify!($cmd),
                                "` input (see the module docs).")]
                pub fn $cmd(input: &[u8], pin: Pin) -> Compared {
                    compare_case(&Case::from_input(stringify!($cmd), input), pin)
                }
            )*
        }
    };
}

compare_fns!(
    clsig,
    isdlock,
    qsigrec,
    qfcommit,
    mnauth,
    spork,
    platformban,
    qgetdata,
    qdata,
    getqrinfo,
    qrinfo,
    mnlistdiff,
    smlentry,
    qsnapshot,
);
