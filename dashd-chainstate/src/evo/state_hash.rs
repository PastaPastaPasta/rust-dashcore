//! Hook H11's values (O8 §5.2): the evo keys of a block's `C` record, built from what the
//! attempt staged for the block (CH7a-5b X2). Nothing here reads a store: an attempt's target, the
//! active layer or VerifyDB's scratch one, is whatever H4 staged into the attempt's pending
//! transaction, so a scratch reconnect hashes its own list, pool and signals.

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::write_commitment;
use dash_dml::codec::canonical_mn_list_hash;
use dash_dml::creditpool::{CreditPool, PoolState};
use dash_dml::store::MinedCommitmentRecord;
use dash_dml::{EvoDelta, MnList};
use dash_num::Hash256;
use dash_types::Numeric as _;

use crate::chain::Fatal;
use crate::connect::{CreditPoolDigest, EvoDigest};

/// The eras H11's keys follow, each the deployment active at the block, `DeploymentActiveAt`
/// (`sd.cpp:188`; patch `validation.cpp:2516`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DigestEras {
    /// DIP0003: `mnl` and `qc`.
    pub dip3: bool,
    /// V20: `cp` and `ehf`.
    pub v20: bool,
}

/// The `C` record's evo keys for a connected block, from the attempt's staging: `list`, the new
/// list H4 left for the commit (`mnlist_updates.new_list`), and `delta`, its staged rows. A key
/// outside its era is `None`. A key in its era whose staging is missing, or a pool that is Core's
/// exception, is a [`Fatal`]: Core's patch throws there (patch `v:2510-2520`), never writes a
/// `null` in era.
// Core: evo/snapshot.cpp:150-155, llmq/blockprocessor.cpp:432 (patch :437), evo/mnhftx.cpp:210,
// statedigest.cpp:161-188
pub fn evo_digest(
    eras: DigestEras,
    list: Option<&MnList>,
    delta: Option<&EvoDelta>,
) -> Result<EvoDigest, Fatal> {
    if !eras.dip3 {
        return Ok(EvoDigest::default());
    }
    let list = list.ok_or_else(|| Fatal::new("H11: no new list staged for a DIP0003 block"))?;
    let delta = delta.ok_or_else(|| Fatal::new("H11: no evo rows staged for a DIP0003 block"))?;
    let mut fields = EvoDigest {
        mnl: Some(canonical_mn_list_hash(list)),
        qc: Some(qc_rows(&delta.mined_commitments)),
        cp: None,
        ehf: None,
    };
    if eras.v20 {
        let pool = delta
            .credit_pool
            .as_deref()
            .ok_or_else(|| Fatal::new("H11: no credit pool staged for a V20 block"))?;
        fields.cp = Some(pool_digest(pool)?);
        let signals = delta
            .mnhf_signals
            .as_ref()
            .ok_or_else(|| Fatal::new("H11: no MNHF signals row staged for a V20 block"))?;
        fields.ehf = Some(signals.iter().collect());
    }
    Ok(fields)
}

/// `qc`'s rows (patch `sd.cpp:161-169`): one `(type, quorumHash, SerializeHash(qc))` per mined
/// commitment, sorted as `std::sort` sorts the tuple, by type and then by each hash's hex. A hex
/// string orders as its display bytes, the reverse of [`Hash256`]'s wire order. Null commitments
/// are never staged (`bp:270`).
pub fn qc_rows(mined: &[MinedCommitmentRecord]) -> Vec<(u8, Hash256, Hash256)> {
    let mut rows: Vec<_> = mined
        .iter()
        .map(|record| {
            let mut bytes = Vec::new();
            write_commitment(&record.commitment, &mut bytes);
            let quorum = Hash256::from(*record.quorum_hash.as_bytes());
            (record.llmq_type.to_base(), quorum, Hash256::from(sha256d(&bytes)))
        })
        .collect();
    rows.sort_by_key(|(t, quorum, hash)| (*t, display(quorum), display(hash)));
    rows
}

/// `cp` (patch `v:2513-2520`): the pool `GetCreditPool(pindex)` returns after the block, or its
/// exception as the [`Fatal`] the patch's `Guard` turns into `AbortNode`.
pub fn pool_digest(pool: &PoolState) -> Result<CreditPoolDigest, Fatal> {
    match pool {
        Ok(pool) => Ok(credit_pool_digest(pool)),
        Err(e) => Err(Fatal::new(format!("H11: GetCreditPool: {e}"))),
    }
}

/// O8 §5.2's `cp` fields of `pool` (`evo/creditpool.h:39-45`): `indexes_hash` is
/// `sha256d(serialize(indexes))`, Core's `CRangesSet` serialization.
pub fn credit_pool_digest(pool: &CreditPool) -> CreditPoolDigest {
    let mut indexes = Vec::new();
    pool.indexes.encode(&mut indexes);
    CreditPoolDigest {
        locked: pool.locked,
        current_limit: pool.current_limit,
        lately_unlocked: pool.lately_unlocked,
        indexes_hash: Hash256::from(sha256d(&indexes)),
    }
}

/// A hash's display bytes: its wire bytes reversed, the order its hex prints in.
fn display(h: &Hash256) -> [u8; 32] {
    let mut bytes = *h.as_bytes();
    bytes.reverse();
    bytes
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_consensus::deployments::EhfSignals;
    use dash_dml::creditpool::PoolError;
    use dash_dml::testkit::numbered;
    use dash_params::LlmqType;
    use dash_primitives::{BlockHash, QuorumHash};

    use super::*;
    use crate::evo::testkit::commitment;

    const DIP3: DigestEras = DigestEras {
        dip3: true,
        v20: false,
    };
    const V20: DigestEras = DigestEras {
        dip3: true,
        v20: true,
    };

    fn record(llmq_type: LlmqType, wire: [u8; 32]) -> MinedCommitmentRecord {
        let quorum_hash = QuorumHash::from(wire);
        MinedCommitmentRecord {
            llmq_type,
            quorum_hash,
            base_height: 1,
            rotation_index: None,
            commitment: Arc::new(commitment(llmq_type, quorum_hash, 1)),
        }
    }

    fn list() -> MnList {
        let mut list = MnList::default_pre_dip3();
        list.try_add(numbered(0), true).expect("adds");
        list.with_block_hash(BlockHash::from([7; 32])).with_height(48)
    }

    fn pool() -> CreditPool {
        let mut indexes = dash_dml::creditpool::RangesSet::default();
        indexes.add(3);
        CreditPool {
            locked: 5,
            current_limit: 4,
            lately_unlocked: 1,
            indexes,
        }
    }

    /// A V20 block's staging: one mined commitment, the pool and a signal for bit 10.
    fn delta() -> EvoDelta {
        let mut signals = EhfSignals::new();
        signals.insert(10, 188);
        EvoDelta {
            block: BlockHash::from([7; 32]),
            height: 48,
            dml: None,
            mined_commitments: vec![record(LlmqType::LlmqTest, [9; 32])],
            credit_data: None,
            credit_pool: Some(Arc::new(Ok(pool()))),
            mnhf_signals: Some(signals),
            erased_mined: Vec::new(),
            evo_tip: None,
        }
    }

    /// `qc`'s rows sort as Core's tuple of hex strings: by type, then by the quorum hash's
    /// display bytes. `a` precedes `b` in wire order (its first byte is lower) but follows it in
    /// display order (its last byte, printed first, is higher); the lower type comes first
    /// whatever its hash.
    #[test]
    fn qc_rows_order_by_type_then_display_hex() {
        let mut a = [0x11; 32];
        (a[0], a[31]) = (0x01, 0xff);
        let mut b = [0x11; 32];
        (b[0], b[31]) = (0x02, 0x00);
        let low = LlmqType::Llmq50_60;
        let high = LlmqType::LlmqTest;
        assert!(low.to_base() < high.to_base());
        let mined = [record(high, a), record(high, b), record(low, [0xee; 32])];
        let rows = qc_rows(&mined);
        let order: Vec<_> = rows.iter().map(|(t, q, _)| (*t, *q.as_bytes())).collect();
        assert_eq!(order, [(low.to_base(), [0xee; 32]), (high.to_base(), b), (high.to_base(), a)]);
        let keys: Vec<_> = rows.iter().map(|(t, q, _)| (*t, display(q))).collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]), "strictly ascending: {keys:?}");
        // The commitment hash is `SerializeHash(qc)`: sha256d of the commitment's own bytes.
        let mut bytes = Vec::new();
        write_commitment(&mined[2].commitment, &mut bytes);
        assert_eq!(rows[0].2, Hash256::from(sha256d(&bytes)));
    }

    #[test]
    fn evo_digest_without_a_dip3_list_is_fatal() {
        let e = evo_digest(DIP3, None, Some(&delta())).expect_err("no list");
        assert!(e.0.contains("no new list"), "{e:?}");
    }

    #[test]
    fn evo_digest_without_a_v20_pool_is_fatal() {
        let d = EvoDelta {
            credit_pool: None,
            ..delta()
        };
        let e = evo_digest(V20, Some(&list()), Some(&d)).expect_err("no pool");
        assert!(e.0.contains("no credit pool"), "{e:?}");
    }

    #[test]
    fn evo_digest_without_a_v20_signals_row_is_fatal() {
        let d = EvoDelta {
            mnhf_signals: None,
            ..delta()
        };
        let e = evo_digest(V20, Some(&list()), Some(&d)).expect_err("no signals");
        assert!(e.0.contains("no MNHF signals row"), "{e:?}");
    }

    #[test]
    fn evo_digest_of_a_pool_error_is_fatal() {
        let d = EvoDelta {
            credit_pool: Some(Arc::new(Err(PoolError::IndexDuplicated))),
            ..delta()
        };
        let e = evo_digest(V20, Some(&list()), Some(&d)).expect_err("the pool's exception");
        assert!(e.0.contains("GetCreditPool"), "{e:?}");
    }

    /// Each era's keys alone: none before DIP0003, even with nothing staged; `mnl` and `qc` from
    /// it, `cp` and `ehf` `None` until V20 even with a pool staged; all four in V20.
    #[test]
    fn evo_digest_outside_its_eras_is_null() {
        assert_eq!(evo_digest(DigestEras::default(), None, None), Ok(EvoDigest::default()));
        let staged = delta();
        let dip3 = evo_digest(DIP3, Some(&list()), Some(&staged)).expect("DIP0003");
        assert_eq!(dip3.mnl, Some(canonical_mn_list_hash(&list())));
        assert_eq!(dip3.qc, Some(qc_rows(&staged.mined_commitments)));
        assert_eq!((dip3.cp, dip3.ehf), (None, None));
        let v20 = evo_digest(V20, Some(&list()), Some(&staged)).expect("V20");
        assert_eq!((v20.mnl, v20.qc), (dip3.mnl, dip3.qc));
        assert_eq!(v20.cp, Some(credit_pool_digest(&pool())));
        assert_eq!(v20.ehf, Some(vec![(10, 188)]));
        let mut indexes = Vec::new();
        pool().indexes.encode(&mut indexes);
        let cp = v20.cp.expect("cp");
        assert_eq!((cp.locked, cp.current_limit, cp.lately_unlocked), (5, 4, 1));
        assert_eq!(cp.indexes_hash, Hash256::from(sha256d(&indexes)));
    }
}
