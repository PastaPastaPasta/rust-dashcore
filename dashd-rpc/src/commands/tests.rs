use std::sync::atomic::Ordering;

use crate::UniValue;
use crate::protocol::RpcError;
use crate::request::JsonRpcRequest;
use crate::testutil::server;

fn call(
    server: &crate::server::RpcServer,
    method: &str,
    params: &str,
) -> Result<UniValue, RpcError> {
    server.execute(&JsonRpcRequest {
        method: method.into(),
        params: UniValue::read(params.as_bytes()).unwrap(),
        ..Default::default()
    })
}

fn ok(server: &crate::server::RpcServer, method: &str, params: &str) -> String {
    call(server, method, params).unwrap().write()
}

#[test]
fn help_is_the_v24_text_for_a_node_without_a_wallet() {
    let (server, _) = server();
    let text = |params| match call(&server, "help", params).unwrap() {
        UniValue::Str(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(text("[]"), dash_rpc_spec::help("", false));
    assert_eq!(text(r#"["stop"]"#), dash_rpc_spec::command("stop").unwrap().help);
    assert_eq!(text(r#"["bls","fromsecret"]"#), dash_rpc_spec::help("bls fromsecret", false));
    assert_eq!(text(r#"["nosuchcommand"]"#), "help: unknown command: nosuchcommand");
    // A wallet command is unknown without the wallet, whether or not anything serves it.
    assert_eq!(text(r#"["getbalance"]"#), dash_rpc_spec::help("getbalance", false));
    let err = call(&server, "help", "[1]").unwrap_err();
    assert_eq!(err.code(), crate::protocol::RPC_TYPE_ERROR);
    assert!(err.to_object().write().contains("Wrong type passed"));
}

#[test]
fn help_dumps_the_argument_conversions() {
    let (server, _) = server();
    assert_eq!(
        ok(&server, "help", r#"["dump_all_command_conversions"]"#),
        server.dump_arg_map().write()
    );
}

#[test]
fn stop_requests_shutdown_and_names_the_node() {
    let (server, control) = server();
    assert_eq!(ok(&server, "stop", "[]"), r#""rdashd stopping""#);
    assert!(control.shutdown.load(Ordering::SeqCst));
    // The hidden wait, in milliseconds.
    let started = std::time::Instant::now();
    assert_eq!(ok(&server, "stop", "[50]"), r#""rdashd stopping""#);
    assert!(started.elapsed() >= std::time::Duration::from_millis(50));
    let err = call(&server, "stop", "[1e10]").unwrap_err();
    assert_eq!(err.to_object().write(), r#"{"code":-1,"message":"JSON integer out of range"}"#);
}

#[test]
fn uptime_and_getrpcinfo() {
    let (server, _) = server();
    assert_eq!(ok(&server, "uptime", "[]"), "100");
    let info = ok(&server, "getrpcinfo", "[]");
    // getrpcinfo lists itself.
    let prefix = r#"{"active_commands":[{"method":"getrpcinfo","duration":"#;
    assert!(info.starts_with(prefix), "{info}");
    assert!(info.ends_with(r#"}],"logpath":"/data/regtest/debug.log"}"#), "{info}");
}

#[test]
fn logging_toggles_categories() {
    let (server, _) = server();
    assert_eq!(ok(&server, "logging", "[]"), r#"{"http":false,"net":false,"rpc":false}"#);
    assert_eq!(
        ok(&server, "logging", r#"[["net","rpc"]]"#),
        r#"{"http":false,"net":true,"rpc":true}"#
    );
    assert_eq!(
        ok(&server, "logging", r#"[[],["net"]]"#),
        r#"{"http":false,"net":false,"rpc":true}"#
    );
    assert_eq!(
        ok(&server, "logging", r#"[["all"],["rpc"]]"#),
        r#"{"http":true,"net":true,"rpc":false}"#
    );
    let err = call(&server, "logging", r#"[["nope"]]"#).unwrap_err();
    assert_eq!(err.to_object().write(), r#"{"code":-8,"message":"unknown logging category nope"}"#);
    let err = call(&server, "logging", r#"[[1]]"#).unwrap_err();
    assert_eq!(err.code(), crate::protocol::RPC_TYPE_ERROR);
}

#[test]
fn debug_replaces_the_categories() {
    let (server, _) = server();
    assert_eq!(ok(&server, "debug", r#"["net+rpc"]"#), r#""Debug mode: net, rpc""#);
    assert_eq!(ok(&server, "debug", r#"["http"]"#), r#""Debug mode: http""#);
    assert_eq!(ok(&server, "debug", r#"["0"]"#), r#""Debug mode: ""#);
    assert_eq!(ok(&server, "debug", r#"["net+0"]"#), r#""Debug mode: ""#);
}

#[test]
fn echo_returns_its_arguments() {
    let (server, _) = server();
    assert_eq!(ok(&server, "echo", r#"["a",1,true]"#), r#"["a",1,true]"#);
    assert_eq!(ok(&server, "echojson", r#"[{"x":1.50}]"#), r#"[{"x":1.50}]"#);
    assert_eq!(ok(&server, "echoipc", r#"["hi"]"#), r#""hi""#);
    let bug = call(&server, "echo", r#"[0,1,2,3,4,5,6,7,8,"trigger_internal_bug"]"#)
        .unwrap_err()
        .to_object()
        .write();
    assert!(bug.starts_with(r#"{"code":-1,"message":"Internal bug detected: request.params[9].get_str() != \"trigger_internal_bug\"\n"#), "{bug}");
    assert!(bug.contains(r#" (echo)\nrdashd v0.0.0\nPlease report this issue here: https://github.com/dashpay/rust-dashcore/issues\n"}"#), "{bug}");
}

#[test]
fn setmocktime_is_regtest_only_and_range_checked() {
    let (server, control) = server();
    assert_eq!(ok(&server, "setmocktime", "[1700000000]"), "null");
    assert_eq!(ok(&server, "setmocktime", "[0]"), "null");
    assert_eq!(ok(&server, "setmocktime", "[9223372036]"), "null");
    for (params, time) in [("[-1]", "-1"), ("[9223372037]", "9223372037")] {
        let e = call(&server, "setmocktime", params).unwrap_err();
        assert_eq!(
            (e.code(), e.to_object().write()),
            (
                -8,
                format!(
                    r#"{{"code":-8,"message":"Mocktime must be in the range [0, 9223372036], not {time}."}}"#
                )
            )
        );
    }
    assert_eq!(
        *control.mocks.lock().unwrap(),
        ["mocktime 1700000000", "mocktime 0", "mocktime 9223372036"]
    );
    // The chain check comes before the range check.
    control.main_chain.store(true, Ordering::SeqCst);
    let e = call(&server, "setmocktime", "[-1]").unwrap_err();
    assert_eq!(
        (e.code(), e.to_object().write()),
        (
            -1,
            r#"{"code":-1,"message":"setmocktime is for regression testing (-regtest mode) only"}"#
                .into()
        )
    );
}

#[test]
fn mockscheduler_forwards_between_one_second_and_an_hour() {
    let (server, control) = server();
    assert_eq!(ok(&server, "mockscheduler", "[1]"), "null");
    assert_eq!(ok(&server, "mockscheduler", "[3600]"), "null");
    for params in ["[0]", "[-1]", "[3601]"] {
        let e = call(&server, "mockscheduler", params).unwrap_err();
        assert_eq!(
            (e.code(), e.to_object().write()),
            (
                -1,
                r#"{"code":-1,"message":"delta_time must be between 1 and 3600 seconds (1 hr)"}"#
                    .into()
            ),
            "{params}"
        );
    }
    assert_eq!(*control.mocks.lock().unwrap(), ["scheduler 1", "scheduler 3600"]);
    control.main_chain.store(true, Ordering::SeqCst);
    let e = call(&server, "mockscheduler", "[1]").unwrap_err();
    assert!(e.to_object().write().contains("mockscheduler is for regression testing"), "{e:?}");
}
