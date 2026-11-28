use super::{BlockArg, BlockFailure, BlockRef, EvoError, NodeEvo, SmlDiffFacts};
use crate::UniValue;
use crate::blockchain::parse_hash_v;
use crate::core_write::TxJsonParams;
use crate::core_write::sml::sml_diff_to_json;
use crate::protocol::{RPC_INTERNAL_ERROR, RpcError};
use crate::request::{JsonRpcRequest, bad_block_arg, get_int_i32, param, parse_bool_v};
use crate::server::NodeControl;

// Core: rpc/evo.cpp:1876-1918 protx_diff
pub(super) fn diff(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    p: &TxJsonParams,
    control: &dyn NodeControl,
) -> Result<UniValue, RpcError> {
    let facts = read_diff(request, evo)?;
    // The node parsed `extended` before it built the diff; this parses it again from the same
    // input, so the two agree.
    let value = param(&request.params, 2);
    let extended = !value.is_null() && parse_bool_v(value, "extended", evo.permissive_bool())?;
    sml_diff_to_json(&facts, extended, p).map_err(|e| e.into_rpc_error(control))
}

pub(super) fn read_diff(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
) -> Result<SmlDiffFacts, RpcError> {
    let base = block_arg(param(&request.params, 0), "baseBlock", false);
    let block = block_arg(param(&request.params, 1), "block", false);
    if let Err(RpcError::Type(message)) = &base.syntax {
        return Err(RpcError::Type(message.clone()));
    }
    let value = param(&request.params, 2);
    let extended = if value.is_null() {
        Ok(false)
    } else {
        parse_bool_v(value, "extended", evo.permissive_bool())
    };
    evo.sml_diff(base, block, extended).map_err(diff_error)
}

// Core: rpc/evo.cpp:1839
pub(super) fn block_arg(value: &UniValue, name: &'static str, index: bool) -> BlockArg {
    let syntax = parse_hash_v(value, name).map(BlockRef::Hash).or_else(|_| {
        if value.is_num() {
            return get_int_i32(value).map(BlockRef::Height);
        }
        let text = value.get_str()?;
        text.parse::<i32>().ok().map(BlockRef::Height).ok_or_else(|| bad_block_arg(name, value))
    });
    BlockArg {
        name,
        value_str: value.get_val_str().into(),
        syntax: if index {
            syntax.map_err(|_| bad_block_arg(name, value))
        } else {
            syntax
        },
    }
}

pub(super) fn diff_error(error: EvoError) -> RpcError {
    error.into_rpc(|error| match error {
        EvoError::Block {
            name,
            value_str,
            failure,
        } => match failure {
            BlockFailure::NotFound(hash) => RpcError::Runtime(format!("block {hash} not found")),
            BlockFailure::BadArg => bad_block_text(name, &value_str),
        },
        _ => RpcError::new(RPC_INTERNAL_ERROR, "Unexpected block argument for protx RPC"),
    })
}

pub(super) fn bad_block_text(name: &str, value_str: &str) -> RpcError {
    RpcError::Runtime(format!("{name} must be a block hash or chain height and not {value_str}"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use dash_consensus::BlsCtx;
    use dash_consensus::primitives::P2pCommitment;
    use dash_dml::{Dmn, ProTxHash};
    use dash_num::Hash256;
    use dash_primitives::{BlockHash, CollateralShare, Commitment, PayoutShare};
    use dash_types::codec::BaseCodec as _;

    use super::*;
    use crate::evo::{
        MnListAt, MnListDiffFacts, MnListRequest, QuorumFacts, QuorumListFacts, SmlEntryFacts,
        register_evo_commands,
    };
    use crate::request::{find_value, parse_hex};
    use crate::server::RpcServer;

    /// A legacy-service Evo node (`decode_dmn`'s disk form, as `quorum`'s tests build it).
    fn dmn() -> Dmn {
        let mut bytes = vec![1; 32];
        bytes.push(0);
        bytes.extend([2; 32]);
        bytes.extend(0u32.to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        for n in [2i32, 1, 0, 0, 0, -1, -1] {
            bytes.extend(n.to_le_bytes());
        }
        bytes.extend(0u16.to_le_bytes());
        bytes.extend([0; 64 + 20 + 48 + 20]);
        bytes.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 1, 2, 3, 4]);
        bytes.extend(19999u16.to_be_bytes());
        bytes.extend([0; 2 + 20]);
        bytes.extend(26656u16.to_le_bytes());
        bytes.extend(443u16.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        let mut input = bytes.as_slice();
        let dmn = dash_dml::codec::core_disk::decode_dmn(&mut input).unwrap();
        assert!(input.is_empty());
        dmn
    }

    fn p2pkh(n: u8) -> Vec<u8> {
        let mut script = vec![0x76, 0xa9, 0x14];
        script.extend([n; 20]);
        script.extend([0x88, 0xac]);
        script
    }

    /// A regular masternode with a payout script, and a version-3 Evo node with a payout list,
    /// shares and an operator payout.
    fn entries() -> Vec<SmlEntryFacts> {
        let mut regular = dmn();
        regular.mn_type.0 = 0;
        let mut cold = (*regular.cold).clone();
        cold.version = 2;
        cold.script_payout = p2pkh(7);
        regular.cold = Arc::new(cold);
        let mut evo = dmn();
        evo.pro_tx_hash = ProTxHash::from([5; 32]);
        evo.mn_type.0 = 1;
        let mut cold = (*evo.cold).clone();
        cold.version = 3;
        cold.payouts = vec![PayoutShare::new(p2pkh(8).into(), 10_000)];
        cold.shares = vec![CollateralShare::new(
            500 * 100_000_000,
            p2pkh(9).into(),
            Vec::new().into(),
            [6; 20].into(),
        )];
        cold.script_operator_payout = p2pkh(10);
        evo.cold = Arc::new(cold);
        [regular, evo]
            .map(|dmn| {
                let dmn = Arc::new(dmn);
                SmlEntryFacts {
                    entry: dmn.to_sml_entry(),
                    dmn,
                }
            })
            .to_vec()
    }

    fn facts() -> SmlDiffFacts {
        let bytes =
            parse_hex(include_str!("../../tests/data/evo/bare-commitment-v3.hex").trim()).unwrap();
        SmlDiffFacts {
            base_hash_echo: BlockHash::from([0; 32]),
            block_hash: BlockHash::from([2; 32]),
            cbtx_hex: "00".into(),
            merkle_tree_hex: "01".into(),
            deleted: vec![ProTxHash::from([3; 32])],
            entries: entries(),
            deleted_quorums: vec![(100, [4; 32])],
            new_quorums: vec![P2pCommitment::new(
                Commitment::decode(&mut bytes.as_slice()).unwrap(),
            )],
            roots: Some((Hash256::from([0x0b; 32]), None)),
            cl_sigs: vec![([0xaa; 96], vec![0, 2])],
            bls: BlsCtx::BASIC,
        }
    }

    struct Node {
        permissive: bool,
        extended: Mutex<Vec<Result<bool, RpcError>>>,
    }

    impl NodeEvo for Node {
        fn permissive_bool(&self) -> bool {
            self.permissive
        }
        fn mn_list(&self, _: MnListRequest) -> Result<MnListAt, EvoError> {
            Err(EvoError::Unavailable)
        }
        fn mn_list_diff(&self, _: BlockArg, _: BlockArg) -> Result<MnListDiffFacts, EvoError> {
            Err(EvoError::Unavailable)
        }
        fn sml_diff(
            &self,
            _: BlockArg,
            _: BlockArg,
            extended: Result<bool, RpcError>,
        ) -> Result<SmlDiffFacts, EvoError> {
            self.extended.lock().unwrap().push(extended.clone());
            extended.map_err(EvoError::Param)?;
            Ok(facts())
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

    fn params(service: bool) -> TxJsonParams {
        TxJsonParams {
            key_io: crate::key_io::KeyIoParams {
                pubkey_address: 140,
                script_address: 19,
                secret_key: 239,
                ext_public_key: [0; 4],
                ext_secret_key: [0; 4],
                platform_hrp: Some("tdash".into()),
            },
            service,
        }
    }

    fn keys(value: &UniValue) -> Vec<&str> {
        let UniValue::Obj(fields) = value else {
            panic!("{}", value.write())
        };
        fields.iter().map(|(k, _)| k.as_str()).collect()
    }

    fn field<'a>(value: &'a UniValue, name: &str) -> &'a UniValue {
        let UniValue::Obj(fields) = value else {
            panic!("{}", value.write())
        };
        find_value(fields, name)
    }

    fn entry(diff: &UniValue, i: usize) -> &UniValue {
        let UniValue::Arr(entries) = field(diff, "mnList") else {
            panic!("{}", diff.write())
        };
        &entries[i]
    }

    /// The diff's fields in Core's order: the roots only when the CbTx has them, the
    /// `merkleRootQuorums` one only from CbTx version 2.
    // Core: evo/core_write.cpp:316-376
    #[test]
    fn the_diff_prints_core_fields_in_order() {
        let mut facts = facts();
        let json = sml_diff_to_json(&facts, false, &params(false)).unwrap();
        assert_eq!(
            keys(&json),
            [
                "nVersion",
                "baseBlockHash",
                "blockHash",
                "cbTxMerkleTree",
                "cbTx",
                "deletedMNs",
                "mnList",
                "deletedQuorums",
                "newQuorums",
                "merkleRootMNList",
                "quorumsCLSigs",
            ]
        );
        assert_eq!(*field(&json, "nVersion"), 1i64.into());
        assert_eq!(*field(&json, "baseBlockHash"), "0".repeat(64).into());
        assert_eq!(*field(&json, "blockHash"), BlockHash::from([2; 32]).to_string().into());
        assert_eq!(
            *field(&json, "merkleRootMNList"),
            BlockHash::from([0x0b; 32]).to_string().into()
        );
        let quorum = format!(r#"[{{"llmqType":100,"quorumHash":"{}"}}]"#, "04".repeat(32));
        assert_eq!(field(&json, "deletedQuorums").write(), quorum);
        let sigs = format!(r#"[{{"{}":[0,2]}}]"#, "aa".repeat(96));
        assert_eq!(field(&json, "quorumsCLSigs").write(), sigs);
        let UniValue::Arr(quorums) = field(&json, "newQuorums") else {
            panic!("{}", json.write())
        };
        assert_eq!(keys(&quorums[0])[0], "version");

        facts.roots = Some((Hash256::from([0x0b; 32]), Some(Hash256::from([0x0c; 32]))));
        let json = sml_diff_to_json(&facts, false, &params(false)).unwrap();
        let keys = keys(&json);
        assert_eq!(keys[9..], ["merkleRootMNList", "merkleRootQuorums", "quorumsCLSigs"]);
        facts.roots = None;
        let json = sml_diff_to_json(&facts, false, &params(false)).unwrap();
        assert!(!self::keys(&json).iter().any(|k| k.starts_with("merkleRoot")));
    }

    /// An entry's fields, the service-flag ones in place, and with `extended` its masternode's
    /// payout fields: the list from version 3, else the payout address.
    // Core: evo/core_write.cpp:379-414
    #[test]
    fn entries_print_core_fields_and_extended_payouts() {
        let facts = facts();
        let plain = sml_diff_to_json(&facts, false, &params(false)).unwrap();
        let common = [
            "nVersion",
            "nType",
            "proRegTxHash",
            "confirmedHash",
            "addresses",
            "pubKeyOperator",
            "votingAddress",
            "isValid",
        ];
        assert_eq!(keys(entry(&plain, 0)), common);
        assert_eq!(keys(entry(&plain, 1))[..8], common);
        assert_eq!(keys(entry(&plain, 1))[8..], ["platformNodeID"]);
        assert_eq!(*field(entry(&plain, 1), "nType"), 1i64.into());
        assert!(!keys(field(entry(&plain, 1), "addresses")).contains(&"platform_p2p"));

        let service = sml_diff_to_json(&facts, false, &params(true)).unwrap();
        assert_eq!(keys(entry(&service, 0))[4..6], ["service", "addresses"]);
        assert_eq!(keys(entry(&service, 1))[9..], ["platformHTTPPort", "platformNodeID"]);
        assert_eq!(*field(entry(&service, 1), "service"), "1.2.3.4:19999".into());

        let extended = sml_diff_to_json(&facts, true, &params(false)).unwrap();
        assert_eq!(keys(entry(&extended, 0))[8..], ["payoutAddress"]);
        assert_eq!(
            keys(entry(&extended, 1))[8..],
            ["platformNodeID", "payouts", "shares", "operatorPayoutAddress"]
        );
        let UniValue::Arr(payouts) = field(entry(&extended, 1), "payouts") else {
            panic!("{}", extended.write())
        };
        assert_eq!(*field(&payouts[0], "reward"), 10_000i64.into());
        assert_eq!(*field(&payouts[0], "script"), crate::descriptor::hex(&p2pkh(8)).into());
    }

    /// The handler passes `extended` to the node unevaluated and prints with its value.
    #[test]
    fn the_handler_prints_with_the_extended_the_node_accepted() {
        for permissive in [false, true] {
            let node = Arc::new(Node {
                permissive,
                extended: Mutex::default(),
            });
            let server =
                Arc::new(RpcServer::new(Arc::new(crate::testutil::FakeControl::default())));
            register_evo_commands(&server, node.clone(), params(false));
            server.start();
            server.set_warmup_finished();
            let call = |args: &str| {
                server.execute(&JsonRpcRequest {
                    method: "protx diff".into(),
                    params: UniValue::read(args.as_bytes()).unwrap(),
                    ..Default::default()
                })
            };
            let printed = |reply: UniValue| keys(entry(&reply, 0)).len();
            assert_eq!(printed(call("[1,2]").unwrap()), 8);
            assert_eq!(printed(call("[1,2,null]").unwrap()), 8);
            assert_eq!(printed(call("[1,2,false]").unwrap()), 8);
            assert_eq!(printed(call("[1,2,true]").unwrap()), 9);
            let yes = call(r#"[1,2,"yes"]"#);
            if permissive {
                assert_eq!(printed(yes.unwrap()), 9);
            } else {
                assert_eq!(yes.unwrap_err().code(), -8);
            }
            let asked = node.extended.lock().unwrap();
            assert_eq!(asked[..4], [Ok(false), Ok(false), Ok(false), Ok(true)]);
            assert_eq!(asked[4].is_ok(), permissive);
        }
    }
}
