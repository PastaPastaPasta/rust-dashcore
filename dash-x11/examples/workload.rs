//! Fixed X11 workloads for instruction counts (`contrib/ins-per-call.sh`).
//!
//! `workload <implementation> <stage> <iterations>` runs `x11` on an 80-byte zero buffer, as
//! bench_dash's `Pow_X11_0080b` does, or one chain function as `HashX11` calls it (BLAKE on 80
//! bytes, the others on 64), `<iterations>` times. `<implementation>` is `auto` or a `Use` set:
//! `standard`, `sse`, `sse,aes`, `sse,avx2`, `sse,aes,avx2`. The stage `self-test` runs
//! `Implementation::detect` for that set instead: the startup check's cost.

use std::hint::black_box;

use dash_x11::{Implementation, Stage, Use};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, imp, stage, n] = &args[..] else {
        eprintln!("usage: workload <implementation> <stage|x11> <iterations>");
        std::process::exit(2);
    };
    let allow = match imp.as_str() {
        "auto" => Use::ALL,
        names => names.split(',').fold(Use::STANDARD, |u, n| {
            u | match n {
                "standard" => Use::STANDARD,
                "sse" => Use::SSE,
                "aes" => Use::AES,
                "avx2" => Use::AVX2,
                _ => panic!("unknown backend {n}"),
            }
        }),
    };
    let n: u64 = n.parse().unwrap();
    if stage == "self-test" {
        for _ in 0..n {
            black_box(Implementation::detect(black_box(allow)));
        }
        return;
    }
    let imp = if imp == "auto" {
        Implementation::auto()
    } else {
        Implementation::detect(allow)
    };
    let buf = [0u8; 80];
    if stage == "x11" {
        for _ in 0..n {
            black_box(imp.x11(black_box(&buf)));
        }
        return;
    }
    let stage = *Stage::ALL.iter().find(|s| s.name() == stage).expect("unknown stage");
    let input = if stage == Stage::Blake {
        &buf[..]
    } else {
        &buf[..64]
    };
    for _ in 0..n {
        black_box(imp.stage(stage, black_box(input)));
    }
}
