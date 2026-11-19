//! `CSimplifiedMNListDiff::ToJson` and `CSimplifiedMNListEntry::ToJson`: `protx diff`'s reply.

use dash_num::Hash256;
use dash_primitives::BlockHash;

use super::evo::{
    AddressMode, EvoNet, MN_TYPE_EVO, PROTX_EXT_ADDR, commitment_to_json, lazy_key, payout_list,
    script_address, share_list, uint_hex,
};
use super::{CheckFailed, TxJsonParams};
use crate::UniValue;
use crate::descriptor::hex;
use crate::evo::{SmlDiffFacts, SmlEntryFacts};
use crate::key_io::{Destination, encode_destination};

/// `CSimplifiedMNListDiff::CURRENT_VERSION`, the only `nVersion` Core builds.
// Core: evo/smldiff.h:47
const DIFF_VERSION: i64 = 1;

// Core: evo/core_write.cpp:316-376 CSimplifiedMNListDiff::ToJson
pub(crate) fn sml_diff_to_json(
    diff: &SmlDiffFacts,
    extended: bool,
    p: &TxJsonParams,
) -> Result<UniValue, CheckFailed> {
    let mut obj = UniValue::new_object();
    obj.push_kv_end("nVersion", DIFF_VERSION.into());
    obj.push_kv_end("baseBlockHash", diff.base_hash_echo.to_string().into());
    obj.push_kv_end("blockHash", diff.block_hash.to_string().into());
    obj.push_kv_end("cbTxMerkleTree", diff.merkle_tree_hex.clone().into());
    obj.push_kv_end("cbTx", diff.cbtx_hex.clone().into());
    obj.push_kv_end(
        "deletedMNs",
        UniValue::Arr(diff.deleted.iter().map(|h| h.to_string().into()).collect()),
    );
    let entries = diff.entries.iter().map(|e| sml_entry_to_json(e, extended, p));
    obj.push_kv_end("mnList", UniValue::Arr(entries.collect::<Result<_, _>>()?));
    let deleted_quorums = diff.deleted_quorums.iter().map(|(llmq_type, hash)| {
        let mut e = UniValue::new_object();
        e.push_kv_end("llmqType", i64::from(*llmq_type).into());
        e.push_kv_end("quorumHash", BlockHash::from(*hash).to_string().into());
        e
    });
    obj.push_kv_end("deletedQuorums", UniValue::Arr(deleted_quorums.collect()));
    obj.push_kv_end(
        "newQuorums",
        UniValue::Arr(diff.new_quorums.iter().map(commitment_to_json).collect()),
    );
    if let Some((mn_list_root, quorums_root)) = &diff.roots {
        obj.push_kv_end("merkleRootMNList", uint256(mn_list_root));
        if let Some(root) = quorums_root {
            obj.push_kv_end("merkleRootQuorums", uint256(root));
        }
    }
    let cl_sigs = diff.cl_sigs.iter().map(|(sig, indexes)| {
        let indexes = indexes.iter().map(|i| i64::from(*i).into()).collect();
        UniValue::Obj(vec![(hex(sig), UniValue::Arr(indexes))])
    });
    obj.push_kv_end("quorumsCLSigs", UniValue::Arr(cl_sigs.collect()));
    Ok(obj)
}

/// `uint256::ToString()`.
fn uint256(hash: &Hash256) -> UniValue {
    BlockHash::from(*hash.as_bytes()).to_string().into()
}

/// The entry's own fields, then with `extended` the payout fields of its masternode, which the
/// wire entry does not carry.
// Core: evo/core_write.cpp:379-414 CSimplifiedMNListEntry::ToJson
pub(crate) fn sml_entry_to_json(
    facts: &SmlEntryFacts,
    extended: bool,
    p: &TxJsonParams,
) -> Result<UniValue, CheckFailed> {
    let (e, cold) = (&facts.entry, &facts.dmn.cold);
    let net = EvoNet::new(&e.net_info, e.mn_type.0, None, Some(e.platform_http_port));
    let mut obj = UniValue::new_object();
    obj.push_kv_end("nVersion", i64::from(e.version).into());
    obj.push_kv_end("nType", i64::from(e.mn_type.0).into());
    obj.push_kv_end("proRegTxHash", e.pro_reg_tx_hash.to_string().into());
    obj.push_kv_end("confirmedHash", e.confirmed_hash.to_string().into());
    if p.service {
        obj.push_kv_end("service", net.service());
    }
    obj.push_kv_end("addresses", net.addresses(AddressMode::Sml));
    obj.push_kv_end("pubKeyOperator", lazy_key(&cold.pub_key_operator));
    obj.push_kv_end(
        "votingAddress",
        encode_destination(&Destination::PkHash(e.voting_key_id), &p.key_io).into(),
    );
    obj.push_kv_end("isValid", e.is_valid.into());
    if e.mn_type.0 == MN_TYPE_EVO {
        if p.service {
            obj.push_kv_end("platformHTTPPort", net.platform_port(false)?);
        }
        obj.push_kv_end("platformNodeID", uint_hex(&e.platform_node_id));
    }
    if extended {
        if e.version >= PROTX_EXT_ADDR {
            obj.push_kv_end("payouts", payout_list(&cold.payouts, p));
        } else if let Some(address) = script_address(&cold.script_payout, p) {
            obj.push_kv_end("payoutAddress", address);
        }
        if !cold.shares.is_empty() {
            obj.push_kv_end("shares", share_list(&cold.shares, p));
        }
        if let Some(address) = script_address(&cold.script_operator_payout, p) {
            obj.push_kv_end("operatorPayoutAddress", address);
        }
    }
    Ok(obj)
}
