//! Writes `dash-dml/tests/data/core_sign_strings.txt` (register quirk 27, EV1-4): Core's
//! `MakeSignString` and the `CMessageSigner` hash its key signs, from the oracle
//! (`oracle_special_tx_sign_hash`), for each external-collateral ProRegTx of
//! `dash-dml/tests/data/core_signed_protx.txt` on mainnet, testnet and regtest parameters.
//! `dash_dml::specialtx::sig`'s `quirk_27_make_sign_string` replays them without the oracle.
//!
//! `cargo run -p node-l1 --features oracle --example specialtx_sign_strings`

use std::fmt::Write as _;

use dash_consensus::primitives::decode_tx_exact;
use dash_oracle_sys::{Chain, Oracle, Pin};

fn main() -> std::io::Result<()> {
    let data = format!("{}/../dash-dml/tests/data", env!("CARGO_MANIFEST_DIR"));
    let signed = std::fs::read_to_string(format!("{data}/core_signed_protx.txt"))?;
    let oracle = Oracle::new(Pin::V24_0);
    let mut out = String::from(
        "# Core's MakeSignString and CMessageSigner hash (register quirk 27) for the external-collateral\n\
         # ProRegTxs of core_signed_protx.txt on mainnet, testnet and regtest parameters, from\n\
         # dash-oracle-sys's oracle_special_tx_sign_hash at v24.0.0-rc.4 (Pin::V24_0). Written by\n\
         # `cargo run -p node-l1 --features oracle --example specialtx_sign_strings`.\n\
         # <txid> <network> <message hash, hex, internal byte order> <sign string>\n",
    );
    let mut count = 0;
    for line in signed.lines().filter(|l| !l.starts_with('#')) {
        let [tx_type, _, ids, raw] = line.split(' ').collect::<Vec<_>>()[..] else {
            panic!("a fixture line: {line}");
        };
        // An external collateral's ProRegTx names its signer; a shared one's payload does.
        if tx_type != "1" || ids == "-" {
            continue;
        }
        let raw: Vec<u8> = (0..raw.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).expect("hex"))
            .collect();
        let txid = decode_tx_exact(&raw).expect("Core's transaction").into_value().txid();
        for (network, chain) in
            [("mainnet", Chain::main()), ("testnet", Chain::test()), ("regtest", Chain::regtest())]
        {
            let core = oracle.special_tx_sign_hash(&chain, &raw).expect("Core's sign hashes");
            let (text, hash) = core.sign_message.expect("an external collateral's sign string");
            let hash: String = hash.iter().map(|b| format!("{b:02x}")).collect();
            writeln!(out, "{txid} {network} {hash} {text}").expect("a string");
            count += 1;
        }
    }
    std::fs::write(format!("{data}/core_sign_strings.txt"), out)?;
    eprintln!("core_sign_strings.txt: {count} lines");
    Ok(())
}
