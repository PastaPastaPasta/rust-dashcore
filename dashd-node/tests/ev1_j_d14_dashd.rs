//! EV1-J J0b-3 (OD-CP-74): D14's scan-capacity rows, D14-33 and D14-34 of
//! `contrib/node/ev1-j-rows.toml`, each judged by dashd v24.0.0's `submitblock` and by V on
//! the same bytes, as `ev1_j_dashd` judges the other block rows (its module docs say what is
//! compared), on the llmq_60_75 devnet of `ev1_j/d14.rs`. They claim capacity and no over-scan
//! for llmq_60_75 under `-llmqplatform` on a devnet, and nothing about mainnet or testnet, whose
//! Platform types scan 25.
//!
//! The scan an unlock's block reads holds `signingActiveQuorumCount + 1` = 33 Platform quorums
//! (`evo/assetlocktx.cpp:122`), 33 being V's `MAX_QUORUM_SCAN`
//! (`dash-dml/src/specialtx/quorum.rs`), and every hash or `SeamError::ScanCapacity`, never a
//! truncation (EV1 design D14). At the tip, 1491, Core's scan order is `[B0..B31, A0, A1]`
//! (index rounds, `llmq/blockprocessor.cpp:735-815`), which the test first reads from both
//! nodes: Core's `quorum list 34` and V's unbounded `OnVBinding::scan_quorums` of 34 (not the
//! 33-slot block seam), equal, with A1 the 34th.
//!
//! - D14-33: U33, an unlock under A0, the 33rd, in block 1492 from the producer's mempool and
//!   template: accept on both. A V scanning 32 rejects it (`bad-assetunlock-too-old-quorum`).
//!   Its control is the same block with U34 in U33's place (`bad-assetunlock-too-old-quorum`).
//! - D14-34: U34, under A1, the 34th, on the tip's template: `bad-assetunlock-too-old-quorum`,
//!   BLOCK_CONSENSUS, on both. A scan of 34 would accept it; V's 33 slots stop such a scan
//!   with ScanCapacity, so a V scanning 34 goes down on the block. Its control is U33 in its
//!   place (`bad-cbtx-assetlocked-amount`, the special-tx loop answered first), and its age
//!   control (`b_age_control`) the same U34, byte for byte, in block 1481 on validators of the
//!   setup chain to 1480, where the scan holds A0 and A1: accept.
//!
//! A1 is newer than A0 by height, so a V scanning a rotated type by height alone fails the order
//! precondition and both rows (`dash-evo/src/llmq/scan.rs`'s `mined_indexed_until`); V's side of
//! the precondition is asserted after the rows, so such a V shows their verdicts too. V must be
//! up after each probe: a ScanCapacity abort is a failure of the probe, never a hang.
//!
//! The members' order for rotated quorums (`ev1_j/d14.rs`) closes J-D9 for rotation only, on two
//! checks. The full order: for each of the 34 quorums (A0, A1, B0 to B31), V's ordered members,
//! the vector its commitment is signed from, equal Core's ordered members from `quorum info`
//! (`rpc/quorums.cpp:214-239`, `BuildQuorumInfo`) at every one of the 60 positions; asserted after
//! the rows, so a V with another order shows their verdicts too, and the first differing position
//! of each quorum is printed. And Core binds the member set to the signature (OD-CP-74 q4): each B
//! commitment marks two members invalid, and Core refuses B0's twin, its invalid bits moved.
//!
//! Needs dashd v24.0.0: `DASHD_V24_PATH`, else setup-dashd's cache
//! (`DASHVERSION=24.0.0 python3 contrib/setup-dashd.py`). `SKIP_DASHD_TESTS` skips the
//! dashd test; the rows' check always runs.

// The J0 fixtures' modules, which this target shares and partly uses.
#[allow(dead_code)]
mod ev1_j;
mod v24;
// D14's devnet fixture, beside J0b-1's; only this target builds it.
#[path = "ev1_j/d14.rs"]
mod d14;

use std::path::Path;
use std::sync::Arc;

use dash_consensus::pow::PowParams;
use dash_consensus::primitives::{Header, core_serialize};
use dash_consensus::{Block, Tx};
use dash_primitives::BlockHash;
use dashd_mining::{Grind, grind};
use ev1_j::harness::*;

use d14::{BEFORE_B, D14Chain, DEVNET, TIME, TIP};

/// The stage this target runs.
const STAGE: &str = "J0b-3";

/// A block of `template`'s header and transactions with `extra` appended, ground on `pow`. A
/// devnet's CbTx is version 3 (V24 inactive) and commits to no unlock, so it stays as it is: the
/// template's, which counts none of `extra`.
fn appended(template: &Block, extra: &[Arc<Tx>], pow: &PowParams) -> Arc<Block> {
    assert_eq!(template.txs()[0].raw().extra_payload()[..2], [3, 0], "a version 3 CbTx");
    let mut txs = template.txs().to_vec();
    txs.extend(extra.iter().cloned());
    ground(template, txs, pow)
}

/// `block` with `from` replaced by `to`, ground on `pow`.
fn replaced(block: &Block, from: &Tx, to: &Arc<Tx>, pow: &PowParams) -> Arc<Block> {
    let mut txs = block.txs().to_vec();
    let at = txs.iter().position(|t| t.txid() == from.txid()).expect("the block carries it");
    txs[at] = Arc::clone(to);
    ground(block, txs, pow)
}

/// `txs` on `on`'s header, the merkle root theirs, ground on `pow`.
fn ground(on: &Block, txs: Vec<Arc<Tx>>, pow: &PowParams) -> Arc<Block> {
    let mut block = Block::new(Header::new(*on.header().raw()), txs);
    let mut tries = 1_000_000;
    assert_eq!(grind(&mut block, pow, &mut tries, &|| false), Grind::Found);
    Arc::new(block)
}

/// U33 and U34 differ in their quorum hash and signature only: the payload's last 128 bytes
/// (`assetlocktx.h:109-119`: version, index, fee, requested height, quorum hash, signature).
fn differ_in_quorum_only(u33: &Tx, u34: &Tx) {
    let (a, b) = (core_serialize(u33), core_serialize(u34));
    assert_eq!(a.len(), b.len(), "U33 and U34 have one size");
    let tail = 32 + 96;
    assert_eq!(a[..a.len() - tail], b[..b.len() - tail], "U33 and U34 differ in their tail only");
    assert_ne!(a[a.len() - tail..a.len() - 96], b[b.len() - tail..b.len() - 96], "their quorums");
}

/// D14's probes and controls, built before any validator sees them.
fn probes(chain: &D14Chain, binary: &Path, scratch: &Path, pow: &PowParams) -> Vec<Probe> {
    differ_in_quorum_only(&chain.u33, &chain.u34);
    let probe = |row, name: &str, expect, block| Probe {
        row,
        name: name.to_owned(),
        expect,
        block,
        below: None,
        cl_checks: None,
    };
    let mined = Arc::new(chain.mined_with(binary, scratch, &chain.producer, TIP, &chain.u33, pow));
    let age =
        chain.mined_with(binary, scratch, &chain.producer_before_b, BEFORE_B, &chain.u34, pow);
    vec![
        probe(
            "D14-33",
            "U34 (A1, the 34th) in U33's place",
            "b_control",
            replaced(&mined, &chain.u33, &chain.u34, pow),
        ),
        probe("D14-33", "U33 (A0, the 33rd) from the mempool at 1491", "b", mined),
        probe(
            "D14-34",
            "U34 (A1, the 34th) on the template at 1491",
            "b",
            appended(&chain.template, &[Arc::clone(&chain.u34)], pow),
        ),
        probe(
            "D14-34",
            "U33 in U34's place on the template",
            "b_control",
            appended(&chain.template, &[Arc::clone(&chain.u33)], pow),
        ),
        probe(
            "D14-34",
            "U34 (A1, in the scan of [A0, A1]) from the mempool at 1480",
            "b_age_control",
            Arc::new(age),
        ),
    ]
}

/// The displayed hash of a base block's internal bytes.
fn shown(hash: &[u8; 32]) -> String {
    BlockHash::from(*hash).to_string()
}

/// Prints the fixture: heights, commitments and members, Core's refusals, the order on both
/// nodes.
fn report(chain: &D14Chain) {
    for (name, q) in chain.quorums() {
        eprintln!(
            "{name:<4} base {} index {:<2} mined {} invalid {:?} members {}..{} ({})",
            shown(&q.hash),
            q.index,
            q.mined,
            q.invalid,
            &q.members[0][..8],
            &q.members[q.members.len() - 1][..8],
            q.members.len()
        );
    }
    for r in &chain.refused {
        eprintln!("Core refused {}: {}", r.what, r.line);
    }
    eprintln!("Core's quorum list 34 at {TIP}: {:#?}", chain.core_order);
    eprintln!("V's scan of 34 at {TIP}: {:#?}", chain.v_order);
}

/// The gate runs this whole target, with no name filter: it requires `test-support`, so
/// node-gate.yml picks it as a feature-gated target (`.github/workflows/node-gate.yml:311-363`)
/// for the `features` shard's `test` step (`:715`). Scheduled and nightly dispatches skip that
/// shard job (`:614`).
mod sut {
    use super::*;

    /// The rows file holds: its check is `ev1_j_dashd`'s.
    #[test]
    fn d14_rows_are_well_formed() {
        let problems = rows_file_problems(&rows_text(), &pairs_text());
        assert!(problems.is_empty(), "{problems:#?}");
        let rows = parse_rows(&rows_text());
        for id in ["D14-33", "D14-34"] {
            let row = rows.iter().find(|r| r.id() == id).expect("the row");
            assert_eq!(run_stage(row, "b"), Some(STAGE), "{id}");
        }
        assert!(REACHED.contains(&STAGE));
    }

    /// The unlock's sign hash takes the chain's Platform type, regtest's by default, so the
    /// J0b-1 and J0b-2 unlocks sign as before.
    #[test]
    fn the_unlock_sign_hash_defaults_to_regtest_platform() {
        use dash_params::LlmqType;
        let unlock = ev1_j::Unlock {
            version: 1,
            index: 1,
            outputs: 1,
            requested: 1480,
            quorum: [0x42; 32],
        };
        let p2pkh = [
            0x76, 0xa9, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88,
            0xac,
        ];
        let regtest = unlock.sign_hash_as(LlmqType::LlmqTestPlatform, &p2pkh);
        assert_eq!(unlock.sign_hash(&p2pkh), regtest);
        assert_ne!(unlock.sign_hash_as(d14::PLATFORM, &p2pkh), regtest);
    }

    /// J0b-3: D14-33 and D14-34 on the devnet, each probe on fresh validators.
    #[test]
    fn j0b3_d14_rows_equal_dashds() {
        assert!(rows_file_problems(&rows_text(), &pairs_text()).is_empty());
        let Some(binary) = v24::dashd_binary() else {
            return;
        };
        let scratch = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let chain = d14::produce_d14(&binary, &scratch.path().join("d14"));
        report(&chain);

        // The order precondition, from both sides. V's side is asserted after the rows are
        // judged, so a V that scans in another order shows its rows' verdicts too.
        let mut expected: Vec<String> = chain.b.iter().map(|q| shown(&q.hash)).collect();
        expected.extend(chain.a.iter().map(|q| shown(&q.hash)));
        assert_eq!(chain.core_order, expected, "Core's scan order at {TIP}: [B0..B31, A0, A1]");
        assert_eq!(chain.core_order[33], shown(&chain.a[1].hash), "the 34th is A1");
        // A0 and A1 share their mined block, the one `give_and_mine` found both commitments in;
        // B0 to B31 theirs. Core's `minedBlock` for each.
        for (cycle, quorums) in [(d14::A_CYCLE, &chain.a), (d14::B_CYCLE, &chain.b)] {
            let carried = chain.block(cycle + d14::WINDOW_START).hash().to_string();
            for q in quorums.iter() {
                assert_eq!(
                    q.core_mined,
                    carried,
                    "Core mined {} in block {}",
                    shown(&q.hash),
                    q.mined
                );
            }
        }
        assert_ne!(chain.a[0].hash, chain.a[1].hash);
        if chain.v_order != chain.core_order {
            eprintln!("V's scan order at {TIP} is not Core's");
        }
        // The full member order, V's against Core's `quorum info`.
        let order = chain.member_order_problems();
        if order.is_empty() {
            let positions: Vec<usize> =
                chain.quorums().map(|(_, q)| q.core_members.len()).collect();
            assert!(positions.iter().all(|&n| n == 60), "full quorums: {positions:?}");
            eprintln!(
                "member order: {} quorums, 60 positions each, V's equal Core's quorum info",
                positions.len()
            );
        } else {
            eprintln!("V's member order is not Core's: {order:#?}");
        }

        // Validators of the setup chain to 1480, then to the tip.
        let before_b = scratch.path().join("validator-1480");
        let at_tip = scratch.path().join("validator");
        let setup_before_b = chain.setup_to(BEFORE_B);
        let pow = setup_validators_on(
            DEVNET,
            &binary,
            scratch.path(),
            &before_b,
            TIME,
            setup_before_b,
            &[],
            0,
        );
        copy_dir(&before_b, &at_tip);
        let pow_tip = setup_validators_on(
            DEVNET,
            &binary,
            scratch.path(),
            &at_tip,
            TIME,
            &chain.setup,
            &[],
            setup_before_b.len(),
        );
        assert_eq!(pow, pow_tip, "one proof-of-work rule");
        eprintln!("D14 chain on fresh validators at {:?}", started.elapsed());

        let probes = probes(&chain, &binary, scratch.path(), &pow);
        let validators = |base, setup| Validators {
            chain: DEVNET,
            binary: &binary,
            base,
            setup,
            stale: &[],
            time: TIME,
            scratch: scratch.path(),
        };
        let (old, new) = (validators(&before_b, setup_before_b), validators(&at_tip, &chain.setup));
        let rows = parse_rows(&rows_text());
        let on = |p: &Probe| {
            if p.expect == "b_age_control" {
                &old
            } else {
                &new
            }
        };
        judge_stage_on(&rows, STAGE, "b", &probes, on, started);
        assert_eq!(chain.v_order, chain.core_order, "V's scan order at {TIP} is Core's");
        assert!(order.is_empty(), "V's member order is Core's quorum info: {order:#?}");
        assert_eq!(chain.quorums().count(), 34, "A0, A1 and B0 to B31");
        eprintln!(
            "D14: fixture built in {:?}; the stage done at {:?}",
            chain.built_in,
            started.elapsed()
        );
    }
}
