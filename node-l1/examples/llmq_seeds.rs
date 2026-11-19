//! Writes the LQ1a targets' public seeds (`node_l1::llmq::seed_inputs`), each file one target
//! input named by the first 8 bytes of its SHA-256, to `node-fuzz/public-seeds/<target>/`.
//! The inputs are scenarios and call bytes only (structured, and splitmix64 from fixed seeds),
//! judged by the oracle when run.
//!
//! `cargo run -p node-l1 --features oracle --example llmq_seeds`

use std::collections::{BTreeMap, BTreeSet};

use bitcoin_hashes::sha256;

fn main() -> std::io::Result<()> {
    let out = format!("{}/../node-fuzz/public-seeds", env!("CARGO_MANIFEST_DIR"));
    let mut written = BTreeMap::<&str, BTreeSet<String>>::new();
    for (target, input) in node_l1::llmq::seed_inputs() {
        let dir = format!("{out}/{target}");
        std::fs::create_dir_all(&dir)?;
        let digest = sha256::Hash::hash(&input).to_byte_array();
        let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(format!("{dir}/{name}"), &input)?;
        written.entry(target).or_default().insert(name);
    }
    for (target, names) in written {
        eprintln!("{target}: {} seeds", names.len());
    }
    Ok(())
}
