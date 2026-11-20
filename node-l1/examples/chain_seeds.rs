//! Writes each chain target's public seeds (`node-l1/chain-encodings.toml`): every program in
//! `tests/chain_seeds/*.prog`, and in `tests/chain_seeds/<target>/*.prog` if the target has its
//! own, encoded under the target's encoding (O2b §3.5: programs are committed, not bytes; O2b-4
//! §3a.2), named by the first 8 bytes of the encoding's SHA-256, to
//! `node-fuzz/public-seeds/<target>/`; a planted search's leave out its bugs' witnesses
//! (`PLANT_SEARCH_EXCLUDES`). The programs are synthetic chains on regtest.
//!
//! `cargo run -p node-l1 --features oracle --example chain_seeds`

use std::path::Path;

use node_l1::chain::intent::{chain_encodings, target_seeds};

fn main() -> std::io::Result<()> {
    let root = env!("CARGO_MANIFEST_DIR");
    let progs = Path::new(root).join("tests/chain_seeds");
    for (target, encoding) in chain_encodings() {
        let out = format!("{root}/../node-fuzz/public-seeds/{target}");
        std::fs::create_dir_all(&out)?;
        let seeds = target_seeds(&progs, target, encoding)?;
        for (name, bytes) in &seeds {
            std::fs::write(format!("{out}/{name}"), bytes)?;
        }
        eprintln!("{target}: {} seeds ({})", seeds.len(), encoding.name());
    }
    Ok(())
}
