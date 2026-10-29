//! Writes `diff_bls_decode`'s public seeds (`node_l1::bls`), each file one target input named
//! by the first 8 bytes of its SHA-256, to `node-fuzz/public-seeds/diff_bls_decode/`: the first
//! sample of every class of base-sdk's Core-judged BLS wrapper corpus
//! (`pkgs/pkc/corpus/bls_wrapper.json5`, eager rows), as class-0 (raw) input on every leg:
//! public keys and signatures eager and lazy, secret keys eager, each under both flags.
//!
//! `cargo run -p node-l1 --features oracle --example bls_seeds -- <bls_wrapper.json5>`, the
//! corpus at the pinned base SDK revision.

use bitcoin_hashes::sha256;
use serde_json::Value;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn main() -> std::io::Result<()> {
    let path = std::env::args().nth(1).expect("the path of bls_wrapper.json5");
    let text = std::fs::read_to_string(&path)?;
    let json: String =
        text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    let corpus: Value = serde_json::from_str(&json).expect(&path);

    let dir = format!("{}/../node-fuzz/public-seeds/diff_bls_decode", env!("CARGO_MANIFEST_DIR"));
    // A corpus change can drop a class: start empty.
    if std::path::Path::new(&dir).exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let mut written = std::collections::BTreeSet::new();
    for (kind, name) in [(0u8, "pk"), (1, "sig"), (2, "sk")] {
        let mut classes = std::collections::BTreeSet::new();
        for row in corpus[name]["eager"].as_array().expect(name) {
            if !classes.insert(row["class"].as_str().expect("class")) {
                continue;
            }
            let bytes = unhex(row["in"].as_str().expect("in"));
            let modes: &[u8] = if kind == 2 {
                &[0]
            } else {
                &[0, 4]
            };
            for mode in modes {
                for legacy in [0, 8] {
                    let input = [&[kind | mode | legacy][..], &bytes].concat();
                    let digest = sha256::Hash::hash(&input).to_byte_array();
                    let file: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
                    std::fs::write(format!("{dir}/{file}"), &input)?;
                    written.insert(file);
                }
            }
        }
        eprintln!("{name}: {} classes", classes.len());
    }
    eprintln!("diff_bls_decode: {} seeds", written.len());
    Ok(())
}
