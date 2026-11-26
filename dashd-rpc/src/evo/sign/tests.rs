use std::sync::{Arc, Mutex};

use dash_primitives::BlockHash;

use super::*;
use crate::evo::{
    BlockArg, MnListAt, MnListDiffFacts, MnListRequest, QuorumFacts, QuorumListFacts, SmlDiffFacts,
    register_sign_commands,
};
use crate::server::RpcServer;
use crate::testutil;

struct Evo;

impl NodeEvo for Evo {
    fn permissive_bool(&self) -> bool {
        false
    }
    fn mn_list(&self, _: MnListRequest) -> Result<MnListAt, EvoError> {
        unreachable!()
    }
    fn mn_list_diff(&self, _: BlockArg, _: BlockArg) -> Result<MnListDiffFacts, EvoError> {
        unreachable!()
    }
    fn sml_diff(
        &self,
        _: BlockArg,
        _: BlockArg,
        _: Result<bool, RpcError>,
    ) -> Result<SmlDiffFacts, EvoError> {
        unreachable!()
    }
    fn quorum_list(&self, _: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
        unreachable!()
    }
    fn quorum_list_extended(&self, _: Option<i32>) -> Result<Vec<QuorumListFacts>, EvoError> {
        unreachable!()
    }
    fn llmq_type_known(&self, llmq_type: u8) -> bool {
        [100, 104, 106].contains(&llmq_type)
    }
    fn quorum_info(&self, _: u8, _: BlockHash, _: bool) -> Result<QuorumFacts, EvoError> {
        unreachable!()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Sign(SignArgs, bool),
    Share(SignArgs),
    Select(u8, [u8; 32]),
    Verify(VerifyArgs),
}

/// An active masternode whose only quorum is `[9; 32]`, unless `masternode` or `context` is off.
struct Node {
    masternode: bool,
    context: bool,
    share: Option<OwnSigShare>,
    fault: Option<EvoError>,
    calls: Mutex<Vec<Call>>,
}

impl Default for Node {
    fn default() -> Self {
        Self {
            masternode: true,
            context: true,
            share: Some(share()),
            fault: None,
            calls: Mutex::default(),
        }
    }
}

impl Node {
    fn record<T>(&self, call: Call, answer: T) -> Result<T, EvoError> {
        self.calls.lock().unwrap().push(call);
        self.fault.clone().map_or(Ok(answer), Err)
    }
}

impl NodeQuorumSign for Node {
    fn masternode_mode(&self) -> bool {
        self.masternode
    }
    fn llmq_context(&self) -> Result<(), EvoError> {
        if self.context {
            Ok(())
        } else {
            Err(EvoError::LlmqUnavailable)
        }
    }
    fn platform_type(&self) -> u8 {
        106
    }
    fn async_sign_if_member(&self, args: SignArgs, allow_diff: bool) -> Result<bool, EvoError> {
        self.record(Call::Sign(args, allow_diff), args.quorum_hash.is_none_or(|q| q == [9; 32]))
    }
    fn create_sig_share(&self, args: SignArgs) -> Result<Option<Option<OwnSigShare>>, EvoError> {
        let found = args.quorum_hash.is_none_or(|q| q == [9; 32]);
        self.record(Call::Share(args), found.then(|| self.share.clone()))
    }
    fn select_quorum(&self, t: u8, id: &[u8; 32]) -> Result<Option<SelectedQuorum>, EvoError> {
        let selected = (t != 104).then(|| SelectedQuorum {
            quorum_hash: [9; 32],
            recovery_members: vec![[5; 32], [6; 32]],
        });
        self.record(Call::Select(t, *id), selected)
    }
    fn signature_readable(&self, signature: &[u8; 96]) -> bool {
        signature[0] != 0
    }
    /// Quorum `[9; 32]` signed `[2; 32]`; the latest quorums at the tip or height 50 hold it.
    fn verify(&self, args: VerifyArgs) -> Result<Option<bool>, EvoError> {
        let signed = args.msg_hash == [2; 32];
        let answer = match args.at {
            VerifyAt::Quorum(q) => (q == [9; 32]).then_some(signed),
            VerifyAt::Height(h) => Some(signed && (h == -1 || h == 50)),
        };
        self.record(Call::Verify(args), answer)
    }
}

fn share() -> OwnSigShare {
    OwnSigShare {
        quorum_hash: [9; 32],
        member: 3,
        sign_hash: [7; 32],
        signature: (0..96).collect(),
    }
}

fn serve(node: Node) -> (Arc<RpcServer>, Arc<Node>) {
    let server = Arc::new(RpcServer::new(Arc::new(testutil::FakeControl::default())));
    let node = Arc::new(node);
    register_sign_commands(&server, Arc::new(Evo), Arc::clone(&node) as _);
    server.start();
    server.set_warmup_finished();
    (server, node)
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

fn d(bytes: [u8; 32]) -> String {
    BlockHash::from(bytes).to_string()
}

fn sign_args(t: u8, quorum_hash: Option<[u8; 32]>) -> SignArgs {
    SignArgs {
        llmq_type: t,
        id: [1; 32],
        msg_hash: [2; 32],
        quorum_hash,
    }
}

#[test]
fn submit_signs_through_the_share_manager_with_the_quorum_hash_optional() {
    let (server, node) = serve(Node::default());
    let (id, msg, q) = (d([1; 32]), d([2; 32]), d([9; 32]));
    for (args, want) in [
        (format!(r#"[100,"{id}","{msg}"]"#), true),
        (format!(r#"[100,"{id}","{msg}",""]"#), true),
        (format!(r#"[100,"{id}","{msg}",null,true]"#), true),
        (format!(r#"[100,"{id}","{msg}","{q}"]"#), true),
        (format!(r#"[100,"{id}","{msg}","{msg}"]"#), false),
        (format!(r#"[356,"{id}","{msg}"]"#), true),
    ] {
        assert_eq!(call(&server, "quorum sign", &args).unwrap(), want.into(), "{args}");
    }
    assert_eq!(
        call(&server, "quorum platformsign", &format!(r#"["{id}","{msg}"]"#)),
        Ok(true.into())
    );
    let none = sign_args(100, None);
    assert_eq!(
        *node.calls.lock().unwrap(),
        [
            Call::Sign(none, false),
            Call::Sign(none, false),
            Call::Sign(none, false),
            Call::Sign(sign_args(100, Some([9; 32])), false),
            Call::Sign(sign_args(100, Some([2; 32])), false),
            Call::Sign(none, false),
            Call::Sign(sign_args(106, None), true),
        ],
        "only the platform type may sign a different message hash"
    );
}

// Core: v24.0.0 src/rpc/quorums.cpp:560-583
#[test]
fn no_submit_returns_the_share_in_cores_field_order() {
    let (server, node) = serve(Node::default());
    let (id, msg, q) = (d([1; 32]), d([2; 32]), d([9; 32]));
    let got =
        call(&server, "quorum sign", &format!(r#"[356,"{id}","{msg}","{q}",false]"#)).unwrap();
    assert_eq!(
        got.write(),
        format!(
            r#"{{"llmqType":100,"quorumHash":"{q}","quorumMember":3,"id":"{id}","msgHash":"{msg}","signHash":"{s}","signature":"{sig}"}}"#,
            s = d([7; 32]),
            sig = (0..96).map(|b| format!("{b:02x}")).collect::<String>(),
        )
    );
    let selected = call(&server, "quorum sign", &format!(r#"[100,"{id}","{msg}","",false]"#));
    assert_eq!(selected, Ok(got));
    let platform = call(&server, "quorum platformsign", &format!(r#"["{id}","{msg}","",false]"#));
    assert_eq!(field(&platform.unwrap(), "llmqType"), 106i64.into());
    assert_eq!(
        *node.calls.lock().unwrap(),
        [
            Call::Share(sign_args(100, Some([9; 32]))),
            Call::Share(sign_args(100, None)),
            Call::Share(sign_args(106, None)),
        ]
    );
    error(
        &server,
        "quorum sign",
        &format!(r#"[100,"{id}","{msg}","{id}",false]"#),
        -8,
        "quorum not found",
    );
    let (none, _) = serve(Node {
        share: None,
        ..Node::default()
    });
    error(
        &none,
        "quorum sign",
        &format!(r#"[100,"{id}","{msg}","",false]"#),
        -8,
        "failed to create sigShare",
    );
}

fn field(v: &UniValue, key: &str) -> UniValue {
    let UniValue::Obj(fields) = v else {
        panic!("{}", v.write())
    };
    fields.iter().find(|(k, _)| k == key).unwrap().1.clone()
}

// Core: v24.0.0 src/rpc/quorums.cpp:514-545, :621-625
#[test]
fn sign_errors_follow_cores_order() {
    let (id, msg) = (d([1; 32]), d([2; 32]));
    let (not_mn, _) = serve(Node {
        masternode: false,
        context: false,
        ..Node::default()
    });
    error(&not_mn, "quorum sign", r#"[1.5,"x","y"]"#, -1, "JSON integer out of range");
    for (method, args) in
        [("quorum sign", r#"[255,"x","y"]"#), ("quorum platformsign", r#"["x","y"]"#)]
    {
        error(&not_mn, method, args, -32603, "Only available in masternode mode.");
    }
    let (no_context, _) = serve(Node {
        context: false,
        ..Node::default()
    });
    error(&no_context, "quorum sign", r#"[255,"x","y"]"#, -32603, "Node LLMQ context not found");
    let (server, node) = serve(Node::default());
    for t in ["255", "-1", "0"] {
        error(&server, "quorum sign", &format!(r#"[{t},"x","y"]"#), -8, "invalid LLMQ type");
    }
    let bad = "g".repeat(64);
    for (method, prefix) in [("quorum sign", "100,"), ("quorum platformsign", "")] {
        let e = |args: String, text: &str| {
            error(&server, method, &format!("[{prefix}{args}]"), -8, text)
        };
        e(r#""x","y""#.into(), "id must be of length 64 (not 1, for 'x')");
        e(format!(r#""{bad}","y""#), &format!("id must be hexadecimal string (not '{bad}')"));
        e(format!(r#""{id}","y""#), "msgHash must be of length 64 (not 1, for 'y')");
        e(format!(r#""{id}","{msg}","z""#), "quorumHash must be of length 64 (not 1, for 'z')");
        e(
            format!(r#""{id}","{msg}","","1""#),
            "submit must be a JSON boolean. Pass -deprecatedrpc=permissive_bool to allow legacy boolean parsing.",
        );
        error(&server, method, "[100]", -1, dash_rpc_spec::command(method).unwrap().help);
    }
    assert!(node.calls.lock().unwrap().is_empty(), "nothing is signed before the arguments parse");
}

// Core: v24.0.0 src/rpc/quorums.cpp:836-866
#[test]
fn selectquorum_answers_the_quorum_and_its_recovery_members() {
    let (server, node) = serve(Node {
        masternode: false,
        ..Node::default()
    });
    let id = d([1; 32]);
    assert_eq!(
        call(&server, "quorum selectquorum", &format!(r#"[356,"{id}"]"#)).unwrap().write(),
        format!(
            r#"{{"quorumHash":"{}","recoveryMembers":["{}","{}"]}}"#,
            d([9; 32]),
            d([5; 32]),
            d([6; 32])
        )
    );
    error(&server, "quorum selectquorum", &format!(r#"[104,"{id}"]"#), -1, "no quorums active");
    assert_eq!(
        *node.calls.lock().unwrap(),
        [Call::Select(100, [1; 32]), Call::Select(104, [1; 32])]
    );
    let (no_context, _) = serve(Node {
        context: false,
        ..Node::default()
    });
    error(
        &no_context,
        "quorum selectquorum",
        r#"[1.5,"x"]"#,
        -32603,
        "Node LLMQ context not found",
    );
    error(&server, "quorum selectquorum", r#"[1.5,"x"]"#, -1, "JSON integer out of range");
    error(&server, "quorum selectquorum", r#"[255,"x"]"#, -8, "invalid LLMQ type");
    error(
        &server,
        "quorum selectquorum",
        r#"[100,"x"]"#,
        -8,
        "id must be of length 64 (not 1, for 'x')",
    );
}

#[test]
fn a_fault_under_a_call_is_an_error_and_the_context_gone_is_cores() {
    for (fault, code, text) in [
        (EvoError::LlmqUnavailable, -32603, "Node LLMQ context not found"),
        (EvoError::Failed("lookup failed".into()), -1, "lookup failed"),
    ] {
        let (server, _) = serve(Node {
            fault: Some(fault),
            ..Node::default()
        });
        let (id, msg) = (d([1; 32]), d([2; 32]));
        for (method, args) in [
            ("quorum sign", format!(r#"[100,"{id}","{msg}"]"#)),
            ("quorum sign", format!(r#"[100,"{id}","{msg}","",false]"#)),
            ("quorum selectquorum", format!(r#"[100,"{id}"]"#)),
        ] {
            error(&server, method, &args, code, text);
        }
    }
}

fn verify_args(msg: u8, at: VerifyAt) -> Call {
    Call::Verify(VerifyArgs {
        llmq_type: 100,
        id: [1; 32],
        msg_hash: [msg; 32],
        signature: [7; 96],
        at,
    })
}

/// Both forms: the latest quorums at a height (the tip by default), when the quorum hash is
/// absent or empty with a height; else the quorum named.
// Core: v24.0.0 src/rpc/quorums.cpp:700-718
#[test]
fn verify_checks_the_latest_quorums_at_a_height_or_the_quorum_named() {
    let (server, node) = serve(Node {
        masternode: false,
        ..Node::default()
    });
    let (id, msg, other, q, sig) =
        (d([1; 32]), d([2; 32]), d([3; 32]), d([9; 32]), "07".repeat(96));
    for (args, want) in [
        (format!(r#"[356,"{id}","{msg}","{sig}"]"#), true),
        (format!(r#"[100,"{id}","{msg}","{sig}","",50]"#), true),
        (format!(r#"[100,"{id}","{msg}","{sig}",null,50]"#), true),
        (format!(r#"[100,"{id}","{msg}","{sig}","",7]"#), false),
        (format!(r#"[100,"{id}","{other}","{sig}"]"#), false),
        (format!(r#"[100,"{id}","{msg}","{sig}","{q}"]"#), true),
        (format!(r#"[100,"{id}","{msg}","{sig}","{q}",7]"#), true),
        (format!(r#"[100,"{id}","{other}","{sig}","{q}"]"#), false),
    ] {
        assert_eq!(call(&server, "quorum verify", &args), Ok(want.into()), "{args}");
    }
    let (height, quorum) = (VerifyAt::Height, VerifyAt::Quorum([9; 32]));
    assert_eq!(
        *node.calls.lock().unwrap(),
        [
            verify_args(2, height(-1)),
            verify_args(2, height(50)),
            verify_args(2, height(50)),
            verify_args(2, height(7)),
            verify_args(3, height(-1)),
            verify_args(2, quorum),
            verify_args(2, quorum),
            verify_args(3, quorum),
        ]
    );
    let args = format!(r#"[100,"{id}","{msg}","{sig}","{id}"]"#);
    error(&server, "quorum verify", &args, -8, "quorum not found");
}

// Core: v24.0.0 src/rpc/quorums.cpp:684-713
#[test]
fn verify_errors_follow_cores_order() {
    let (no_context, _) = serve(Node {
        context: false,
        ..Node::default()
    });
    error(
        &no_context,
        "quorum verify",
        r#"[1.5,"x","y","z"]"#,
        -32603,
        "Node LLMQ context not found",
    );
    let (server, node) = serve(Node::default());
    let (id, msg, sig) = (d([1; 32]), d([2; 32]), "07".repeat(96));
    let e =
        |args: String, code: i32, text: &str| error(&server, "quorum verify", &args, code, text);
    e(r#"[1.5,"x","y","z"]"#.into(), -1, "JSON integer out of range");
    e(r#"[255,"x","y","z"]"#.into(), -8, "invalid LLMQ type");
    e(r#"[100,"x","y","z"]"#.into(), -8, "id must be of length 64 (not 1, for 'x')");
    e(format!(r#"[100,"{id}","y","z"]"#), -8, "msgHash must be of length 64 (not 1, for 'y')");
    let format = "invalid signature format";
    for bad in ["z".to_owned(), "07".repeat(95), "07".repeat(97), format!("00{}", "07".repeat(95))]
    {
        e(format!(r#"[100,"{id}","{msg}","{bad}","z"]"#), -8, format);
    }
    let wrong = |position: &str, got: &str, want: &str| {
        format!(
            "Wrong type passed:\n{{\n    \"Position {position}\": \"JSON value of type {got} is not of expected type {want}\"\n}}"
        )
    };
    e(format!(r#"[100,"{id}","{msg}",7]"#), -3, &wrong("4 (signature)", "number", "string"));
    let empty = "quorumHash must be of length 64 (not 0, for '')";
    e(format!(r#"[100,"{id}","{msg}","{sig}",""]"#), -8, empty);
    e(format!(r#"[100,"{id}","{msg}","{sig}","",null]"#), -8, empty);
    e(
        format!(r#"[100,"{id}","{msg}","{sig}","z",7]"#),
        -8,
        "quorumHash must be of length 64 (not 1, for 'z')",
    );
    let quorum_hash = wrong("5 (quorumHash)", "number", "string");
    e(format!(r#"[100,"{id}","{msg}","{sig}",7,7]"#), -3, &quorum_hash);
    let sign_height = wrong("6 (signHeight)", "string", "number");
    e(format!(r#"[100,"{id}","{msg}","{sig}","","x"]"#), -3, &sign_height);
    error(
        &server,
        "quorum verify",
        "[100]",
        -1,
        dash_rpc_spec::command("quorum verify").unwrap().help,
    );
    assert!(
        node.calls.lock().unwrap().is_empty(),
        "nothing is verified before the arguments parse"
    );
}

#[test]
fn a_verify_fault_is_an_error_and_the_context_gone_is_cores() {
    let (id, msg, sig) = (d([1; 32]), d([2; 32]), "07".repeat(96));
    for (fault, code, text) in [
        (EvoError::LlmqUnavailable, -32603, "Node LLMQ context not found"),
        (EvoError::Failed("lookup failed".into()), -1, "lookup failed"),
    ] {
        let (server, _) = serve(Node {
            fault: Some(fault),
            ..Node::default()
        });
        for args in [
            format!(r#"[100,"{id}","{msg}","{sig}"]"#),
            format!(r#"[100,"{id}","{msg}","{sig}","{id}"]"#),
        ] {
            error(&server, "quorum verify", &args, code, text);
        }
    }
}
