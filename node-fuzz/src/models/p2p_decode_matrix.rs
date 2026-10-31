//! The pinned acceptance matrix of the P2P payloads `dash-p2p` decodes and no Core oracle judges
//! (DEC-178): one row per command, listing what Core's handler reads at its read site, field by
//! field, as Core's serializers read it at v24.0.0-rc.3. [`judge`] walks a payload the way Core's
//! `DataStream` does and returns the bytes read or why the read stopped. It shares no code with
//! base-sdk's decoders. An embedded consensus type (a transaction, a block) is a
//! [`Field::Judged`] field, delegated to the decoder the L1 targets judge against Core (BS4a's
//! Core-judged decode corpora) and named. For `headers2`, [`headers2_values`] also gives the
//! headers Core's `Uncompress` builds, since the bytes read do not show them.
//!
//! Out of scope: the commands Core's oracle judges (`clsig`, `isdlock`, `qsigrec`, `qfcommit`,
//! `mnauth`, `spork`, `platformban`, `qgetdata`, `qdata`, `getqrinfo`, `qrinfo`, `mnlistdiff`),
//! the rows that read nothing (`ReadPlan::Nothing`, `getsporks` among them), and the rows
//! base-sdk keeps raw (the CoinJoin, DKG and signing-share messages).
//!
//! Only the read is judged: the connection-state checks before it (`policy::PreRead`) and
//! `version`'s outbound services check (`net_processing.cpp:3963`) are the processor's, and are
//! taken to pass. Two handlers stop without a throw after a read the payload decides
//! ([`CoreThrow::HandlerReturn`]): `headers`/`headers2` over `GetHeadersLimit`, and `version`
//! below `MIN_PEER_PROTO_VERSION`, whose tail Core then never reads.

use dash_consensus::hash::x11;
use dash_consensus::{decode_block, decode_tx};
use dash_p2p_core::{CommandString, DecodedMsg, P2pDecodeError};

/// `MAX_SIZE` (`serialize.h:42`).
pub(super) const MAX_SIZE: u64 = 0x0200_0000;
/// `MAX_BLOOM_FILTER_SIZE` (`common/bloom.h:20`).
const MAX_BLOOM_FILTER_SIZE: u64 = 36000;
/// `ADDRV2_FORMAT`, the stream-version flag of the BIP155 address encoding (`netaddress.h:30`).
const ADDRV2_FORMAT: i32 = 0x2000_0000;
/// `MIN_PEER_PROTO_VERSION` (`version.h:20`).
const MIN_PEER_PROTO_VERSION: i32 = 70221;
/// `INCREASE_MAX_HEADERS2_VERSION` (`version.h:50`).
const INCREASE_MAX_HEADERS2_VERSION: i32 = 70235;
/// `"Vector length limit exceeded"`, `LIMITED_VECTOR`'s throw (`serialize.h:1030`).
const VECTOR_LIMIT: &str = "Vector length limit exceeded";
/// `"String length limit exceeded"`, `LIMITED_STRING`'s throw (`serialize.h:781`).
const STRING_LIMIT: &str = "String length limit exceeded";

/// Why Core's read of a payload stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CoreThrow {
    /// `DataStream::read(): end of data` (`streams.h:287`).
    EndOfData,
    /// `non-canonical ReadCompactSize()` (`serialize.h:304-316`).
    NonCanonical,
    /// `ReadCompactSize(): size too large`, a count above `MAX_SIZE` (`serialize.h:318-319`).
    SizeTooLarge,
    /// Any other `std::ios_base::failure`, with Core's text (`DataStream::ignore(): end of data`,
    /// the `LIMITED_*` texts, BIP155 address sizes, the compact-block index throws, …).
    Other(String),
    /// A [`Field::Judged`] field's decoder refused it; that judge owns the verdict's details.
    Judged(&'static str),
    /// Not a throw: the handler checked what it had read and returned (the reason).
    HandlerReturn(&'static str),
}

impl CoreThrow {
    /// Core's `e.what()` before libstdc++'s `": iostream error"`, for a throw whose text Core
    /// fixes here (not a judged field's, nor a handler return).
    #[cfg(test)]
    pub(super) fn what(&self) -> Option<&str> {
        match self {
            Self::EndOfData => Some("DataStream::read(): end of data"),
            Self::NonCanonical => Some("non-canonical ReadCompactSize()"),
            Self::SizeTooLarge => Some("ReadCompactSize(): size too large"),
            Self::Other(text) => Some(text),
            Self::Judged(_) | Self::HandlerReturn(_) => None,
        }
    }
}

/// One field of a read.
enum Field {
    /// `n` bytes Core takes whatever they hold: integers, enums, `uint256`, byte arrays.
    Fixed(usize),
    /// `DataStream::ignore(n)`, which throws its own end-of-data text (`streams.h:298-303`).
    Ignore(usize),
    /// A C++ `bool`: any byte, non-zero is `true` (`serialize.h:239`).
    Bool,
    /// A range-checked CompactSize Core reads and drops.
    Size,
    /// A vector: a range-checked CompactSize count, then the items (`serialize.h:850`, `:1135`).
    Vec(&'static [F]),
    /// `LIMITED_VECTOR`/`LIMITED_STRING`: as [`Field::Vec`], and a count above the limit throws
    /// the text before any item (`serialize.h:779-781`, `:1003-1030`).
    Limited(u64, &'static str, &'static [F]),
    /// A count the handler compares with a limit for the stream version before reading the items;
    /// above it the handler returns.
    Bounded(fn(i32) -> u64, &'static [F]),
    /// Read only while at least `n` bytes remain (`version`'s `!vRecv.empty()`, `pong`).
    IfAvail(usize, &'static [F]),
    /// An embedded consensus type whose read domain another gate judges against Core.
    Judged(Judge),
    /// A read whose layout depends on what was read or on the stream version.
    Custom(Custom),
}

/// A field and its Core source: the field's name, C++ type and the `Unserialize` that reads it.
struct F {
    field: Field,
    core: &'static str,
}

const fn f(field: Field, core: &'static str) -> F {
    F {
        field,
        core,
    }
}

/// A type judged elsewhere: its judge's name, its read (true when it decodes) and a minimal valid
/// encoding.
struct Judge {
    name: &'static str,
    read: fn(&mut &[u8]) -> bool,
    sample: fn(&mut Vec<u8>),
}

/// A custom read and its minimal valid encoding.
struct Custom {
    read: fn(&mut Reader<'_>) -> Result<(), CoreThrow>,
    sample: fn(&mut Sample),
}

/// A command's row: what its handler reads.
struct Row {
    cmd: &'static str,
    /// The handler's branch and its reads in `net_processing.cpp` (or the file named).
    #[expect(dead_code, reason = "the row's source citation, for review")]
    read_site: &'static str,
    fields: &'static [F],
}

const fn r(cmd: &'static str, read_site: &'static str, fields: &'static [F]) -> Row {
    Row {
        cmd,
        read_site,
        fields,
    }
}

/// Core's stream over a payload, with the stream version (`GetCommonVersion`, plus
/// `ADDRV2_FORMAT` for `addrv2`).
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    version: i32,
    /// The vector counts read so far, in order.
    counts: Vec<u64>,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], version: i32) -> Self {
        Self {
            data,
            pos: 0,
            version,
            counts: Vec::new(),
        }
    }

    /// `DataStream::read` (`streams.h:280-295`).
    fn read(&mut self, n: usize) -> Result<&'a [u8], CoreThrow> {
        let bytes = self.data[self.pos..].get(..n).ok_or(CoreThrow::EndOfData)?;
        self.pos += n;
        Ok(bytes)
    }

    /// `DataStream::ignore` (`streams.h:298-303`).
    fn ignore(&mut self, n: u64) -> Result<(), CoreThrow> {
        if n > (self.data.len() - self.pos) as u64 {
            return Err(CoreThrow::Other("DataStream::ignore(): end of data".into()));
        }
        self.pos += n as usize;
        Ok(())
    }

    /// A little-endian unsigned integer of `n` bytes (`ser_readdata*`, `CustomUintFormatter`).
    fn uint(&mut self, n: usize) -> Result<u64, CoreThrow> {
        let mut buf = [0u8; 8];
        buf[..n].copy_from_slice(self.read(n)?);
        Ok(u64::from_le_bytes(buf))
    }

    /// `ReadCompactSize(range_check)` (`serialize.h:292-321`).
    fn compact_size(&mut self, range_check: bool) -> Result<u64, CoreThrow> {
        let (n, min) = match self.uint(1)? {
            n @ 0..=252 => return Ok(n),
            253 => (self.uint(2)?, 253),
            254 => (self.uint(4)?, 0x1_0000),
            _ => (self.uint(8)?, 0x1_0000_0000),
        };
        if n < min {
            return Err(CoreThrow::NonCanonical);
        }
        if range_check && n > MAX_SIZE {
            return Err(CoreThrow::SizeTooLarge);
        }
        Ok(n)
    }

    /// A vector's count, recorded.
    fn count(&mut self) -> Result<u64, CoreThrow> {
        let n = self.compact_size(true)?;
        self.counts.push(n);
        Ok(n)
    }

    /// Whether the stream reads addresses in the BIP155 encoding.
    fn addrv2(&self) -> bool {
        self.version & ADDRV2_FORMAT != 0
    }

    fn items(&mut self, n: u64, items: &[F]) -> Result<(), CoreThrow> {
        // Every item reads at least a byte, so a hostile count ends at the end of the data.
        for _ in 0..n {
            self.fields(items)?;
        }
        Ok(())
    }

    fn fields(&mut self, fields: &[F]) -> Result<(), CoreThrow> {
        for field in fields {
            match &field.field {
                Field::Fixed(n) => drop(self.read(*n)?),
                Field::Ignore(n) => self.ignore(*n as u64)?,
                Field::Bool => drop(self.read(1)?),
                Field::Size => drop(self.compact_size(true)?),
                Field::Vec(items) => {
                    let n = self.count()?;
                    self.items(n, items)?;
                }
                Field::Limited(limit, text, items) => {
                    let n = self.count()?;
                    if n > *limit {
                        return Err(CoreThrow::Other((*text).into()));
                    }
                    self.items(n, items)?;
                }
                Field::Bounded(limit, items) => {
                    let n = self.count()?;
                    if n > limit(self.version) {
                        return Err(CoreThrow::HandlerReturn(field.core));
                    }
                    self.items(n, items)?;
                }
                Field::IfAvail(n, inner) => {
                    if self.data.len() - self.pos >= *n {
                        self.fields(inner)?;
                    }
                }
                Field::Judged(judge) => {
                    let mut rest = &self.data[self.pos..];
                    if !(judge.read)(&mut rest) {
                        return Err(CoreThrow::Judged(judge.name));
                    }
                    self.pos = self.data.len() - rest.len();
                }
                Field::Custom(custom) => (custom.read)(self)?,
            }
        }
        Ok(())
    }
}

/// A sample payload being built.
struct Sample {
    out: Vec<u8>,
    version: i32,
    bool_byte: u8,
    /// Where the first range-checked count outside a judged field starts.
    #[cfg_attr(not(test), allow(dead_code))]
    first_count: Option<usize>,
}

impl Sample {
    /// Marks a count starting here.
    fn count(&mut self) {
        self.first_count.get_or_insert(self.out.len());
    }
}

/// Appends the minimal encoding of `fields`: zeros, `bool_byte` for every bool, one item in every
/// vector, every optional tail present.
fn sample_fields(s: &mut Sample, fields: &[F]) {
    for field in fields {
        match &field.field {
            Field::Fixed(n) | Field::Ignore(n) => s.out.resize(s.out.len() + n, 0),
            Field::Bool => s.out.push(s.bool_byte),
            Field::Size => s.out.push(0),
            Field::Vec(items) | Field::Limited(_, _, items) | Field::Bounded(_, items) => {
                s.count();
                s.out.push(1);
                sample_fields(s, items);
            }
            Field::IfAvail(_, inner) => sample_fields(s, inner),
            Field::Judged(judge) => (judge.sample)(&mut s.out),
            Field::Custom(custom) => (custom.sample)(s),
        }
    }
}

// ---- Judged types --------------------------------------------------------------------------

/// `CTransaction` (`primitives/transaction.h:354`, `SERIALIZE_METHODS(CMutableTransaction)`).
const TX: Judge = Judge {
    name: "CTransaction: dash_consensus::decode_tx, judged by L1 diff_tx_decode (BS4a)",
    read: |data| decode_tx(data).is_ok(),
    sample: |out| out.extend([1, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
};

/// `CBlock` (`primitives/block.h:213`): the header and `std::vector<CTransactionRef>`.
const BLOCK: Judge = Judge {
    name: "CBlock: dash_consensus::decode_block, judged by L1 diff_block_decode (BS4a)",
    read: |data| decode_block(data).is_ok(),
    sample: |out| {
        out.resize(out.len() + 80, 0);
        out.push(1);
        (TX.sample)(out);
    },
};

// ---- Custom reads --------------------------------------------------------------------------

fn no_sample(_: &mut Sample) {}

/// `version`'s `nVersion`; the sample carries the caller's version, so the tail is read.
const PEER_VERSION: Custom = Custom {
    read: |r| r.read(4).map(drop),
    sample: |s| s.out.extend(s.version.to_le_bytes()),
};

/// `version` below `MIN_PEER_PROTO_VERSION`: disconnect, the tail unread
/// (`net_processing.cpp:3970-3975`).
const OBSOLETE_VERSION: Custom = Custom {
    read: |r| {
        let n = i32::from_le_bytes([r.data[0], r.data[1], r.data[2], r.data[3]]);
        if n < MIN_PEER_PROTO_VERSION {
            return Err(CoreThrow::HandlerReturn("nVersion < MIN_PEER_PROTO_VERSION: disconnect"));
        }
        Ok(())
    },
    sample: no_sample,
};

/// `addrv2`: the handler adds `ADDRV2_FORMAT` to the stream version
/// (`net_processing.cpp:4389-4396`).
const ADDRV2_STREAM: Custom = Custom {
    read: |r| {
        r.version |= ADDRV2_FORMAT;
        Ok(())
    },
    sample: |s| s.version |= ADDRV2_FORMAT,
};

/// `CAddress::nServices`: `CompactSizeFormatter<false>` (no range check) in V2, else 8 bytes
/// (`protocol.h:466-472`).
const SERVICES: Custom = Custom {
    read: |r| {
        if r.addrv2() {
            r.compact_size(false).map(drop)
        } else {
            r.read(8).map(drop)
        }
    },
    sample: |s| {
        s.out.resize(
            s.out.len()
                + if s.version & ADDRV2_FORMAT != 0 {
                    1
                } else {
                    8
                },
            0,
        )
    },
};

/// `CNetAddr::Unserialize` (`netaddress.h:256-262`): 16 bytes in V1 (`UnserializeV1Stream`,
/// `:416-422`); in V2 (`UnserializeV2Stream`, `:429-480`) the BIP155 network id, a
/// `COMPACTSIZE` length (range-checked) at most `MAX_ADDRV2_SIZE` (512), a known network's exact
/// size (`SetNetFromBIP155Network`, `netaddress.cpp:44-97`) or an unknown id's bytes skipped.
fn net_addr(r: &mut Reader<'_>) -> Result<(), CoreThrow> {
    if !r.addrv2() {
        return r.read(16).map(drop);
    }
    let net = r.uint(1)?;
    let size = r.compact_size(true)?;
    if size > 512 {
        return Err(CoreThrow::Other(format!("Address too long: {size} > 512")));
    }
    let known = match net {
        1 => Some(("IPv4", 4)),
        2 => Some(("IPv6", 16)),
        4 => Some(("TORv3", 32)),
        5 => Some(("I2P", 32)),
        6 => Some(("CJDNS", 16)),
        _ => None,
    };
    match known {
        Some((name, want)) if size != want => Err(CoreThrow::Other(format!(
            "BIP155 {name} address with length {size} (should be {want})"
        ))),
        Some(_) => r.read(size as usize).map(drop),
        None => r.ignore(size),
    }
}

const NET_ADDR: Custom = Custom {
    read: net_addr,
    sample: |s| {
        if s.version & ADDRV2_FORMAT != 0 {
            s.out.extend([1, 4, 0, 0, 0, 0]);
        } else {
            s.out.resize(s.out.len() + 16, 0);
        }
    },
};

/// `CompressedHeaderBitField`'s version offset bits and flags (`primitives/block.h:75-82`); a
/// flag set leaves its field on the wire (`IsCompressed`, `:84-87`).
const H2_VERSION_OFFSET: u8 = 0b111;
const H2_PREV_BLOCK_HASH: u8 = 0b1000;
const H2_TIMESTAMP: u8 = 0b1_0000;
const H2_NBITS: u8 = 0b10_0000;
/// `max_backwards_look_ups`, the `last_unique_versions` bound (`primitives/block.cpp:50`).
const MAX_BACKWARDS_LOOK_UPS: usize = 7;

/// `CompressibleBlockHeader` (`primitives/block.h:166-185`): the bit field, then each field its
/// bits leave on the wire, as `CBlockHeader`'s six fields' bytes (empty when left off; `nTime`'s
/// are `int16_t time_offset`'s when it is compressed). `Uncompress` (`primitives/block.cpp:95-134`)
/// never throws: a first header's compressed fields and a version offset past the list stay zero.
fn compressed_header<'a>(r: &mut Reader<'a>) -> Result<(u8, [&'a [u8]; 6]), CoreThrow> {
    /// `yes` bytes when `on`, else `no`.
    fn n(on: bool, yes: usize, no: usize) -> usize {
        if on {
            yes
        } else {
            no
        }
    }
    let bits = r.read(1)?[0];
    let wire = |flag: u8| bits & flag != 0;
    let lengths = [
        n(bits & H2_VERSION_OFFSET == 0, 4, 0), // nVersion: version offset 0
        n(wire(H2_PREV_BLOCK_HASH), 32, 0),     // hashPrevBlock
        32,                                     // hashMerkleRoot
        n(wire(H2_TIMESTAMP), 4, 2),            // nTime, else int16_t time_offset
        n(wire(H2_NBITS), 4, 0),                // nBits
        4,                                      // nNonce
    ];
    let mut fields = [&[][..]; 6];
    for (field, len) in fields.iter_mut().zip(lengths) {
        *field = r.read(len)?;
    }
    Ok((bits, fields))
}

/// A `CBlockHeader` (`primitives/block.h:28-33`) as `Uncompress` leaves it; a field neither read
/// nor restored keeps `SetNull`'s zero (`:42-50`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Header {
    version: i32,
    prev_block: [u8; 32],
    merkle_root: [u8; 32],
    time: u32,
    bits: u32,
    nonce: u32,
}

impl Header {
    /// `CBlockHeader`'s serialization (`primitives/block.h:40`).
    pub(super) fn bytes(&self) -> [u8; 80] {
        let mut out = [0u8; 80];
        out[..4].copy_from_slice(&self.version.to_le_bytes());
        out[4..36].copy_from_slice(&self.prev_block);
        out[36..68].copy_from_slice(&self.merkle_root);
        out[68..72].copy_from_slice(&self.time.to_le_bytes());
        out[72..76].copy_from_slice(&self.bits.to_le_bytes());
        out[76..].copy_from_slice(&self.nonce.to_le_bytes());
        out
    }

    /// `GetHash`: X11 of the serialization (`primitives/block.cpp:13-19`).
    fn hash(&self) -> [u8; 32] {
        x11(&self.bytes())
    }
}

/// `SaveVersionAsMostRecent` (`primitives/block.cpp:45-55`): to the front, evicting the oldest
/// past seven.
fn save_version(versions: &mut Vec<i32>, version: i32) {
    versions.insert(0, version);
    if versions.len() > MAX_BACKWARDS_LOOK_UPS {
        versions.pop();
    }
}

/// `CompressibleBlockHeader::Uncompress` (`primitives/block.cpp:95-134`) of a header
/// [`compressed_header`] read, against the header before it (`previous`, uncompressed) and
/// `last_unique_versions` (`versions`, front first), with `SaveVersionAsMostRecent` and
/// `MarkVersionAsMostRecent` (`:37-55`).
fn uncompress(
    bits: u8,
    fields: [&[u8]; 6],
    previous: Option<&Header>,
    versions: &mut Vec<i32>,
) -> Header {
    let word = |b: &[u8]| b.try_into().map_or(0, u32::from_le_bytes);
    let hash = |b: &[u8]| b.try_into().unwrap_or([0; 32]);
    let mut h = Header {
        version: fields[0].try_into().map_or(0, i32::from_le_bytes),
        prev_block: hash(fields[1]),
        merkle_root: hash(fields[2]),
        time: if bits & H2_TIMESTAMP != 0 {
            word(fields[3])
        } else {
            0
        },
        bits: word(fields[4]),
        nonce: word(fields[5]),
    };
    let Some(last) = previous else {
        // :97-101: the first header is taken as read; its version, 0 when left off, is saved.
        save_version(versions, h.version);
        return h;
    };
    match usize::from(bits & H2_VERSION_OFFSET) {
        // :115-118
        0 => save_version(versions, h.version),
        // :107-114: the offset-th version, spliced to the front (`MarkVersionAsMostRecent`,
        // :37-43); an offset past the list leaves `nVersion` 0 and the list as it is.
        offset if offset <= versions.len() => {
            h.version = versions.remove(offset - 1);
            versions.insert(0, h.version);
        }
        _ => {}
    }
    // :120-123
    if bits & H2_PREV_BLOCK_HASH == 0 {
        h.prev_block = last.hash();
    }
    // :125-128: `uint32_t + int16_t` converts to `uint32_t`, modulo 2^32.
    if bits & H2_TIMESTAMP == 0 {
        let offset = fields[3].try_into().map_or(0, i16::from_le_bytes);
        h.time = last.time.wrapping_add_signed(i32::from(offset));
    }
    // :130-133
    if bits & H2_NBITS == 0 {
        h.bits = last.bits;
    }
    h
}

const COMPRESSED_HEADER: Custom = Custom {
    read: |r| compressed_header(r).map(drop),
    sample: |s| {
        s.out.push(0b0011_1000);
        s.out.resize(s.out.len() + 80, 0);
    },
};

/// `PrefilledTransaction::index`: `COMPACTSIZE` into a `uint16_t`, range-checked, then the
/// type's bound (`blockencodings.h:79`; `serialize.h:718-723`).
const PREFILLED_INDEX: Custom = Custom {
    read: |r| {
        if r.compact_size(true)? > u64::from(u16::MAX) {
            return Err(CoreThrow::Other("CompactSize exceeds limit of type".into()));
        }
        Ok(())
    },
    sample: |s| s.out.push(0),
};

/// `CBlockHeaderAndShortTxIDs` after the read: more than 65535 transactions throw
/// (`blockencodings.h:118-122`).
const TX_COUNT: Custom = Custom {
    read: |r| {
        let [.., short_ids, prefilled] = r.counts[..] else {
            unreachable!("cmpctblock reads two vectors first")
        };
        if short_ids + prefilled > u64::from(u16::MAX) {
            return Err(CoreThrow::Other("indexes overflowed 16 bits".into()));
        }
        Ok(())
    },
    sample: no_sample,
};

/// `BlockTransactionsRequest::indexes`: `VectorFormatter<DifferenceFormatter>` into
/// `uint16_t`s; the running index past 65535 throws (`blockencodings.h:35-41`, `:50-53`).
const DIFF_INDEXES: Custom = Custom {
    read: |r| {
        let n = r.count()?;
        let mut shift = 0u64;
        for _ in 0..n {
            shift += r.compact_size(true)?;
            if shift > u64::from(u16::MAX) {
                return Err(CoreThrow::Other("differential value overflow".into()));
            }
            shift += 1;
        }
        Ok(())
    },
    sample: |s| {
        s.count();
        s.out.extend([1, 0]);
    },
};

/// `CGovernanceVote::vchSig` on a network read: `LIMITED_VECTOR(vchSig, BLS_SIG_SIZE)`, then a
/// size other than 65 or 96 throws (`governance/vote.h:197-201`).
const VOTE_SIG: Custom = Custom {
    read: |r| {
        let n = r.count()?;
        if n > 96 {
            return Err(CoreThrow::Other(VECTOR_LIMIT.into()));
        }
        r.read(n as usize)?;
        if n != 65 && n != 96 {
            return Err(CoreThrow::Other("bad governance vote signature size".into()));
        }
        Ok(())
    },
    sample: |s| {
        s.count();
        s.out.push(65);
        s.out.resize(s.out.len() + 65, 0);
    },
};

/// `GetHeadersLimit` (`net_processing.cpp:1210-1216`).
fn headers_limit(_: i32) -> u64 {
    2000
}

fn headers2_limit(version: i32) -> u64 {
    if version >= INCREASE_MAX_HEADERS2_VERSION {
        8000
    } else {
        2000
    }
}

// ---- Field lists ---------------------------------------------------------------------------

use Field::{Bool, Bounded, Fixed, Ignore, Judged, Limited, Size, Vec as List};

const BYTE: &[F] = &[f(Fixed(1), "unsigned char")];
const HASH: &[F] = &[f(Fixed(32), "uint256")];

/// `CAddress` on the network (`protocol.h:432-476`).
const ADDRESS: &[F] = &[
    f(Fixed(4), "nTime: uint32_t, LossyChronoFormatter, protocol.h:463"),
    f(Field::Custom(SERVICES), "nServices: ServiceFlags, protocol.h:466-472"),
    f(Field::Custom(NET_ADDR), "CService::CNetAddr, protocol.h:475"),
    f(Fixed(2), "CService::port: uint16_t big-endian, netaddress.h:566"),
];

/// `CInv` (`protocol.h:537`).
const CINV: &[F] = &[f(Fixed(4), "type: uint32_t"), f(Fixed(32), "hash: uint256")];

/// `CBlockLocator` then `hashStop` (`primitives/block.h:255-260`: `nVersion` is read on a
/// network stream, which is never `SER_GETHASH`).
const LOCATOR_STOP: &[F] = &[
    f(Fixed(4), "CBlockLocator::nVersion: int, primitives/block.h:257-259"),
    f(List(HASH), "CBlockLocator::vHave: std::vector<uint256>, primitives/block.h:260"),
    f(Fixed(32), "hashStop: uint256"),
];

/// `CBlockHeader` and the tx count Core reads and ignores (`net_processing.cpp:5332-5333`).
const HEADER_ENTRY: &[F] = &[
    f(Fixed(80), "CBlockHeader: int32_t, uint256 x2, uint32_t x3, primitives/block.h:40"),
    f(Size, "tx count: ReadCompactSize(vRecv), ignored, net_processing.cpp:5333"),
];

/// `CBloomFilter` (`common/bloom.h:78`); `IsWithinSizeConstraints` is checked after the read.
const BLOOM: &[F] = &[
    f(
        Limited(MAX_BLOOM_FILTER_SIZE, VECTOR_LIMIT, BYTE),
        "vData: LIMITED_VECTOR(vData, MAX_BLOOM_FILTER_SIZE), common/bloom.h:78",
    ),
    f(Fixed(4), "nHashFuncs: unsigned int"),
    f(Fixed(4), "nTweak: unsigned int"),
    f(Fixed(1), "nFlags: unsigned char"),
];

/// `PrefilledTransaction` (`blockencodings.h:79`).
const PREFILLED: &[F] = &[
    f(Field::Custom(PREFILLED_INDEX), "index: COMPACTSIZE(uint16_t), blockencodings.h:79"),
    f(Judged(TX), "tx: CTransactionRef, blockencodings.h:79"),
];

const SHORT_ID: &[F] = &[f(Fixed(6), "shorttxid: CustomUintFormatter<6>, blockencodings.h:118")];
const TX_ITEM: &[F] = &[f(Judged(TX), "CTransactionRef")];
const HEADERS2_ENTRY: &[F] =
    &[f(Field::Custom(COMPRESSED_HEADER), "CompressibleBlockHeader, primitives/block.h:166")];

// `version`'s optional tail, each part read only while bytes remain (net_processing.cpp:3977-4003).
const VERSION_FROM: &[F] =
    &[f(Ignore(26), "addrFrom: vRecv.ignore(26), :3982"), f(Fixed(8), "nNonce: uint64_t, :3983")];
const VERSION_SUBVER: &[F] = &[f(
    Limited(256, STRING_LIMIT, BYTE),
    "strSubVer: LIMITED_STRING(MAX_SUBVERSION_LENGTH), :3987",
)];
const VERSION_HEIGHT: &[F] = &[f(Fixed(4), "starting_height: int, :3991")];
const VERSION_RELAY: &[F] = &[f(Bool, "fRelay: bool, net_processing.cpp:3994, serialize.h:239")];
const VERSION_CHALLENGE: &[F] = &[f(Fixed(32), "receivedMNAuthChallenge: uint256, :3997")];
const VERSION_MN: &[F] =
    &[f(Bool, "fOtherMasternode: bool, net_processing.cpp:4002, serialize.h:239")];

const VERSION: &[F] = &[
    f(Field::Custom(PEER_VERSION), "nVersion: int, :3950"),
    f(Fixed(8), "nServices: CustomUintFormatter<8>, :3950"),
    f(Fixed(8), "nTime: int64_t, :3950"),
    f(Ignore(8), "addrMe services: vRecv.ignore(8), :3954"),
    f(Field::Custom(NET_ADDR), "addrMe: CService::CNetAddr, :3955"),
    f(Fixed(2), "addrMe: CService::port, :3955"),
    f(Field::Custom(OBSOLETE_VERSION), "nVersion < MIN_PEER_PROTO_VERSION, :3970"),
    f(Field::IfAvail(1, VERSION_FROM), "!vRecv.empty(), :3977"),
    f(Field::IfAvail(1, VERSION_SUBVER), "!vRecv.empty(), :3985"),
    f(Field::IfAvail(1, VERSION_HEIGHT), "!vRecv.empty(), :3990"),
    f(Field::IfAvail(1, VERSION_RELAY), "!vRecv.empty(), :3993"),
    f(Field::IfAvail(1, VERSION_CHALLENGE), "!vRecv.empty(), :3995"),
    f(Field::IfAvail(1, VERSION_MN), "!vRecv.empty(), :4000"),
];

/// `getcfilters`' and `getcfheaders`' request (`net_processing.cpp:3489`, `:3520`).
const CF_REQUEST: &[F] = &[
    f(Fixed(1), "filter_type_ser: uint8_t"),
    f(Fixed(4), "start_height: uint32_t"),
    f(Fixed(32), "stop_hash: uint256"),
];

const INV: &[F] = &[f(List(CINV), "vInv: std::vector<CInv>; MAX_INV_SZ is checked after the read")];
const ADDR: &[F] = &[f(List(ADDRESS), "vAddr: std::vector<CAddress>, :4399")];
const ADDRV2: &[F] = &[
    f(Field::Custom(ADDRV2_STREAM), "stream_version |= ADDRV2_FORMAT, :4393"),
    f(List(ADDRESS), "vAddr: std::vector<CAddress>, :4399"),
];

/// `nProp`, read outside the handler's catch, then `CBloomFilter` (`common/bloom.h:78`) inside it.
const GOVSYNC: &[F] = &[
    f(Fixed(32), "nProp: uint256, governance/net_governance.cpp:93"),
    f(
        Limited(MAX_BLOOM_FILTER_SIZE, VECTOR_LIMIT, BYTE),
        "filter.vData: LIMITED_VECTOR(vData, MAX_BLOOM_FILTER_SIZE), common/bloom.h:78",
    ),
    f(Fixed(4), "filter.nHashFuncs: unsigned int"),
    f(Fixed(4), "filter.nTweak: unsigned int"),
    f(Fixed(1), "filter.nFlags: unsigned char"),
];

const GOVOBJ: &[F] = &[
    f(Fixed(32), "hashParent: uint256, governance/common.h:70-80"),
    f(Fixed(4), "revision: int"),
    f(Fixed(8), "time: int64_t"),
    f(Fixed(32), "collateralHash: uint256"),
    f(List(BYTE), "vchData: std::vector<unsigned char>, serialize.h:1135"),
    f(Fixed(4), "type: GovernanceObject (int enum), serialize.h:966"),
    f(Fixed(36), "masternodeOutpoint: COutPoint, primitives/transaction.h:70"),
    f(List(BYTE), "vchSig: std::vector<unsigned char>, governance/common.h:81-82"),
];

const GOVOBJVOTE: &[F] = &[
    f(Fixed(36), "masternodeOutpoint: COutPoint, governance/vote.h:192"),
    f(Fixed(32), "nParentHash: uint256"),
    f(Fixed(4), "nVoteOutcome: vote_outcome_enum_t (int)"),
    f(Fixed(4), "nVoteSignal: vote_signal_enum_t (int)"),
    f(Fixed(8), "nTime: int64_t"),
    f(Field::Custom(VOTE_SIG), "vchSig: LIMITED_VECTOR(96), 65 or 96, governance/vote.h:198-200"),
];

/// The matrix, in `allNetMessageTypes` order. Line numbers are v24.0.0-rc.3's
/// `net_processing.cpp` unless a file is named.
const ROWS: &[Row] = &[
    r(
        "version",
        "net_processing.cpp:3935 (head :3950-3955, check :3970, tail :3977-4003)",
        VERSION,
    ),
    r("addr", "net_processing.cpp:4388 (read :4399, MAX_ADDR_TO_SEND after it)", ADDR),
    r("addrv2", "net_processing.cpp:4388 (ADDRV2_FORMAT :4393, read :4399)", ADDRV2),
    r("inv", "net_processing.cpp:4507 (read :4509)", INV),
    r("getdata", "net_processing.cpp:4613 (read :4615)", INV),
    r("getblocks", "net_processing.cpp:4636 (read :4639, MAX_LOCATOR_SZ after it)", LOCATOR_STOP),
    r("getheaders", "net_processing.cpp:4760 (read :4763, MAX_LOCATOR_SZ after it)", LOCATOR_STOP),
    r(
        "tx",
        "net_processing.cpp:4861 (read :4878)",
        &[f(Judged(TX), "ptx: CTransactionRef, :4878")],
    ),
    r(
        "headers",
        "net_processing.cpp:5309 (count :5323, limit :5324, headers :5329-5334)",
        &[f(Bounded(headers_limit, HEADER_ENTRY), "nCount > GetHeadersLimit, :5324")],
    ),
    r(
        "block",
        "net_processing.cpp:5348 (read :5357)",
        &[f(Judged(BLOCK), "*pblock: CBlock, :5357")],
    ),
    r("ping", "net_processing.cpp:5456 (read :5458)", &[f(Fixed(8), "nonce: uint64_t, :5458")]),
    r(
        "pong",
        "net_processing.cpp:5474 (nAvail >= sizeof(nonce) :5481, read :5482)",
        &[f(Field::IfAvail(8, &[f(Fixed(8), "nonce: uint64_t, :5482")]), ":5481")],
    ),
    r("notfound", "net_processing.cpp:5744 (read :5747, size check after it)", INV),
    r("filterload", "net_processing.cpp:5529 (read :5537 in a catch, size check after it)", BLOOM),
    r(
        "filteradd",
        "net_processing.cpp:5560 (read :5571 in a catch)",
        &[f(
            Limited(520, VECTOR_LIMIT, BYTE),
            "vData: LIMITED_VECTOR(vData, MAX_SCRIPT_ELEMENT_SIZE), :5571",
        )],
    ),
    r(
        "sendcmpct",
        "net_processing.cpp:4273 (read :4276)",
        &[
            f(Bool, "sendcmpct_hb: bool, net_processing.cpp:4276, serialize.h:239"),
            f(Fixed(8), "sendcmpct_version: uint64_t, :4276"),
        ],
    ),
    r(
        "cmpctblock",
        "net_processing.cpp:5052 (read :5061)",
        &[
            f(Fixed(80), "header: CBlockHeader, blockencodings.h:118"),
            f(Fixed(8), "nonce: uint64_t"),
            f(List(SHORT_ID), "shorttxids: std::vector<uint64_t>, blockencodings.h:118"),
            f(List(PREFILLED), "prefilledtxn: std::vector<PrefilledTransaction>"),
            f(Field::Custom(TX_COUNT), "BlockTxCount() > 65535, blockencodings.h:120-122"),
        ],
    ),
    r(
        "getblocktxn",
        "net_processing.cpp:4704 (read :4706)",
        &[
            f(Fixed(32), "blockhash: uint256, blockencodings.h:52"),
            f(Field::Custom(DIFF_INDEXES), "indexes: DifferenceFormatter, blockencodings.h:52"),
        ],
    ),
    r(
        "blocktxn",
        "net_processing.cpp:5295 (read :5304)",
        &[
            f(Fixed(32), "blockhash: uint256, blockencodings.h:68"),
            f(List(TX_ITEM), "txn: std::vector<CTransactionRef>, blockencodings.h:68"),
        ],
    ),
    r("getcfilters", "net_processing.cpp:5638 (ProcessGetCFilters, read :3489)", CF_REQUEST),
    r("getcfheaders", "net_processing.cpp:5643 (ProcessGetCFHeaders, read :3520)", CF_REQUEST),
    r(
        "getcfcheckpt",
        "net_processing.cpp:5648 (ProcessGetCFCheckPt, read :3563)",
        &[f(Fixed(1), "filter_type_ser: uint8_t"), f(Fixed(32), "stop_hash: uint256")],
    ),
    r(
        "sendtxrcncl",
        "net_processing.cpp:4310 (read :4341)",
        &[f(Fixed(4), "peer_txreconcl_version: uint32_t"), f(Fixed(8), "remote_salt: uint64_t")],
    ),
    r(
        "senddsq",
        "net_processing.cpp:4485 (read :4488)",
        &[f(Bool, "b (fSendDSQueue): bool, net_processing.cpp:4488, serialize.h:239")],
    ),
    r(
        "ssc",
        "node/sync_manager.cpp:333 (read :340)",
        &[f(Fixed(4), "nItemID: int"), f(Fixed(4), "nCount: int")],
    ),
    r("govsync", "governance/net_governance.cpp:86 (nProp :93, filter :97 in a catch)", GOVSYNC),
    r("govobj", "governance/net_governance.cpp:171 (read :174)", GOVOBJ),
    r("govobjvote", "governance/net_governance.cpp:202 (read :208 in a catch)", GOVOBJVOTE),
    r(
        "getmnlistd",
        "net_processing.cpp:5611 (read :5613)",
        &[
            f(Fixed(32), "baseBlockHash: uint256, evo/smldiff.h:41"),
            f(Fixed(32), "blockHash: uint256, evo/smldiff.h:41"),
        ],
    ),
    r(
        "qsendrecsigs",
        "net_processing.cpp:4500 (read :4502)",
        &[f(Bool, "b (fSendRecSigs): bool, net_processing.cpp:4502, serialize.h:239")],
    ),
    r("getheaders2", "net_processing.cpp:4760 (read :4763, MAX_LOCATOR_SZ after it)", LOCATOR_STOP),
    r(
        "headers2",
        "net_processing.cpp:5309 (count :5323, limit :5324, headers :5335-5343)",
        &[f(Bounded(headers2_limit, HEADERS2_ENTRY), "nCount > GetHeadersLimit, :5324")],
    ),
];

fn row(cmd: &str) -> Option<&'static Row> {
    ROWS.iter().find(|r| r.cmd == cmd)
}

/// Whether the boundary's verdict `ours` is Core's `core` for `cmd`: the same bytes read, or both
/// refused. A handler return after the read is the boundary's count refusal for `headers` and
/// `headers2`, and either verdict for `version` (the processor checks the version after the
/// read). The boundary's handler refusals (`TooManyEntries`, `HandlerPayloadTooLarge`) agree with
/// nothing else: Core's throw is not its handler's return. `VersionTail` is base-sdk's carrier of
/// a throw in `version`'s tail, after the head checks (`policy::ThrowOutcome::VersionTail`), so it
/// is also `version`'s ordinary throw.
pub(super) fn agrees(
    cmd: &CommandString,
    core: &Result<usize, CoreThrow>,
    ours: &Result<DecodedMsg, P2pDecodeError>,
) -> bool {
    match (core, ours) {
        (Ok(n), Ok(d)) => *n == d.consumed,
        (
            Err(CoreThrow::HandlerReturn(_)),
            Err(P2pDecodeError::TooManyEntries {
                ..
            }),
        ) => cmd.as_str().starts_with("headers"),
        (
            Err(CoreThrow::HandlerReturn(_)),
            Ok(_)
            | Err(P2pDecodeError::VersionTail {
                ..
            }),
        ) => cmd.as_str() == "version",
        (
            Err(_),
            Err(P2pDecodeError::VersionTail {
                ..
            }),
        ) if cmd.as_str() == "version" => true,
        (Err(CoreThrow::HandlerReturn(_)), Err(_))
        | (
            Err(_),
            Err(
                P2pDecodeError::TooManyEntries {
                    ..
                }
                | P2pDecodeError::VersionTail {
                    ..
                }
                | P2pDecodeError::HandlerPayloadTooLarge {
                    ..
                },
            ),
        ) => false,
        (Err(_), Err(_)) => true,
        _ => false,
    }
}

/// Core's read of `cmd`'s payload at the stream version `version` (`GetCommonVersion`): the
/// bytes read, or why the read stopped. `None` for a command the matrix does not cover.
pub(super) fn judge(
    cmd: &CommandString,
    payload: &[u8],
    version: i32,
) -> Option<Result<usize, CoreThrow>> {
    let row = row(cmd.as_str())?;
    let mut reader = Reader::new(payload, version);
    Some(reader.fields(row.fields).map(|()| reader.pos))
}

/// The headers Core's `headers2` handler builds from `payload` at `version`
/// (`net_processing.cpp:5335-5343`): each read by [`compressed_header`] and uncompressed against
/// the ones before it. `None` unless Core reads the payload.
pub(super) fn headers2_values(payload: &[u8], version: i32) -> Option<Vec<Header>> {
    judge(&CommandString::HEADERS2, payload, version)?.ok()?;
    let mut r = Reader::new(payload, version);
    let count = r.count().ok()?;
    // std::list<int32_t> last_unique_versions, :5336
    let mut versions = Vec::new();
    let mut headers: Vec<Header> = Vec::new();
    for _ in 0..count {
        let (bits, fields) = compressed_header(&mut r).ok()?;
        let h = uncompress(bits, fields, headers.last(), &mut versions);
        headers.push(h);
    }
    Some(headers)
}

/// The CompactSize at the front of `payload` as `ReadCompactSize(range_check=false)` reads it,
/// when it reads.
pub(super) fn leading_count(payload: &[u8]) -> Option<u64> {
    Reader::new(payload, 0).compact_size(false).ok()
}

/// Every C++ `bool` the matrix reads: the command and the field's citation.
#[cfg(test)]
pub(super) fn bool_fields() -> impl Iterator<Item = (&'static str, &'static str)> {
    fn walk(cmd: &'static str, fields: &'static [F], out: &mut Vec<(&'static str, &'static str)>) {
        for field in fields {
            match &field.field {
                Field::Bool => out.push((cmd, field.core)),
                Field::Vec(items)
                | Field::Limited(_, _, items)
                | Field::Bounded(_, items)
                | Field::IfAvail(_, items) => walk(cmd, items, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for row in ROWS {
        walk(row.cmd, row.fields, &mut out);
    }
    out.into_iter()
}

/// A minimal payload Core reads whole at `version`: zeros, every bool `bool_byte`, one item in
/// every vector, every optional tail present, judged fields minimal and valid. `version`'s
/// `nVersion` is `version`. `None` for a command the matrix does not cover.
pub(super) fn sample(cmd: &CommandString, version: i32, bool_byte: u8) -> Option<Vec<u8>> {
    Some(build_sample(row(cmd.as_str())?, version, bool_byte).out)
}

fn build_sample(row: &Row, version: i32, bool_byte: u8) -> Sample {
    let mut s = Sample {
        out: Vec::new(),
        version,
        bool_byte,
        first_count: None,
    };
    sample_fields(&mut s, row.fields);
    s
}

/// splitmix64, the tests' generator.
#[cfg(test)]
pub(super) struct Rng(pub(super) u64);

#[cfg(test)]
impl Rng {
    pub(super) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn byte(&mut self) -> u8 {
        self.next() as u8
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a covered row's sample or verdict is always there
mod tests {
    use std::collections::BTreeMap;

    use dash_consensus::primitives::header_bytes;
    use dash_p2p::policy::{self, ReadPlan, Throw};
    use dash_p2p_core::{P2pMsg, ProtoCtx, ProtocolVersion};
    use dash_pkc::bls::BlsCtx;
    use dash_types::Hashable;

    use super::*;

    /// The minimum peer version, ADDRv2, versioned SML entries, `headers2`'s raised limit, v24.
    const VERSIONS: [i32; 5] = [70221, 70223, 70228, 70235, 70242];

    fn cmd(row: &Row) -> CommandString {
        CommandString::from_static(row.cmd)
    }

    fn decode(
        cmd: &CommandString,
        payload: &[u8],
        version: i32,
    ) -> Result<DecodedMsg, P2pDecodeError> {
        let ctx = ProtoCtx::new(ProtocolVersion(version), BlsCtx::BASIC);
        policy::decode(cmd, payload, &ctx)
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn has_judged(fields: &[F]) -> bool {
        fields.iter().any(|field| match &field.field {
            Field::Judged(_) => true,
            Field::Vec(items) | Field::Limited(_, _, items) | Field::Bounded(_, items) => {
                has_judged(items)
            }
            _ => false,
        })
    }

    fn has_tail(fields: &[F]) -> bool {
        fields.iter().any(|field| matches!(field.field, Field::IfAvail(..)))
    }

    /// The verdict's kind, for grouping.
    fn core_kind(core: &Result<usize, CoreThrow>) -> String {
        match core {
            Ok(_) => "Ok".into(),
            Err(CoreThrow::Other(text)) => format!("Other({text:?})"),
            Err(e) => format!("{e:?}"),
        }
    }

    fn kind(ours: &Result<DecodedMsg, P2pDecodeError>) -> String {
        let Err(e) = ours else {
            return "Ok".into();
        };
        let debug = format!("{e:?}");
        let inner = debug.strip_prefix("Consensus(").unwrap_or(&debug);
        inner.split([' ', '(', '{']).next().unwrap_or_default().to_owned()
    }

    fn verdict(ours: &Result<DecodedMsg, P2pDecodeError>) -> String {
        match ours {
            Ok(d) => format!("Ok(consumed {})", d.consumed),
            Err(e) => format!("Err({e})"),
        }
    }

    #[test]
    fn rows_are_policy_rows_that_read() {
        for row in ROWS {
            let policy = policy::lookup(&cmd(row)).unwrap_or_else(|| panic!("{}", row.cmd));
            assert_ne!(policy.read, ReadPlan::Nothing, "{}", row.cmd);
        }
        assert_eq!(ROWS.len(), 32);
        let bools: Vec<_> = bool_fields().map(|(cmd, _)| cmd).collect();
        assert_eq!(bools, ["version", "version", "sendcmpct", "senddsq", "qsendrecsigs"]);
    }

    /// Every bool read as Core reads it, any byte: the matrix and the boundary read the sample
    /// whole for 0, 1, 2 and 255 (DEC-178 (2)).
    #[test]
    fn bools_are_any_byte() {
        let mut findings = Vec::new();
        let rows = ROWS.iter().filter(|r| bool_fields().any(|(cmd, _)| cmd == r.cmd));
        for row in rows {
            for version in VERSIONS {
                for b in [0u8, 1, 2, 255] {
                    let payload = sample(&cmd(row), version, b).unwrap();
                    assert_eq!(judge(&cmd(row), &payload, version), Some(Ok(payload.len())));
                    let ours = decode(&cmd(row), &payload, version);
                    if !matches!(&ours, Ok(d) if d.consumed == payload.len()) {
                        let fields: Vec<_> = bool_fields().filter(|(c, _)| *c == row.cmd).collect();
                        findings.push(format!(
                            "{} v{version} bool {b}: {} ({fields:?}): {}",
                            row.cmd,
                            hex(&payload),
                            verdict(&ours)
                        ));
                    }
                }
            }
        }
        assert!(findings.is_empty(), "the boundary refuses Core's bools:\n{}", findings.join("\n"));
    }

    /// A sample one byte short ends in the data, unless an optional tail makes it whole.
    #[test]
    fn truncated_samples_end_in_the_data() {
        for row in ROWS {
            for version in VERSIONS {
                let mut payload = sample(&cmd(row), version, 1).unwrap();
                payload.pop();
                let got = judge(&cmd(row), &payload, version).unwrap();
                let ok = match &got {
                    Ok(n) => has_tail(row.fields) && *n <= payload.len(),
                    Err(CoreThrow::EndOfData) => !has_tail(row.fields),
                    Err(CoreThrow::Judged(_)) => has_judged(row.fields),
                    Err(_) => false,
                };
                assert!(ok, "{} v{version}: {got:?}", row.cmd);
            }
        }
    }

    /// Non-canonical counts and counts over `MAX_SIZE` throw Core's texts, and the boundary
    /// refuses them with the same text.
    #[test]
    fn bad_counts_throw() {
        let cases: [(&[u8], CoreThrow); 5] = [
            (&[0xfd, 0x10, 0x00], CoreThrow::NonCanonical),
            (&[0xfe, 0xff, 0xff, 0x00, 0x00], CoreThrow::NonCanonical),
            (&[0xff, 1, 0, 0, 0, 0, 0, 0, 0], CoreThrow::NonCanonical),
            (&[0xfe, 0x01, 0x00, 0x00, 0x02], CoreThrow::SizeTooLarge),
            (&[0xff, 0, 0, 0, 0, 1, 0, 0, 0], CoreThrow::SizeTooLarge),
        ];
        let mut findings = Vec::new();
        let mut covered = 0;
        for row in ROWS {
            for version in VERSIONS {
                let s = build_sample(row, version, 1);
                let Some(at) = s.first_count else {
                    continue;
                };
                covered += 1;
                for (count, want) in &cases {
                    let mut payload = s.out[..at].to_vec();
                    payload.extend_from_slice(count);
                    payload.resize(payload.len() + 64, 0);
                    assert_eq!(judge(&cmd(row), &payload, version), Some(Err(want.clone())));
                    let ours = decode(&cmd(row), &payload, version);
                    let text = ours.as_ref().err().map(|e| Throw::from_decode(e).what.into_owned());
                    let want_text = format!("{}: iostream error", want.what().unwrap());
                    if text.as_deref() != Some(&want_text) {
                        findings.push(format!(
                            "{} v{version} {}: {}",
                            row.cmd,
                            hex(&payload),
                            verdict(&ours)
                        ));
                    }
                }
            }
        }
        assert_eq!(covered, 19 * VERSIONS.len());
        assert!(findings.is_empty(), "bad counts:\n{}", findings.join("\n"));
    }

    /// BIP155 rules: services without the range check, sizes per network, unknown ids skipped.
    #[test]
    fn addrv2_addresses() {
        let addrv2 = CommandString::from_static("addrv2");
        let entry = |services: &[u8], net: u8, size: &[u8], bytes: usize| {
            let mut p = vec![1, 0, 0, 0, 0];
            p.extend_from_slice(services);
            p.push(net);
            p.extend_from_slice(size);
            p.resize(p.len() + bytes + 2, 0);
            p
        };
        let other = |text: &str| Some(Err(CoreThrow::Other(text.into())));
        let max_services = [0xff; 9];
        let cases = [
            (entry(&max_services, 1, &[4], 4), Some(Ok(22))),
            (entry(&[0], 3, &[10], 10), Some(Ok(20))),
            (entry(&[0], 99, &[0xfd, 0, 2], 512), Some(Ok(524))),
            (entry(&[0], 99, &[0xfd, 1, 2], 513), other("Address too long: 513 > 512")),
            (entry(&[0], 1, &[16], 16), other("BIP155 IPv4 address with length 16 (should be 4)")),
            (entry(&[0], 99, &[8], 1), other("DataStream::ignore(): end of data")),
        ];
        for (payload, want) in cases {
            assert_eq!(judge(&addrv2, &payload, 70242), want, "{}", hex(&payload));
            let ours = decode(&addrv2, &payload, 70242);
            assert_eq!(ours.is_ok(), matches!(want, Some(Ok(_))), "{}", hex(&payload));
        }
        // `addr` reads the V1 form at the same version.
        let addr = CommandString::from_static("addr");
        let mut v1 = vec![1];
        v1.resize(1 + 30, 0);
        assert_eq!(judge(&addr, &v1, 70242), Some(Ok(31)));
    }

    /// A pseudo-random payload: random bytes, or a sample mutated at random positions.
    fn payload(rng: &mut Rng, row: &Row, version: i32) -> Vec<u8> {
        if rng.below(4) == 0 {
            return (0..rng.below(160)).map(|_| rng.byte()).collect();
        }
        let mut p = sample(&cmd(row), version, rng.byte()).unwrap();
        for _ in 0..1 + rng.below(3) {
            let at = rng.below(p.len() + 1);
            match rng.below(6) {
                0 | 1 if at < p.len() => p[at] = rng.byte(),
                2 if at < p.len() => p[at] = [0, 1, 2, 0xfc, 0xfd, 0xfe, 0xff][rng.below(7)],
                3 => p.truncate(at),
                4 => p.insert(at, rng.byte()),
                _ => p.extend((0..rng.below(16)).map(|_| rng.byte())),
            }
        }
        p
    }

    /// The matrix and the boundary agree on ~2000 payloads per row and version.
    #[test]
    fn agrees_with_the_boundary() {
        let mut groups: BTreeMap<String, (usize, String)> = BTreeMap::new();
        for (i, row) in ROWS.iter().enumerate() {
            for version in VERSIONS {
                let mut rng = Rng(((i as u64) << 32) | version as u64);
                for _ in 0..2000 {
                    let p = payload(&mut rng, row, version);
                    let core = judge(&cmd(row), &p, version).unwrap();
                    let ours = decode(&cmd(row), &p, version);
                    if agrees(&cmd(row), &core, &ours) {
                        continue;
                    }
                    let key = format!(
                        "{}: Core {} / boundary {}",
                        row.cmd,
                        core_kind(&core),
                        kind(&ours)
                    );
                    let example = || {
                        format!(
                            "v{version} {} -> Core {core:?}, boundary {}",
                            hex(&p),
                            verdict(&ours)
                        )
                    };
                    groups.entry(key).or_insert_with(|| (0, example())).0 += 1;
                }
            }
        }
        let report: Vec<_> =
            groups.iter().map(|(k, (n, ex))| format!("{k} ({n}x), e.g. {ex}")).collect();
        assert!(report.is_empty(), "disagreements:\n{}", report.join("\n"));
    }

    /// The boundary's `headers2` headers as `CBlockHeader` serializations, when it reads.
    fn boundary_headers2(ours: &Result<DecodedMsg, P2pDecodeError>) -> Option<Vec<[u8; 80]>> {
        match &ours.as_ref().ok()?.msg {
            P2pMsg::Headers2(h) => Some(h.block_headers().map(header_bytes).collect()),
            _ => None,
        }
    }

    /// A pseudo-random `headers2` payload of up to eight headers over a few versions, and its bit
    /// fields: any bit field (the version offset often small, so offsets hit and miss the list),
    /// or, a third of the time, the forms Core's `Compress` builds (a whole first header, offsets
    /// in the list).
    fn headers2_payload(rng: &mut Rng) -> (Vec<u8>, Vec<u8>) {
        let count = 1 + rng.below(8);
        let built = rng.below(3) == 0;
        let mut p = vec![count as u8];
        let mut flags: Vec<u8> = Vec::new();
        // The list's length before this header, when built.
        let mut listed = 0;
        for i in 0..count {
            let bits = match rng.below(4) {
                _ if built && i == 0 => 0b0011_1000,
                _ if built => (rng.byte() & 0b0011_1000) | rng.below(listed + 1) as u8,
                0 => rng.byte(),
                1 => 0b0011_1000,
                _ => (rng.byte() & 0b0011_1000) | [0, 1, 2, 3, 7][rng.below(5)],
            };
            flags.push(bits);
            p.push(bits);
            if bits & H2_VERSION_OFFSET == 0 || i == 0 {
                listed = (listed + 1).min(MAX_BACKWARDS_LOOK_UPS);
            }
            if bits & H2_VERSION_OFFSET == 0 {
                let v = [1i32, 2, 3, 0x2000_0000, rng.next() as i32][rng.below(5)];
                p.extend(v.to_le_bytes());
            }
            let wire = |flag: u8, n: usize| usize::from(bits & flag != 0) * n;
            let len = 32 + 2 + 4 + wire(H2_PREV_BLOCK_HASH, 32) + wire(H2_TIMESTAMP, 2);
            p.extend((0..len + wire(H2_NBITS, 4)).map(|_| rng.byte()));
        }
        if rng.below(8) == 0 {
            p.truncate(rng.below(p.len() + 1));
        }
        (p, flags)
    }

    /// The boundary's `headers2` headers are the ones Core's `Uncompress` builds, over generated
    /// payloads: a first header with fields left off, version offsets in and past the list, and
    /// headers base-sdk's strict read takes or only the boundary's fallback reads.
    #[test]
    fn headers2_values_match_the_boundary() {
        let headers2 = CommandString::HEADERS2;
        let mut findings = Vec::new();
        let (mut compared, mut first_compressed, mut past_list, mut fallback) = (0, 0, 0, 0);
        // One version: with at most eight headers it only picks a count limit never reached.
        for version in [70242] {
            let mut rng = Rng(0x4832 ^ version as u64);
            for _ in 0..1000 {
                let (p, flags) = headers2_payload(&mut rng);
                let core = judge(&headers2, &p, version).unwrap();
                let ours = decode(&headers2, &p, version);
                if !agrees(&headers2, &core, &ours) {
                    findings.push(format!("v{version} {}: {core:?} / {}", hex(&p), verdict(&ours)));
                    continue;
                }
                let Some(values) = headers2_values(&p, version) else {
                    continue;
                };
                compared += 1;
                let want: Vec<_> = values.iter().map(Header::bytes).collect();
                if boundary_headers2(&ours) != Some(want) {
                    findings.push(format!("v{version} {}: values", hex(&p)));
                }
                if flags[0] & 0b0011_1111 != 0b0011_1000 {
                    first_compressed += 1;
                }
                // The list's length before each later header.
                let mut listed = 1;
                for bits in &flags[1..] {
                    match usize::from(bits & H2_VERSION_OFFSET) {
                        0 => listed = (listed + 1).min(MAX_BACKWARDS_LOOK_UPS),
                        offset if offset > listed => past_list += 1,
                        _ => {}
                    }
                }
                let ctx = ProtoCtx::new(ProtocolVersion(version), BlsCtx::BASIC);
                if P2pMsg::decode_with_policy(&headers2, &p, &ctx).is_err() {
                    fallback += 1;
                }
            }
        }
        assert!(findings.is_empty(), "headers2:\n{}", findings.join("\n"));
        let covered = [compared, first_compressed, past_list, fallback, compared - fallback];
        assert!(covered.iter().all(|&n| n > 150), "{covered:?}");
    }

    /// Five headers worked through Core's `Uncompress` by hand (`primitives/block.cpp:95-134`).
    #[test]
    fn headers2_values_by_hand() {
        let mut p = vec![5];
        // 1: version offset 1, everything compressed: read as left, version 0 saved. [0]
        p.push(0b0000_0001);
        p.extend([0x11; 32]);
        p.extend(0x7fff_i16.to_le_bytes());
        p.extend(11u32.to_le_bytes());
        // 2: all on the wire, version 2 saved. [2, 0]
        p.push(0b0011_1000);
        p.extend(2i32.to_le_bytes());
        p.extend([0xaa; 32]);
        p.extend([0x22; 32]);
        p.extend(3u32.to_le_bytes());
        p.extend(0x1d00_ffff_u32.to_le_bytes());
        p.extend(22u32.to_le_bytes());
        // 3: offset 2 takes 0 to the front, time 3 - 5 wraps. [0, 2]
        p.push(0b0000_0010);
        p.extend([0x33; 32]);
        p.extend((-5i16).to_le_bytes());
        p.extend(33u32.to_le_bytes());
        // 4: offset 7, past the list: version 0, the list as it was. [0, 2]
        p.push(0b0001_0111);
        p.extend([0x44; 32]);
        p.extend(100u32.to_le_bytes());
        p.extend(44u32.to_le_bytes());
        // 5: offset 2 takes 2 to the front; time 100 + 1. [2, 0]
        p.push(0b0010_0010);
        p.extend([0x55; 32]);
        p.extend(1i16.to_le_bytes());
        p.extend(0x1e0f_ffff_u32.to_le_bytes());
        p.extend(55u32.to_le_bytes());

        let h1 = Header {
            merkle_root: [0x11; 32],
            nonce: 11,
            ..Header::default()
        };
        let h2 = Header {
            version: 2,
            prev_block: [0xaa; 32],
            merkle_root: [0x22; 32],
            time: 3,
            bits: 0x1d00_ffff,
            nonce: 22,
        };
        let h3 = Header {
            version: 0,
            prev_block: h2.hash(),
            merkle_root: [0x33; 32],
            time: 0xffff_fffe,
            bits: 0x1d00_ffff,
            nonce: 33,
        };
        let h4 = Header {
            version: 0,
            prev_block: h3.hash(),
            merkle_root: [0x44; 32],
            time: 100,
            bits: 0x1d00_ffff,
            nonce: 44,
        };
        let h5 = Header {
            version: 2,
            prev_block: h4.hash(),
            merkle_root: [0x55; 32],
            time: 101,
            bits: 0x1e0f_ffff,
            nonce: 55,
        };
        let want = vec![h1, h2, h3, h4, h5];
        assert_eq!(judge(&CommandString::HEADERS2, &p, 70242), Some(Ok(p.len())));
        assert_eq!(headers2_values(&p, 70242), Some(want.clone()));
        let bytes: Vec<_> = want.iter().map(Header::bytes).collect();
        let ours = decode(&CommandString::HEADERS2, &p, 70242);
        assert_eq!(boundary_headers2(&ours), Some(bytes));
    }

    /// `n` whole headers with versions 100, 101, …, then one at version offset 7 with every
    /// other field compressed.
    fn headers2_after_versions(n: i32) -> Vec<u8> {
        let mut p = vec![(n + 1) as u8];
        for v in 100..100 + n {
            p.push(0b0011_1000);
            p.extend(v.to_le_bytes());
            p.extend([v as u8; 32]);
            p.extend([v as u8; 32]);
            p.extend(1_400_000_000u32.to_le_bytes());
            p.extend(0x1d00_ffff_u32.to_le_bytes());
            p.extend((v as u32).to_le_bytes());
        }
        p.push(7);
        p.extend([0x77; 32]);
        p.extend(0i16.to_le_bytes());
        p.extend(7u32.to_le_bytes());
        p
    }

    /// The version list holds seven entries (`MAX_BACKWARDS_LOOK_UPS`): after seven versions
    /// offset 7 takes the oldest, 100, and an eighth insertion evicts it, so offset 7 takes 101.
    /// The matrix's own `Uncompress` judges the boundary and `policy::read_headers2`.
    #[test]
    fn headers2_version_list_holds_seven() {
        let ctx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);
        for (n, oldest) in [(7, 100), (8, 101)] {
            let p = headers2_after_versions(n);
            let want = headers2_values(&p, 70242).unwrap();
            assert_eq!(want.last().map(|h| h.version), Some(oldest));
            let bytes: Vec<_> = want.iter().map(Header::bytes).collect();
            let ours = decode(&CommandString::HEADERS2, &p, 70242);
            assert_eq!(boundary_headers2(&ours), Some(bytes.clone()));
            let read = policy::read_headers2(&mut &p[..], &ctx, |h| h.hash()).unwrap();
            assert_eq!(read.iter().map(header_bytes).collect::<Vec<_>>(), bytes);
        }
    }
}
