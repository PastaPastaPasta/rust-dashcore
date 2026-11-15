//! The dash-dml leg of the DEC-177 check (O8 spec §13.4): Core's canonical MN-list bytes, as
//! `getcanonicalmnlist` returned them, decode with [`decode_canonical_list`], re-encode unchanged
//! and hash to the RPC's `hash`.
//!
//! Usage: `canonical_list_check <network>`, Core's network id of the chain the lists come from
//! (`main`, `test`, `devnet` or `regtest`). A list's NetInfo is checked by Core's `Validate()` under
//! that chain's rules, with `-cjdnsreachable` unset as the recorded nodes ran: on regtest a local
//! address is valid (`RequireRoutableExternalIP()` false).
//!
//! Reads lines `<height> <hash> <hex>` on stdin, from `contrib/node/statedigest/dml-leg.sh`, and
//! prints one `{"height":H,"result":"..."}` line each. The result is `match` or says what differs.

use std::io::{self, BufRead};

use dash_dml::codec::{
    canonical_mn_list_hash, decode_canonical_list, encode_canonical_list_unvalidated,
};
use dash_dml::netinfo::{CoreNetInfoValidity, NetInfoConfig, NetInfoRules};
use dash_params::{ChainParamsBuilder, Network, Release};

/// `None` for an odd length or a non-hex digit (`get` fails past the end).
fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn check(net_info: &CoreNetInfoValidity, hash: &str, hex: &str) -> String {
    let Some(bytes) = unhex(hex) else {
        return "error: the hex is not hex".into();
    };
    let mut input = bytes.as_slice();
    let list = match decode_canonical_list(&mut input, net_info) {
        Ok(list) => list,
        Err(e) => return format!("error: {e:?}"),
    };
    if !input.is_empty() {
        return format!("mismatch: {} trailing bytes", input.len());
    }
    let mut again = Vec::new();
    encode_canonical_list_unvalidated(&list, &mut again);
    let ours = canonical_mn_list_hash(&list).to_string();
    match (again == bytes, ours == hash) {
        (true, true) => "match".into(),
        (reencoded, hashed) => {
            format!("mismatch: same bytes {reencoded}, same hash {hashed} (ours {ours})")
        }
    }
}

/// Core's `Validate()` under the rules of the chain whose network id is `id`.
fn net_info(id: &str) -> Result<CoreNetInfoValidity, String> {
    let network = Network::from_network_id(id).ok_or_else(|| format!("unknown network {id:?}"))?;
    let build =
        |n| ChainParamsBuilder::new(n, Release::V24_0).build().map_err(|e| format!("{e:?}"));
    let (params, main) = (build(network)?, build(Network::Main)?);
    Ok(CoreNetInfoValidity {
        rules: NetInfoRules::new(&params, &main, NetInfoConfig::default()),
    })
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let net_info = match &args[..] {
        [id] => net_info(id).map_err(io::Error::other)?,
        _ => return Err(io::Error::other("usage: canonical_list_check <network>")),
    };
    for line in io::stdin().lock().lines() {
        let line = line?;
        let (height, result) = match line.split(' ').collect::<Vec<_>>()[..] {
            [height, hash, hex] => match height.parse::<u32>() {
                Ok(h) => (h.to_string(), check(&net_info, hash, hex)),
                Err(_) => ("null".into(), "error: the height is not a number".into()),
            },
            _ => ("null".into(), "error: not <height> <hash> <hex>".into()),
        };
        println!("{{\"height\":{height},\"result\":{result:?}}}");
    }
    Ok(())
}
