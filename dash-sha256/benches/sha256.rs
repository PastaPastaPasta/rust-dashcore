//! Criterion benches with the shapes and names of Core's `bench_dash` SHA-256 benches.
//! Instruction counts per byte: `contrib/ins-per-byte.sh`.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};

mod workloads;

fn sha256(c: &mut Criterion) {
    let mut group = c.benchmark_group("sha256");
    for mut w in workloads::workloads() {
        group.throughput(Throughput::Bytes(w.bytes as u64));
        group.bench_function(w.name.clone(), |b| b.iter(|| w.run()));
    }
    group.finish();
}

criterion_group!(benches, sha256);
criterion_main!(benches);
