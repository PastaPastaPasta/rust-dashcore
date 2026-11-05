//! Writes `diff_chain`'s public seeds: each program in `tests/chain_seeds/*.prog` encoded to the
//! bytes that decode to it (O2b §3.5: programs are committed, not bytes), named by the first 8
//! bytes of the encoding's SHA-256, to `node-fuzz/public-seeds/diff_chain/`. The programs are
//! synthetic chains on regtest.
//!
//! `cargo run -p node-l1 --features oracle --example chain_seeds`

use bitcoin_hashes::sha256;
use node_l1::chain::intent::Program;

fn main() -> std::io::Result<()> {
    let root = env!("CARGO_MANIFEST_DIR");
    let out = format!("{root}/../node-fuzz/public-seeds/diff_chain");
    std::fs::create_dir_all(&out)?;
    let mut paths: Vec<_> = std::fs::read_dir(format!("{root}/tests/chain_seeds"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "prog"))
        .collect();
    paths.sort();
    for p in &paths {
        let program = Program::parse(&std::fs::read_to_string(p)?)
            .map_err(|e| std::io::Error::other(format!("{}: {e}", p.display())))?;
        let bytes = program.encode();
        let digest = sha256::Hash::hash(&bytes).to_byte_array();
        let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(format!("{out}/{name}"), &bytes)?;
    }
    eprintln!("diff_chain: {} seeds", paths.len());
    Ok(())
}
