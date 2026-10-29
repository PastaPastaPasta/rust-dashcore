//! `p2p_decode_model` (SB6): `dash-p2p`'s per-command decode policy (P2a §3.2.2) against its
//! decode boundary, `policy::decode` (base-sdk's `P2pMsg::decode_with_policy`, with Core's
//! reading where the pinned base-sdk reads otherwise, DEC-178 (2)). The input picks a command
//! (one of Core's 74, or arbitrary command bytes), a common version, the BLS scheme and the
//! payload, raw or built from the matrix's well-formed sample. Checked:
//!
//! - the table: a known command has exactly one row; an unknown one has none;
//! - **Core's verdict** (DEC-178 (1)): for a row the Core-derived acceptance matrix
//!   (`p2p_decode_matrix`) covers, the boundary reads the bytes Core's handler reads, or refuses
//!   where Core's read throws, and a `headers2` it reads holds the headers Core's `Uncompress`
//!   builds (the matrix's model). The rows Core's oracle judges are O2c's overlay targets
//!   (`diff_p2p_*`), which run the same boundary;
//! - **never throws**: a row that cannot throw (`pong`) decodes every payload. A row that reads
//!   nothing is never handed to the decoder (the processor drops its payload, as Core's handler
//!   does), so nothing past the table is checked for it: base-sdk parses `mnlistdiff`, `qrinfo`,
//!   `merkleblock` and the `cf*` replies for SPV's own requests;
//! - **count first**: a `CountThenItems` row whose payload starts with a count over the row's
//!   limit (at this version) is refused with that limit before any item is read, and every
//!   count refusal names the row's limit (a row the decoder keeps raw for its processor, `dss`,
//!   is the processor's to check);
//! - **trailing**: `qgetdata` over 67 bytes is refused before the read, naming 67; the four DKG
//!   messages read their whole payload when they decode;
//! - **the throw**: every refusal maps to a `Throw` whose `what` is one of Core's stream texts
//!   for the stream errors (`DataStream::read(): end of data`, `non-canonical
//!   ReadCompactSize()`, `ReadCompactSize(): size too large`) with libstdc++'s suffix, and whose
//!   log line is Core's `ProcessMessages` line over the sanitized command;
//! - **the read point**: a decoded message re-encodes to exactly the bytes it read. [`LENIENT`]
//!   is only the re-encode allow-list: rows whose message is written canonically after a read
//!   Core allows several encodings of; their re-encoding must decode, whole, to the same
//!   message. Whether a payload is read at all is Core's verdict above, never this list.

use dash_consensus::primitives::header_bytes;
use dash_p2p::policy::{
    self, ALL_NET_MESSAGE_TYPES, GovSyncRead, ReadError, ReadPlan, Throw, ThrowOutcome, Trailing,
};
use dash_p2p_core::{
    CommandString, CompressedHeader, DecodedMsg, P2pDecodeError, P2pMsg, ProtoCtx, ProtocolVersion,
};
use dash_pkc::bls::BlsCtx;
use dash_types::codec::{BaseCodec, DecodeError};
use dash_types::{CompactSize, Hashable};
use node_l1::Divergence;

use super::p2p_decode_matrix::{self as matrix, CoreThrow, MAX_SIZE, leading_count};

/// Common versions around every version-dependent read: `INIT_PROTO_VERSION`, the minimum peer
/// version, ADDRv2, the BLS scheme switch, `headers2`'s raised limit (70235), v23.1 and v24.
const VERSIONS: &[i32] =
    &[209, 70221, 70223, 70224, 70225, 70227, 70228, 70234, 70235, 70240, 70242];

/// The re-encode allow-list: rows Core reads leniently but whose message is written canonically,
/// so the re-encoding may differ from the bytes read:
/// - a `bool` is any non-zero byte (`senddsq`, `dsq`'s `fReady`, `qsendrecsigs`, `version`'s
///   relay and masternode-connection flags, `sendcmpct`, `getqrinfo`'s `extraShare`);
/// - `qsigsinv` and `qgetsigs` read a share inventory's `AUTOBITSET` in either form, writing the
///   shorter one;
/// - `qdata` keeps `UNDEFINED` for an explicit 0xff error byte and a missing one alike, writing
///   neither;
/// - `headers` reads each header's tx count and ignores it, writing 0;
/// - `headers2`, only for a payload base-sdk's strict read refuses: a message with a header only
///   Core's `Uncompress` takes is kept as whole headers (`policy::decode`'s fallback);
/// - `qfcommit`'s BLS fields: bytes canonical only in the other scheme are kept as an invalid
///   object (`CBLSWrapper::Unserialize`'s `Reset`), written as zeros.
const LENIENT: [CommandString; 12] = [
    CommandString::from_static("senddsq"),
    CommandString::from_static("dsq"),
    CommandString::from_static("sendcmpct"),
    CommandString::from_static("getqrinfo"),
    CommandString::from_static("headers2"),
    CommandString::from_static("qsendrecsigs"),
    CommandString::from_static("qsigsinv"),
    CommandString::from_static("qgetsigs"),
    CommandString::from_static("version"),
    CommandString::from_static("qdata"),
    CommandString::from_static("headers"),
    CommandString::from_static("qfcommit"),
];

/// `CBloomFilter`'s `nHashFuncs`, `nTweak` and `nFlags` (`common/bloom.h:78`).
const GOVSYNC_FILTER_TAIL: usize = 9;

/// libstdc++'s `ios_base::failure` texts Core's stream reads throw (v24 `streams.h:287`,
/// `serialize.h:304-319`).
const END_OF_DATA: &str = "DataStream::read(): end of data: iostream error";
const NON_CANONICAL: &str = "non-canonical ReadCompactSize(): iostream error";
const SIZE_TOO_LARGE: &str = "ReadCompactSize(): size too large: iostream error";

fn diverged(field: &'static str) -> Divergence {
    Divergence {
        field,
    }
}

/// The input as a stream of numbers; zeros once it runs out.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn u8(&mut self) -> u8 {
        let (&b, rest) = self.0.split_first().unwrap_or((&0, &[]));
        self.0 = rest;
        b
    }
}

/// The command: mostly one of Core's 74, sometimes 12 raw bytes from the input.
fn command(input: &mut Input<'_>) -> CommandString {
    let pick = input.u8();
    if pick < 250 {
        return ALL_NET_MESSAGE_TYPES[usize::from(pick) % ALL_NET_MESSAGE_TYPES.len()];
    }
    let raw: [u8; 12] = core::array::from_fn(|_| input.u8());
    CommandString::from_bytes(raw)
}

/// The limit a count-first row checks at `version`.
fn count_limit(plan: ReadPlan, version: i32) -> Option<u64> {
    match plan {
        ReadPlan::CountThenItems {
            limit,
        } => Some(limit as u64),
        ReadPlan::CountThenItemsByVersion {
            limit,
            raised_limit,
            raised_from,
        } => Some(if version >= raised_from {
            raised_limit
        } else {
            limit
        } as u64),
        _ => None,
    }
}

/// Core's stream text for a base-sdk stream error, written out from Core's sources.
fn core_stream_text(err: &P2pDecodeError) -> Option<&'static str> {
    match err {
        P2pDecodeError::Consensus(DecodeError::Eof {
            ..
        }) => Some(END_OF_DATA),
        P2pDecodeError::Consensus(DecodeError::NonMinimalCompactSize {
            ..
        }) => Some(NON_CANONICAL),
        P2pDecodeError::Consensus(DecodeError::CompactSizeExceedsLimit {
            limit,
            ..
        }) => Some(if *limit as u64 == MAX_SIZE {
            SIZE_TOO_LARGE
        } else {
            END_OF_DATA
        }),
        _ => None,
    }
}

/// Core's `SanitizeString` with `SAFE_CHARS_DEFAULT` (`util/strencodings.cpp:16-25`).
fn sanitized(cmd: &CommandString) -> String {
    const SAFE: &[u8] =
        b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .,;-_/:?@()";
    cmd.as_bytes()
        .iter()
        .take_while(|&&b| b != 0)
        .filter(|b| SAFE.contains(b))
        .map(|&b| char::from(b))
        .collect()
}

/// The hook calls a hash-once `headers2` read makes, counted from base-sdk's own parse: one per
/// header after the first that reads with its previous hash left off, until the count is met or a
/// header fails to read. It counts calls; sharing the reader's parser, it is no oracle of that
/// parser (the matrix and its `Uncompress` are, D37).
fn derived_hashes(payload: &[u8]) -> usize {
    let mut data = payload;
    let Ok(count) = CompactSize::decode_size(&mut data) else {
        return 0;
    };
    let mut n = 0;
    for i in 0..u64::from(count) {
        let Ok(c) = CompressedHeader::decode(&mut data) else {
            break;
        };
        n += usize::from(i > 0 && c.is_prev_hash_compressed());
    }
    n
}

/// The typed readers a processor calls agree with the boundary: `read_headers2` (hash-once)
/// gives `decode`'s headers and bytes read, or refuses with it and leaves the payload as it was,
/// hashing exactly once per derived header (none over the limit); `read::<GovSyncRead>` reads
/// `decode`'s bytes, its filter fields the last nine of them.
fn readers_agree(
    cmd: &CommandString,
    payload: &[u8],
    ctx: &ProtoCtx,
    decoded: &Result<DecodedMsg, P2pDecodeError>,
) -> Result<(), Divergence> {
    match cmd.as_str() {
        "headers2" => {
            let mut calls = 0usize;
            let mut cursor = payload;
            let hooked = policy::read_headers2(&mut cursor, ctx, |h| {
                calls += 1;
                h.hash()
            });
            match (decoded, &hooked) {
                (Ok(d), Ok(hs)) => {
                    let P2pMsg::Headers2(h) = &d.msg else {
                        return Err(diverged("headers2 decoded as another message"));
                    };
                    if !h.block_headers().eq(hs.iter()) {
                        return Err(diverged("hash-once headers2 headers"));
                    }
                    if payload.len() - cursor.len() != d.consumed {
                        return Err(diverged("hash-once headers2 bytes read"));
                    }
                }
                // Either error leaves the payload as it was (Core's stream has moved on; the
                // message ends either way).
                (Err(_), Err(_)) if cursor.len() != payload.len() => {
                    return Err(diverged("hash-once headers2 moved the payload on an error"));
                }
                (Err(_), Err(_)) => {}
                _ => return Err(diverged("hash-once headers2 verdict")),
            }
            let expected = match hooked {
                Err(ReadError::TooMany {
                    ..
                }) => 0,
                _ => derived_hashes(payload),
            };
            if calls != expected {
                return Err(diverged("hash-once headers2 hook calls"));
            }
        }
        "govsync" => {
            let mut cursor = payload;
            match (decoded, policy::read::<GovSyncRead>(&mut cursor, ctx)) {
                (Ok(d), Ok(m)) => {
                    let Some(tail) = d
                        .consumed
                        .checked_sub(GOVSYNC_FILTER_TAIL)
                        .and_then(|start| payload.get(start..d.consumed))
                    else {
                        return Err(diverged("govsync read under its filter fields"));
                    };
                    let mut image = m.hash_funcs.to_le_bytes().to_vec();
                    image.extend(m.tweak.to_le_bytes());
                    image.push(m.flags);
                    if payload.len() - cursor.len() != d.consumed || image != tail {
                        return Err(diverged("typed govsync filter fields"));
                    }
                }
                (Err(_), Err(_)) => {}
                _ => return Err(diverged("typed govsync verdict")),
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    let mut input = Input(data);
    let cmd = command(&mut input);
    super::note_command(&cmd);
    let version = VERSIONS[usize::from(input.u8()) % VERSIONS.len()];
    let mode = input.u8();
    let bls = if mode & 1 == 0 {
        BlsCtx::LEGACY
    } else {
        BlsCtx::BASIC
    };
    // Mode bit 1: the matrix's well-formed sample (with the next byte in every bool), cut at
    // the next byte's share of its length when that is not 0, then the rest of the input.
    let built;
    let payload = match (mode & 2 != 0).then(|| matrix::sample(&cmd, version, input.u8())) {
        Some(Some(mut sample)) => {
            let cut = usize::from(input.u8());
            if cut != 0 {
                sample.truncate(sample.len() * cut / 256);
            }
            sample.extend_from_slice(input.0);
            built = sample;
            &built[..]
        }
        _ => input.0,
    };
    let ctx = ProtoCtx::new(ProtocolVersion(version), bls);

    let known = ALL_NET_MESSAGE_TYPES.contains(&cmd);
    if policy::is_known(&cmd) != known {
        return Err(diverged("is_known"));
    }
    let row = policy::lookup(&cmd);
    if row.is_some() != known || row.is_some_and(|r| r.cmd != cmd) {
        return Err(diverged("lookup"));
    }

    let decoded = policy::decode(&cmd, payload, &ctx);
    let Some(row) = row else {
        // An unknown command reaches no handler: Core logs it and reads nothing.
        return match decoded {
            Ok(d) if d.msg.is_unknown() => Ok(()),
            Err(P2pDecodeError::InvalidCommand {
                ..
            }) => Ok(()),
            _ => Err(diverged("unknown command decoded")),
        };
    };
    // The processor never decodes a row that reads nothing.
    if row.read == ReadPlan::Nothing {
        return Ok(());
    }
    if let Some(core) = matrix::judge(&cmd, payload, version)
        && !matrix::agrees(&cmd, &core, &decoded)
    {
        return Err(diverged(match (&core, &decoded) {
            (Ok(_), Err(_)) => "Core reads, the boundary refuses",
            (Err(CoreThrow::HandlerReturn(_)), _) => "Core's handler return",
            (Err(_), Ok(_)) => "Core refuses, the boundary reads",
            _ => "bytes read differ from Core's",
        }));
    }

    readers_agree(&cmd, payload, &ctx, &decoded)?;

    match decoded {
        Ok(d) => {
            if d.consumed > payload.len() {
                return Err(diverged("consumed past the payload"));
            }
            if row.trailing == Trailing::WholePayload && d.consumed != payload.len() {
                return Err(diverged("whole-payload row left bytes"));
            }
            // A stub keeps the raw payload for the processor that owns the read (`dss`).
            if !d.msg.is_stub()
                && let Some(limit) = count_limit(row.read, version)
                && leading_count(payload).is_some_and(|n| n > limit)
            {
                return Err(diverged("count over the limit decoded"));
            }
            if !d.msg.is_stub()
                && let Trailing::PayloadAtMost(max) = row.trailing
                && payload.len() > max
            {
                return Err(diverged("payload over the row's cap decoded"));
            }
            // Core's `Uncompress` (the matrix's model) builds the headers.
            if cmd == CommandString::HEADERS2
                && let P2pMsg::Headers2(h) = &d.msg
                && let Some(core) = matrix::headers2_values(payload, version)
                && !h.block_headers().map(header_bytes).eq(core.iter().map(matrix::Header::bytes))
            {
                return Err(diverged("headers2 values"));
            }
            let mut image = Vec::new();
            d.msg.encode_payload(&mut image);
            // `headers2` is rebuilt only when base-sdk's strict read refuses it (the boundary's
            // fallback); a payload it reads keeps its compressed headers.
            let lenient = LENIENT.contains(&cmd)
                && (cmd != CommandString::HEADERS2
                    || P2pMsg::decode_with_policy(&cmd, payload, &ctx).is_err());
            if !lenient {
                if image != payload[..d.consumed] {
                    return Err(diverged("re-encoding differs from the bytes read"));
                }
                return Ok(());
            }
            match policy::decode(&cmd, &image, &ctx) {
                Ok(r) if r.msg == d.msg && r.consumed == image.len() => Ok(()),
                _ => Err(diverged("re-encoding reads as another message")),
            }
        }
        Err(e) => {
            if row.on_throw == ThrowOutcome::NeverThrows {
                return Err(diverged("never-throws row refused"));
            }
            match &e {
                P2pDecodeError::TooManyEntries {
                    max,
                    ..
                } if count_limit(row.read, version).is_some_and(|limit| limit != *max) => {
                    return Err(diverged("count refusal names another limit"));
                }
                P2pDecodeError::HandlerPayloadTooLarge {
                    max,
                    ..
                } if matches!(row.trailing, Trailing::PayloadAtMost(m) if m != *max) => {
                    return Err(diverged("payload refusal names another cap"));
                }
                _ => {}
            }
            let throw = Throw::from_decode(&e);
            if let Some(text) = core_stream_text(&e)
                && throw.what != text
            {
                return Err(diverged("throw text"));
            }
            if !throw.what.ends_with(": iostream error") {
                return Err(diverged("throw suffix"));
            }
            let expected = format!(
                "ProcessMessages({}, {} bytes): Exception '{}' ({}) caught",
                sanitized(&cmd),
                payload.len(),
                throw.what,
                throw.type_name
            );
            if throw.log_line(&cmd, payload.len()) != expected {
                return Err(diverged("log line"));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command, version and mode, over the matrix's samples (cut, padded and with odd
    /// bools) and raw bytes: the boundary keeps every check.
    #[test]
    fn generated_inputs_pass() {
        let mut rng = matrix::Rng(0x5eed);
        let mut failures = Vec::new();
        for (i, cmd) in ALL_NET_MESSAGE_TYPES.iter().enumerate() {
            for _ in 0..400 {
                let r = rng.next();
                let tail_len = (r % 48) as usize;
                let mut input = vec![
                    i as u8,
                    (r >> 8) as u8,
                    (r >> 16) as u8,
                    [0, 2, 255, 1][((r >> 24) % 4) as usize],
                    if (r >> 26).is_multiple_of(2) {
                        0
                    } else {
                        (r >> 32) as u8
                    },
                ];
                input.extend((0..tail_len).map(|_| rng.next() as u8));
                if let Err(d) = check(&input) {
                    failures.push(format!("{}: {}", cmd.as_str(), d.field));
                }
            }
        }
        failures.sort();
        failures.dedup();
        assert!(failures.is_empty(), "{failures:#?}");
    }
}
