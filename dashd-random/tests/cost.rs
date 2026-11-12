//! BL-502 test 9, not a gate: 10⁶ FAST draws of 32 bytes from the process state, or from
//! `OsRand`, for `perf stat -e instructions:u,instructions:k,cycles`. `BL502_COST_SOURCE` picks
//! `fast`, `os`, or `none` (the loop alone, the harness's baseline).

use std::hint::black_box;

use dash_p2p::rng::{OsRand, RandSource};

const DRAWS: usize = 1_000_000;

#[test]
#[ignore = "BL-502 test 9: run under perf stat with BL502_COST_SOURCE set"]
fn draws_for_perf_stat() {
    let source: Option<&dyn RandSource> = match std::env::var("BL502_COST_SOURCE").as_deref() {
        Ok("fast") => Some(&dashd_random::PROC_RAND),
        Ok("os") => Some(&OsRand),
        Ok("none") => None,
        other => panic!("BL502_COST_SOURCE={other:?}: fast, os or none"),
    };
    // The process state's chunk and STARTUP seeding, outside the counted difference.
    dashd_random::static_init();
    let mut out = [0u8; 32];
    for _ in 0..DRAWS {
        if let Some(source) = source {
            source.rand_bytes(&mut out, false);
        }
        black_box(&mut out);
    }
}
