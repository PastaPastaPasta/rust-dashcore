//! A dump of the rows node-l1's generators give (T6 OK-3): the `dml_apply` raw and checked
//! generators and the special-transaction case generator, each over fixed inputs. Two dumps taken
//! at two revisions compare byte for byte, so a change to how a generator writes a payload shows
//! as a changed row.
//!
//! `L1_ROW_DUMP=<file> cargo test -p node-l1 --features oracle --test l1_row_dump -- --ignored`
//!
//! `<file>.hits` gets how many payloads the checked walk's version probes gave
//! (`VERSION_PROBE_HITS`), apart from the rows so that a dump taken at a revision without the
//! counts compares. The dump fails if one of them is never reached.

use std::fmt::Write as _;

use dash_primitives::TxType;
use dash_types::codec::BaseCodec;
use hex_conservative::DisplayHex;
use std::sync::atomic::Ordering;

use node_l1::dml_apply::checked::{FLAG_PROBE, MAX_BLOCKS, VERSION_PROBE_HITS, Walk};
use node_l1::dml_apply::generate as generate_raw;
use node_l1::specialtx::{self, World};

/// SplitMix64, for inputs that are the same at every revision.
struct Inputs(u64);

impl Inputs {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u64() as u8).collect()
    }
}

fn case_tx(case: &specialtx::Case) -> String {
    let mut bytes = Vec::new();
    case.tx().raw().encode(&mut bytes);
    bytes.to_lower_hex_string()
}

#[test]
#[ignore = "writes a dump for a revision comparison"]
fn dump_generated_rows() {
    let path = std::env::var("L1_ROW_DUMP").expect("L1_ROW_DUMP names the output file");
    let mut out = String::new();
    let mut inputs = Inputs(0x0c3d_7e5a_11f0_0001);

    for i in 0..20_000 {
        let len = 4 + (inputs.next_u64() % 96) as usize;
        let input = inputs.bytes(len);
        if let Some(generated) = generate_raw(&input) {
            let _ = writeln!(out, "raw {i} {}", generated.block.to_lower_hex_string());
        }
    }

    for i in 0..20_000 {
        let len = 4 + (inputs.next_u64() % 160) as usize;
        let mut input = inputs.bytes(len);
        if i % 2 == 0 {
            input[1] |= FLAG_PROBE;
        }
        let Some(mut walk) = Walk::new(&input) else {
            continue;
        };
        for b in 0..MAX_BLOCKS {
            let Some(block) = walk.next_block() else {
                break;
            };
            let _ = writeln!(out, "checked {i}.{b} {}", block.to_lower_hex_string());
        }
    }

    for i in 0..20_000 {
        let len = 3 + (inputs.next_u64() % 12) as usize;
        if let Some(case) = specialtx::generate(&inputs.bytes(len)) {
            let _ = writeln!(out, "case {i} {}", case_tx(&case));
        }
    }

    // Every mutation of the two types whose version the harness sets, on every seed, in every
    // world.
    for flags in 0..128u8 {
        for tx_type in [TxType::ProviderUpdateRegistrar, TxType::ProviderUpdateRevoke] {
            let catalogue = specialtx::catalogue(tx_type);
            for variant in specialtx::variants(tx_type) {
                let Some(seed) = specialtx::seed(World::new(flags), tx_type, variant) else {
                    continue;
                };
                for mutation in &catalogue {
                    for arg in 0..4u8 {
                        let mut case = seed.clone();
                        if mutation.apply(&mut case, arg) {
                            let _ = writeln!(
                                out,
                                "mutation {flags} {tx_type:?} {variant} {} {arg} {}",
                                mutation.step,
                                case_tx(&case)
                            );
                        }
                    }
                }
            }
        }
    }

    std::fs::write(&path, out).expect("the dump is written");

    let names = [
        "(9, ProviderUpdateRevoke) if v24",
        "(9, ProviderUpdateRegistrar) if v24",
        "(10, ProviderUpdateRevoke) if max < 3",
    ];
    let hits: Vec<usize> = VERSION_PROBE_HITS.iter().map(|h| h.load(Ordering::Relaxed)).collect();
    let text: String = names.iter().zip(&hits).map(|(name, n)| format!("{name}\t{n}\n")).collect();
    std::fs::write(format!("{path}.hits"), &text).expect("the hit counts are written");
    assert!(hits.iter().all(|&n| n > 0), "a version probe is never reached:\n{text}");
}
