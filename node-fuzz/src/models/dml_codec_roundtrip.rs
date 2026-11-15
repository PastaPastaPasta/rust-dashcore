//! `dml_codec_roundtrip` (EV2 §6.2): `dash_dml::codec`'s readers on arbitrary bytes. Each is a
//! total function (a panic or an unbounded allocation is libFuzzer's crash), and what it accepts
//! re-encodes to exactly the bytes it read: the canonical readers refuse every non-canonical
//! encoding (Core's `CheckCanonicalEncoding`, strict order), our records are canonical bodies
//! with a canonical trailer, and an accepted list keeps its invariants. Core's own verdicts on
//! these readers are the Core-judged vectors in `dash-dml/tests/data/codec/` (the targets'
//! seeds); the O2a-EV oracle entry points will make this an L1 target.
//!
//! The first byte picks the reader (modulo 6): the canonical list, the canonical diff, a DML
//! record, Core's evodb list or diff, or a snapshot record and a diff record applied to it (the
//! collision trailer's restore: the result keeps its invariants and its records round-trip). The
//! network-info rule is EV1's port of Core's `Validate()` on mainnet (`core_net_info`).

use dash_dml::codec::{
    DmlRecord, canonical_mn_list_hash, core_disk, decode_canonical_diff, decode_canonical_list,
    decode_record, encode_canonical_diff, encode_canonical_list, encode_canonical_list_unvalidated,
    encode_diff_record, encode_snapshot_record,
};
use dash_dml::{MnList, SameRecord};
use node_l1::Divergence;
use node_l1::dml::core_net_info;

fn ensure(ok: bool, field: &'static str) -> Result<(), Divergence> {
    if ok {
        Ok(())
    } else {
        Err(Divergence {
            field,
        })
    }
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    let Some((mode, body)) = data.split_first() else {
        return Ok(());
    };
    match mode % 6 {
        0 => canonical_list(body),
        1 => canonical_diff(body),
        2 => record(body),
        3 => evodb_list(body),
        4 => evodb_diff(body),
        _ => apply(body),
    }
}

fn consumed<'a>(body: &'a [u8], rest: &[u8]) -> &'a [u8] {
    &body[..body.len() - rest.len()]
}

/// An accepted list keeps its invariants.
fn sound(list: &MnList) -> Result<(), Divergence> {
    ensure(list.check_invariants().is_ok(), "decoded list invariants")
}

fn canonical_list(body: &[u8]) -> Result<(), Divergence> {
    let mut rest = body;
    let Ok(list) = decode_canonical_list(&mut rest, core_net_info()) else {
        return Ok(());
    };
    sound(&list)?;
    let mut again = Vec::new();
    encode_canonical_list_unvalidated(&list, &mut again);
    ensure(again == consumed(body, rest), "canonical list re-encoding")?;
    let mut validated = Vec::new();
    ensure(
        encode_canonical_list(&list, core_net_info(), &mut validated).is_ok(),
        "storage encoder refused",
    )?;
    ensure(validated == again, "storage encoder bytes")?;
    let copy = decode_canonical_list(&mut again.as_slice(), core_net_info());
    ensure(
        copy.is_ok_and(|c| canonical_mn_list_hash(&c) == canonical_mn_list_hash(&list)),
        "canonical list second decode",
    )
}

fn canonical_diff(body: &[u8]) -> Result<(), Divergence> {
    let mut rest = body;
    let Ok(diff) = decode_canonical_diff(&mut rest, &mut { usize::MAX }, core_net_info()) else {
        return Ok(());
    };
    let mut again = Vec::new();
    encode_canonical_diff(&diff, &mut again);
    ensure(again == consumed(body, rest), "canonical diff re-encoding")
}

fn record(body: &[u8]) -> Result<(), Divergence> {
    let Ok(record) = decode_record(body, core_net_info()) else {
        return Ok(());
    };
    let again = match &record {
        DmlRecord::Snapshot(list) => {
            sound(list)?;
            encode_snapshot_record(list, core_net_info())
        }
        DmlRecord::Diff(diff) => encode_diff_record(diff, core_net_info()),
    };
    ensure(again.is_ok_and(|bytes| bytes.as_bytes() == body), "record re-encoding")
}

fn evodb_list(body: &[u8]) -> Result<(), Divergence> {
    let Ok(list) = core_disk::decode_list_s3(&mut &body[..]) else {
        return Ok(());
    };
    sound(&list)?;
    // The canonical form of an imported list reads back as the same list, wherever the storage
    // encoder takes it.
    let mut canonical = Vec::new();
    if encode_canonical_list(&list, core_net_info(), &mut canonical).is_ok() {
        let copy = decode_canonical_list(&mut canonical.as_slice(), core_net_info());
        ensure(copy.is_ok_and(|c| c.is_equal(&list)), "imported list canonical round trip")?;
    }
    Ok(())
}

fn evodb_diff(body: &[u8]) -> Result<(), Divergence> {
    let Ok(diff) = core_disk::decode_diff_d4(&mut &body[..]) else {
        return Ok(());
    };
    // Only that the encoder takes whatever the importer reads.
    encode_canonical_diff(&diff, &mut Vec::new());
    Ok(())
}

/// A snapshot record (its length first, a little-endian u16) and a diff record: where the diff
/// applies, the list keeps its invariants, its Core order is a permutation of its keys, and its
/// snapshot record reads back as the same list in the same order or is refused with a canonical
/// reader's text. The writer, like Core's `EvoSnapshot::Validate`, checks each entry's bytes,
/// not that the list reads back, so a refusal is Core's verdict to judge (an L1 follow-up).
fn apply(body: &[u8]) -> Result<(), Divergence> {
    let Some((len, rest)) = body.split_first_chunk::<2>() else {
        return Ok(());
    };
    let Some((snapshot, diff)) = rest.split_at_checked(usize::from(u16::from_le_bytes(*len)))
    else {
        return Ok(());
    };
    let (Ok(DmlRecord::Snapshot(list)), Ok(DmlRecord::Diff(diff))) =
        (decode_record(snapshot, core_net_info()), decode_record(diff, core_net_info()))
    else {
        return Ok(());
    };
    let mut list = (*list).clone();
    let block = *list.block_hash();
    if list.apply_record(block, &diff).is_err() {
        return Ok(());
    }
    sound(&list)?;
    let order = list.core_order();
    let mut sorted = order.to_vec();
    sorted.sort();
    ensure(
        sorted.iter().eq(list.iter().map(|dmn| &dmn.pro_tx_hash)),
        "applied list core order is not a permutation of its keys",
    )?;
    let Ok(bytes) = encode_snapshot_record(&list, core_net_info()) else {
        return Ok(());
    };
    match decode_record(bytes.as_bytes(), core_net_info()) {
        Ok(DmlRecord::Snapshot(copy)) => ensure(
            (*copy).same_record(&list) && copy.core_order() == order,
            "applied list snapshot round trip",
        ),
        Ok(_) => ensure(false, "applied list snapshot record kind"),
        Err(e) => ensure(
            ["noncanonical", "invalid canonical", "oversized canonical", "canonical MN"]
                .iter()
                .any(|p| e.0.starts_with(p)),
            "applied list snapshot refusal",
        ),
    }
}
