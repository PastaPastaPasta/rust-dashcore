use super::{EvoError, NodeEvo, QuorumFacts};
use crate::UniValue;
use crate::blockchain::parse_hash_v;
use crate::core_write::TxJsonParams;
use crate::core_write::evo::{AddressMode, EvoNet, lazy_key};
use crate::descriptor::hex;
use crate::protocol::{RPC_INTERNAL_ERROR, RPC_INVALID_PARAMETER, RpcError};
use crate::request::{JsonRpcRequest, get_int_i32, param, parse_bool_v};

// Core: rpc/quorums.cpp:80
pub(super) fn list(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    _p: &TxJsonParams,
) -> Result<UniValue, RpcError> {
    let count = optional_int(param(&request.params, 0))?;
    if count.is_some_and(|count| count < -1) {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "count can't be negative"));
    }
    let mut ret = UniValue::new_object();
    for group in evo.quorum_list(count).map_err(rpc_error)? {
        ret.push_kv_end(
            group.type_name,
            UniValue::Arr(group.quorums.into_iter().map(|q| q.hash.to_string().into()).collect()),
        );
    }
    Ok(ret)
}

// Core: rpc/quorums.cpp:147
pub(super) fn list_extended(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    _p: &TxJsonParams,
) -> Result<UniValue, RpcError> {
    let height = optional_int(param(&request.params, 0))?;
    let mut ret = UniValue::new_object();
    for group in evo.quorum_list_extended(height).map_err(rpc_error)? {
        let quorums = group
            .quorums
            .iter()
            .map(|quorum| {
                let mut details = UniValue::new_object();
                if quorum.rotated {
                    details.push_kv_end("quorumIndex", i64::from(quorum.index).into());
                }
                details.push_kv_end("creationHeight", i64::from(quorum.base_height).into());
                details.push_kv_end("minedBlockHash", quorum.mined_block.to_string().into());
                details.push_kv_end("numValidMembers", (quorum.valid_count as u64).into());
                let ratio = if quorum.members.is_empty() {
                    0.0
                } else {
                    quorum.valid_count as f64 / quorum.members.len() as f64
                };
                details.push_kv_end("healthRatio", format!("{ratio:.2}").into());
                UniValue::Obj(vec![(quorum.hash.to_string(), details)])
            })
            .collect();
        ret.push_kv_end(group.type_name, UniValue::Arr(quorums));
    }
    Ok(ret)
}

// Core: rpc/quorums.cpp:291
pub(super) fn info(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    p: &TxJsonParams,
) -> Result<UniValue, RpcError> {
    let quorum = read_info(request, evo)?;
    Ok(info_json(&quorum, p))
}

pub(super) fn read_info(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
) -> Result<QuorumFacts, RpcError> {
    let llmq_type = get_int_i32(param(&request.params, 0))? as u8;
    if !evo.llmq_type_known(llmq_type) {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "invalid LLMQ type"));
    }
    let hash = parse_hash_v(param(&request.params, 1), "quorumHash")?;
    let include = param(&request.params, 2);
    if !include.is_null() {
        parse_bool_v(include, "includeSkShare", evo.permissive_bool())?;
    }
    evo.quorum_info(llmq_type, hash).map_err(rpc_error)
}

/// Fresh observers have no contributions or watchquorums data. Multi-member pubKeyShare and
/// secretKeyShare parity is deferred to QM1a/DK1; valid single-member shares are operator keys.
// Core: rpc/quorums.cpp:192
fn info_json(quorum: &QuorumFacts, p: &TxJsonParams) -> UniValue {
    let mut ret = UniValue::new_object();
    ret.push_kv_end("height", i64::from(quorum.base_height).into());
    ret.push_kv_end("type", quorum.type_name.clone().into());
    ret.push_kv_end("quorumHash", quorum.hash.to_string().into());
    ret.push_kv_end("quorumIndex", i64::from(quorum.index).into());
    ret.push_kv_end("minedBlock", quorum.mined_block.to_string().into());
    if let Some(failures) = quorum.prev_failures {
        ret.push_kv_end("previousConsecutiveDKGFailures", i64::from(failures).into());
    }
    let members = quorum
        .members
        .iter()
        .map(|(member, valid)| {
            let cold = &member.cold;
            let net = EvoNet::new(
                &cold.net_info,
                member.mn_type.0,
                Some(cold.platform_p2p_port),
                Some(cold.platform_http_port),
            );
            let mut ret = UniValue::new_object();
            ret.push_kv_end("proTxHash", member.pro_tx_hash.to_string().into());
            if p.service {
                ret.push_kv_end("service", net.service());
            }
            ret.push_kv_end("addresses", net.addresses(AddressMode::FullState));
            ret.push_kv_end("pubKeyOperator", lazy_key(&cold.pub_key_operator));
            ret.push_kv_end("valid", (*valid).into());
            if *valid && quorum.single_member {
                ret.push_kv_end("pubKeyShare", lazy_key(&cold.pub_key_operator));
            }
            ret
        })
        .collect();
    ret.push_kv_end("members", UniValue::Arr(members));
    ret.push_kv_end("quorumPublicKey", hex(&quorum.quorum_pk).into());
    ret
}

fn optional_int(value: &UniValue) -> Result<Option<i32>, RpcError> {
    if value.is_null() {
        Ok(None)
    } else {
        get_int_i32(value).map(Some)
    }
}

fn rpc_error(error: EvoError) -> RpcError {
    error
        .into_rpc(|_| RpcError::new(RPC_INTERNAL_ERROR, "Unexpected block argument for quorum RPC"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evo::{
        BlockArg, MnListAt, MnListDiffFacts, MnListRequest, QuorumListFacts, SmlDiffFacts,
        register_evo_commands,
    };
    use crate::request::find_value;
    use crate::server::RpcServer;
    use dash_consensus::BlsCtx;
    use dash_dml::Dmn;
    use dash_primitives::BlockHash;
    use std::sync::{Arc, Mutex};

    struct Node {
        permissive: bool,
        groups: Vec<QuorumListFacts>,
        asked: Mutex<Vec<String>>,
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
            _: Result<bool, RpcError>,
        ) -> Result<SmlDiffFacts, EvoError> {
            Err(EvoError::Unavailable)
        }
        fn quorum_list(&self, count: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
            self.asked.lock().unwrap().push(format!("list {count:?}"));
            Ok(self.groups.clone())
        }
        fn quorum_list_extended(
            &self,
            height: Option<i32>,
        ) -> Result<Vec<QuorumListFacts>, EvoError> {
            self.asked.lock().unwrap().push(format!("extended {height:?}"));
            if height.is_some_and(|h| !(0..=10).contains(&h)) {
                Err(EvoError::HeightOutOfRange)
            } else {
                Ok(self.groups.clone())
            }
        }
        fn llmq_type_known(&self, llmq_type: u8) -> bool {
            llmq_type == 100
        }
        fn quorum_info(&self, llmq_type: u8, hash: BlockHash) -> Result<QuorumFacts, EvoError> {
            self.asked.lock().unwrap().push(format!("info {llmq_type} {hash}"));
            if llmq_type != 100 {
                return Err(EvoError::InvalidQuorumType);
            }
            self.groups
                .iter()
                .flat_map(|g| &g.quorums)
                .find(|q| q.hash == hash)
                .cloned()
                .ok_or(EvoError::QuorumNotFound)
        }
    }

    fn member() -> Arc<Dmn> {
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
        Arc::new(dmn)
    }

    fn quorum() -> QuorumFacts {
        QuorumFacts {
            type_name: "llmq_test".into(),
            rotated: true,
            base_height: 5,
            hash: BlockHash::from([1; 32]),
            index: 2,
            mined_block: BlockHash::from([2; 32]),
            prev_failures: Some(3),
            members: vec![(member(), true), (member(), false), (member(), true)],
            valid_count: 2,
            single_member: false,
            bls: BlsCtx::from_legacy(false),
            quorum_pk: [0; 48],
        }
    }

    fn setup(permissive: bool, groups: Vec<QuorumListFacts>) -> (Arc<RpcServer>, Arc<Node>) {
        let node = Arc::new(Node {
            permissive,
            groups,
            asked: Mutex::default(),
        });
        let server = Arc::new(RpcServer::new(Arc::new(crate::testutil::FakeControl::default())));
        register_evo_commands(&server, node.clone(), params(false));
        server.start();
        server.set_warmup_finished();
        (server, node)
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

    fn call(server: &RpcServer, method: &str, args: &str) -> Result<UniValue, RpcError> {
        server.execute(&JsonRpcRequest {
            method: method.into(),
            params: UniValue::read(args.as_bytes()).unwrap(),
            ..Default::default()
        })
    }

    fn error(server: &RpcServer, method: &str, args: &str, code: i32, text: &str) {
        assert_eq!(
            call(server, method, args).unwrap_err().to_object(),
            RpcError::new(code, text).to_object(),
            "{method} {args}"
        );
    }

    fn field<'a>(value: &'a UniValue, name: &str) -> &'a UniValue {
        let UniValue::Obj(fields) = value else {
            panic!("{}", value.write())
        };
        find_value(fields, name)
    }

    #[test]
    fn quorum_list_parameters_and_order() {
        let first = quorum();
        let mut second = first.clone();
        second.hash = BlockHash::from([3; 32]);
        let (server, node) = setup(
            false,
            vec![
                QuorumListFacts {
                    type_name: "z".into(),
                    quorums: vec![first.clone(), second.clone()],
                },
                QuorumListFacts {
                    type_name: "a".into(),
                    quorums: vec![],
                },
            ],
        );
        let expected = format!(r#"{{"z":["{}","{}"],"a":[]}}"#, first.hash, second.hash);
        for (args, count) in [
            ("[]", "None"),
            ("[null]", "None"),
            ("[-1]", "Some(-1)"),
            ("[0]", "Some(0)"),
            ("[1]", "Some(1)"),
            ("[25]", "Some(25)"),
        ] {
            assert_eq!(call(&server, "quorum list", args).unwrap().write(), expected);
            assert_eq!(node.asked.lock().unwrap().last().unwrap(), &format!("list {count}"));
        }
        for (method, args) in [
            ("quorum", r#"["list",1]"#),
            ("quorum", r#"{"command":"list","count":1}"#),
            ("quorum list", r#"{"count":1}"#),
        ] {
            assert_eq!(call(&server, method, args).unwrap().write(), expected);
        }
        error(&server, "quorum list", "[-2]", -8, "count can't be negative");
    }

    #[test]
    fn quorum_integer_and_dispatch_errors() {
        let (server, _) = setup(false, vec![]);
        for method in ["quorum list", "quorum listextended"] {
            for args in ["[1.0]", "[1e0]", "[1.5]", "[2147483648]", "[-2147483649]"] {
                error(&server, method, args, -1, "JSON integer out of range");
            }
            error(&server, method, "[null,null]", -1, dash_rpc_spec::command(method).unwrap().help);
            for (value, kind) in
                [(r#""x""#, "string"), ("true", "bool"), ("[]", "array"), ("{}", "object")]
            {
                let name = if method.ends_with("listextended") {
                    "height"
                } else {
                    "count"
                };
                error(
                    &server,
                    method,
                    &format!("[{value}]"),
                    -3,
                    &format!(
                        "Wrong type passed:\n{{\n    \"Position 1 ({name})\": \"JSON value of type {kind} is not of expected type number\"\n}}"
                    ),
                );
            }
        }
        for args in ["[]", "[100]", r#"[100,"x",false,1]"#] {
            error(
                &server,
                "quorum info",
                args,
                -1,
                dash_rpc_spec::command("quorum info").unwrap().help,
            );
        }
        for value in ["1.5", "1e0", "2147483648"] {
            error(
                &server,
                "quorum info",
                &format!(r#"[{value},"x"]"#),
                -1,
                "JSON integer out of range",
            );
        }
        for (value, kind) in [
            ("null", "null"),
            (r#""x""#, "string"),
            ("true", "bool"),
            ("[]", "array"),
            ("{}", "object"),
        ] {
            error(
                &server,
                "quorum info",
                &format!(r#"[{value},"x"]"#),
                -3,
                &format!(
                    "Wrong type passed:\n{{\n    \"Position 1 (llmqType)\": \"JSON value of type {kind} is not of expected type number\"\n}}"
                ),
            );
        }
        for (value, kind) in
            [("null", "null"), ("1", "number"), ("true", "bool"), ("[]", "array"), ("{}", "object")]
        {
            error(
                &server,
                "quorum info",
                &format!("[100,{value}]"),
                -3,
                &format!(
                    "Wrong type passed:\n{{\n    \"Position 2 (quorumHash)\": \"JSON value of type {kind} is not of expected type string\"\n}}"
                ),
            );
        }
    }

    #[test]
    fn quorum_extended_heights_and_health() {
        let q = quorum();
        let (server, node) = setup(
            false,
            vec![QuorumListFacts {
                type_name: q.type_name.clone(),
                quorums: vec![q.clone()],
            }],
        );
        let expected = format!(
            r#"{{"llmq_test":[{{"{}":{{"quorumIndex":2,"creationHeight":5,"minedBlockHash":"{}","numValidMembers":2,"healthRatio":"0.67"}}}}]}}"#,
            q.hash, q.mined_block
        );
        for (args, height) in [
            ("[]", "None"),
            ("[null]", "None"),
            ("[0]", "Some(0)"),
            ("[5]", "Some(5)"),
            ("[10]", "Some(10)"),
            (r#"{"height":5}"#, "Some(5)"),
        ] {
            assert_eq!(call(&server, "quorum listextended", args).unwrap().write(), expected);
            assert_eq!(node.asked.lock().unwrap().last().unwrap(), &format!("extended {height}"));
        }
        for args in ["[-1]", "[11]"] {
            error(&server, "quorum listextended", args, -8, "Block height out of range");
        }
        let mut empty = q;
        empty.rotated = false;
        empty.members.clear();
        empty.valid_count = 0;
        let (server, _) = setup(
            false,
            vec![QuorumListFacts {
                type_name: "empty".into(),
                quorums: vec![empty],
            }],
        );
        let result = call(&server, "quorum listextended", "[]").unwrap().write();
        assert!(result.contains(r#""numValidMembers":0,"healthRatio":"0.00""#));
        assert!(!result.contains("quorumIndex"));
    }

    #[test]
    fn quorum_info_members_and_observer_shares() {
        let mut q = quorum();
        for service in [false, true] {
            let result = info_json(&q, &params(service));
            let text = result.write();
            assert!(text.starts_with(&format!(r#"{{"height":5,"type":"llmq_test","quorumHash":"{}","quorumIndex":2,"minedBlock":"{}","previousConsecutiveDKGFailures":3,"members":["#, q.hash, q.mined_block)));
            let UniValue::Arr(members) = field(&result, "members") else {
                panic!()
            };
            assert_eq!(members.len(), 3);
            assert_eq!(field(&members[1], "valid"), &false.into());
            assert_eq!(text.contains(r#""service":"1.2.3.4:19999""#), service);
            assert!(text.contains(r#""addresses":{"core_p2p":["1.2.3.4:19999"],"platform_https":["1.2.3.4:443"],"platform_p2p":["1.2.3.4:26656"]},"pubKeyOperator":"#));
            assert!(!text.contains("pubKeyShare"));
            assert!(!text.contains("secretKeyShare"));
        }
        q.single_member = true;
        q.members.truncate(1);
        q.valid_count = 1;
        q.rotated = false;
        q.prev_failures = None;
        let result = info_json(&q, &params(false));
        let UniValue::Arr(members) = field(&result, "members") else {
            panic!()
        };
        assert_eq!(field(&members[0], "pubKeyShare"), field(&members[0], "pubKeyOperator"));
        assert!(!result.write().contains("previousConsecutiveDKGFailures"));
        q.members[0].1 = false;
        assert!(!info_json(&q, &params(false)).write().contains("pubKeyShare"));
    }

    #[test]
    fn quorum_info_hash_types_and_alias() {
        let q = quorum();
        let (server, _) = setup(
            false,
            vec![QuorumListFacts {
                type_name: q.type_name.clone(),
                quorums: vec![q.clone()],
            }],
        );
        let expected = info_json(&q, &params(false));
        for t in [100, 356, -156] {
            for tail in ["", ",null", ",false", ",true"] {
                assert_eq!(
                    call(&server, "quorum info", &format!(r#"[{t},"{}"{tail}]"#, q.hash)).unwrap(),
                    expected
                );
            }
        }
        assert_eq!(
            call(
                &server,
                "quorum info",
                &format!(r#"{{"quorumHash":"{}","llmqType":356}}"#, q.hash)
            )
            .unwrap(),
            expected
        );
        for t in [255, -1] {
            error(
                &server,
                "quorum info",
                &format!(r#"[{t},"{}"]"#, q.hash),
                -8,
                "invalid LLMQ type",
            );
        }
        error(
            &server,
            "quorum info",
            r#"[100,"x"]"#,
            -8,
            "quorumHash must be of length 64 (not 1, for 'x')",
        );
        let bad = "g".repeat(64);
        error(
            &server,
            "quorum info",
            &format!(r#"[100,"{bad}"]"#),
            -8,
            &format!("quorumHash must be hexadecimal string (not '{bad}')"),
        );
        error(
            &server,
            "quorum info",
            &format!(r#"[100,"{}"]"#, "0".repeat(64)),
            -8,
            "quorum not found",
        );
    }

    #[test]
    fn quorum_info_combined_invalid_parameters() {
        let q = quorum();
        let (server, node) = setup(
            false,
            vec![QuorumListFacts {
                type_name: q.type_name.clone(),
                quorums: vec![q.clone()],
            }],
        );
        error(&server, "quorum info", r#"[1.5,"x","x"]"#, -1, "JSON integer out of range");
        for t in [255, -1] {
            for hash in ["x".to_string(), "0".repeat(64)] {
                for tail in ["", r#", "x""#, ", {}"] {
                    error(
                        &server,
                        "quorum info",
                        &format!(r#"[{t},"{hash}"{tail}]"#),
                        -8,
                        "invalid LLMQ type",
                    );
                }
            }
        }
        for t in [100, 356] {
            error(
                &server,
                "quorum info",
                &format!(r#"[{t},"x","x"]"#),
                -8,
                "quorumHash must be of length 64 (not 1, for 'x')",
            );
            error(
                &server,
                "quorum info",
                &format!(r#"[{t},"{}","x"]"#, "0".repeat(64)),
                -8,
                "includeSkShare must be a JSON boolean. Pass -deprecatedrpc=permissive_bool to allow legacy boolean parsing.",
            );
        }
        assert!(node.asked.lock().unwrap().is_empty());
        assert_eq!(
            call(&server, "quorum info", &format!(r#"[356,"{}"]"#, q.hash)).unwrap(),
            info_json(&q, &params(false))
        );
        assert_eq!(node.asked.lock().unwrap().as_slice(), [format!("info 100 {}", q.hash)]);
        for error in
            [RpcError::new(-8, "invalid parameter"), RpcError::Runtime("read failed".into())]
        {
            assert_eq!(rpc_error(EvoError::Param(error.clone())).to_object(), error.to_object());
        }
    }

    #[test]
    fn quorum_info_bool_forms_and_error_precedence() {
        for permissive in [false, true] {
            let q = quorum();
            let (server, _) = setup(
                permissive,
                vec![QuorumListFacts {
                    type_name: q.type_name.clone(),
                    quorums: vec![q.clone()],
                }],
            );
            for value in ["true", "false", "null", "1", "0", r#""YES""#, r#""no""#, r#""1""#] {
                let args = format!(r#"[100,"{}",{value}]"#, q.hash);
                if permissive || ["true", "false", "null"].contains(&value) {
                    assert_eq!(
                        call(&server, "quorum info", &args).unwrap(),
                        info_json(&q, &params(false))
                    );
                } else {
                    error(
                        &server,
                        "quorum info",
                        &args,
                        -8,
                        "includeSkShare must be a JSON boolean. Pass -deprecatedrpc=permissive_bool to allow legacy boolean parsing.",
                    );
                }
            }
            for value in ["1.0", "1e0", "2147483648", r#""X""#, "[]", "{}", "2"] {
                let args = format!(r#"[100,"{}",{value}]"#, "0".repeat(64));
                let (code, message) = if !permissive {
                    (-8, "includeSkShare must be a JSON boolean. Pass -deprecatedrpc=permissive_bool to allow legacy boolean parsing.".to_string())
                } else if ["1.0", "1e0", "2147483648"].contains(&value) {
                    (-1, "JSON integer out of range".to_string())
                } else {
                    let text = if value == r#""X""# {
                        "x"
                    } else if value == "2" {
                        "2"
                    } else {
                        ""
                    };
                    (
                        -8,
                        format!(
                            "includeSkShare must be true, false, yes, no, 1 or 0 (not '{text}')"
                        ),
                    )
                };
                error(&server, "quorum info", &args, code, &message);
            }
        }
    }
}
