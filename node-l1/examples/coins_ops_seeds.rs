//! Writes `diff_coins_ops`' public seeds (O2a-CO, CH2c): the hand-written inputs of
//! `tests/support/coins_ops_seeds.rs`, each behind its mode byte, to
//! `node-fuzz/public-seeds/diff_coins_ops/<name>`.
//!
//! `cargo run -p node-l1 --features oracle --example coins_ops_seeds`

#[path = "../tests/support/coins_ops_seeds.rs"]
mod coins_ops_seeds;

fn main() -> std::io::Result<()> {
    let dir = format!("{}/../node-fuzz/public-seeds/diff_coins_ops", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(&dir)?;
    for seed in coins_ops_seeds::SEEDS {
        let input = [&[seed.mode.byte()][..], seed.input].concat();
        std::fs::write(format!("{dir}/{}", seed.name), input)?;
    }
    eprintln!("diff_coins_ops: {} seeds", coins_ops_seeds::SEEDS.len());
    Ok(())
}
