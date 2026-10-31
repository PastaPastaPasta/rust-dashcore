//! Writes `diff_check_special_tx`'s public seeds (`node_l1::specialtx::seed_inputs`), each file
//! one target input named by the first 8 bytes of its SHA-256, to
//! `node-fuzz/public-seeds/diff_check_special_tx/`. The inputs are generator bytes only, judged by
//! the oracle when run.
//!
//! `cargo run -p node-l1 --features oracle --example specialtx_seeds`

use bitcoin_hashes::sha256;

fn main() -> std::io::Result<()> {
    let dir =
        format!("{}/../node-fuzz/public-seeds/diff_check_special_tx", env!("CARGO_MANIFEST_DIR"));
    if std::path::Path::new(&dir).exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let mut written = std::collections::BTreeSet::new();
    for input in node_l1::specialtx::seed_inputs() {
        let digest = sha256::Hash::hash(&input).to_byte_array();
        let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(format!("{dir}/{name}"), &input)?;
        written.insert(name);
    }
    eprintln!("diff_check_special_tx: {} seeds", written.len());
    Ok(())
}
