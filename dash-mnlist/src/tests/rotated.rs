//! Step 5 over LQ1's rotated captures (`test-vectors/lq1-rot`, provenance in its
//! `PROVENANCE.md`): one LLMQ_60_75 cycle per era on mainnet and testnet, with the work-block lists
//! projected to the SML (`CDeterministicMN::to_sml_entry`), the coinbases and the previous cycles'
//! rows, and Core's row for the cycle. Over the engine's views every index gets Core's members,
//! every mined commitment verifies, and a list with a signer's and a non-signer's operator keys
//! swapped does not. LQ1's row for the cycle is Core's, and replaces a wrong candidate (D20). The
//! two activation cycles read the rows of cycles before rotation, which Core never stores: its
//! absence, so they compute too.

use std::io::Read;
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::Commitment;
use dash_types::codec::BaseCodec;
use serde_json::Value;

use super::{hex, u32_of};
use crate::convert::snapshot;
use crate::test_util::{CaptureChain, dml_lists, held};
use crate::view::HeldRow;
use crate::{LlmqConfig, QuorumState, RequestKey};
use dash_evo::llmq::QuorumSnapshot;
use dash_p2p_core::QuorumSnapshot as WireSnapshot;

fn row(hex_row: &Value) -> QuorumSnapshot {
    let bytes = hex(hex_row.as_str().expect("a row"));
    let mut r = &bytes[..];
    let wire = WireSnapshot::decode(&mut r).expect("Core's row");
    assert!(r.is_empty());
    snapshot(&wire)
}

fn read_gz(path: &std::path::Path) -> Value {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).expect("a capture"))
        .read_to_end(&mut bytes)
        .expect("gzip");
    serde_json::from_slice(&bytes).expect("json")
}

#[test]
fn rotated_captures_verify_over_the_sml_views() {
    let dir = format!("{}/../test-vectors/lq1-rot", env!("CARGO_MANIFEST_DIR"));
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("the captures")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.to_string_lossy().ends_with(".json.gz"))
        .collect();
    paths.sort();
    let (mut eras, mut verified, mut swaps, mut activation, mut no_row) = (0, 0, 0, 0, 0);
    for path in &paths {
        let v = read_gz(path);
        let net = v["network"].as_str().expect("a network");
        let network = match net {
            "mainnet" => Network::Main,
            "testnet" => Network::Testnet3,
            other => panic!("{other}"),
        };
        let params = ChainParamsBuilder::new(network, Release::V24_0).build().expect("params");
        let cfg = LlmqConfig::new(Arc::new(params)).expect("in the domain");
        let cycle = u32_of(&v["cycle"]);
        let chain = CaptureChain::new(
            u32_of(&v["first_height"]),
            &hex(v["headers"].as_str().expect("headers")),
        );
        let mut engine = held(&v, &chain, &cfg, None, dml_lists);
        let mut swapped_once = false;
        let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
        for q in v["quorums"].as_array().expect("quorums") {
            let index = u32_of(&q["index"]);
            let base = BlockId(cycle + index);
            let members = engine
                .members(&chain, &cfg, rotated, base, false)
                .unwrap_or_else(|e| panic!("{net} {cycle} index {index}: {e:?}"));
            if v["rows"].as_array().expect("rows").is_empty() {
                activation += 1;
            }
            let ours: Vec<String> = members
                .iter()
                .map(|m| {
                    let mut h = *m.pro_reg_tx_hash.as_bytes();
                    h.reverse();
                    h.iter().map(|b| format!("{b:02x}")).collect()
                })
                .collect();
            let core: Vec<&str> = q["members"]
                .as_array()
                .expect("members")
                .iter()
                .map(|m| m.as_str().expect("hex"))
                .collect();
            assert_eq!(ours, core, "{net} {cycle} index {index}");
            let Some(qc) = q.get("commitment") else {
                continue;
            };
            assert_eq!(q["verifies"], true, "Core accepts every captured commitment");
            let qc = Commitment::decode(&mut &hex(qc.as_str().expect("hex"))[..]).expect("Core's");
            let mined = u32_of(&q["mined_height"]);
            let state = engine.step5(qc.clone(), mined, &chain, &cfg);
            assert!(matches!(state, QuorumState::Verified(_)), "{net} {cycle} {index}: {state:?}");
            verified += 1;
            // Discrimination, once per capture: a signer's and a non-signer's operator keys
            // swapped fail the members signature.
            if !swapped_once {
                let signed = |i: usize| qc.signers().get(i as u64) == Some(true);
                let s = (0..members.len()).find(|i| signed(*i));
                let n = (0..members.len()).find(|i| !signed(*i));
                if let (Some(s), Some(n)) = (s, n) {
                    let swap = (members[s].operator_key, members[n].operator_key);
                    let swapped = held(&v, &chain, &cfg, Some(swap), dml_lists);
                    assert_eq!(
                        swapped.step5(qc, mined, &chain, &cfg),
                        QuorumState::Unverified,
                        "{net} {cycle} {index}: a swapped signer verifies"
                    );
                    swapped_once = true;
                    swaps += 1;
                }
            }
        }
        // A wrong candidate for the cycle's own row, then the row LQ1 computes: Core's. Where Core
        // stores none (its early return, 28e), LQ1 confirms none and the candidate stays.
        let core = (!v["row"].is_null()).then(|| row(&v["row"]));
        let mut wrong = core.clone().unwrap_or_default();
        wrong.active_quorum_members.push(true);
        engine.hold_candidate(rotated, chain.hash(cycle), wrong.clone());
        engine.confirm_held_rows(&chain, &cfg).expect("no stop");
        let expected = match core {
            Some(core) => HeldRow::Core(core),
            None => HeldRow::Candidates(vec![wrong]),
        };
        assert_eq!(engine.held_row(rotated, chain.hash(cycle)), Some(&expected), "{net} {cycle}");
        no_row += usize::from(v["row"].is_null());
        eras += 1;
    }
    assert_eq!(eras, 12);
    assert_eq!(verified, 228, "every mined commitment of the captures");
    // Two captures mine only commitments every member signed: nothing to swap.
    assert_eq!(swaps, 9 - 2, "swapped-signer checks");
    assert_eq!(activation, 2 * 32, "the two DIP0024 activation cycles");
    assert_eq!(no_row, 1, "Core's early return");
}

/// D20 (validator r1 B, Sol r2 minor 1): `BlsUnsupported` under a candidate row is no verdict. In
/// the pre-V19 era `mainnet-1738656` (the legacy scheme), cycle H − C holds one wrong candidate
/// row, and a masternode that signs only among its members gets an operator key outside the
/// prime-order subgroup, a legacy-decoded `an exotic key` operand of BS12's Core-backed corpus
/// (base-sdk `pkgs/pkc/corpus/bls_oracle/edge.json5`; Core: false, the legacy flag's verdict
/// dash-pkc cannot give). Step 5 asks for the cycle's `qrinfo` again rather than ending
/// `Unverified`; a later honest candidate, Core's row, verifies.
#[test]
fn an_unsupported_key_under_a_wrong_row_is_no_verdict() {
    const EXOTIC: &str = "99d56efc828f1263dc540de1fb59a7d550e13c7e9aa82d81cd6a2420412255f7ceed3ab7dbc5fa186c9470566de12a54";
    let path =
        format!("{}/../test-vectors/lq1-rot/mainnet-1738656.json.gz", env!("CARGO_MANIFEST_DIR"));
    let v = read_gz(path.as_ref());
    let params = ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("params");
    let cfg = LlmqConfig::new(Arc::new(params)).expect("in the domain");
    let v19 = cfg.params().consensus.v19_height.to_u32();
    let rotated = cfg.params().consensus.llmq_type_dip0024_instant_send;
    let cycle = u32_of(&v["cycle"]);
    let chain = CaptureChain::new(
        u32_of(&v["first_height"]),
        &hex(v["headers"].as_str().expect("headers")),
    );
    let h_c = v["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .max_by_key(|r| u32_of(&r["cycle"]))
        .expect("a row");
    let (h_c_cycle, core_row) = (chain.hash(u32_of(&h_c["cycle"])), row(&h_c["snapshot"]));
    let mut wrong = core_row.clone();
    for bit in &mut wrong.active_quorum_members {
        *bit = !*bit;
    }

    let honest = held(&v, &chain, &cfg, None, dml_lists);
    let mut engine = held(&v, &chain, &cfg, None, dml_lists);
    engine.hold_candidates(rotated, h_c_cycle, vec![wrong]);
    // The first commitment with a signer only the wrong row makes a member.
    let (qc, mined, pro) = v["quorums"]
        .as_array()
        .expect("quorums")
        .iter()
        .find_map(|q| {
            let qc =
                Commitment::decode(&mut &hex(q.get("commitment")?.as_str()?)[..]).expect("Core's");
            let base = BlockId(cycle + u32_of(&q["index"]));
            let cores = honest.members(&chain, &cfg, rotated, base, false).expect("Core's members");
            let under_wrong = engine.members(&chain, &cfg, rotated, base, true).ok()?;
            let pro = under_wrong.iter().enumerate().find_map(|(i, m)| {
                (qc.signers().get(i as u64) == Some(true)
                    && !cores.iter().any(|c| c.pro_reg_tx_hash == m.pro_reg_tx_hash))
                .then_some(m.pro_reg_tx_hash)
            })?;
            Some((qc, u32_of(&q["mined_height"]), pro))
        })
        .expect("a signer only the wrong row makes a member");
    assert!(mined < v19, "the legacy scheme");
    engine.set_operator_key(&pro, hex(EXOTIC).try_into().expect("48 bytes"));
    let state = engine.step5(qc.clone(), mined, &chain, &cfg);
    assert!(matches!(state, QuorumState::Pending(RequestKey::QrInfo(_))), "{state:?}");

    engine.hold_candidate(rotated, h_c_cycle, core_row);
    let state = engine.step5(qc, mined, &chain, &cfg);
    assert!(matches!(state, QuorumState::Verified(_)), "{state:?}");
}

#[test]
fn public_replay_verifies_every_mined_rotated_commitment() {
    let dir = format!("{}/../test-vectors/lq1-rot", env!("CARGO_MANIFEST_DIR"));
    let mut captures = 0;
    let mut commitments = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.to_string_lossy().ends_with(".json.gz") {
            continue;
        }
        let replay =
            crate::test_util::replay_rotated_capture(&std::fs::read(&path).unwrap(), dml_lists);
        if path.file_name().unwrap() == "testnet-838368.json.gz" {
            assert!(replay.is_empty());
        }
        for (ty, hash, state, qc) in replay {
            assert_eq!(ty, qc.llmq_type());
            assert_eq!(hash, qc.quorum_hash());
            assert!(matches!(state, QuorumState::Verified(_)), "{}: {state:?}", path.display());
            commitments += 1;
        }
        captures += 1;
    }
    assert_eq!((captures, commitments), (12, 228));
}
