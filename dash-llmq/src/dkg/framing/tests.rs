//! `MaxDKGMessageSize` and `CheckDKGMessageWireStructure` (`l/net_dkg.cpp:44-185`, v24.0.0)
//! over hand-framed payloads of `llmq_test` (size 3, minSize 2, threshold 2).

use dash_params::{ChainParamsBuilder, LlmqParams, LlmqType, Network, Release};

use super::{check_wire_structure, max_dkg_message_size};
use crate::dkg::session::DkgMsgKind;

fn params() -> LlmqParams {
    let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    *setup.consensus.llmq(LlmqType::LlmqTest).expect("regtest configures llmq_test")
}

fn contribution(vvec: u8, blobs: &[u8]) -> Vec<u8> {
    let mut out = vec![0; 65];
    out.push(vvec);
    out.extend(vec![0; usize::from(vvec) * 48 + 48 + 32]);
    out.push(blobs.len() as u8);
    for len in blobs {
        out.push(*len);
        out.extend(vec![7; usize::from(*len)]);
    }
    out.extend([0; 96]);
    out
}

fn complaint(bad: (u8, u8), complain_for: (u8, u8)) -> Vec<u8> {
    let mut out = vec![0; 65];
    out.extend([bad.0, bad.1, complain_for.0, complain_for.1]);
    out.extend([0; 96]);
    out
}

fn justification(count: u8) -> Vec<u8> {
    let mut out = vec![0; 65];
    out.push(count);
    out.extend(vec![0; usize::from(count) * 36 + 96]);
    out
}

fn pcommit(bits: u8, byte: u8) -> Vec<u8> {
    let mut out = vec![0; 65];
    out.extend([bits, byte]);
    out.extend(vec![0; 48 + 32 + 192]);
    out
}

#[test]
fn the_size_caps_are_cores() {
    let p = params();
    assert_eq!((p.size, p.threshold), (3, 2));
    assert_eq!(max_dkg_message_size(DkgMsgKind::Contribution, &p), 65 + 101 + 85 + 399 + 96 + 1024);
    assert_eq!(max_dkg_message_size(DkgMsgKind::Justification, &p), 65 + 113 + 96 + 1024);
    assert_eq!(max_dkg_message_size(DkgMsgKind::Complaint, &p), 65 + 12 + 96 + 1024);
    assert_eq!(max_dkg_message_size(DkgMsgKind::PrematureCommitment, &p), 65 + 6 + 80 + 192 + 1024);
    let huge = LlmqParams {
        size: 10_000,
        ..p
    };
    assert_eq!(max_dkg_message_size(DkgMsgKind::Contribution, &huge), 1 << 20);
}

#[test]
fn a_contribution_needs_threshold_keys_and_min_to_size_blobs() {
    let (p, k) = (params(), DkgMsgKind::Contribution);
    assert!(check_wire_structure(k, &contribution(2, &[3, 0]), &p));
    assert!(check_wire_structure(k, &contribution(2, &[1, 2, 3]), &p));
    assert!(!check_wire_structure(k, &contribution(1, &[3, 3]), &p), "vvec below threshold");
    assert!(!check_wire_structure(k, &contribution(3, &[3, 3]), &p), "vvec above threshold");
    assert!(!check_wire_structure(k, &contribution(2, &[3]), &p), "blobs below minSize");
    assert!(!check_wire_structure(k, &contribution(2, &[3; 4]), &p), "blobs above size");
    let ok = contribution(2, &[3, 3]);
    assert!(!check_wire_structure(k, &ok[..ok.len() - 1], &p), "truncated");
    assert!(!check_wire_structure(k, &[ok.as_slice(), &[0]].concat(), &p), "trailing byte");
    let mut long_blob = ok.clone();
    long_blob[65 + 1 + 96 + 80 + 1] = 200;
    assert!(!check_wire_structure(k, &long_blob, &p), "a blob past the end");
    let mut non_canonical = contribution(2, &[3, 3]);
    non_canonical.splice(65..66, [253, 2, 0]);
    assert!(!check_wire_structure(k, &non_canonical, &p), "a non-canonical compact size");
}

#[test]
fn a_complaint_needs_two_equal_bounded_bitsets_with_clear_padding() {
    let (p, k) = (params(), DkgMsgKind::Complaint);
    assert!(check_wire_structure(k, &complaint((3, 0b101), (3, 0b010)), &p));
    assert!(!check_wire_structure(k, &complaint((3, 0), (2, 0)), &p), "sizes differ");
    assert!(!check_wire_structure(k, &complaint((4, 0), (4, 0)), &p), "above size");
    assert!(!check_wire_structure(k, &complaint((3, 0b1000), (3, 0)), &p), "a padding bit");
    assert!(!check_wire_structure(k, &complaint((3, 0), (3, 0x80)), &p), "the top padding bit");
    let mut empty = vec![0; 65];
    empty.extend([0, 0]);
    empty.extend([0; 96]);
    assert!(check_wire_structure(k, &empty, &p), "two empty bitsets");
}

#[test]
fn a_justification_holds_at_most_size_shares() {
    let (p, k) = (params(), DkgMsgKind::Justification);
    assert!(check_wire_structure(k, &justification(0), &p));
    assert!(check_wire_structure(k, &justification(3), &p));
    assert!(!check_wire_structure(k, &justification(4), &p));
    let ok = justification(1);
    assert!(!check_wire_structure(k, &ok[..ok.len() - 1], &p));
}

#[test]
fn a_premature_commitment_has_a_bounded_bitset_and_fixed_fields() {
    let (p, k) = (params(), DkgMsgKind::PrematureCommitment);
    assert!(check_wire_structure(k, &pcommit(3, 0b111), &p));
    assert!(!check_wire_structure(k, &pcommit(4, 0), &p), "above size");
    assert!(!check_wire_structure(k, &pcommit(2, 0b100), &p), "a padding bit");
    let ok = pcommit(3, 0);
    assert!(!check_wire_structure(k, &[ok.as_slice(), &[0]].concat(), &p));
    assert!(!check_wire_structure(k, &ok[..64], &p), "no prefix");
}
