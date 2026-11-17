//! P3's codec half (SB5 §10.3): `hdr` rows round-trip, and the leaf deltas of `step_rows`.

use proptest::prelude::*;

use super::*;

fn record() -> impl Strategy<Value = HeaderRecord> {
    (
        (any::<u32>(), any::<[u8; 32]>(), any::<u32>(), any::<[u8; 80]>(), any::<Option<u32>>()),
        (any::<u32>(), any::<[u8; 32]>(), any::<u32>(), any::<bool>()),
    )
        .prop_map(|((id, hash, height, header, skip), (status, work, time_max, context))| {
            HeaderRecord {
                id: BlockId(id),
                hash: BlockHash::from(hash),
                height,
                header: BlockHeader::decode(&mut &header[..]).expect("80 bytes decode"),
                skip: skip.map(BlockId),
                status: Status(status),
                chain_work: Work::from_bendian(work),
                time_max,
                kind: if context {
                    RecordKind::Context
                } else {
                    RecordKind::Accepted
                },
            }
        })
}

proptest! {
    /// `from_row ∘ to_row` is the identity, and the row's columns are the record's bytes: the
    /// 80-byte header as decoded, the work big-endian.
    #[test]
    fn row_round_trip(rec in record()) {
        let row = rec.to_row();
        prop_assert_eq!(HeaderRecord::from_row(&row), Ok(rec.clone()));
        prop_assert_eq!(row.header, header_bytes(&rec.header));
        prop_assert_eq!(row.chain_work, rec.chain_work.to_bendian());
        prop_assert_eq!(row.kind, u8::from(rec.kind == RecordKind::Context));
    }

    /// Every 80-byte header column decodes and re-encodes to itself, so `to_row ∘ from_row` is
    /// the identity on rows with a valid kind.
    #[test]
    fn row_bytes_round_trip(header in any::<[u8; 80]>(), work in any::<[u8; 32]>(), kind in 0u8..2) {
        let row = HeaderRow { header, skip: None, chain_work: work, kind, ..rec_row() };
        prop_assert_eq!(HeaderRecord::from_row(&row).map(|r| r.to_row()), Ok(row));
    }

    #[test]
    fn row_bad_kind(kind in 2u8..) {
        let row = HeaderRecord::from_row(&HeaderRow { kind, ..rec_row() });
        prop_assert_eq!(row, Err(RowError::Kind(kind)));
    }
}

fn rec_row() -> HeaderRow {
    HeaderRow {
        hash: [1; 32],
        id: 1,
        height: 1,
        header: [0; 80],
        skip: Some(0),
        status: 0,
        chain_work: [0; 32],
        time_max: 0,
        kind: 0,
    }
}

/// An accepted record adds itself to `hdr_leaf` and removes its parent; a context record
/// touches neither.
#[test]
fn step_rows_leaf_deltas() {
    let rows = |r: &HeaderRecord, prev| r.clone().step_rows(prev).collect::<Vec<_>>();
    let accepted = HeaderRecord::from_row(&rec_row()).unwrap();
    let record = StepRow::Record(accepted.clone());
    let (add, remove) = (StepRow::LeafAdd(BlockId(1)), StepRow::LeafRemove(BlockId(0)));
    assert_eq!(rows(&accepted, Some(BlockId(0))), [record.clone(), add.clone(), remove]);
    assert_eq!(rows(&accepted, None), [record, add]);
    let context = HeaderRecord {
        kind: RecordKind::Context,
        ..accepted
    };
    assert_eq!(rows(&context, Some(BlockId(0))), [StepRow::Record(context.clone())]);
}
