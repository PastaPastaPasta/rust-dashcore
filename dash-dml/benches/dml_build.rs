//! The PF0 rows of the build (EV2 §6.5 BG; PLAN EV2b) over two mainnet windows of 2,000 blocks
//! as [`Window`]s, on the fast path and on the reference twin (EV2b D24):
//!
//! - w1, mainnet's first 2,000 DIP3 blocks, 1,028,160..1,030,159, from the default list
//!   (`data/mainnet-dml-1028160-1030159.bin`, EV2b D29);
//! - w2, 1,371,028..1,373,027, from the list after 1,371,027, the largest of the lists sampled
//!   below the oracle's 2,090,000 ceiling, with its 94 quorums' members
//!   (`data/mainnet-dml-1371028-1373027.bin`, EV2b D32; a proxy for EV2's tip − 50k window).
//!
//! The rows, per window `w`:
//!
//! - `dml_build/w_{fast,twin}` (`crit:dml_build_w1`, `_w2`): `build_new_list_from_block` over
//!   the 2,000 blocks from the window's start list, each list's block hash set as
//!   `ProcessBlock` sets it;
//! - `dml_payee/{fast,twin}` (w1), `dml_payee/w2_{fast,twin}` (`crit:dml_payee`): 10⁴ `payee()`
//!   calls on the window's last list;
//! - `dml_core_order/fast` (w1), `dml_core_order/w2_fast` (`crit:dml_core_order`): `core_order()`
//!   on that list with its cache reset first (a hot update, outside the timed region). The twin
//!   runs the same order code on every call, so it has no row.
//!
//! Every iteration of `dml_build` builds the whole window; the bench checks that its last list
//! is the one node-l1's window found equal to Core's (EV2b D26, D32). The payloads are decoded
//! (and cached) by that first build, before any timing, as `CheckSpecialTx` decodes them first
//! in the node. The twin's start list is made outside the timed region.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use dash_dml::NoTrace;
use dash_dml::codec::canonical_mn_list_hash;
use dash_dml::testkit::replay::{mainnet, payee_rules};
use dash_dml::testkit::window::Window;

const W1: &[u8] = include_bytes!("data/mainnet-dml-1028160-1030159.bin");
/// `CanonicalMNListHash` at 1,030,159, Core's (node-l1's window, EV2b D26).
const W1_LAST: &str = "2e6ac0121845496697ee5d80baa5fa30d38d2f833fa8fe04aee7353f620e6000";
const W2: &[u8] = include_bytes!("data/mainnet-dml-1371028-1373027.bin");
/// `CanonicalMNListHash` at 1,373,027, Core's from the same start list (EV2b D32).
const W2_LAST: &str = "c65d020d929c74508afdba3c5fcedaba1ff5bc16488313c5199c010e35e6539e";
/// The `payee()` calls per `dml_payee` iteration.
const PAYEES: usize = 10_000;

/// The rows of window `name` (`bytes`, whose last list's hash is `last_hash`); the payee and
/// order rows' names start with `prefix`.
fn rows(c: &mut Criterion, name: &str, prefix: &str, bytes: &[u8], last_hash: &str) {
    let window = Window::decode(bytes).expect("the window");
    let twin_start = || window.twin_start();
    let last = window.build(window.start().clone(), &mut NoTrace, |_, _| {}).expect("builds");
    assert_eq!(
        canonical_mn_list_hash(&last).to_string(),
        last_hash,
        "{name}'s last list is Core's"
    );
    let twin = window.build_ref(twin_start()).expect("the twin builds it");
    let rules = payee_rules(mainnet(), last.height());

    let mut group = c.benchmark_group("dml_build");
    group.sample_size(10);
    group.bench_function(format!("{name}_fast"), |b| {
        b.iter(|| {
            black_box(window.build(window.start().clone(), &mut NoTrace, |_, _| {}))
                .expect("builds")
        })
    });
    group.bench_function(format!("{name}_twin"), |b| {
        b.iter_batched(
            twin_start,
            |start| black_box(window.build_ref(start)).expect("the twin builds it"),
            BatchSize::LargeInput,
        )
    });
    group.finish();

    let mut group = c.benchmark_group("dml_payee");
    group.sample_size(10);
    group.bench_function(format!("{prefix}fast"), |b| {
        b.iter(|| (0..PAYEES).filter(|_| black_box(&last).payee(rules).is_some()).count())
    });
    group.bench_function(format!("{prefix}twin"), |b| {
        b.iter(|| (0..PAYEES).filter(|_| black_box(&twin).payee(rules).is_some()).count())
    });
    group.finish();

    // A hot update resets the list's cached order.
    let touched = last.iter().next().expect("masternodes");
    let mut group = c.benchmark_group("dml_core_order");
    group.sample_size(10);
    group.bench_function(format!("{prefix}fast"), |b| {
        b.iter_batched_ref(
            || {
                let mut list = last.clone();
                list.update_hot(&touched.pro_tx_hash, touched.hot).expect("listed");
                list
            },
            |list| black_box(list.core_order()),
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn dml(c: &mut Criterion) {
    rows(c, "w1", "", W1, W1_LAST);
    rows(c, "w2", "w2_", W2, W2_LAST);
}

criterion_group!(benches, dml);
criterion_main!(benches);
