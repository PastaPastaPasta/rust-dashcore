//! Error classes common to Core's exceptions and base-sdk's decode errors.
//! `predict` reports them; the differentials compare verdicts (DEC-98), and
//! `bs7_diff_headers2.py` uses `strict` to tell base-sdk's refusal of a
//! header Core's `Compress` never builds (BSX D-07).

use dash_p2p_core::P2pDecodeError;
use dash_types::MAX_SER_SIZE;
use dash_types::codec::DecodeError;

/// The class of an exception Core's reader threw, from its text.
pub fn core(what: &str) -> &'static str {
    const CLASSES: [(&str, &str); 8] = [
        ("end of data", "eof"),
        ("non-canonical ReadCompactSize()", "noncanonical"),
        ("ReadCompactSize(): size too large", "size"),
        ("differential value overflow", "overflow16"),
        ("CompactSize exceeds limit of type", "type_range"),
        ("indexes overflowed 16 bits", "total16"),
        ("Vector length limit exceeded", "length_limit"),
        ("String length limit exceeded", "length_limit"),
    ];
    CLASSES.iter().find(|(text, _)| what.contains(text)).map_or("other", |(_, class)| class)
}

/// The class of base-sdk's error for `cmd`, in Core's terms. The fixed
/// limits are the ones each command's fields carry; any other error is
/// "other".
pub fn ours(cmd: &str, e: &P2pDecodeError) -> &'static str {
    match e {
        P2pDecodeError::Consensus(d) => decode(cmd, d),
        P2pDecodeError::TooManyEntries { .. } => "too_many",
        P2pDecodeError::PayloadTooLarge { .. } => "too_large",
        P2pDecodeError::ContentsTooLarge { .. } => "too_large",
        P2pDecodeError::InvalidCommand { .. } => "invalid_command",
        _ => "other",
    }
}

fn decode(cmd: &str, e: &DecodeError) -> &'static str {
    match (cmd, e) {
        (_, DecodeError::Eof { .. }) => "eof",
        (_, DecodeError::NonMinimalCompactSize { .. }) => "noncanonical",
        (_, DecodeError::CompactSizeExceedsLimit { limit, .. }) if *limit == MAX_SER_SIZE => "size",
        ("getblocktxn", DecodeError::CompactSizeExceedsLimit { limit: 65535, .. }) => "overflow16",
        ("cmpctblock", DecodeError::CompactSizeExceedsLimit { limit: 65535, .. }) => "type_range",
        ("cmpctblock", DecodeError::BadLen { .. }) => "total16",
        ("filterload", DecodeError::CompactSizeExceedsLimit { limit: 36000, .. }) => "length_limit",
        ("filteradd", DecodeError::CompactSizeExceedsLimit { limit: 520, .. }) => "length_limit",
        // A header Core's `Compress` never builds, which base-sdk refuses.
        ("headers2", DecodeError::InvalidValue { .. }) => "strict",
        _ => "other",
    }
}
