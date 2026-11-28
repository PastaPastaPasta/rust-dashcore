use std::sync::Mutex;

use dash_primitives::Commitment;
use dash_types::codec::BaseCodec;

use super::*;
use crate::core_write::evo::commitment_to_json;
use crate::key_io::KeyIoParams;
use crate::protocol::{RPC_INVALID_ADDRESS_OR_KEY, RPC_MISC_ERROR, RPC_TYPE_ERROR};
use crate::request::{bad_block_arg, parse_bool_v, parse_hex};
use crate::testutil;

struct Unavailable;

impl NodeEvo for Unavailable {
    fn permissive_bool(&self) -> bool {
        false
    }
    fn mn_list(&self, _request: MnListRequest) -> Result<MnListAt, EvoError> {
        Err(EvoError::Unavailable)
    }
    fn mn_list_diff(&self, _base: BlockArg, _block: BlockArg) -> Result<MnListDiffFacts, EvoError> {
        Err(EvoError::Unavailable)
    }
    fn sml_diff(
        &self,
        _base: BlockArg,
        _block: BlockArg,
        _extended: Result<bool, RpcError>,
    ) -> Result<SmlDiffFacts, EvoError> {
        Err(EvoError::Unavailable)
    }
    fn quorum_list(&self, _count: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
        Err(EvoError::Unavailable)
    }
    fn quorum_list_extended(&self, _height: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
        Err(EvoError::Unavailable)
    }
    fn llmq_type_known(&self, llmq_type: u8) -> bool {
        llmq_type == 100
    }
    fn quorum_info(
        &self,
        _llmq_type: u8,
        _hash: BlockHash,
        _include_sk_share: bool,
    ) -> Result<QuorumFacts, EvoError> {
        Err(EvoError::Unavailable)
    }
}

fn server() -> Arc<RpcServer> {
    server_with(Arc::new(Unavailable))
}

fn server_with(evo: Arc<dyn NodeEvo>) -> Arc<RpcServer> {
    let server = Arc::new(RpcServer::new(Arc::new(testutil::FakeControl::default())));
    crate::commands::register_control_commands(&server);
    register_evo_commands(
        &server,
        evo,
        TxJsonParams {
            key_io: KeyIoParams {
                pubkey_address: 140,
                script_address: 19,
                secret_key: 239,
                ext_public_key: [0; 4],
                ext_secret_key: [0; 4],
                platform_hrp: Some("tdash".into()),
            },
            service: false,
        },
    );
    server.start();
    server.set_warmup_finished();
    server
}

fn execute(server: &RpcServer, method: &str, params: Vec<UniValue>) -> Result<UniValue, RpcError> {
    execute_params(server, method, UniValue::Arr(params))
}

fn execute_params(
    server: &RpcServer,
    method: &str,
    params: UniValue,
) -> Result<UniValue, RpcError> {
    server.execute(&JsonRpcRequest {
        method: method.into(),
        params,
        ..Default::default()
    })
}

#[test]
fn family_usage_and_unknown_subcommands_match_core() {
    let server = server();
    for family in ["protx", "quorum"] {
        let help = dash_rpc_spec::command(family).unwrap().help;
        for args in [vec![], vec!["foo".into(), "bar".into()], vec![1.into(), UniValue::Null]] {
            let error = execute(&server, family, args).unwrap_err();
            assert_eq!(error.code(), RPC_MISC_ERROR);
            assert_eq!(error.to_object(), RpcError::Runtime(help.into()).to_object());
        }
        for sub in ["foo", "dkginfo"] {
            let error = execute(&server, family, vec![sub.into()]).unwrap_err();
            assert_eq!(
                error.to_object(),
                RpcError::new(RPC_INVALID_PARAMETER, "Must be a valid command").to_object()
            );
        }
        for value in [5.into(), UniValue::Null, true.into(), UniValue::new_object()] {
            assert_eq!(execute(&server, family, vec![value]).unwrap_err().code(), RPC_TYPE_ERROR);
        }
        for (params, code) in [
            (UniValue::Obj(vec![]), RPC_MISC_ERROR),
            (UniValue::Obj(vec![("command".into(), "foo".into())]), RPC_INVALID_PARAMETER),
            (UniValue::Obj(vec![("command".into(), 5.into())]), RPC_TYPE_ERROR),
        ] {
            assert_eq!(execute_params(&server, family, params).unwrap_err().code(), code);
        }
        assert_eq!(execute(&server, "help", vec![family.into()]).unwrap(), help.into());
    }
}

#[test]
fn every_command_dispatches_through_spec_checks() {
    let server = server();
    let hash: UniValue = "0".repeat(64).into();
    for (name, params) in [
        ("protx list", vec!["registered".into(), false.into(), 1.into()]),
        ("protx info", vec![hash.clone(), hash.clone()]),
        ("protx listdiff", vec![1.into(), 1.into()]),
        ("protx diff", vec![1.into(), 1.into(), false.into()]),
        ("quorum list", vec![1.into()]),
        ("quorum listextended", vec![1.into()]),
        ("quorum info", vec![100.into(), hash, false.into()]),
        ("quorum dkgstatus", vec![1.into()]),
    ] {
        let spec = dash_rpc_spec::command(name).unwrap();
        let (family, sub) = name.split_once(' ').unwrap();
        let invoke = |params: Vec<UniValue>| {
            let direct = execute(&server, name, params.clone()).unwrap_err();
            let dispatched =
                execute(&server, family, [vec![sub.into()], params].concat()).unwrap_err();
            assert_eq!(direct.to_object(), dispatched.to_object(), "{name}");
            direct
        };
        assert_eq!(invoke(params.clone()).code(), RPC_INTERNAL_ERROR, "{name}");
        let required =
            spec.args.iter().rposition(|a| !a.fallback.is_optional()).map_or(0, |i| i + 1);
        let missing = invoke(vec![]);
        assert_eq!(
            missing.code(),
            if required == 0 {
                RPC_INTERNAL_ERROR
            } else {
                RPC_MISC_ERROR
            },
            "{name} missing"
        );
        if required > 0 {
            assert_eq!(
                invoke(params[..required - 1].to_vec()).to_object(),
                RpcError::Runtime(spec.help.into()).to_object(),
                "{name} missing last required"
            );
        }
        for mut extra in [params.clone(), vec![UniValue::new_object(); params.len()]] {
            extra.push(UniValue::Null);
            assert_eq!(
                invoke(extra).to_object(),
                RpcError::Runtime(spec.help.into()).to_object(),
                "{name} excess or combined invalid"
            );
        }
        let named: Vec<_> = spec
            .args
            .iter()
            .zip(&params)
            .map(|(arg, value)| (arg.names.split('|').next().unwrap().into(), value.clone()))
            .collect();
        assert_eq!(
            execute_params(&server, name, UniValue::Obj(named.clone())).unwrap_err().code(),
            RPC_INTERNAL_ERROR,
            "{name} named"
        );
        let squashed = [("command".into(), sub.into())].into_iter().chain(named).collect();
        assert_eq!(
            execute_params(&server, family, UniValue::Obj(squashed)).unwrap_err().code(),
            RPC_INTERNAL_ERROR,
            "{name} squashed object"
        );
        for (i, arg) in spec.args.iter().enumerate() {
            for value in [UniValue::new_object(), UniValue::Null] {
                let optional_null = arg.fallback.is_optional() && value.is_null();
                let mut invalid = params.clone();
                invalid[i] = value.clone();
                let error = invoke(invalid);
                let expected = if arg.skip_type_check && name == "protx listdiff" && i == 0 {
                    RPC_MISC_ERROR
                } else if arg.skip_type_check && name == "protx diff" && i == 0 {
                    RPC_TYPE_ERROR
                } else if arg.skip_type_check
                    && !value.is_null()
                    && matches!(name, "protx list" | "quorum info")
                {
                    RPC_INVALID_PARAMETER
                } else if arg.skip_type_check || optional_null {
                    RPC_INTERNAL_ERROR
                } else {
                    RPC_TYPE_ERROR
                };
                assert_eq!(error.code(), expected, "{name} {} {value:?}", arg.names);
                let mut invalid_named: Vec<_> = spec
                    .args
                    .iter()
                    .zip(&params)
                    .map(|(arg, value)| {
                        (arg.names.split('|').next().unwrap().into(), value.clone())
                    })
                    .collect();
                invalid_named[i].1 = value;
                assert_eq!(
                    execute_params(&server, name, UniValue::Obj(invalid_named)).unwrap_err().code(),
                    expected,
                    "{name} named {}",
                    arg.names
                );
            }
        }
    }
    assert_eq!(
        execute(&server, "quorum", vec!["rotationinfo".into()]).unwrap_err().code(),
        RPC_INVALID_PARAMETER
    );
    assert_eq!(execute(&server, "not_registered", vec![]).unwrap_err().code(), -32601);
}

#[test]
fn parse_bool_v_matches_core() {
    for permissive in [false, true] {
        for value in [false, true] {
            assert_eq!(parse_bool_v(&value.into(), "detailed", permissive).unwrap(), value);
        }
    }
    for (text, value) in [
        ("true", true),
        ("TRUE", true),
        ("yEs", true),
        ("1", true),
        ("false", false),
        ("FALSE", false),
        ("No", false),
        ("0", false),
    ] {
        assert_eq!(parse_bool_v(&text.into(), "detailed", true).unwrap(), value);
    }
    for (text, value) in [("1", true), ("0", false), ("-0", false)] {
        assert_eq!(parse_bool_v(&UniValue::Num(text.into()), "detailed", true).unwrap(), value);
    }
    for value in
        [UniValue::Null, UniValue::new_array(), UniValue::new_object(), "yes".into(), 1.into()]
    {
        assert_eq!(parse_bool_v(&value, "detailed", false).unwrap_err().to_object(), RpcError::new(RPC_INVALID_PARAMETER, "detailed must be a JSON boolean. Pass -deprecatedrpc=permissive_bool to allow legacy boolean parsing.").to_object());
    }
    for (value, text) in [
        ("MAYBE".into(), "maybe"),
        (" true".into(), " true"),
        (2.into(), "2"),
        (UniValue::Null, ""),
        (UniValue::new_object(), ""),
        (UniValue::new_array(), ""),
    ] {
        assert_eq!(
            parse_bool_v(&value, "extended", true).unwrap_err().to_object(),
            RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("extended must be true, false, yes, no, 1 or 0 (not '{text}')")
            )
            .to_object()
        );
    }
    for text in ["1.0", "1e0", "2147483648", "-2147483649"] {
        assert_eq!(
            parse_bool_v(&UniValue::Num(text.into()), "includeSkShare", true)
                .unwrap_err()
                .to_object(),
            RpcError::Runtime("JSON integer out of range".into()).to_object()
        );
    }
}

#[test]
fn evo_errors_keep_storage_failures_and_rpc_block_policies() {
    let value: UniValue = "AB".repeat(32).into();
    let arg = diff::block_arg(&value, "baseBlock", false);
    let hash = BlockHash::from_hex(&"ab".repeat(32)).unwrap();
    assert_eq!(
        protx::info_error(arg.failure(BlockFailure::NotFound(hash))).to_object(),
        RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found").to_object()
    );
    assert_eq!(
        diff::diff_error(arg.failure(BlockFailure::NotFound(hash))).to_object(),
        RpcError::Runtime(format!("block {hash} not found")).to_object()
    );
    for failure in [BlockFailure::NotFound(hash), BlockFailure::BadArg] {
        assert_eq!(
            protx::list_diff_error(arg.failure(failure)).to_object(),
            bad_block_arg("baseBlock", &value).to_object()
        );
    }
    for value in [1.into(), "0".into(), UniValue::Null, UniValue::new_object()] {
        let arg = diff::block_arg(&value, "block", false);
        assert_eq!(
            diff::diff_error(arg.failure(BlockFailure::BadArg)).to_object(),
            bad_block_arg("block", &value).to_object()
        );
    }
    for store in [
        StoreError::ListUnavailable(hash),
        StoreError::read("fixture"),
        StoreError::inconsistent("fixture"),
    ] {
        let message = store.to_string();
        let error = EvoError::from(store);
        assert!(matches!(error, EvoError::Store(_) | EvoError::ListUnavailable(_)));
        assert_eq!(protx::info_error(error).to_object(), RpcError::Runtime(message).to_object());
    }
    let param = RpcError::new(-8, "deferred parameter error");
    for mapper in [diff::diff_error, protx::list_diff_error, protx::info_error] {
        assert_eq!(mapper(EvoError::Param(param.clone())), param);
    }
    assert_eq!(EvoError::from(StoreError::NotBuilt("historical lists")), EvoError::Unavailable);
    assert_eq!(protx::info_error(EvoError::Unavailable).code(), RPC_INTERNAL_ERROR);
}

#[test]
fn bare_commitment_fixture_has_core_keys_and_version_encoding() {
    let bytes =
        parse_hex(include_str!("../../tests/data/evo/bare-commitment-v3.hex").trim()).unwrap();
    let mut input = bytes.as_slice();
    let commitment = P2pCommitment::new(Commitment::decode(&mut input).unwrap());
    assert!(input.is_empty());
    assert_eq!(commitment.version(), 3);
    let value = commitment_to_json(&commitment);
    assert_eq!(
        value,
        UniValue::read(include_bytes!("../../tests/data/evo/bare-commitment-v3.json")).unwrap()
    );
    let UniValue::Obj(fields) = &value else {
        panic!("bare commitment object")
    };
    assert_eq!(
        fields.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
        [
            "version",
            "llmqType",
            "quorumHash",
            "quorumIndex",
            "signersCount",
            "signers",
            "validMembersCount",
            "validMembers",
            "quorumPublicKey",
            "quorumVvecHash",
            "quorumSig",
            "membersSig",
        ]
    );
    let mut encoded = Vec::new();
    commitment.encode(&mut encoded);
    assert_eq!(encoded, bytes);
}

#[derive(Default)]
struct Scripted {
    fault: Mutex<Option<EvoError>>,
    calls: Mutex<usize>,
}

fn hash(byte: u8) -> BlockHash {
    BlockHash::from([byte; 32])
}

impl Scripted {
    fn enter(&self) -> Result<(), EvoError> {
        *self.calls.lock().unwrap() += 1;
        self.fault.lock().unwrap().take().map_or(Ok(()), Err)
    }

    fn height_hash(height: i32) -> Option<BlockHash> {
        (0..=10).contains(&height).then(|| {
            hash(if height == 0 {
                0xaa
            } else {
                height as u8
            })
        })
    }

    fn lookup(hash_value: BlockHash, genesis_alias: bool) -> Option<(i32, bool)> {
        if genesis_alias && hash_value == hash(0) {
            return Some((0, true));
        }
        if hash_value == hash(0x77) {
            return Some((4, false));
        }
        (0..=10)
            .find(|height| Self::height_hash(*height) == Some(hash_value))
            .map(|height| (height, true))
    }

    fn resolve(arg: &BlockArg, genesis_alias: bool) -> Result<(i32, bool), EvoError> {
        let hash = arg.hash(10, Self::height_hash)?;
        Self::lookup(hash, genesis_alias).ok_or_else(|| arg.failure(BlockFailure::NotFound(hash)))
    }
}

impl NodeEvo for Scripted {
    fn permissive_bool(&self) -> bool {
        false
    }
    fn mn_list(&self, request: MnListRequest) -> Result<MnListAt, EvoError> {
        self.enter()?;
        match request {
            MnListRequest::Height {
                height,
                ..
            } if !(1..=10).contains(&height.unwrap_or(10)) => Err(EvoError::InvalidHeight),
            MnListRequest::Info {
                pro_tx_hash,
                block,
            } => {
                if let Some(block) = block {
                    let arg = BlockArg {
                        name: "blockHash",
                        value_str: block.to_string(),
                        syntax: Ok(BlockRef::Hash(block)),
                    };
                    let (_, active) = Self::resolve(&arg, false)?;
                    if !active {
                        return Err(EvoError::NotInMainChain);
                    }
                }
                Err(EvoError::MasternodeNotFound(pro_tx_hash))
            }
            _ => Err(EvoError::Unavailable),
        }
    }
    fn mn_list_diff(&self, base: BlockArg, block: BlockArg) -> Result<MnListDiffFacts, EvoError> {
        self.enter()?;
        let resolve = |arg: &BlockArg| {
            Self::resolve(arg, false).map_err(|_| arg.failure(BlockFailure::BadArg))
        };
        let (_, base_active) = resolve(&base)?;
        let (_, block_active) = resolve(&block)?;
        if !base_active || !block_active {
            return Err(EvoError::NotInMainChain);
        }
        Err(EvoError::Unavailable)
    }
    fn sml_diff(
        &self,
        base: BlockArg,
        block: BlockArg,
        extended: Result<bool, RpcError>,
    ) -> Result<SmlDiffFacts, EvoError> {
        self.enter()?;
        let base_hash = base.hash(10, Self::height_hash)?;
        let block_hash = block.hash(10, Self::height_hash)?;
        let _extended = extended.map_err(EvoError::Param)?;
        let (base_height, base_active) = Self::lookup(base_hash, true)
            .ok_or_else(|| base.failure(BlockFailure::NotFound(base_hash)))?;
        let (block_height, block_active) = Self::lookup(block_hash, false)
            .ok_or_else(|| block.failure(BlockFailure::NotFound(block_hash)))?;
        if !base_active || !block_active {
            return Err(EvoError::Failed(format!(
                "block {base_hash} and {block_hash} are not in the same chain"
            )));
        }
        if base_height > block_height {
            return Err(EvoError::Failed(format!(
                "base block {base_hash} is higher then block {block_hash}"
            )));
        }
        Ok(SmlDiffFacts {
            base_hash_echo: base_hash,
            block_hash,
            cbtx_hex: String::new(),
            merkle_tree_hex: String::new(),
            deleted: vec![],
            entries: vec![],
            deleted_quorums: vec![],
            new_quorums: vec![],
            roots: None,
            cl_sigs: vec![],
            bls: BlsCtx::BASIC,
        })
    }
    fn quorum_list(&self, _count: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
        self.enter()?;
        Err(EvoError::Unavailable)
    }
    fn quorum_list_extended(&self, _height: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
        self.enter()?;
        Err(EvoError::Unavailable)
    }
    fn llmq_type_known(&self, llmq_type: u8) -> bool {
        llmq_type == 100
    }
    fn quorum_info(
        &self,
        llmq_type: u8,
        hash_value: BlockHash,
        _include_sk_share: bool,
    ) -> Result<QuorumFacts, EvoError> {
        self.enter()?;
        if hash_value != hash(1) {
            return Err(EvoError::QuorumNotFound);
        }
        Ok(QuorumFacts {
            type_name: llmq_type.to_string(),
            rotated: false,
            base_height: 1,
            hash: hash_value,
            index: 0,
            mined_block: hash(2),
            prev_failures: None,
            members: vec![],
            valid_count: 0,
            single_member: false,
            bls: BlsCtx::BASIC,
            quorum_pk: [0; 48],
            pub_key_shares: vec![],
            sk_share: None,
        })
    }
}

fn strict_bool_error(name: &str) -> RpcError {
    RpcError::new(
        -8,
        format!(
            "{name} must be a JSON boolean. Pass -deprecatedrpc=permissive_bool to allow legacy boolean parsing."
        ),
    )
}

fn hash_length_error(name: &str, text: &str) -> RpcError {
    RpcError::new(-8, format!("{name} must be of length 64 (not {}, for '{text}')", text.len()))
}

#[test]
fn diff_and_listdiff_nonstring_blocks_match_core() {
    let scripted = server_with(Arc::new(Scripted::default()));
    for value in [true.into(), UniValue::Null, UniValue::new_array(), UniValue::new_object()] {
        let expected = RpcError::Type(format!(
            "JSON value of type {} is not of expected type string",
            value.value_type().name()
        ));
        for (method, params, label) in [
            ("protx diff", vec![value.clone(), 1.into()], "diff baseBlock"),
            ("protx diff", vec![1.into(), value.clone()], "diff block"),
        ] {
            assert_eq!(
                execute(&scripted, method, params).unwrap_err().to_object(),
                expected.to_object(),
                "{label} {value:?}"
            );
        }
        for (method, params, name, label) in [
            ("protx listdiff", vec![value.clone(), 1.into()], "baseBlock", "listdiff baseBlock"),
            ("protx listdiff", vec![1.into(), value.clone()], "block", "listdiff block"),
        ] {
            assert_eq!(
                execute(&scripted, method, params).unwrap_err().to_object(),
                bad_block_arg(name, &value).to_object(),
                "{label} {value:?}"
            );
        }
    }
}

#[test]
fn diff_base_resolution_precedes_block_type_error() {
    let node = Arc::new(Scripted::default());
    let server = server_with(node);
    let cases = [
        (
            11.into(),
            true.into(),
            RpcError::Runtime("baseBlock must be a block hash or chain height and not 11".into()),
        ),
        (
            "zz".into(),
            true.into(),
            RpcError::Runtime("baseBlock must be a block hash or chain height and not zz".into()),
        ),
        (
            0.into(),
            UniValue::new_object(),
            RpcError::Runtime("baseBlock must be a block hash or chain height and not 0".into()),
        ),
        (
            UniValue::Num("1.5".into()),
            true.into(),
            RpcError::Runtime("JSON integer out of range".into()),
        ),
    ];
    for (base, block, expected) in cases {
        assert_eq!(
            execute(&server, "protx diff", vec![base, block]).unwrap_err().to_object(),
            expected.to_object()
        );
    }
}

fn combined_invalid(case: usize) {
    let node = Arc::new(Scripted::default());
    let server = server_with(node.clone());
    let a: UniValue = hash(0xab).to_string().into();
    let b: UniValue = hash(0xcd).to_string().into();
    let known: UniValue = hash(1).to_string().into();
    let side: UniValue = hash(0x77).to_string().into();
    let bad = |name, text| {
        RpcError::Runtime(format!("{name} must be a block hash or chain height and not {text}"))
    };
    let bool_type =
        || RpcError::Type("JSON value of type bool is not of expected type string".into());
    let (method, params, expected) = match case {
        1 => ("protx diff", vec![0.into(), 1.into(), "x".into()], bad("baseBlock", "0")),
        2 => ("protx diff", vec![11.into(), "zz".into()], bad("baseBlock", "11")),
        3 => ("protx diff", vec![1.into(), "zz".into(), "x".into()], bad("block", "zz")),
        4 => ("protx diff", vec![true.into(), "zz".into()], bool_type()),
        5 => ("protx diff", vec![1.into(), true.into()], bool_type()),
        6 => ("protx diff", vec![a, b, "x".into()], strict_bool_error("extended")),
        7 => (
            "protx diff",
            vec![a, b, true.into()],
            RpcError::Runtime(format!("block {} not found", hash(0xab))),
        ),
        8 => (
            "protx diff",
            vec![known, b],
            RpcError::Runtime(format!("block {} not found", hash(0xcd))),
        ),
        9 => (
            "protx diff",
            vec![hash(0xab).to_string().to_ascii_uppercase().into(), 1.into()],
            RpcError::Runtime(format!("block {} not found", hash(0xab))),
        ),
        10 => {
            let facts = diff::read_diff(
                &JsonRpcRequest {
                    params: UniValue::Arr(vec![hash(0).to_string().into(), 5.into()]),
                    ..Default::default()
                },
                node.as_ref(),
            )
            .unwrap();
            assert_eq!(facts.base_hash_echo, hash(0));
            assert_eq!(facts.block_hash, hash(5));
            return;
        }
        11 => ("protx listdiff", vec![a, b], bad("baseBlock", &hash(0xab).to_string())),
        12 => ("protx listdiff", vec![1.into(), b], bad("block", &hash(0xcd).to_string())),
        13 => ("protx listdiff", vec![11.into(), "zz".into()], bad("baseBlock", "11")),
        14 => {
            for (value, text) in [(true, "1"), (false, "")] {
                let error =
                    execute(&server, "protx listdiff", vec![value.into(), 1.into()]).unwrap_err();
                assert_eq!(error.to_object(), bad("baseBlock", text).to_object());
            }
            return;
        }
        15 => ("protx listdiff", vec![side, b], bad("block", &hash(0xcd).to_string())),
        16 => (
            "protx listdiff",
            vec![side, 5.into()],
            RpcError::new(-8, "Block is not in main chain"),
        ),
        17 => ("quorum info", vec![255.into(), "x".into()], RpcError::new(-8, "invalid LLMQ type")),
        18 => ("quorum info", vec![100.into(), "x".into()], hash_length_error("quorumHash", "x")),
        19 => ("quorum info", vec![100.into(), a, "x".into()], strict_bool_error("includeSkShare")),
        20 => (
            "quorum info",
            vec![356.into(), known],
            RpcError::Runtime("JSON integer out of range".into()),
        ),
        21 => (
            "protx list",
            vec!["foo".into(), "x".into()],
            RpcError::new(-8, "invalid type specified"),
        ),
        22 => (
            "protx list",
            vec!["registered".into(), "x".into(), 999.into()],
            strict_bool_error("detailed"),
        ),
        23 => (
            "protx list",
            vec!["registered".into(), true.into(), 0.into()],
            RpcError::new(-8, "invalid height specified"),
        ),
        24 => ("protx info", vec!["zz".into(), "yy".into()], hash_length_error("proTxHash", "zz")),
        25 => ("protx info", vec![known, "yy".into()], hash_length_error("blockHash", "yy")),
        26 => ("protx info", vec![a, b], RpcError::new(-5, "Block not found")),
        27 => {
            for (method, params) in [
                ("protx list", vec![]),
                ("protx info", vec![known.clone()]),
                ("protx listdiff", vec![1.into(), 5.into()]),
                ("protx diff", vec![1.into(), 5.into()]),
                ("quorum list", vec![]),
                ("quorum listextended", vec![]),
                ("quorum info", vec![100.into(), known]),
            ] {
                for fault in [
                    EvoError::ListUnavailable("list unavailable fixture".into()),
                    EvoError::Store("store fixture".into()),
                ] {
                    let expected = match &fault {
                        EvoError::ListUnavailable(text) | EvoError::Store(text) => text.clone(),
                        _ => unreachable!(),
                    };
                    *node.fault.lock().unwrap() = Some(fault);
                    assert_eq!(
                        execute(&server, method, params.clone()).unwrap_err().to_object(),
                        RpcError::Runtime(expected).to_object(),
                        "{method}"
                    );
                    let before = *node.calls.lock().unwrap();
                    let _ = execute(&server, method, params.clone());
                    assert_eq!(
                        *node.calls.lock().unwrap(),
                        before + 1,
                        "node still answers {method}"
                    );
                }
            }
            return;
        }
        _ => unreachable!(),
    };
    assert_eq!(
        execute(&server, method, params).unwrap_err().to_object(),
        expected.to_object(),
        "D2-{case}"
    );
}

macro_rules! combined_invalid_tests {
    ($($name:ident => $case:literal),* $(,)?) => { $(#[test] fn $name() { combined_invalid($case); })* };
}
combined_invalid_tests! {
    d2_01_base_range_before_bool => 1,
    d2_02_base_range_before_target_syntax => 2,
    d2_03_target_syntax_before_bool => 3,
    d2_04_base_bool_runtime => 4,
    d2_05_target_bool_runtime => 5,
    d2_06_bool_before_hash_lookup => 6,
    d2_07_base_lookup_before_target => 7,
    d2_08_target_lookup_error => 8,
    d2_09_missing_hash_is_canonical => 9,
    d2_10_null_base_echoes_genesis_alias => 10,
    d2_11_listdiff_base_lookup => 11,
    d2_12_listdiff_target_lookup => 12,
    d2_13_listdiff_base_range => 13,
    d2_14_listdiff_bool_value_strings => 14,
    d2_15_listdiff_target_lookup_before_contains => 15,
    d2_16_listdiff_contains_error => 16,
    d2_17_quorum_type_before_hash => 17,
    d2_18_quorum_hash_syntax => 18,
    d2_19_quorum_bool_before_lookup => 19,
    d2_20_quorum_type_out_of_range => 20,
    d2_21_list_type_before_bool => 21,
    d2_22_list_bool_before_height => 22,
    d2_23_list_height_range => 23,
    d2_24_info_protx_hash_first => 24,
    d2_25_info_block_hash_second => 25,
    d2_26_info_block_before_masternode_lookup => 26,
    d2_27_store_failures_preserve_node_liveness => 27,
}

#[test]
fn block_argument_syntax_matches_core() {
    for (value, height) in [
        ("+1", 1),
        ("-1", -1),
        ("01", 1),
        ("-0", 0),
        ("2147483647", i32::MAX),
        ("-2147483648", i32::MIN),
    ] {
        assert_eq!(
            diff::block_arg(&value.into(), "block", false).syntax,
            Ok(BlockRef::Height(height))
        );
    }
    for value in ["+-1", "++1", " 1", "1 ", "1\0", "1.0", "1e0", "2147483648", "-2147483649", ""] {
        let value: UniValue = value.into();
        assert_eq!(
            diff::block_arg(&value, "block", false).syntax.unwrap_err(),
            bad_block_arg("block", &value)
        );
    }
    for value in ["1.0", "1e0", "2147483648"] {
        let value = UniValue::Num(value.into());
        assert_eq!(
            diff::block_arg(&value, "block", false).syntax.unwrap_err(),
            RpcError::Runtime("JSON integer out of range".into())
        );
        assert_eq!(
            diff::block_arg(&value, "block", true).syntax.unwrap_err(),
            bad_block_arg("block", &value)
        );
    }
    for (value, kind) in [
        (UniValue::Null, "null"),
        (UniValue::new_array(), "array"),
        (UniValue::new_object(), "object"),
        (true.into(), "bool"),
    ] {
        assert_eq!(
            diff::block_arg(&value, "block", false).syntax.unwrap_err(),
            RpcError::Type(format!("JSON value of type {kind} is not of expected type string"))
        );
        assert_eq!(
            diff::block_arg(&value, "block", true).syntax.unwrap_err(),
            bad_block_arg("block", &value)
        );
    }
}
