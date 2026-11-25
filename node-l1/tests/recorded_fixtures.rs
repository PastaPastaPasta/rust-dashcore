//! The recorded expectations of `dash-consensus/tests/primitives_recorded.rs` (CS0 §9.1):
//! computed by Dash Core's oracle at the linked pin, and checked against it here on every run.
//!
//! Only rows that decode no generated transaction are recorded: merkle roots over symbolic
//! leaves, block813851 (Core's public benchmark block), and Core's `CheckBlock` message for a
//! transaction that fails `CheckTransaction`. Every codec row takes its expectations live
//! instead (`l1_boundaries.rs`).
//!
//! Regenerate with `NODE_L1_REGENERATE=1 cargo test -p node-l1 --features oracle --test
//! recorded_fixtures`, then review the diff.

#[path = "../../dash-consensus/tests/support/recorded.rs"]
mod recorded;
#[path = "support/spec.rs"]
mod spec;

use dash_core_vectors::hex::hex_str;
use dash_core_vectors::read_core_file;
use dash_oracle_sys::{Chain, Oracle, Pin, Rejection, ResultCode, Verdict};

use recorded::{BlockCase, CheckBlockCase, MerkleCase, Recorded};
use spec::{In, TxSpec, block_of};

/// The value every pin returns; panics if they disagree.
fn agree<T: PartialEq>(oracles: &[Oracle], f: impl Fn(&Oracle) -> T) -> T {
    let mut values: Vec<T> = oracles.iter().map(f).collect();
    assert!(values.windows(2).all(|w| w[0] == w[1]), "the pins disagree");
    values.swap_remove(0)
}

/// A block whose second transaction spends one outpoint twice: `CheckTransaction` rejects it.
fn check_block_input() -> Vec<u8> {
    let duplicate = In {
        script: vec![],
        ..In::spend(7)
    };
    let spends_twice = TxSpec {
        inputs: vec![duplicate.clone(), duplicate],
        ..TxSpec::plain()
    };
    block_of(&[TxSpec::coinbase().bytes(), spends_twice.bytes()])
}

fn merkle_inputs() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("empty", vec![]),
        ("one leaf", vec![1]),
        ("[a,b,c] not mutated", vec![1, 2, 3]),
        ("[a,b,b] not mutated: the odd tail's duplicate is not compared", vec![1, 2, 2]),
        ("[a,a,b] mutated", vec![1, 1, 2]),
        ("[A,B,B,C] not mutated: only pairs at even offsets", vec![1, 2, 2, 3]),
        ("[A,B,C,C] mutated", vec![1, 2, 3, 3]),
        ("[a,b,c,d,e,f]", vec![1, 2, 3, 4, 5, 6]),
        ("[a,b,c,d,e,f,e,f]: same root, mutated one level up", vec![1, 2, 3, 4, 5, 6, 5, 6]),
        ("five leaves", vec![1, 2, 3, 4, 5]),
    ]
}

fn compute() -> Recorded {
    let oracles: Vec<Oracle> = Oracle::all().collect();

    let merkle = merkle_inputs()
        .into_iter()
        .map(|(name, leaves)| {
            let hashes: Vec<[u8; 32]> = leaves.iter().map(|&n| [n; 32]).collect();
            let (root, mutated) = agree(&oracles, |o| o.merkle_root(&hashes).expect("merkle_root"));
            MerkleCase {
                name: name.into(),
                leaves,
                root: hex_str(&root),
                mutated,
            }
        })
        .collect();

    let raw = read_core_file("src/bench/data/block813851.raw");
    let (hash, root, mutated) = agree(&oracles, |o| {
        let decoded = o.block_decode(&raw).expect("block813851 decodes");
        assert_eq!(decoded.consumed, raw.len());
        assert_eq!(decoded.reencoded, raw);
        (decoded.hash, decoded.merkle_root, decoded.mutated)
    });
    let block813851 = BlockCase {
        hash: hex_str(&hash),
        merkle_root: hex_str(&root),
        mutated,
    };

    let input = check_block_input();
    let rejection: Rejection = agree(&oracles, |o| {
        match o.check_block(&Chain::regtest(), &input, false, false).expect("check_block") {
            Verdict::Reject(r) => r,
            Verdict::Accept => panic!("CheckBlock accepted the duplicate-input block"),
        }
    });
    let check_block_tx_failure = CheckBlockCase {
        block_hex: hex_str(&input),
        result: match rejection.result {
            ResultCode::Block(code) => code,
            other => panic!("CheckBlock returned a {other:?} result"),
        },
        reason: rejection.reason,
        debug_message: rejection.debug_message,
    };

    Recorded {
        provenance: format!(
            "Computed by Dash Core's oracle (dash-oracle-sys), identical on pins {}; regenerate with node-l1's recorded_fixtures test.",
            oracles.iter().map(Oracle::core_version).collect::<Vec<_>>().join(" and ")
        ),
        merkle,
        block813851,
        check_block_tx_failure,
    }
}

/// The committed values must equal the oracle's. The provenance line names the oracle build, so it
/// changes with every shim revision and is not compared.
#[test]
fn recorded_fixtures_match_the_oracle() {
    let computed = compute();
    if std::env::var_os("NODE_L1_REGENERATE").is_some() {
        let json = serde_json::to_string_pretty(&computed).expect("serializes") + "\n";
        std::fs::write(recorded::path(), json).expect("writes the fixtures");
        return;
    }
    let mut committed = recorded::load();
    committed.provenance.clone_from(&computed.provenance);
    assert!(
        committed == computed,
        "the committed fixtures differ from the oracle; regenerate and review"
    );
}

#[test]
fn pins_are_linked() {
    for pin in Pin::ALL {
        assert!(!Oracle::new(pin).core_version().is_empty());
    }
}
