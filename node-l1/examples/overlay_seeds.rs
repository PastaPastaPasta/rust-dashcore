//! Writes the overlay targets' public seeds (O2c): every recorded input (BS8's Core-judged corpus
//! and the O4 fixtures, `tests/support/overlay_corpus.rs`) of at most 64 KiB, as the input of its
//! target `diff_p2p_<command>`, to `node-fuzz/public-seeds/diff_p2p_<command>/<sha256 prefix>`.
//!
//! `cargo run -p node-l1 --features oracle --example overlay_seeds`

use std::collections::BTreeMap;

use bitcoin_hashes::sha256;
use overlay_corpus::MAX_SEED;

#[path = "../tests/support/overlay_corpus.rs"]
mod overlay_corpus;

fn main() -> std::io::Result<()> {
    let out = format!("{}/../node-fuzz/public-seeds", env!("CARGO_MANIFEST_DIR"));
    let mut written = BTreeMap::<&str, usize>::new();
    for r in overlay_corpus::bs8_corpus().into_iter().chain(overlay_corpus::o4_fixtures()) {
        let Some(input) = r.case.to_input().filter(|i| i.len() <= MAX_SEED) else {
            continue;
        };
        let dir = format!("{out}/diff_p2p_{}", r.case.cmd);
        std::fs::create_dir_all(&dir)?;
        let digest = sha256::Hash::hash(&input).to_byte_array();
        let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(format!("{dir}/{name}"), &input)?;
        *written.entry(r.case.cmd).or_default() += 1;
    }
    for (cmd, n) in written {
        eprintln!("diff_p2p_{cmd}: {n} seeds");
    }
    Ok(())
}
