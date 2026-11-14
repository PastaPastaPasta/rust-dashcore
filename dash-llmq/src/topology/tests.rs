use dash_consensus::hash::{sha256, sha256d};
use dash_types::Numeric;
use serde_json::Value;

use super::*;

fn members(n: usize) -> Vec<ProTxHash> {
    (0..n)
        .map(|i| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&(i as u64).to_le_bytes());
            bytes[31] = 255 - i as u8;
            ProTxHash::from(bytes)
        })
        .collect()
}

fn hash_parts(parts: &[&[u8]]) -> [u8; 32] {
    sha256d(&parts.concat())
}

fn hex_bytes(value: &Value) -> Vec<u8> {
    let text = value.as_str().expect("a hex string");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("valid hex"))
        .collect()
}

fn stored_hash(value: &Value) -> ProTxHash {
    let mut bytes: [u8; 32] = hex_bytes(value).try_into().expect("a 32-byte hash");
    bytes.reverse();
    ProTxHash::from(bytes)
}

fn stored_hash_str(value: &str) -> ProTxHash {
    stored_hash(&Value::String(value.to_owned()))
}

fn lane_c_fixture() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../test-vectors/lane-c/regtest-v24.0.0.json");
    let bytes = std::fs::read(path).expect("the lane-c transcript");
    let digest: String = sha256(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(digest, "a02daa19a3a96a36c4ecea2553ee4f2b20fe22f20206340fd6c29dac0f3f7758");
    serde_json::from_slice(&bytes).expect("valid lane-c JSON")
}

fn lane_c_type_name(type_id: u8) -> &'static str {
    match type_id {
        100 => "llmq_test",
        103 => "llmq_test_dip0024",
        106 => "llmq_test_platform",
        _ => panic!("unexpected lane-c quorum type {type_id}"),
    }
}

fn lane_c_type(type_id: u8) -> LlmqType {
    LlmqType::from_base(type_id)
}

#[test]
fn ring_matches_brute_force() {
    for n in 1usize..=64 {
        let members = members(n);
        let mut edges = vec![vec![false; n]; n];
        if n > 1 {
            let largest_k = (n - 1).ilog2().saturating_sub(1).max(1);
            for (i, row) in edges.iter_mut().enumerate() {
                for k in 0..=largest_k {
                    let j = (i + (1 << k)) % n;
                    row[j] = i != j;
                }
            }
        }
        for (i, me) in members.iter().enumerate() {
            for only_outbound in [false, true] {
                let expected = members
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| edges[i][*j] || (!only_outbound && edges[*j][i]))
                    .map(|(_, member)| *member)
                    .collect();
                assert_eq!(quorum_relay_members(&members, me, only_outbound), expected, "n={n}");
                assert_eq!(
                    quorum_connections(&members, me, only_outbound, false),
                    expected,
                    "n={n}"
                );
                if n <= 3 {
                    assert_eq!(expected.len(), n - 1);
                }
            }
        }
    }
}

#[test]
fn ring_inbound_is_reverse_outbound() {
    for n in 1..=64 {
        let members = members(n);
        let outbound: Vec<_> =
            members.iter().map(|me| quorum_relay_members(&members, me, true)).collect();
        for (i, a) in members.iter().enumerate() {
            let both = quorum_relay_members(&members, a, false);
            for (j, b) in members.iter().enumerate() {
                assert_eq!(both.contains(b), outbound[i].contains(b) || outbound[j].contains(a));
            }
        }
    }
}

#[test]
fn mesh_has_one_outbound_side_per_pair() {
    let members = members(64);
    let outbound: Vec<_> =
        members.iter().map(|me| quorum_connections(&members, me, true, true)).collect();
    for (i, a) in members.iter().enumerate() {
        let both = quorum_connections(&members, a, false, true);
        assert_eq!(both, members.iter().copied().filter(|b| b != a).collect());
        assert!(!outbound[i].contains(a));
        for (j, b) in members.iter().enumerate().skip(i + 1) {
            assert_ne!(outbound[i].contains(b), outbound[j].contains(a));
            assert_eq!(
                outbound[i].contains(b),
                deterministic_outbound(*a.as_bytes(), *b.as_bytes()) == *b.as_bytes()
            );
        }
    }
}

#[test]
fn empty_and_nonmember_topology() {
    let members = members(5);
    let outsider = ProTxHash::from([255; 32]);
    for only_outbound in [false, true] {
        for mesh in [false, true] {
            assert!(quorum_connections(&[], &outsider, only_outbound, mesh).is_empty());
        }
        assert!(quorum_relay_members(&members, &outsider, only_outbound).is_empty());
        assert!(quorum_connections(&members, &outsider, only_outbound, false).is_empty());
    }
    assert_eq!(
        quorum_connections(&members, &outsider, false, true),
        members.iter().copied().collect()
    );
    let expected = members
        .iter()
        .copied()
        .filter(|peer| {
            deterministic_outbound(*outsider.as_bytes(), *peer.as_bytes()) == *peer.as_bytes()
        })
        .collect();
    assert_eq!(quorum_connections(&members, &outsider, true, true), expected);
}

#[test]
fn outbound_matches_independent_hashes() {
    let members = members(32);
    for a in &members {
        assert_eq!(deterministic_outbound(*a.as_bytes(), *a.as_bytes()), *a.as_bytes());
        for b in &members {
            let mut sorted = [a.as_bytes(), b.as_bytes()];
            sorted.sort_unstable();
            let h_a = hash_parts(&[sorted[0], sorted[1], a.as_bytes()]);
            let h_b = hash_parts(&[sorted[0], sorted[1], b.as_bytes()]);
            let expected = if h_a < h_b {
                a
            } else {
                b
            };
            assert_eq!(deterministic_outbound(*a.as_bytes(), *b.as_bytes()), *expected.as_bytes());
        }
    }
}

#[test]
fn p7_recorded_outbound_vectors_in_place() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../contrib/node/p7-captures/README.md");
    let bytes = std::fs::read(path).expect("the original P7 capture README");
    let digest: String = sha256(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(digest, "c89ec1832600f1cefd821fd208889249c28cc742c49a3ff6fa3306c3299d8e89");
    let script = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../contrib/node/p7-captures/outbound.py"
    ))
    .expect("the original P7 capture script");
    let script_digest: String = sha256(&script).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(script_digest, "620af1f7fe0df0d5a957d13d6947c9b8a541fbb365aae22c7760d31bef6e99f4");
    let readme = std::str::from_utf8(&bytes).expect("UTF-8");
    let mut inputs = [[0; 32]; 6];
    for (input, byte) in inputs[1..5].iter_mut().zip([1, 2, 3, 42]) {
        input[0] = byte;
    }
    inputs[5][31] = 1;
    let mut checked = 0;
    for line in readme.lines() {
        let cells: Vec<_> = line.split('|').map(str::trim).collect();
        if cells.len() != 4 {
            continue;
        }
        let Some((a, b)) = cells[1].split_once(", ") else {
            continue;
        };
        let a: usize = a.parse().expect("input index");
        let b: usize = b.parse().expect("input index");
        let winner: usize = cells[2].parse().expect("winner index");
        assert_eq!(deterministic_outbound(inputs[a], inputs[b]), inputs[winner], "{a}, {b}");
        assert_eq!(deterministic_outbound(inputs[b], inputs[a]), inputs[winner], "{b}, {a}");
        let pair = [ProTxHash::from(inputs[a]), ProTxHash::from(inputs[b])];
        assert_eq!(
            quorum_connections(&pair, &pair[0], true, true).contains(&pair[1]),
            inputs[winner] == inputs[b]
        );
        checked += 1;
    }
    assert_eq!(checked, 15);
    assert!(inputs[1] > inputs[5]);
    let mut reversed = inputs;
    reversed.iter_mut().for_each(|bytes| bytes.reverse());
    assert!(reversed[1] < reversed[5]);
}

#[test]
fn watch_hash_chain_and_duplicate_draws() {
    let seed = [19; 32];
    let base = BlockHash::from([71; 32]);
    for t in [LlmqType::Llmq50_60, LlmqType::LlmqTest, LlmqType::Unknown(254)] {
        for n in [1, 3, 32] {
            let mut rnd = seed;
            let mut expected = BTreeSet::new();
            for count in 0..=64 {
                assert_eq!(watch_connections(t, &base, n, count, &seed), expected);
                rnd = hash_parts(&[&rnd, &[t.to_base()], base.as_bytes()]);
                let word = u64::from_le_bytes(rnd[..8].try_into().unwrap());
                expected.insert(word as usize % n);
            }
            assert!(expected.len() < 64);
        }
    }
    assert!(watch_connections(LlmqType::LlmqTest, &base, 0, 0, &seed).is_empty());
    let original = watch_connections(LlmqType::LlmqTest, &base, 256, 8, &seed);
    assert_ne!(original, watch_connections(LlmqType::Llmq50_60, &base, 256, 8, &seed));
    assert_ne!(original, watch_connections(LlmqType::LlmqTest, &base, 256, 8, &[20; 32]));
    assert_ne!(
        original,
        watch_connections(LlmqType::LlmqTest, &BlockHash::from([72; 32]), 256, 8, &seed)
    );
}

#[test]
fn recovery_order_matches_independent_hashes() {
    let members = members(64);
    for id in [[0; 32], [1; 32], [255; 32]] {
        let hashes: Vec<_> =
            members.iter().map(|member| hash_parts(&[member.as_bytes(), &id])).collect();
        let mut expected: Vec<_> = (0..members.len()).collect();
        expected.sort_by(|a, b| hashes[*a].iter().cmp(hashes[*b].iter()));
        assert_eq!(recovery_member_order(&members, &id), expected);
        let mut reversed: Vec<_> = (0..members.len()).collect();
        reversed.sort_by(|a, b| hashes[*a].iter().rev().cmp(hashes[*b].iter().rev()));
        assert_ne!(expected, reversed);
        let reversed_members: Vec<_> = members.iter().copied().rev().collect();
        assert_eq!(
            recovery_member_order(&reversed_members, &id)
                .into_iter()
                .map(|i| reversed_members[i])
                .collect::<Vec<_>>(),
            expected.into_iter().map(|i| members[i]).collect::<Vec<_>>()
        );
    }
    assert!(recovery_member_order(&[], &[0; 32]).is_empty());
    assert_eq!(recovery_member_order(&members[..1], &[0; 32]), vec![0]);
}

#[test]
fn recovery_attempt_bounds_and_wrap() {
    let members = members(3);
    let id = [31; 32];
    let order = recovery_member_order(&members, &id);
    for attempt in 0..12 {
        assert_eq!(
            select_member_for_recovery(&members, &id, attempt, 11),
            (attempt < 11).then(|| order[attempt as usize % 3])
        );
    }
    assert_eq!(select_member_for_recovery(&members, &id, 0, 0), None);
    assert_eq!(select_member_for_recovery(&[], &id, 0, 1), None);
    assert_eq!(select_member_for_recovery(&members, &id, u32::MAX, u32::MAX), None);
    assert_eq!(
        select_member_for_recovery(&members, &id, u32::MAX - 1, u32::MAX),
        Some(order[(u32::MAX - 1) as usize % 3])
    );
    assert_eq!(select_member_for_recovery(&members[..1], &id, 3, 4), Some(0));
}

#[test]
fn lane_c_transcript_quorum_connections() {
    let fixture = lane_c_fixture();
    let signers = fixture["signers"].as_object().expect("signers");
    let masternodes: Vec<(&str, ProTxHash)> = signers
        .iter()
        .filter_map(|(node, signer)| {
            signer["proTxHash"].as_str().map(|hash| (node.as_str(), stored_hash_str(hash)))
        })
        .collect();
    assert_eq!(masternodes.len(), 11);

    for phase_name in ["ring", "mesh"] {
        let phase = fixture["phases"][phase_name].as_object().expect("phase object");
        let spork = phase["sporks"]["SPORK_21_QUORUM_ALL_CONNECTED"].as_i64().expect("spork value");
        let all_members_connected = phase_name == "mesh";
        assert_eq!(spork == 0, all_members_connected, "{phase_name} spork");
        let quorums = phase["quorums"].as_array().expect("phase quorums");
        assert!(!quorums.is_empty(), "{phase_name} has quorums");

        for quorum in quorums {
            let type_id = quorum["type"].as_u64().expect("quorum type") as u8;
            assert_eq!(lane_c_type(type_id).to_base(), type_id);
            let type_name = lane_c_type_name(type_id);
            let quorum_hash = quorum["quorumHash"].as_str().expect("quorum hash");
            let variant = quorum["quorum_info"]["variants"]
                .as_array()
                .and_then(|variants| variants.first())
                .expect("quorum info variant");
            let quorum_index = variant["quorumIndex"].as_i64().expect("quorum index");
            let members: Vec<ProTxHash> = variant["members"]
                .as_array()
                .expect("quorum members")
                .iter()
                .map(|member| stored_hash(&member["proTxHash"]))
                .collect();
            assert!(!members.is_empty(), "{phase_name} quorum {quorum_hash} members");
            let dkgstatus = quorum["dkgstatus"].as_object().expect("dkg status");

            for (node, me) in &masternodes {
                let status = dkgstatus.get(*node).expect("status for every MN");
                let entries =
                    status["quorumConnections"].as_array().expect("quorum connection records");
                let matching: Vec<&Value> = entries
                    .iter()
                    .filter(|entry| {
                        entry["llmqType"].as_str() == Some(type_name)
                            && entry["quorumHash"].as_str() == Some(quorum_hash)
                            && entry["quorumIndex"].as_i64() == Some(quorum_index)
                    })
                    .collect();
                assert_eq!(matching.len(), 1, "{phase_name} quorum {quorum_hash} node {node}");
                let records = matching[0]["quorumConnections"]
                    .as_array()
                    .expect("recorded quorum connections");
                let recorded: BTreeSet<ProTxHash> =
                    records.iter().map(|record| stored_hash(&record["proTxHash"])).collect();
                let recorded_outbound: BTreeSet<ProTxHash> = records
                    .iter()
                    .filter(|record| record["outbound"].as_bool() == Some(true))
                    .map(|record| stored_hash(&record["proTxHash"]))
                    .collect();
                assert_eq!(
                    quorum_connections(&members, me, false, all_members_connected),
                    recorded,
                    "{phase_name} quorum {quorum_hash} node {node} all"
                );
                assert_eq!(
                    quorum_connections(&members, me, true, all_members_connected),
                    recorded_outbound,
                    "{phase_name} quorum {quorum_hash} node {node} outbound"
                );
            }
        }
    }
}
