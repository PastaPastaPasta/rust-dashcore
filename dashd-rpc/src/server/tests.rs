use std::sync::{Arc, Mutex};

use super::*;
use crate::testutil::{FakeControl, server};

fn call(server: &RpcServer, method: &str, params: &str) -> Result<UniValue, RpcError> {
    let request = JsonRpcRequest {
        method: method.into(),
        params: UniValue::read(params.as_bytes()).unwrap(),
        ..Default::default()
    };
    server.execute(&request)
}

fn error(server: &RpcServer, method: &str, params: &str) -> (i32, String) {
    match call(server, method, params).unwrap_err() {
        RpcError::Json {
            code,
            message,
        } => (code, message),
        RpcError::Type(m) => (RPC_TYPE_ERROR, m),
        RpcError::Runtime(m) => (-1, m),
    }
}

fn written(v: Result<UniValue, RpcError>) -> String {
    v.unwrap().write()
}

#[test]
fn warmup_answers_with_the_init_message_until_it_finishes() {
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    crate::commands::register_control_commands(&server);
    server.start();
    assert_eq!(error(&server, "help", "[]"), (RPC_IN_WARMUP, "RPC server started".into()));
    server.set_warmup_status("Loading block index…");
    assert_eq!(error(&server, "help", "[]"), (RPC_IN_WARMUP, "Loading block index…".into()));
    server.set_warmup_finished();
    // A status after warmup changes nothing.
    server.set_warmup_status("Done loading");
    assert_eq!(server.warmup_status(), None);
    assert_eq!(written(call(&server, "uptime", "[]")), "100");
}

#[test]
fn an_unregistered_method_is_not_found() {
    let (server, _) = server();
    for method in ["getblockcount", "", "Help", "help ", "bls", "protx"] {
        assert_eq!(
            error(&server, method, "[]"),
            (RPC_METHOD_NOT_FOUND, "Method not found".into()),
            "{method:?}"
        );
    }
}

/// The method and parameters (as JSON) of each call a test command saw.
type Calls = Arc<Mutex<Vec<(String, String)>>>;

/// A command `bls fromsecret` registered for the test, recording what it is called with.
fn with_bls_fromsecret() -> (Arc<RpcServer>, Calls) {
    let control = Arc::new(FakeControl::default());
    let server = Arc::new(RpcServer::new(control as _));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    server.append_command(Command::new("bls fromsecret", move |r| {
        seen.lock().unwrap().push((r.method.clone(), r.params.write()));
        Ok(UniValue::Null)
    }));
    server.start();
    server.set_warmup_finished();
    (server, calls)
}

#[test]
fn a_sub_command_is_squashed_into_its_method() {
    let (server, calls) = with_bls_fromsecret();
    written(call(&server, "bls", r#"["fromsecret","ab"]"#));
    written(call(&server, "bls", r#"["fromsecret","ab",true]"#));
    // Named arguments: the sub-command is the first value, as in Core.
    written(call(&server, "bls", r#"{"sub":"fromsecret","x":"ab"}"#));
    assert_eq!(
        *calls.lock().unwrap(),
        [
            ("blsfromsecret".to_owned(), r#"["ab"]"#.to_owned()),
            ("blsfromsecret".to_owned(), r#"["ab",true]"#.to_owned()),
            ("blsfromsecret".to_owned(), r#"["ab"]"#.to_owned()),
        ]
    );
    // An unknown sub-command, or none, finds no `bls` command.
    for params in [r#"["generate"]"#, "[]", "[1]"] {
        assert_eq!(error(&server, "bls", params).0, RPC_METHOD_NOT_FOUND, "{params}");
    }
}

#[test]
fn the_argument_count_is_checked_against_the_spec() {
    let (server, _) = server();
    let help_help = dash_rpc_spec::command("help").unwrap().help;
    assert_eq!(error(&server, "help", r#"["a","b","c"]"#), (-1, help_help.to_owned()));
    let echoipc_help = dash_rpc_spec::command("echoipc").unwrap().help;
    assert_eq!(error(&server, "echoipc", "[]"), (-1, echoipc_help.to_owned()));
    // Sub-command arity counts the arguments after the sub-command.
    let (bls, _) = with_bls_fromsecret();
    let fromsecret_help = dash_rpc_spec::command("bls fromsecret").unwrap().help;
    assert_eq!(error(&bls, "bls", r#"["fromsecret"]"#), (-1, fromsecret_help.to_owned()));
}

#[test]
fn a_wrong_type_lists_every_position() {
    let (server, _) = server();
    assert_eq!(
        error(&server, "logging", r#"["net",{}]"#),
        (
            RPC_TYPE_ERROR,
            "Wrong type passed:\n{\n    \"Position 1 (include)\": \"JSON value of type string is not of expected type array\",\n    \"Position 2 (exclude)\": \"JSON value of type object is not of expected type array\"\n}"
                .into()
        )
    );
    // Null is accepted for an optional argument, not for a required one.
    written(call(&server, "logging", "[null,[]]"));
    assert_eq!(
        error(&server, "debug", "[null]"),
        (
            RPC_TYPE_ERROR,
            "Wrong type passed:\n{\n    \"Position 1 (category)\": \"JSON value of type null is not of expected type string\"\n}"
                .into()
        )
    );
    // `echo`'s arguments skip the check.
    assert_eq!(written(call(&server, "echo", r#"[1,{"a":[]},null]"#)), r#"[1,{"a":[]},null]"#);
}

#[test]
fn named_arguments_map_to_positions() {
    let (server, _) = server();
    let echo = |params: &str| written(call(&server, "echo", params));
    assert_eq!(echo(r#"{"arg0":1,"arg2":3}"#), "[1,null,3]");
    assert_eq!(echo(r#"{"arg9":"x"}"#), r#"[null,null,null,null,null,null,null,null,null,"x"]"#);
    assert_eq!(echo("{}"), "[]");
    // `args` holds positional arguments, named ones follow.
    assert_eq!(echo(r#"{"args":[1,2],"arg3":4}"#), "[1,2,null,4]");
    assert_eq!(echo(r#"{"args":[1,2]}"#), "[1,2]");
    // `args` that is not an array is dropped.
    assert_eq!(echo(r#"{"args":5,"arg1":2}"#), "[null,2]");
    let err = |params: &str| error(&server, "echo", params);
    assert_eq!(
        err(r#"{"arg0":1,"arg0":2}"#),
        (RPC_INVALID_PARAMETER, "Parameter arg0 specified multiple times".into())
    );
    assert_eq!(
        err(r#"{"arg0":1,"foo":2}"#),
        (RPC_INVALID_PARAMETER, "Unknown named parameter foo".into())
    );
    assert_eq!(
        err(r#"{"args":[1],"arg0":2}"#),
        (
            RPC_INVALID_PARAMETER,
            "Parameter arg0 specified twice both as positional and named argument".into()
        )
    );
    assert_eq!(
        err(r#"{"args":[1,2],"arg1":2}"#),
        (
            RPC_INVALID_PARAMETER,
            "Parameter arg1 specified twice both as positional and named argument".into()
        )
    );
    assert_eq!(
        error(&server, "help", r#"{"command":"x","extra":1}"#).1,
        "Unknown named parameter extra"
    );
    let uptime_help = dash_rpc_spec::command("uptime").unwrap().help;
    assert_eq!(
        written(call(&server, "help", r#"{"command":"uptime"}"#)),
        UniValue::Str(uptime_help.into()).write()
    );
}

#[test]
fn running_commands_are_listed() {
    let control = Arc::new(FakeControl::default());
    let server = Arc::new(RpcServer::new(control as _));
    let (entered, release) =
        (Arc::new(std::sync::Barrier::new(2)), Arc::new(std::sync::Barrier::new(2)));
    let (e, r) = (Arc::clone(&entered), Arc::clone(&release));
    server.append_command(Command::new("uptime", move |_| {
        e.wait();
        r.wait();
        Ok(UniValue::Null)
    }));
    server.start();
    server.set_warmup_finished();
    let running = Arc::clone(&server);
    let thread = std::thread::spawn(move || call(&running, "uptime", "[]").unwrap());
    entered.wait();
    let active = server.active_commands();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].0, "uptime");
    release.wait();
    thread.join().unwrap();
    assert!(server.active_commands().is_empty());
}

#[test]
fn interruption_ends_running_and_runs_once() {
    let (server, _) = server();
    assert!(server.interruption_point().is_ok());
    server.interrupt();
    server.interrupt();
    assert!(!server.is_running());
    let err = server.interruption_point().unwrap_err();
    assert_eq!(
        (err.code(), err.to_object().write()),
        (RPC_CLIENT_NOT_CONNECTED, r#"{"code":-9,"message":"Shutting down"}"#.to_owned())
    );
}

#[test]
#[should_panic(expected = "commands are added before the RPC server runs")]
fn commands_are_added_before_the_server_runs() {
    let (server, _) = server();
    server.append_command(Command::new("uptime", |_| Ok(UniValue::Null)));
}

#[test]
fn the_arg_map_lists_every_name_and_alias() {
    let (server, _) = server();
    let map = server.dump_arg_map().write();
    assert!(map.contains(r#"["echo",9,"arg9",true]"#));
    assert!(map.contains(r#"["stop",0,"wait",false]"#));
    // Sub-commands are left out (Core lists them under their method's own map).
    assert!(!map.contains(r#"["bls fromsecret""#));
}

#[test]
fn deprecated_rpcs_are_enabled_by_name() {
    let args = dash_args::ArgsManager::new();
    args.add_arg(
        "-deprecatedrpc=<method>",
        "",
        dash_args::ArgsManager::ALLOW_ANY,
        dash_args::OptionsCategory::Rpc,
    );
    args.parse_parameters(&["x", "-deprecatedrpc=a", "-deprecatedrpc=b"]).unwrap();
    assert!(is_deprecated_rpc_enabled(&args, "a"));
    assert!(is_deprecated_rpc_enabled(&args, "b"));
    assert!(!is_deprecated_rpc_enabled(&args, "c"));
}

/// `Started` slots connect only before `start`; an `interrupt` does not reopen them, though
/// the server no longer runs.
// Core: rpc/server.cpp:76-79, :307-312
#[test]
#[should_panic(expected = "Started slots connect before the RPC server starts")]
fn started_slots_connect_before_start_even_after_an_interrupt() {
    let server = RpcServer::new(Arc::new(FakeControl::default()));
    server.start();
    server.interrupt();
    server.on_started(|| ());
}

/// `Stopped` slots connect at any time until `stop`, while the server runs too, and each runs
/// once at `stop`; after it they are refused.
// Core: rpc/server.cpp:81-84, :325-336
#[test]
fn stopped_slots_connect_until_stop() {
    let server = RpcServer::new(Arc::new(FakeControl::default()));
    let ran = Arc::new(Mutex::new(Vec::new()));
    let slot = |name: &'static str| {
        let ran = Arc::clone(&ran);
        move || ran.lock().unwrap().push(name)
    };
    server.on_started(slot("started"));
    server.on_stopped(slot("before start"));
    server.start();
    server.on_stopped(slot("running"));
    server.interrupt();
    server.on_stopped(slot("interrupted"));
    server.stop();
    assert_eq!(*ran.lock().unwrap(), ["started", "before start", "running", "interrupted"]);
    let late = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server.on_stopped(|| ())));
    assert!(late.is_err(), "a slot after stop would never run");
}

/// `StopRPC` asserts in release builds too that `InterruptRPC` came first.
// Core: rpc/server.cpp:328
#[test]
#[should_panic(expected = "StopRPC follows InterruptRPC")]
fn stop_before_interrupt_panics() {
    let server = RpcServer::new(Arc::new(FakeControl::default()));
    server.start();
    server.stop();
}
