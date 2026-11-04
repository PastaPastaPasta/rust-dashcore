//! Writes the public seeds of `diff_mnsync`, `diff_netfulfilled`, `diff_mncache` and
//! `diff_flatdb_load` (`node_l1::mnsync::*_seed_inputs`, BL-451b), each file one target input
//! named by the first 8 bytes of its SHA-256, to `node-fuzz/public-seeds/<target>/`. The inputs
//! are script bytes and Core-written files, judged by the oracle when run.
//!
//! `cargo run -p node-l1 --features oracle --example mnsync_seeds`

use bitcoin_hashes::sha256;
use node_l1::mnsync;

fn write(target: &str, inputs: Vec<Vec<u8>>) -> std::io::Result<()> {
    let dir = format!("{}/../node-fuzz/public-seeds/{target}", env!("CARGO_MANIFEST_DIR"));
    if std::path::Path::new(&dir).exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let mut written = std::collections::BTreeSet::new();
    for input in inputs {
        let digest = sha256::Hash::hash(&input).to_byte_array();
        let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(format!("{dir}/{name}"), &input)?;
        written.insert(name);
    }
    eprintln!("{target}: {} seeds", written.len());
    Ok(())
}

fn main() -> std::io::Result<()> {
    write("diff_mnsync", mnsync::mnsync_seed_inputs())?;
    write("diff_netfulfilled", mnsync::netfulfilled_seed_inputs())?;
    write("diff_mncache", mnsync::mncache_seed_inputs())?;
    write("diff_flatdb_load", mnsync::flatdb_seed_inputs())
}
