//! Criterion benches of X11 with the shapes of Core's `bench_dash` `Pow_X11_*` benches (an
//! all-zero input per iteration), plus each chain function on its 64-byte input. Instruction
//! counts per call: `contrib/ins-per-call.sh`.
//!
//! `x11/0080b/<backends>` pin their selection (`standard`, `sse`, `sse-aes`, `sse-avx2`, `all`)
//! for PF0's counters, so a host or a self-test fallback cannot change what they measure: a
//! selection this CPU cannot run is not registered (PF0 then reports the ID missing). The
//! unpinned `x11/<n>b` and `stage/<name>` use `dash_x11::init`'s choice.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use dash_x11::{Implementation, Stage, Use};

fn x11(c: &mut Criterion) {
    let mut group = c.benchmark_group("x11");
    for bytes in [80usize, 128, 512, 1024] {
        let input = vec![0u8; bytes];
        group.throughput(Throughput::Bytes(bytes as u64));
        group.bench_function(format!("{bytes:04}b"), |b| {
            b.iter(|| dash_x11::x11(black_box(&input)))
        });
    }
    let header = [0u8; 80];
    group.throughput(Throughput::Bytes(80));
    let pinned = [
        ("standard", Use::STANDARD, "standard"),
        ("sse", Use::SSE, "sse"),
        ("sse-aes", Use::SSE | Use::AES, "sse,aes"),
        ("sse-avx2", Use::SSE | Use::AVX2, "sse,avx2"),
        ("all", Use::ALL, "sse,aes,avx2"),
    ];
    for (id, allow, name) in pinned {
        let imp = Implementation::detect(allow);
        if imp.name() != name {
            eprintln!("x11/0080b/{id}: this CPU runs '{}' instead; not registered", imp.name());
            continue;
        }
        group.bench_function(format!("0080b/{id}"), |b| b.iter(|| imp.x11(black_box(&header))));
    }
    group.finish();
}

fn stages(c: &mut Criterion) {
    let mut group = c.benchmark_group("stage");
    let imp = Implementation::auto();
    let buf = [0u8; 80];
    for stage in Stage::ALL {
        let input = if stage == Stage::Blake {
            &buf[..]
        } else {
            &buf[..64]
        };
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_function(stage.name(), |b| b.iter(|| imp.stage(stage, black_box(input))));
    }
    group.finish();
}

criterion_group!(benches, x11, stages);
criterion_main!(benches);
