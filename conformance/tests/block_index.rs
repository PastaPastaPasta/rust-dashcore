//! `BlockIndex for Rpc` against real dashd replies: a local server answers each request with
//! the reply the testnet reference oracle gave to it (`rpc-replies/oracle-blockindex-testnet.json`,
//! provenance inside), HTTP status included, so dashd's error codes reach the client as they
//! do from dashd.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

use dash_conformance::check::parse_rev_hex;
use dash_conformance::rpc::{Auth, Rpc};
use dash_conformance::run::{BlockIndex, IndexedHeader};
use serde_json::{Value, json};

const FIXTURE: &str = include_str!("rpc-replies/oracle-blockindex-testnet.json");

/// Serves the fixture's replies, matched by method and params, on a local port.
fn serve() -> String {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let replies = fixture["replies"].as_array().unwrap().clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            let req: Value = serde_json::from_slice(&body).unwrap();
            let r = replies
                .iter()
                .find(|r| r["method"] == req["method"] && r["params"] == req["params"])
                .unwrap_or_else(|| panic!("no captured reply for {req}"));
            let out = serde_json::to_vec(&r["reply"]).unwrap();
            write!(
                stream,
                "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                r["status"],
                out.len()
            )
            .unwrap();
            stream.write_all(&out).unwrap();
        }
    });
    url
}

/// The fixture's request hashes, in display hex: height 1000, genesis, the headers-only tip.
fn hashes() -> [String; 3] {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let r = &fixture["replies"];
    [0, 1, 2].map(|i| r[i]["params"][0].as_str().unwrap().to_string())
}

fn h(hex: &str) -> [u8; 32] {
    parse_rev_hex(hex).unwrap()
}

#[test]
fn rpc_block_index_reads_dashd_replies() {
    let rpc = Rpc::new(serve(), Auth::UserPass("u".into(), "p".into()));
    let [b1000, genesis, headers_only] = hashes();
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let prev_1000 = fixture["replies"][0]["reply"]["result"]["previousblockhash"].clone();
    let prev_tip = fixture["replies"][2]["reply"]["result"]["previousblockhash"].clone();
    let tip_height = fixture["replies"][2]["reply"]["result"]["height"].as_u64().unwrap();

    assert_eq!(
        rpc.header(&h(&b1000)).unwrap(),
        Some(IndexedHeader {
            height: 1000,
            prev: h(prev_1000.as_str().unwrap()),
            active: true,
            has_data: true,
        })
    );
    assert_eq!(
        rpc.header(&h(&genesis)).unwrap(),
        Some(IndexedHeader {
            height: 0,
            prev: [0; 32],
            active: true,
            has_data: true,
        }),
        "genesis has no previousblockhash"
    );
    assert_eq!(
        rpc.header(&h(&headers_only)).unwrap(),
        Some(IndexedHeader {
            height: tip_height as u32,
            prev: h(prev_tip.as_str().unwrap()),
            active: false,
            has_data: false,
        }),
        "a header without data, off the active chain (confirmations -1, nTx 0)"
    );
    assert_eq!(rpc.header(&h(&format!("{}aa", "00".repeat(31)))).unwrap(), None, "-5");
    assert_eq!(rpc.active_at(1000).unwrap(), Some(h(&b1000)));
    assert_eq!(rpc.active_at(99_999_999).unwrap(), None, "-8 above the tip");
    assert_eq!(rpc.try_call("getblockhash", json!([99_999_999])).unwrap(), Err(-8));
}
