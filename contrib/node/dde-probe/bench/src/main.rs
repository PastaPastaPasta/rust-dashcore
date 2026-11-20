//! Block decode cost: base-sdk at 33fcc59 against base-sdk with BS4a.
//!
//! `bench old|new` decodes every block of base-sdk's mainnet `blocks.json5`
//! corpus `ITERS` times (default 200). Compare `perf stat -e instructions:u`
//! counts, after subtracting an `ITERS=0` run for loading the corpus.

fn h(s: &str) -> Vec<u8> {
    hex::decode(s).expect("hex")
}

fn main() {
    let which = std::env::args().nth(1).unwrap_or_default();
    assert!(which == "old" || which == "new", "usage: bench old|new");
    bench(&which);
}

fn corpus_blocks() -> Vec<Vec<u8>> {
    let root = std::env::var("BSDK_ROOT").unwrap_or_else(|_| "/work/home/agent/workspace/base-sdk".into());
    let text = std::fs::read_to_string(format!("{root}/pkgs/primitives/corpus/blocks.json5")).expect("blocks corpus");
    text.match_indices("\"raw\": \"")
        .map(|(i, m)| {
            let rest = &text[i + m.len()..];
            h(&rest[..rest.find('"').expect("end")])
        })
        .collect()
}

fn bench(which: &str) {
    let blocks = corpus_blocks();
    let iters: usize = std::env::var("ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(200);
    let total: usize = blocks.iter().map(|b| b.len()).sum();
    let start = std::time::Instant::now();
    let mut txs = 0usize;
    for _ in 0..iters {
        for b in &blocks {
            let mut c = &b[..];
            if which == "old" {
                use old_types::codec::BaseCodec;
                txs += std::hint::black_box(old_prim::Block::decode(&mut c).expect("old")).transactions.len();
            } else {
                use new_types::codec::BaseCodec;
                txs += std::hint::black_box(new_prim::Block::decode(&mut c).expect("new")).transactions.len();
            }
        }
    }
    let el = start.elapsed();
    eprintln!("{which}: {} blocks, {} bytes, {iters} iters, {txs} txs, {:?} total, {:.1} MB/s",
        blocks.len(), total, el, (total * iters) as f64 / el.as_secs_f64() / 1e6);
}
