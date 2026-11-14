use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::sync::Barrier;

use super::*;
use crate::net::resolve_numeric;
use crate::server::Command;
use crate::testutil::{FakeControl, test_args};

/// A call with `-rpcuser=u -rpcpassword=p`, its connection closed after the reply.
fn call(addr: SocketAddr, method: &str) -> String {
    let body = format!(r#"{{"method":"{method}","id":1}}"#);
    let request = format!(
        "POST / HTTP/1.1\r\nAuthorization: Basic dTpw\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply.split_once("\r\n\r\n").unwrap().1.to_owned()
}

/// Calls run side by side: one that waits does not hold up the others (Core's workers share
/// nothing but the command table).
#[test]
fn a_running_call_does_not_block_others() {
    let t = test_args(&["-rpcport=0", "-rpcuser=u", "-rpcpassword=p", "-rpcthreads=2"]);
    let server = Arc::new(RpcServer::new(Arc::new(FakeControl::default())));
    crate::commands::register_control_commands(&server);
    let (entered, release) = (Arc::new(Barrier::new(2)), Arc::new(Barrier::new(2)));
    let (e, r) = (Arc::clone(&entered), Arc::clone(&release));
    server.append_command(Command::new("waitfornewblock", move |_| {
        e.wait();
        r.wait();
        Ok(UniValue::Null)
    }));
    server.start();
    server.set_warmup_finished();
    let mut http = HttpServer::init(&t.args, &resolve_numeric, Arc::new(|| false)).unwrap();
    let rpc = HttpRpc::start(Arc::clone(&server), &http, &t.args).unwrap();
    http.start();
    let addr = *http.local_addrs().iter().find(|a| a.is_ipv4()).unwrap();
    let waiting = std::thread::spawn(move || call(addr, "waitfornewblock"));
    entered.wait();
    assert!(call(addr, "uptime").starts_with(r#"{"result":100,"error":null"#));
    release.wait();
    assert_eq!(waiting.join().unwrap(), "{\"result\":null,\"error\":null,\"id\":1}\n");
    server.interrupt();
    http.interrupt();
    rpc.stop(&http);
    http.stop();
}
