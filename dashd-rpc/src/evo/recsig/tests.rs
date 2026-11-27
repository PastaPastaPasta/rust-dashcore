use std::sync::{Arc, Mutex};

use dash_consensus::hash::sha256d;
use dash_evo::locks::RecoveredSig;
use dash_params::LlmqType;
use dash_primitives::{BlockHash, QuorumHash};

use super::*;
use crate::evo::{
    BlockArg, MnListAt, MnListDiffFacts, MnListRequest, QuorumFacts, QuorumListFacts, SmlDiffFacts,
    register_recsig_commands,
};
use crate::server::RpcServer;
use crate::testutil;

/// Regtest's LLMQ types for the type check; the reads come from `sigs`.
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

type Read = (&'static str, u8, [u8; 32], [u8; 32]);

/// One stored signature; `context` false is a node with no signing manager.
#[derive(Default)]
struct Sigs {
    context: bool,
    stored: Option<RecoveredSig>,
    fault: Option<EvoError>,
    reads: Mutex<Vec<Read>>,
}

impl Sigs {
    fn read<T>(
        &self,
        what: &'static str,
        t: u8,
        id: &[u8; 32],
        msg: &[u8; 32],
        answer: impl FnOnce(Option<&RecoveredSig>) -> T,
    ) -> Result<T, EvoError> {
        self.reads.lock().unwrap().push((what, t, *id, *msg));
        if let Some(fault) = &self.fault {
            return Err(fault.clone());
        }
        let same_id = self.stored.as_ref().filter(|s| s.llmq_type.to_base() == t && s.id == *id);
        Ok(answer(same_id))
    }
}

impl NodeRecSigs for Sigs {
    fn llmq_context(&self) -> Result<(), EvoError> {
        if self.context {
            Ok(())
        } else {
            Err(EvoError::LlmqUnavailable)
        }
    }
    fn has_recovered_sig(&self, t: u8, id: &[u8; 32], msg: &[u8; 32]) -> Result<bool, EvoError> {
        self.read("has", t, id, msg, |s| s.is_some_and(|s| s.msg_hash == *msg))
    }
    fn get_recovered_sig(
        &self,
        t: u8,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, EvoError> {
        self.read("get", t, id, msg, |s| s.filter(|s| s.msg_hash == *msg).cloned())
    }
    fn is_conflicting(&self, t: u8, id: &[u8; 32], msg: &[u8; 32]) -> Result<bool, EvoError> {
        self.read("conflicting", t, id, msg, |s| s.is_some_and(|s| s.msg_hash != *msg))
    }
}

const METHODS: [&str; 3] = ["quorum hasrecsig", "quorum getrecsig", "quorum isconflicting"];

fn stored() -> RecoveredSig {
    RecoveredSig {
        llmq_type: LlmqType::from_base(100),
        quorum_hash: QuorumHash::from([1; 32]),
        id: [2; 32],
        msg_hash: [3; 32],
        sig: core::array::from_fn(|i| i as u8),
    }
}

fn server(sigs: Sigs) -> (Arc<RpcServer>, Arc<Sigs>) {
    let server = Arc::new(RpcServer::new(Arc::new(testutil::FakeControl::default())));
    let sigs = Arc::new(sigs);
    register_recsig_commands(&server, Arc::new(Evo), Arc::clone(&sigs) as _);
    server.start();
    server.set_warmup_finished();
    (server, sigs)
}

fn with_sig() -> (Arc<RpcServer>, Arc<Sigs>) {
    server(Sigs {
        context: true,
        stored: Some(stored()),
        ..Sigs::default()
    })
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

fn display(bytes: [u8; 32]) -> String {
    BlockHash::from(bytes).to_string()
}

fn args(t: &str, id: [u8; 32], msg: [u8; 32]) -> String {
    format!(r#"[{t},"{}","{}"]"#, display(id), display(msg))
}

#[test]
fn reads_answer_from_the_signing_manager_by_type_id_and_message() {
    let (server, sigs) = with_sig();
    let ok = |method, a: &str| call(&server, method, a).unwrap();
    assert_eq!(ok("quorum hasrecsig", &args("100", [2; 32], [3; 32])), true.into());
    assert_eq!(ok("quorum hasrecsig", &args("100", [2; 32], [4; 32])), false.into());
    assert_eq!(ok("quorum hasrecsig", &args("104", [2; 32], [3; 32])), false.into());
    assert_eq!(ok("quorum isconflicting", &args("100", [2; 32], [4; 32])), true.into());
    assert_eq!(ok("quorum isconflicting", &args("100", [2; 32], [3; 32])), false.into());
    assert_eq!(ok("quorum isconflicting", &args("100", [5; 32], [4; 32])), false.into());
    assert_eq!(
        sigs.reads.lock().unwrap()[0],
        ("has", 100, [2; 32], [3; 32]),
        "id and msgHash are read in display order"
    );
    let named = format!(
        r#"{{"msgHash":"{}","id":"{}","llmqType":100}}"#,
        display([3; 32]),
        display([2; 32])
    );
    assert_eq!(ok("quorum hasrecsig", &named), true.into());
}

// Core: v24.0.0 src/llmq/core_write.cpp:103-113
#[test]
fn getrecsig_is_to_json_in_cores_field_order() {
    let (server, _) = with_sig();
    let sig = stored();
    let got = call(&server, "quorum getrecsig", &args("100", [2; 32], [3; 32])).unwrap();
    let mut want = UniValue::new_object();
    want.push_kv_end("llmqType", 100i64.into());
    want.push_kv_end("quorumHash", display([1; 32]).into());
    want.push_kv_end("id", display([2; 32]).into());
    want.push_kv_end("msgHash", display([3; 32]).into());
    want.push_kv_end("sig", crate::descriptor::hex(&sig.sig).into());
    want.push_kv_end("hash", display(sha256d(&sig.sig)).into());
    assert_eq!(got, want);
    assert_eq!(
        got.write(),
        format!(
            r#"{{"llmqType":100,"quorumHash":"{q}","id":"{i}","msgHash":"{m}","sig":"{s}","hash":"{h}"}}"#,
            q = "01".repeat(32),
            i = "02".repeat(32),
            m = "03".repeat(32),
            s = (0..96).map(|b| format!("{b:02x}")).collect::<String>(),
            h = display(sha256d(&sig.sig)),
        )
    );
    error(
        &server,
        "quorum getrecsig",
        &args("100", [2; 32], [4; 32]),
        -8,
        "recovered signature not found",
    );
}

// Core: v24.0.3 src/rpc/quorums.cpp:50-57, :743-749; src/rpc/server_util.cpp:113-119
#[test]
fn errors_follow_cores_order() {
    let (none, _) = server(Sigs::default());
    let (server, sigs) = with_sig();
    let good = display([2; 32]);
    for method in METHODS {
        for t in ["255", "356"] {
            let args = format!(r#"[{t},"x","y"]"#);
            error(&none, method, &args, -32603, "Node LLMQ context not found");
        }
        // Core: v24.0.3 test/functional/feature_llmq_signing.py:135-142
        for value in ["1.5", "1e0", "2147483648", "-1", "-0", "256", "356", "-156"] {
            error(
                &server,
                method,
                &format!(r#"[{value},"x","y"]"#),
                -1,
                "JSON integer out of range",
            );
        }
        for t in ["255", "0"] {
            error(&server, method, &format!(r#"[{t},"x","y"]"#), -8, "invalid LLMQ type");
        }
        error(&server, method, r#"[100,"x","y"]"#, -8, "id must be of length 64 (not 1, for 'x')");
        let bad = "g".repeat(64);
        error(
            &server,
            method,
            &format!(r#"[100,"{bad}","y"]"#),
            -8,
            &format!("id must be hexadecimal string (not '{bad}')"),
        );
        error(
            &server,
            method,
            &format!(r#"[100,"{good}","y"]"#),
            -8,
            "msgHash must be of length 64 (not 1, for 'y')",
        );
        error(
            &server,
            method,
            &format!(r#"[100,"{good}","{bad}"]"#),
            -8,
            &format!("msgHash must be hexadecimal string (not '{bad}')"),
        );
        error(&server, method, "[100]", -1, dash_rpc_spec::command(method).unwrap().help);
    }
    assert!(sigs.reads.lock().unwrap().is_empty(), "no read before the arguments parse");
}

#[test]
fn a_read_that_finds_the_manager_gone_or_failing_is_an_error() {
    for (fault, code, text) in [
        (EvoError::LlmqUnavailable, -32603, "Node LLMQ context not found"),
        (EvoError::Failed("read failed".into()), -1, "read failed"),
    ] {
        let (server, _) = server(Sigs {
            context: true,
            fault: Some(fault),
            ..Sigs::default()
        });
        for method in METHODS {
            error(&server, method, &args("100", [2; 32], [3; 32]), code, text);
        }
    }
}
