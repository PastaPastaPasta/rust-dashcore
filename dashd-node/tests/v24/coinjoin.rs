//! CJ1's D17 builders: `contrib/node/cj1-transcripts/capture.py`'s CoinJoin transactions, built
//! on one regtest dashd with one wallet over [`Rpc::call`].

use serde_json::{Value, json};

use super::{Rpc, unhex};

/// Core's `COIN`, in duffs.
pub const COIN: u64 = 100_000_000;
/// Core's `DEFAULT_MAX_RAW_TX_FEE{COIN / 10}` (`coinjoin/coinjoin.cpp:25`), checked by `ATMPIfSaneFee` (:440-456).
pub const MAX_RAW_TX_FEE: u64 = COIN / 10;
/// The fee delta `CommitFinalTransaction` gives the final tx (`coinjoin/server.cpp:481-482`), in duffs;
/// the delta persists per txid.
pub const PRIORITISE_DELTA: i64 = (COIN / 10) as i64;
/// The smallest denomination (`coinjoin/common.h:46`) that `fund` outputs and `mixed_tx` pays.
pub const DENOMINATION: u64 = COIN / 1000 + 1;
/// Core's `GetCollateralAmount()` (`coinjoin/common.h:117`), checked by `IsCollateralValid` (`coinjoin/coinjoin.cpp:491`).
pub const COLLATERAL_AMOUNT: u64 = DENOMINATION / 10;
/// The collateral output `fund` funds (capture.py:1013).
pub const COLLATERAL_INPUT: u64 = COIN / 100;
/// The collateral tx's output (capture.py:1022).
pub const COLLATERAL_OUTPUT: u64 = 990_000;
/// The collateral tx's fee, equal to [`COLLATERAL_AMOUNT`].
pub const COLLATERAL_FEE: u64 = COLLATERAL_INPUT - COLLATERAL_OUTPUT;
/// The excessive-fee collateral's fee: one duff above [`MAX_RAW_TX_FEE`].
pub const EXCESSIVE_FEE: u64 = MAX_RAW_TX_FEE + 1;

/// One participant of capture.py's `fund` (:1006-1031).
#[derive(Clone, Debug)]
pub struct Client {
    pub collateral: Vec<u8>,
    pub acceptance: Value,
    pub txid: String,
    pub vout: u32,
    pub script: Vec<u8>,
    pub dest: Vec<u8>,
}

/// `duffs` as an RPC amount string.
pub fn amount(duffs: u64) -> String {
    format!("{}.{:08}", duffs / COIN, duffs % COIN)
}

/// Mines `count` blocks to a new wallet address, returning their hashes.
pub fn mine(dashd: &impl Rpc, count: u32) -> Value {
    let address = dashd.call("getnewaddress", json!([]));
    dashd.call("generatetoaddress", json!([count, address]))
}

/// The wallet tx `txid`, decoded (`gettransaction` in place of capture.py's `getrawtransaction`).
fn decoded(dashd: &impl Rpc, txid: &Value) -> Value {
    dashd.call("gettransaction", json!([txid, false, true]))["decoded"].clone()
}

fn output<'a>(decoded: &'a Value, address: &Value) -> &'a Value {
    decoded["vout"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["scriptPubKey"]["address"] == *address)
        .expect("the address's output")
}

/// Spends `txid:vout` to one new address paying `out` duffs, signed by the wallet.
fn spend(dashd: &impl Rpc, txid: &Value, vout: &Value, dest: &Value, out: u64) -> Vec<u8> {
    let raw = dashd.call(
        "createrawtransaction",
        json!([[{"txid": txid, "vout": vout}], {dest.as_str().unwrap(): amount(out)}]),
    );
    let signed = dashd.call("signrawtransactionwithwallet", json!([raw]));
    assert_eq!(signed["complete"], json!(true), "{signed}");
    unhex(signed["hex"].as_str().unwrap())
}

/// capture.py's `fund` (:1006-1031): one `sendmany`, one block, a signed collateral per client;
/// mines one block; needs exactly one loaded wallet.
pub fn fund(dashd: &impl Rpc, count: usize) -> Vec<Client> {
    let mut outputs = serde_json::Map::new();
    let specs: Vec<[Value; 3]> = (0..count)
        .map(|_| {
            let spec = [(); 3].map(|_| dashd.call("getnewaddress", json!([])));
            outputs.insert(spec[0].as_str().unwrap().to_owned(), json!(amount(DENOMINATION)));
            outputs.insert(spec[1].as_str().unwrap().to_owned(), json!(amount(COLLATERAL_INPUT)));
            spec
        })
        .collect();
    let parent = dashd.call("sendmany", json!(["", outputs]));
    mine(dashd, 1);
    let decoded = decoded(dashd, &parent);
    specs
        .iter()
        .map(|[denom, collateral, dest]| {
            let (d, col) = (output(&decoded, denom), output(&decoded, collateral));
            let collateral = spend(dashd, &parent, &col["n"], dest, COLLATERAL_OUTPUT);
            let acceptance =
                dashd.call("testmempoolaccept", json!([[super::hex(&collateral)]]))[0].clone();
            assert_eq!(acceptance["allowed"], json!(true), "{acceptance}");
            let address = dashd.call("getnewaddress", json!([]));
            let info = dashd.call("getaddressinfo", json!([address]));
            Client {
                collateral,
                acceptance,
                txid: parent.as_str().unwrap().to_owned(),
                vout: d["n"].as_u64().unwrap() as u32,
                script: unhex(d["scriptPubKey"]["hex"].as_str().unwrap()),
                dest: unhex(info["scriptPubKey"].as_str().unwrap()),
            }
        })
        .collect()
}

/// A collateral tx like `fund`'s paying [`EXCESSIVE_FEE`], over `ATMPIfSaneFee`'s limit (coinjoin.cpp:449);
/// mines one block; needs exactly one loaded wallet.
pub fn excessive_collateral(dashd: &impl Rpc) -> Vec<u8> {
    let [source, dest] = [(); 2].map(|_| dashd.call("getnewaddress", json!([])));
    let funding =
        dashd.call("sendtoaddress", json!([source, amount(COLLATERAL_OUTPUT + EXCESSIVE_FEE)]));
    mine(dashd, 1);
    let decoded = decoded(dashd, &funding);
    spend(dashd, &funding, &output(&decoded, &source)["n"], &dest, COLLATERAL_OUTPUT)
}

fn compact(n: usize) -> Vec<u8> {
    match n {
        0..0xfd => vec![n as u8],
        0xfd..=0xffff => [&[0xfd][..], &(n as u16).to_le_bytes()].concat(),
        _ => [&[0xfe][..], &(n as u32).to_le_bytes()].concat(),
    }
}

/// capture.py's `txin` (:1033-1034).
pub fn txin(client: &Client, script: &[u8]) -> Vec<u8> {
    let mut txid = unhex(&client.txid);
    txid.reverse();
    [
        txid,
        client.vout.to_le_bytes().to_vec(),
        compact(script.len()),
        script.to_vec(),
        vec![0xff; 4],
    ]
    .concat()
}

/// capture.py's `txout` (:1037-1038).
pub fn txout(client: &Client) -> Vec<u8> {
    [(DENOMINATION as i64).to_le_bytes().to_vec(), compact(client.dest.len()), client.dest.clone()]
        .concat()
}

/// capture.py's `entry` (:1041-1044).
pub fn entry(client: &Client, empty: bool) -> Vec<u8> {
    let (ins, outs) = if empty {
        (vec![0], vec![0])
    } else {
        ([&[1][..], &txin(client, &[])].concat(), [&[1][..], &txout(client)].concat())
    };
    [ins, client.collateral.clone(), outs].concat()
}

/// capture.py's `mixed_tx` (:1047-1051): version 2, BIP69 order, lock time 0, unsigned.
pub fn mixed_tx(clients: &[Client]) -> Vec<u8> {
    let mut sorted: Vec<&Client> = clients.iter().collect();
    sorted.sort_by(|a, b| (&a.txid, a.vout).cmp(&(&b.txid, b.vout)));
    let mut outs: Vec<Vec<u8>> = sorted.iter().map(|c| txout(c)).collect();
    outs.sort();
    [
        2i32.to_le_bytes().to_vec(),
        compact(sorted.len()),
        sorted.iter().flat_map(|c| txin(c, &[])).collect(),
        compact(outs.len()),
        outs.concat(),
        vec![0; 4],
    ]
    .concat()
}

/// capture.py's `signed_mix` (:1054-1057): `mixed_tx` signed `ALL|ANYONECANPAY` by the wallet.
pub fn signed_mix(dashd: &impl Rpc, clients: &[Client]) -> Vec<u8> {
    let signed = dashd.call(
        "signrawtransactionwithwallet",
        json!([super::hex(&mixed_tx(clients)), [], "ALL|ANYONECANPAY"]),
    );
    assert_eq!(signed["complete"], json!(true), "{signed}");
    let hex = signed["hex"].as_str().unwrap();
    let decoded = dashd.call("decoderawtransaction", json!([hex]));
    let vin = decoded["vin"].as_array().unwrap();
    assert_eq!(vin.len(), clients.len(), "{decoded}");
    for input in vin {
        let asm = input["scriptSig"]["asm"].as_str().unwrap();
        assert!(asm.contains("[ALL|ANYONECANPAY]"), "{asm}");
    }
    unhex(hex)
}
