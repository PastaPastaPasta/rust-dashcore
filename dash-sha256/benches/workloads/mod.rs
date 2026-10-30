//! The workloads of Core's `bench_dash` SHA-256 benches (`src/bench/crypto_hash.cpp`), shared by
//! `benches/sha256.rs` (criterion) and `examples/workload.rs` (for instruction counters).
//!
//! Names follow Core's; `_STANDARD`, `_SSE4`, `_AVX2` and `_SHANI` select implementations as
//! Core's benches do. Like Core's, a `_SHANI` bench on a CPU without SHA-NI measures what
//! detection falls back to.

use std::hint::black_box;

use dash_sha256::{Implementation, Sha256, Sha256d, Use};

/// Core's `BUFFER_SIZE`.
const BUFFER_SIZE: usize = 1000 * 1000;

/// A bench: its name, the bytes one iteration processes (Core's `batch`), and the state one
/// iteration runs on.
pub struct Workload {
    pub name: String,
    pub bytes: usize,
    pub state: State,
}

pub enum State {
    Sha256 {
        imp: &'static Implementation,
        input: Vec<u8>,
    },
    Sha256In32 {
        imp: &'static Implementation,
        buf: [u8; 32],
    },
    D64 {
        imp: &'static Implementation,
        input: Vec<[u8; 64]>,
        out: Vec<[u8; 32]>,
    },
    Dsha256 {
        input: Vec<u8>,
    },
}

impl Workload {
    /// One iteration.
    #[inline(never)]
    pub fn run(&mut self) {
        match &mut self.state {
            // CSHA256().Write(in).Finalize(hash)
            State::Sha256 {
                imp,
                input,
            } => {
                let mut h = Sha256::with_implementation(imp);
                h.update(black_box(input));
                black_box(h.finalize());
            }
            // CSHA256().Write(in, 32).Finalize(in): each hash feeds the next.
            State::Sha256In32 {
                imp,
                buf,
            } => {
                let mut h = Sha256::with_implementation(imp);
                h.update(black_box(&*buf));
                *buf = h.finalize();
            }
            // SHA256D64(in, in, 1024): Core hashes in place; here into a separate buffer.
            State::D64 {
                imp,
                input,
                out,
            } => {
                imp.sha256d64(out, black_box(input));
                black_box(&out);
            }
            // CHash256().Write(in).Finalize(hash)
            State::Dsha256 {
                input,
            } => {
                let mut h = Sha256d::new();
                h.update(black_box(input));
                black_box(h.finalize());
            }
        }
    }
}

pub fn workloads() -> Vec<Workload> {
    let selections = [
        ("STANDARD", Use::STANDARD),
        ("SSE4", Use::SSE4),
        ("AVX2", Use::SSE4 | Use::AVX2),
        ("SHANI", Use::SSE4 | Use::SHANI),
    ];
    let mut out = Vec::new();
    for (suffix, allow) in selections {
        let imp = Implementation::detect(allow);
        out.push(Workload {
            name: format!("SHA256_{suffix}"),
            bytes: BUFFER_SIZE,
            state: State::Sha256 {
                imp,
                input: vec![0; BUFFER_SIZE],
            },
        });
        out.push(Workload {
            name: format!("SHA256_32b_{suffix}"),
            bytes: 32,
            state: State::Sha256In32 {
                imp,
                buf: [0; 32],
            },
        });
        out.push(Workload {
            name: format!("SHA256D64_1024_{suffix}"),
            bytes: 64 * 1024,
            state: State::D64 {
                imp,
                input: vec![[0; 64]; 1024],
                out: vec![[0; 32]; 1024],
            },
        });
    }
    out.push(Workload {
        name: "DSHA256_1M".to_owned(),
        bytes: BUFFER_SIZE,
        state: State::Dsha256 {
            input: vec![0; BUFFER_SIZE],
        },
    });
    out
}
