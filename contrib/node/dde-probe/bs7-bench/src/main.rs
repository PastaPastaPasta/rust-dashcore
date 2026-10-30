//! Decodes Core-built `headers2` replies ITERS times with one base-sdk
//! revision, for `perf stat -e instructions:u` (subtract an ITERS=0 run).
//!
//! Usage: bs7-bench old|new FILE.hex... (ITERS in the environment, default 200)

use std::hint::black_box;

/// A decoder for one revision: the payload in, the header count out.
macro_rules! decoder {
    ($p2p:ident, $pkc:ident) => {{
        let ctx = $p2p::ProtoCtx::new($p2p::ProtocolVersion::new(70240), $pkc::bls::BlsCtx::BASIC);
        Box::new(move |p: &[u8]| {
            match $p2p::P2pMsg::decode_with_policy(
                &$p2p::CommandString::HEADERS2,
                black_box(p),
                &ctx,
            ) {
                Ok($p2p::DecodedMsg { msg: $p2p::P2pMsg::Headers2(h), .. }) => {
                    black_box(h).headers.len()
                }
                other => panic!("{other:?}"),
            }
        }) as Box<dyn Fn(&[u8]) -> usize>
    }};
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let iters: usize = std::env::var("ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(200);
    let decode = match args.get(1).map(String::as_str) {
        Some("old") => decoder!(old_p2p, old_pkc),
        Some("new") => decoder!(new_p2p, new_pkc),
        _ => panic!("usage: bs7-bench old|new FILE.hex..."),
    };
    let payloads: Vec<Vec<u8>> = args[2..]
        .iter()
        .map(|f| hex::decode(std::fs::read_to_string(f).expect("read").trim()).expect("hex"))
        .collect();
    let mut headers = 0usize;
    for _ in 0..iters {
        for p in &payloads {
            headers += decode(p);
        }
    }
    println!("{headers}");
}
