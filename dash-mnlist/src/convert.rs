//! The payload types' conversions to `dash-evo`'s (D3): base-sdk's list entry, partial merkle tree
//! and snapshot to the SML entry, the CbTx proof tree and LQ1's row.

use dash_consensus::hash::sha256d;
use dash_evo::cbtx::PartialMerkleTree;
use dash_evo::llmq::QuorumSnapshot;
use dash_evo::sml::SmlEntry;
use dash_evo::types::{ProTxHash, RawMnType};
use dash_p2p_core::{
    PartialMerkleTree as WireTree, QuorumSnapshot as WireSnapshot, SimplifiedMnListEntry,
};
use dash_primitives::{Commitment, DynBitset, PlatformNodeId};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

/// Core's `CSimplifiedMNListEntry` as the SML holds it. Every field Core hashes carries over; an
/// absent platform port or node id is Core's zero (`CalcHash` writes them only when the version and
/// type carry them).
pub(crate) fn sml_entry(e: &SimplifiedMnListEntry) -> SmlEntry {
    SmlEntry {
        version: e.version,
        pro_reg_tx_hash: ProTxHash::from(*e.pro_reg_tx_hash.as_bytes()),
        confirmed_hash: e.confirmed_hash,
        net_info: e.net_info.clone(),
        operator_key: *e.operator_key.as_bytes(),
        voting_key_id: *e.voting_key_id.as_bytes(),
        is_valid: e.is_valid,
        mn_type: RawMnType(e.mn_type.to_base()),
        platform_http_port: e.platform_http_port.unwrap_or(0),
        platform_node_id: e.platform_node_id.unwrap_or(PlatformNodeId::from([0; 20])),
    }
}

/// The CbTx proof as `dash-evo` reads it: the same three fields.
pub(crate) fn proof_tree(t: &WireTree) -> PartialMerkleTree {
    PartialMerkleTree {
        transactions: t.transactions,
        hashes: t.hashes.iter().map(|h| h.to_lendian()).collect(),
        flags: t.flag_bytes.clone(),
    }
}

/// A `qrinfo` row as LQ1 reads it (`CQuorumSnapshot`).
pub(crate) fn snapshot(s: &WireSnapshot) -> QuorumSnapshot {
    QuorumSnapshot {
        active_quorum_members: bits(&s.active_quorum_members),
        skip_list_mode: s.skip_list_mode.to_base(),
        skip_list: s.skip_list.clone(),
    }
}

fn bits(b: &DynBitset) -> Vec<bool> {
    (0..b.num_bits).map(|i| b.get(i) == Some(true)).collect()
}

/// `::SerializeHash(qc)`: the commitment's leaf in `merkleRootQuorums` (`evo/cbtx.cpp` rc.3:107).
pub(crate) fn commitment_hash(qc: &Commitment) -> [u8; 32] {
    let mut buf = Vec::new();
    qc.encode(&mut buf);
    sha256d(&buf)
}
