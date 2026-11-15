//! Writes `tests/data/chain-v1-golden.json` (O2b-4 §3a.4, G-ENC-1): synthetic inputs and what the
//! `chain-v1` decoder makes of them, pinned before the decoder changes. Each case keeps its input
//! bytes, the decoded program's text form, its canonical bytes, and the operator each `Mutate` and
//! `Propose` resolves to. The inputs are synthetic, apart from the 15 public seed files of
//! `node-fuzz/public-seeds/diff_chain`, which are synthetic chains on regtest themselves.
//!
//! An existing golden is the pin a changed decoder is held to, so it is never overwritten by
//! default: replacing it takes `--re-pin`, a decision the change's review must name.
//!
//! `cargo run -p node-l1 --features oracle --example chain_v1_golden [-- --re-pin]`

use node_l1::chain::intent::golden;

fn main() -> std::io::Result<()> {
    let re_pin = match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => false,
        [flag] if flag == "--re-pin" => true,
        _ => {
            eprintln!("usage: chain_v1_golden [--re-pin]");
            std::process::exit(2)
        }
    };
    let root = env!("CARGO_MANIFEST_DIR");
    let path = format!("{root}/tests/data/chain-v1-golden.json");
    if !re_pin && std::fs::exists(&path)? {
        eprintln!("chain_v1_golden: {path} exists, the pinned golden; --re-pin replaces it");
        std::process::exit(1)
    }
    let seeds = golden::public_seeds(&format!("{root}/../node-fuzz/public-seeds/diff_chain"))?;
    let text = golden::render(&golden::cases(&seeds));
    std::fs::write(&path, text)?;
    eprintln!("chain-v1 golden written");
    Ok(())
}
