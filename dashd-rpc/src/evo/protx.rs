use dash_dml::{Dmn, ProTxHash};
use dash_primitives::BlockHash;

use super::diff::{bad_block_text, block_arg};
use super::{BlockFailure, EvoError, MnListAt, MnListRequest, MnListType, NodeEvo};
use crate::UniValue;
use crate::blockchain::parse_hash_v;
use crate::core_write::dmn::{dmn_to_json, meta_to_json, state_diff_to_json};
use crate::core_write::{CheckFailed, TxJsonParams};
use crate::protocol::{
    RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RpcError,
};
use crate::request::{JsonRpcRequest, get_int_i32, param, parse_bool_v};
use crate::server::NodeControl;

// Core: rpc/evo.cpp:1646-1769
pub(super) fn list(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    p: &TxJsonParams,
    control: &dyn NodeControl,
) -> Result<UniValue, RpcError> {
    let value = param(&request.params, 0);
    let kind = if value.is_null() {
        "registered"
    } else {
        value.get_str()?
    };
    let mn_type = match kind {
        "wallet" => {
            return Err(RpcError::Runtime(
                "\"protx list wallet\" not supported when wallet is disabled".into(),
            ));
        }
        "registered" => MnListType::Registered,
        "valid" => MnListType::Valid,
        "evo" => MnListType::Evo,
        _ => return Err(RpcError::new(RPC_INVALID_PARAMETER, "invalid type specified")),
    };
    let detailed = param(&request.params, 1);
    let detailed =
        !detailed.is_null() && parse_bool_v(detailed, "detailed", evo.permissive_bool())?;
    let height = param(&request.params, 2);
    let height = if height.is_null() {
        None
    } else {
        Some(get_int_i32(height)?)
    };
    let at = evo
        .mn_list(MnListRequest::Height {
            height,
            mn_type,
            detailed,
        })
        .map_err(info_error)?;
    let mut ret = UniValue::new_array();
    for pro_tx_hash in at.list.core_order().iter() {
        let Some(dmn) = at.list.get(pro_tx_hash) else {
            continue;
        };
        if !mn_type.includes(dmn) {
            continue;
        }
        ret.push_back(list_entry(dmn, detailed, &at, None, p, control)?);
    }
    Ok(ret)
}

// Core: rpc/evo.cpp:1771-1837
pub(super) fn info(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    p: &TxJsonParams,
    control: &dyn NodeControl,
) -> Result<UniValue, RpcError> {
    let hash: BlockHash = parse_hash_v(param(&request.params, 0), "proTxHash")?;
    let pro_tx_hash = ProTxHash::from(*hash.as_bytes());
    let value = param(&request.params, 1);
    let block = if value.is_null() {
        None
    } else {
        Some(parse_hash_v(value, "blockHash")?)
    };
    let at = evo
        .mn_list(MnListRequest::Info {
            pro_tx_hash,
            block,
        })
        .map_err(info_error)?;
    let dmn = at
        .list
        .get(&pro_tx_hash)
        .ok_or_else(|| info_error(EvoError::MasternodeNotFound(pro_tx_hash)))?;
    list_entry(dmn, true, &at, Some(at.height), p, control)
}

// Core: rpc/evo.cpp:1920-2006
pub(super) fn list_diff(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    p: &TxJsonParams,
    control: &dyn NodeControl,
) -> Result<UniValue, RpcError> {
    let base = block_arg(param(&request.params, 0), "baseBlock", true);
    let block = block_arg(param(&request.params, 1), "block", true);
    if let Err(error) = &base.syntax {
        return Err(error.clone());
    }
    let facts = evo.mn_list_diff(base, block).map_err(list_diff_error)?;
    let mut ret = UniValue::new_object();
    ret.push_kv_end("baseHeight", i64::from(facts.base_height).into());
    ret.push_kv_end("blockHeight", i64::from(facts.block_height).into());
    let mut added = UniValue::new_array();
    for dmn in &facts.added {
        added.push_back(dmn_to_json(dmn, p).map_err(|e| e.into_rpc_error(control))?);
    }
    ret.push_kv_end("addedMNs", added);
    let mut removed = UniValue::new_array();
    for pro_tx_hash in &facts.removed {
        removed.push_back(pro_tx_hash.to_string().into());
    }
    ret.push_kv_end("removedMNs", removed);
    let mut updated = UniValue::new_array();
    for (dmn, diff) in &facts.updated {
        let mut obj = UniValue::new_object();
        obj.push_kv_end(dmn.pro_tx_hash.to_string(), state_diff_to_json(dmn, diff, p));
        updated.push_back(obj);
    }
    ret.push_kv_end("updatedMNs", updated);
    Ok(ret)
}

/// `BuildDMNListEntry`: the hash alone, or the masternode with its collateral's confirmations
/// and meta info. At a requested block (`info`), unspent collateral counts confirmations back
/// from it; spent collateral fails Core's `CHECK_NONFATAL` without txindex, which the node's
/// adapter reports before this runs.
// Core: rpc/evo.cpp:1593-1642
fn list_entry(
    dmn: &Dmn,
    detailed: bool,
    at: &MnListAt,
    block_height: Option<i32>,
    p: &TxJsonParams,
    control: &dyn NodeControl,
) -> Result<UniValue, RpcError> {
    if !detailed {
        return Ok(dmn.pro_tx_hash.to_string().into());
    }
    let mut obj = dmn_to_json(dmn, p).map_err(|e| e.into_rpc_error(control))?;
    let mut confirmations = at.confirmations.get(&dmn.pro_tx_hash).copied().unwrap_or(-1);
    if let Some(height) = block_height {
        if confirmations < 0 {
            return Err(CheckFailed {
                check: "pindexMined != nullptr",
                func: "BuildDMNListEntry",
                file: file!(),
                line: line!(),
            }
            .into_rpc_error(control));
        }
        confirmations -= at.tip_height - height;
    }
    obj.push_kv_end("confirmations", i64::from(confirmations).into());
    let meta = at.meta.get(&dmn.pro_tx_hash).cloned().unwrap_or_default();
    obj.push_kv_end("metaInfo", meta_to_json(&meta, at.now));
    Ok(obj)
}

pub(crate) fn info_error(error: EvoError) -> RpcError {
    error.into_rpc(|error| match error {
        EvoError::Block {
            failure: BlockFailure::NotFound(_),
            ..
        } => RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"),
        EvoError::Block {
            name,
            value_str,
            ..
        } => bad_block_text(name, &value_str),
        _ => RpcError::new(RPC_INTERNAL_ERROR, "Unexpected block argument for protx RPC"),
    })
}

pub(super) fn list_diff_error(error: EvoError) -> RpcError {
    error.into_rpc(|error| match error {
        EvoError::Block {
            name,
            value_str,
            ..
        } => bad_block_text(name, &value_str),
        _ => RpcError::new(RPC_INTERNAL_ERROR, "Unexpected block argument for protx RPC"),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use dash_dml::{MnList, MnStateDiff};

    use super::*;
    use crate::core_write::dmn::tests::{evo_dmn, params};
    use crate::evo::{
        BlockArg, MetaFacts, MnListDiffFacts, QuorumFacts, QuorumListFacts, SmlDiffFacts,
        register_evo_commands,
    };
    use crate::server::RpcServer;

    /// A tip at 10 whose list holds an Evo node (1), a banned Evo node (2) and a Regular node
    /// (3, collateral spent); every request reads it at `height`, and is recorded.
    struct Node {
        height: i32,
        asked: Mutex<Vec<MnListRequest>>,
    }

    fn dmns() -> [Arc<Dmn>; 3] {
        let mut banned = evo_dmn(2, 5);
        banned.hot.ban_if_not_banned(8);
        let regular = Dmn {
            mn_type: dash_evo::types::RawMnType(0),
            ..evo_dmn(3, 6)
        };
        [Arc::new(evo_dmn(1, 4)), Arc::new(banned), Arc::new(regular)]
    }

    impl NodeEvo for Node {
        fn permissive_bool(&self) -> bool {
            false
        }
        fn mn_list(&self, request: MnListRequest) -> Result<MnListAt, EvoError> {
            self.asked.lock().unwrap().push(request);
            if let MnListRequest::Height {
                height: Some(h),
                ..
            } = request
                && !(1..=10).contains(&h)
            {
                return Err(EvoError::InvalidHeight);
            }
            let mut list = MnList::new(BlockHash::from([7; 32]), self.height, 3);
            let dmns = dmns();
            for dmn in &dmns {
                list.try_add(Arc::clone(dmn), true).unwrap();
            }
            let read: Vec<_> =
                request.detailed_entries(&list).iter().map(|dmn| dmn.pro_tx_hash).collect();
            let mut confirmations = BTreeMap::from([
                (dmns[0].pro_tx_hash, 9),
                (dmns[1].pro_tx_hash, 4),
                (dmns[2].pro_tx_hash, -1),
            ]);
            confirmations.retain(|hash, _| read.contains(hash));
            let mut meta = BTreeMap::from([(
                dmns[0].pro_tx_hash,
                MetaFacts {
                    last_outbound_attempt: 1_000,
                    ..MetaFacts::default()
                },
            )]);
            meta.retain(|hash, _| read.contains(hash));
            Ok(MnListAt {
                height: self.height,
                block: BlockHash::from([7; 32]),
                list: Arc::new(list),
                tip_height: 10,
                confirmations,
                meta,
                now: 1_500,
            })
        }
        /// Two updated entries whose internal-id order (3 at id 0, then 2) is not their
        /// proTxHash order.
        fn mn_list_diff(&self, _: BlockArg, _: BlockArg) -> Result<MnListDiffFacts, EvoError> {
            let [added, banned, regular] = dmns();
            let banned_diff = MnStateDiff::between(&evo_dmn(2, 5), &banned);
            let mut paid = Dmn {
                internal_id: 0,
                ..(*regular).clone()
            };
            paid.hot.last_paid_height = 7;
            let paid_diff = MnStateDiff::between(&regular, &paid);
            Ok(MnListDiffFacts {
                base_height: 3,
                block_height: 8,
                added: vec![added],
                removed: vec![regular.pro_tx_hash],
                updated: vec![(Arc::new(paid), paid_diff), (banned, banned_diff)],
            })
        }
        fn sml_diff(
            &self,
            _: BlockArg,
            _: BlockArg,
            _: Result<bool, RpcError>,
        ) -> Result<SmlDiffFacts, EvoError> {
            Err(EvoError::Unavailable)
        }
        fn quorum_list(&self, _: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
            Err(EvoError::Unavailable)
        }
        fn quorum_list_extended(&self, _: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
            Err(EvoError::Unavailable)
        }
        fn llmq_type_known(&self, _: u8) -> bool {
            false
        }
        fn quorum_info(&self, _: u8, _: BlockHash, _: bool) -> Result<QuorumFacts, EvoError> {
            Err(EvoError::Unavailable)
        }
    }

    fn server(height: i32) -> Arc<RpcServer> {
        server_of(Arc::new(Node {
            height,
            asked: Mutex::default(),
        }))
    }

    fn server_of(node: Arc<Node>) -> Arc<RpcServer> {
        let server = Arc::new(RpcServer::new(Arc::new(crate::testutil::FakeControl::default())));
        register_evo_commands(&server, node, params(false));
        server.start();
        server.set_warmup_finished();
        server
    }

    fn call(server: &RpcServer, method: &str, params: &str) -> Result<UniValue, RpcError> {
        server.execute(&JsonRpcRequest {
            method: method.into(),
            params: UniValue::read(params.as_bytes()).unwrap(),
            ..Default::default()
        })
    }

    fn get(value: &UniValue, key: &str) -> UniValue {
        let UniValue::Obj(fields) = value else {
            panic!("{}", value.write())
        };
        fields.iter().find(|(k, _)| k == key).unwrap().1.clone()
    }

    fn hashes(dmns: &[&Arc<Dmn>]) -> UniValue {
        UniValue::Arr(dmns.iter().map(|d| d.pro_tx_hash.to_string().into()).collect())
    }

    #[test]
    fn list_filters_by_type_in_core_order() {
        let server = server(10);
        let [evo, banned, regular] = dmns();
        let order: Vec<_> = {
            let mut list = MnList::new(BlockHash::from([7; 32]), 10, 3);
            for dmn in dmns() {
                list.try_add(dmn, true).unwrap();
            }
            list.core_order().iter().map(|h| h.to_string()).collect()
        };
        let registered = call(&server, "protx list", "[]").unwrap();
        assert_eq!(
            registered,
            UniValue::Arr(order.iter().map(|h| h.clone().into()).collect::<Vec<_>>())
        );
        let valid = call(&server, "protx list", r#"["valid"]"#).unwrap();
        let expected: Vec<UniValue> = order
            .iter()
            .filter(|h| **h != banned.pro_tx_hash.to_string())
            .map(|h| h.clone().into())
            .collect();
        assert_eq!(valid, UniValue::Arr(expected));
        let evo_only = call(&server, "protx list", r#"["evo",false,5]"#).unwrap();
        let expected: Vec<UniValue> = order
            .iter()
            .filter(|h| **h != regular.pro_tx_hash.to_string())
            .map(|h| h.clone().into())
            .collect();
        assert_eq!(expected.len(), 2);
        assert!(expected.contains(&evo.pro_tx_hash.to_string().into()));
        assert_eq!(evo_only, UniValue::Arr(expected));
    }

    /// The handler hands the node the type and `detailed`, which decide what the node reads.
    #[test]
    fn list_passes_type_and_detailed_to_the_node() {
        let node = Arc::new(Node {
            height: 10,
            asked: Mutex::default(),
        });
        let server = server_of(Arc::clone(&node));
        for params in ["[]", r#"["valid",true]"#, r#"["evo",false,5]"#] {
            call(&server, "protx list", params).unwrap();
        }
        let info = format!(r#"["{}"]"#, dmns()[0].pro_tx_hash);
        call(&server, "protx info", &info).unwrap();
        let height = |height, mn_type, detailed| MnListRequest::Height {
            height,
            mn_type,
            detailed,
        };
        assert_eq!(
            *node.asked.lock().unwrap(),
            [
                height(None, MnListType::Registered, false),
                height(None, MnListType::Valid, true),
                height(Some(5), MnListType::Evo, false),
                MnListRequest::Info {
                    pro_tx_hash: dmns()[0].pro_tx_hash,
                    block: None
                },
            ]
        );
    }

    #[test]
    fn detailed_list_entries_carry_raw_confirmations_and_meta() {
        let server = server(5);
        let UniValue::Arr(entries) =
            call(&server, "protx list", r#"["registered",true,5]"#).unwrap()
        else {
            panic!()
        };
        let [evo, _, regular] = dmns();
        let entry = |dmn: &Dmn| {
            entries
                .iter()
                .find(|e| get(e, "proTxHash") == UniValue::from(dmn.pro_tx_hash.to_string()))
                .unwrap()
                .clone()
        };
        let evo_entry = entry(&evo);
        let UniValue::Obj(fields) = &evo_entry else {
            panic!()
        };
        let keys: Vec<_> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "type",
                "proTxHash",
                "collateralHash",
                "collateralIndex",
                "operatorReward",
                "state",
                "confirmations",
                "metaInfo"
            ]
        );
        assert_eq!(get(&evo_entry, "confirmations").write(), "9");
        let meta = get(&evo_entry, "metaInfo");
        assert_eq!(get(&meta, "lastOutboundAttemptElapsed").write(), "500");
        assert_eq!(get(&meta, "lastOutboundSuccessElapsed").write(), "1500");
        let regular_entry = entry(&regular);
        assert_eq!(get(&regular_entry, "confirmations").write(), "-1");
        assert_eq!(
            get(&get(&regular_entry, "metaInfo"), "lastOutboundAttemptElapsed").write(),
            "1500"
        );
    }

    #[test]
    fn info_counts_confirmations_back_from_its_block() {
        let server = server(6);
        let [evo, banned, regular] = dmns();
        let info = |dmn: &Dmn| call(&server, "protx info", &format!(r#"["{}"]"#, dmn.pro_tx_hash));
        assert_eq!(get(&info(&evo).unwrap(), "confirmations").write(), "5");
        assert_eq!(get(&info(&banned).unwrap(), "confirmations").write(), "0");
        let error = info(&regular).unwrap_err();
        assert_eq!(error.code(), -1);
        let UniValue::Obj(error) = error.to_object() else {
            panic!()
        };
        let message = error.iter().find(|(k, _)| k == "message").unwrap().1.get_str().unwrap();
        let mut lines = message.lines();
        assert_eq!(lines.next(), Some("Internal bug detected: pindexMined != nullptr"));
        assert!(lines.next().unwrap().ends_with(" (BuildDMNListEntry)"));
        assert_eq!(lines.next(), Some("rdashd v0.0.0"));
        assert_eq!(
            lines.next(),
            Some(format!("Please report this issue here: {}", crate::BUG_REPORT_URL).as_str())
        );
        let unknown = ProTxHash::from([9; 32]);
        assert_eq!(
            call(&server, "protx info", &format!(r#"["{unknown}"]"#)).unwrap_err().to_object(),
            RpcError::new(RPC_INVALID_PARAMETER, format!("{unknown} not found")).to_object()
        );
    }

    #[test]
    fn listdiff_prints_heights_and_the_three_arrays() {
        let server = server(8);
        let reply = call(&server, "protx listdiff", "[3,8]").unwrap();
        let UniValue::Obj(fields) = &reply else {
            panic!()
        };
        let keys: Vec<_> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["baseHeight", "blockHeight", "addedMNs", "removedMNs", "updatedMNs"]);
        let [added, banned, regular] = dmns();
        assert_eq!(get(&reply, "baseHeight").write(), "3");
        assert_eq!(get(&reply, "blockHeight").write(), "8");
        assert_eq!(
            get(&reply, "addedMNs"),
            UniValue::Arr(vec![dmn_to_json(&added, &params(false)).unwrap()])
        );
        assert_eq!(get(&reply, "removedMNs"), hashes(&[&regular]));
        // Internal-id order: 3 (id 0) before 2, against proTxHash order.
        assert_eq!(
            get(&reply, "updatedMNs").write(),
            format!(
                r#"[{{"{}":{{"lastPaidHeight":7}}}},{{"{}":{{"PoSeBanHeight":8}}}}]"#,
                regular.pro_tx_hash, banned.pro_tx_hash
            )
        );
    }
}
