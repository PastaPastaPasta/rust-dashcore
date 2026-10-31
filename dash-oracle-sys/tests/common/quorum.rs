//! Quorums a node can store: masternodes with known operator keys and the commitments they
//! sign, which `ProcessBlock()`'s signature checks and `CFinalCommitment::Verify()` accept with
//! those masternodes as the quorum's members (llmq/commitment.cpp at the pin).

use dash_pkc::bls::{BlsPublicKey, BlsScIetf, BlsSecretKey, BlsSigId, BlsSignature};
use dashcore::hashes::{Hash, sha256d};

/// The BLS key from ikm `[seed; 32]` (Core signs and verifies in the basic scheme once V19 is
/// active).
pub fn bls_key(seed: u8) -> BlsSecretKey<BlsScIetf> {
    BlsSecretKey::from_ikm(&[seed; 32]).unwrap()
}

pub fn hash256(bytes: &[u8]) -> [u8; 32] {
    sha256d::Hash::hash(bytes).to_byte_array()
}

/// A valid masternode as evodb stores it (`CDeterministicMN`, SER_DISK): proTxHash
/// `[seed; 32]`, internal id `id` (below 128), a collateral of its own, no operator reward, a
/// version 2 state (basic-scheme operator key) registered at height 0, unpaid and unbanned,
/// with a confirmed hash (`CalculateQuorum()` skips a masternode without one) and the
/// `confirmedHashWithProRegTxHash` `UpdateConfirmedHash()` derives from it, operator key
/// `bls_key(seed)` and service 1.1.1.`seed`:9999; nType regular.
pub fn member_dmn(id: u8, seed: u8) -> Vec<u8> {
    let pro_tx_hash = [seed; 32];
    let confirmed = [seed ^ 0xc0; 32];
    let mut state = Vec::new();
    for v in [2i32, 0, 0, 0, 0, -1, -1] {
        state.extend(v.to_le_bytes());
    }
    state.extend(0u16.to_le_bytes()); // nRevocationReason
    state.extend(confirmed);
    state.extend(hash256(&[pro_tx_hash, confirmed].concat()));
    state.extend([seed; 20]); // keyIDOwner
    state.extend(bls_key(seed).public_key().to_bytes());
    state.extend([seed; 20]); // keyIDVoting
    let mut service = [0u8; 18]; // ADDRv1 CService: IPv4-mapped address, big-endian port
    service[10..12].copy_from_slice(&[0xff, 0xff]);
    service[12..16].copy_from_slice(&[1, 1, 1, seed]);
    service[16..].copy_from_slice(&9999u16.to_be_bytes());
    state.extend(service);
    state.extend([0, 0]); // scriptPayout, scriptOperatorPayout
    state.extend([0; 20 + 4]); // platformNodeID, platformP2PPort, platformHTTPPort

    let mut out = pro_tx_hash.to_vec();
    out.push(id);
    out.extend([id + 1; 32]);
    out.extend(0u32.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(state);
    out.extend(0u16.to_le_bytes());
    out
}

/// The masternodes of `seeds`, internal ids 0, 1, ...
pub fn member_dmns(seeds: &[u8]) -> Vec<Vec<u8>> {
    seeds.iter().enumerate().map(|(id, &seed)| member_dmn(id as u8, seed)).collect()
}

/// A list as evodb stores it (`CDeterministicMNList`, SER_DISK) for the block `block_hash` at
/// `height`: every masternode of `seeds` registered.
pub fn member_list(block_hash: [u8; 32], height: i32, seeds: &[u8]) -> Vec<u8> {
    let mut out = block_hash.to_vec();
    out.extend(height.to_le_bytes());
    out.extend((seeds.len() as u32).to_le_bytes());
    out.push(seeds.len() as u8);
    for dmn in member_dmns(seeds) {
        out.extend(dmn);
    }
    out
}

/// The masternodes the tests' quorums have as their members (both test types' size, 3).
pub const MEMBERS: [u8; 3] = [0x31, 0x32, 0x33];

/// A version 3 `CFinalCommitment` (network serialization: basic scheme, not indexed) of the
/// quorum `(llmq_type, quorum_hash)` of `size`: the first `bits`
/// members valid and signing, the masternodes of `signers` the ones whose operator keys sign
/// (membersSig is their secure aggregate, as `VerifySecureAggregated()` checks it), the quorum
/// key `bls_key(quorum_seed)` signing quorumSig, both over `BuildCommitmentHash()`; vvec hash
/// `[0x11; 32]`.
pub fn signed_commitment(
    llmq_type: u8,
    quorum_hash: [u8; 32],
    size: usize,
    bits: usize,
    signers: &[u8],
    quorum_seed: u8,
) -> Vec<u8> {
    assert!(size < 0xfd, "a one-byte CompactSize");
    let mut bitset = vec![size as u8];
    bitset.resize(1 + size.div_ceil(8), 0);
    for i in 0..bits {
        bitset[1 + i / 8] |= 1 << (i % 8);
    }
    let quorum_key = bls_key(quorum_seed);
    let public_key = quorum_key.public_key().to_bytes();
    let vvec_hash = [0x11; 32];
    // BuildCommitmentHash(): llmqType, quorumHash, DYNBITSET(validMembers), the key, vvec hash.
    let hash =
        hash256(&[&[llmq_type][..], &quorum_hash, &bitset, &public_key, &vvec_hash].concat());
    let keys: Vec<_> = signers.iter().map(|&s| bls_key(s)).collect();
    let sigs: Vec<BlsSignature<BlsScIetf>> =
        keys.iter().map(|k| k.sign_with(&hash, BlsSigId::Basic)).collect();
    let pks: Vec<BlsPublicKey<BlsScIetf>> = keys.iter().map(|k| k.public_key()).collect();
    let members_sig = BlsSignature::secure_aggregate(
        &sigs.iter().collect::<Vec<_>>(),
        &pks.iter().collect::<Vec<_>>(),
    )
    .unwrap();

    let mut qc = 3u16.to_le_bytes().to_vec();
    qc.push(llmq_type);
    qc.extend(quorum_hash);
    qc.extend(&bitset); // signers
    qc.extend(&bitset); // validMembers
    qc.extend(public_key);
    qc.extend(vvec_hash);
    qc.extend(quorum_key.sign_with(&hash, BlsSigId::Basic).to_bytes());
    qc.extend(members_sig.to_bytes());
    qc
}

/// The commitment of a quorum of `size` whose members are the masternodes of `members`, all
/// valid and signing.
pub fn quorum_commitment(
    llmq_type: u8,
    quorum_hash: [u8; 32],
    size: usize,
    members: &[u8],
    quorum_seed: u8,
) -> Vec<u8> {
    signed_commitment(llmq_type, quorum_hash, size, members.len(), members, quorum_seed)
}
