//! LQ3a's wire forms: the `qsigrec` read, the three lock messages' inventory hashes, and how Core
//! reads their signature bytes, on Core's InstantSend vector and constructed bytes (quirk 38).

use dash_consensus::hash::sha256d;
use dash_pkc::bls::{BlsEagerSig, BlsSchemeKind, BlsSigBytes};
use dash_primitives::Commitment;
use dash_types::codec::BaseCodec;

use super::tests::{hex, legacy_sign_bytes, qhash, secret, sig, sign_bytes};
use super::*;

/// Core's `deserialize_instantlock_from_realdata2` lock (`src/test/evo_islock_tests.cpp`
/// v24:177-229), as the `getislocks` hex Core's test reads.
fn realdata2() -> Vec<u8> {
    hex(concat!(
        "0101497915895c30eebfad0c5fcfb9e0e72308c7e92cd3749be2fd49c8320c4c58b6010000005b9d05c613c2a5",
        "f8ca60800f65f47f46bebbc934571b9ceae813a6ff8e96337bd674ea572a713d6b07deef085b9ce97e1e354055",
        "b91b0bbd0b00000000000000997d0b36738a9eef46ceeb4405998ff7235317708f277402799ffe05258015cae9",
        "b6bae43683f992b2f50f70f8f0cb9c0f26af340b00903e93995c1345d1b2c5b697ebecdbe5811dd112e1188910",
        "1dcb4553b2bc206ab304026b96c07dec4f24"
    ))
}

/// A basic-scheme signature by key 1, and the same signature in the legacy encoding; each is
/// canonical only in its own scheme.
fn basic_and_legacy() -> ([u8; 96], [u8; 96]) {
    let basic = sign_bytes(1, &[0x5a; 32]);
    let legacy =
        BlsEagerSig::new(sig(1, &[0x5a; 32]), BlsSchemeKind::Basic).to_bytes(BlsSchemeKind::Legacy);
    assert_eq!(read_lazy_sig(&basic, BlsCtx::LEGACY), SigRead::Invalid);
    assert_eq!(read_lazy_sig(&legacy, BlsCtx::BASIC), SigRead::Invalid);
    (basic, legacy)
}

fn decode_recsig(wire: &[u8]) -> RecoveredSig {
    RecoveredSig::decode(&mut &wire[..]).expect("193 bytes")
}

/// An `isdlock` and a `qsigrec` carrying `sig` hash over their wire bytes.
fn assert_lazy_hashes_are_wire(sig: [u8; 96]) {
    let is = islock(sig);
    assert_eq!(isdlock_hash(&is), sha256d(&islock_wire(&is)));
    let wire = recsig_wire(sig);
    assert_eq!(decode_recsig(&wire).hash(), sha256d(&wire));
}

/// Bytes canonical in neither scheme.
const NEITHER: [u8; 96] = [0xff; 96];

fn clsig_wire(clsig: &ClSig) -> Vec<u8> {
    [&clsig.height.to_le_bytes()[..], clsig.block_hash.as_bytes(), clsig.sig.as_bytes()].concat()
}

fn clsig(sig: [u8; 96]) -> ClSig {
    ClSig {
        height: 1234,
        block_hash: BlockHash::from([0xab; 32]),
        sig: BlsSigBytes::from(sig),
    }
}

fn islock(sig: [u8; 96]) -> IsdLock {
    IsdLock {
        version: 1,
        inputs: vec![
            OutPoint {
                hash: TxHash::from([3; 32]),
                index: 7,
            },
            OutPoint {
                hash: TxHash::from([4; 32]),
                index: u32::MAX,
            },
        ],
        txid: TxHash::from([0x11; 32]),
        cycle_hash: BlockHash::from([0xc0; 32]),
        sig: BlsSigBytes::from(sig),
    }
}

fn islock_wire(islock: &IsdLock) -> Vec<u8> {
    let mut wire = vec![islock.version, u8::try_from(islock.inputs.len()).expect("few")];
    for input in &islock.inputs {
        wire.extend_from_slice(input.hash.as_bytes());
        wire.extend_from_slice(&input.index.to_le_bytes());
    }
    wire.extend_from_slice(islock.txid.as_bytes());
    wire.extend_from_slice(islock.cycle_hash.as_bytes());
    wire.extend_from_slice(islock.sig.as_bytes());
    wire
}

fn recsig_wire(sig: [u8; 96]) -> Vec<u8> {
    [&[100u8][..], &[0x22; 32], &[0x33; 32], &[0x44; 32], &sig].concat()
}

/// A version-3 (basic) `qfcommit` with no members, whose `quorumSig` is `quorum_sig`; and the
/// offset of that field.
fn qfcommit_wire(quorum_sig: [u8; 96]) -> (Vec<u8>, usize) {
    let mut wire = 3u16.to_le_bytes().to_vec();
    wire.push(100);
    wire.extend_from_slice(&[0x22; 32]);
    // `signers` and `validMembers`: empty DYNBITSETs.
    wire.extend_from_slice(&[0, 0]);
    wire.extend_from_slice(&secret(1).public_key().to_bytes());
    wire.extend_from_slice(&[0x11; 32]);
    let at = wire.len();
    wire.extend_from_slice(&quorum_sig);
    wire.extend_from_slice(&sign_bytes(2, &[0x5a; 32]));
    (wire, at)
}

/// Quirk 38: Core hashes a `clsig` and a `qfcommit` over their re-serialization, an `isdlock` and
/// a `qsigrec` over their wire bytes (`net_processing.cpp` v24:5798, :6653;
/// `llmq/blockprocessor.cpp` v24:115; `llmq/signing.h` v24:90-100;
/// `instantsend/net_instantsend.cpp` v24:258). The two agree except for an eager field whose
/// bytes are canonical only in the other scheme: Core reads it as the invalid object and writes
/// it back as zeros (quirk 37).
#[test]
fn quirk_38_reserialization_vs_wire_hashes() {
    let (basic, legacy) = basic_and_legacy();

    // A signature valid in the scheme, or zeros: every hash is over the wire bytes.
    for bytes in [basic, [0; 96]] {
        let cl = clsig(bytes);
        assert_eq!(
            clsig_hash(&cl, ClsigHashSchemes::both(BlsCtx::BASIC)),
            Some(sha256d(&clsig_wire(&cl)))
        );
        assert_lazy_hashes_are_wire(bytes);
    }

    // Canonical only in the other scheme. The ChainLock hash is over zeros in the signature's
    // place, under either flag.
    for (bytes, bls) in [(legacy, BlsCtx::BASIC), (basic, BlsCtx::LEGACY)] {
        let cl = clsig(bytes);
        assert_eq!(read_eager_sig(&bytes, bls), SigRead::Invalid);
        assert_ne!(clsig_hash(&cl, ClsigHashSchemes::both(bls)), Some(sha256d(&clsig_wire(&cl))));
        assert_eq!(
            clsig_hash(&cl, ClsigHashSchemes::both(bls)),
            Some(sha256d(&clsig_wire(&clsig([0; 96]))))
        );
    }
    // The InstantSend lock and the recovered signature keep the wire bytes: the lazy field reads
    // the invalid object only when first used, after the hash.
    assert_lazy_hashes_are_wire(legacy);
    assert_ne!(isdlock_hash(&islock(legacy)), isdlock_hash(&islock([0; 96])));
    assert_ne!(decode_recsig(&recsig_wire(legacy)).hash(), sha256d(&recsig_wire([0; 96])));

    // The final commitment (LQ2's message), through base-sdk's reader: its eager `quorumSig`
    // reads as the invalid object and writes back as zeros, so Core's hash is over the zeros.
    let (wire, at) = qfcommit_wire(legacy);
    let qc = Commitment::decode(&mut &wire[..]).expect("a commitment");
    assert!(!qc.quorum_sig.is_valid());
    assert_eq!(qc.quorum_sig.to_bytes(BlsSchemeKind::Basic), [0; 96]);
    assert!(qc.members_sig.is_valid());
    let mut rewritten = wire.clone();
    rewritten[at..at + 96].copy_from_slice(&qc.quorum_sig.to_bytes(BlsSchemeKind::Basic));
    assert_ne!(sha256d(&rewritten), sha256d(&wire));
    // Valid bytes write back as read.
    let (wire, at) = qfcommit_wire(basic);
    let qc = Commitment::decode(&mut &wire[..]).expect("a commitment");
    assert_eq!(qc.quorum_sig.to_bytes(BlsSchemeKind::Basic)[..], wire[at..at + 96]);

    // Canonical in neither scheme: Core's eager readers throw, so there is no ChainLock and no
    // commitment to hash; the lazy fields read and hash the bytes.
    assert_eq!(clsig_hash(&clsig(NEITHER), ClsigHashSchemes::both(BlsCtx::BASIC)), None);
    assert!(Commitment::decode(&mut &qfcommit_wire(NEITHER).0[..]).is_err());
    assert_lazy_hashes_are_wire(NEITHER);
}

/// Sol r1 M2: the V19 block connects between Core's read of a `clsig` and its `SerializeHash`
/// (`net_processing.cpp` v24:5797-5807, no lock held across them). The eager signature holds the
/// point it read under the legacy flag and writes it under the basic one: a real legacy signature
/// is hashed in its basic encoding, which neither single-scheme hash gives. Disconnecting the
/// block is the reverse.
#[test]
fn a_clsig_hashed_across_the_v19_switch_writes_the_other_encoding() {
    let legacy = legacy_sign_bytes(1, &[0x5a; 32]);
    let point = BlsSigValue::from_canonical(&legacy, BlsSchemeKind::Legacy).expect("legacy");
    let basic = point.to_bytes(BlsSchemeKind::Basic);
    assert_ne!(basic, legacy);
    let cl = clsig(legacy);
    let mixed = sha256d(&clsig_wire(&clsig(basic)));
    assert_eq!(
        clsig_hash(
            &cl,
            ClsigHashSchemes {
                read: BlsCtx::LEGACY,
                write: BlsCtx::BASIC
            }
        ),
        Some(mixed)
    );
    // Read and hashed before the switch: the wire bytes. Read after it: the invalid object, zeros.
    let before = clsig_hash(&cl, ClsigHashSchemes::both(BlsCtx::LEGACY));
    assert_eq!(before, Some(sha256d(&clsig_wire(&cl))));
    let after = clsig_hash(&cl, ClsigHashSchemes::both(BlsCtx::BASIC));
    assert_eq!(after, Some(sha256d(&clsig_wire(&clsig([0; 96])))));
    assert_ne!(before, Some(mixed));
    assert_ne!(after, Some(mixed));
    // The reverse: the basic encoding read before the block disconnects, hashed after it.
    let reverse = clsig_hash(
        &clsig(basic),
        ClsigHashSchemes {
            read: BlsCtx::BASIC,
            write: BlsCtx::LEGACY,
        },
    );
    assert_eq!(reverse, Some(sha256d(&clsig_wire(&cl))));
}

/// Core's realdata2 lock: its inventory hash is over its wire bytes whatever the flag, and its
/// basic-scheme signature reads as the invalid object under the legacy flag ("Using legacy will
/// result in the signature being all zeros", Core's test v24:195).
#[test]
fn cores_islock_hash_and_lazy_read() {
    let bytes = realdata2();
    let islock = IsdLock::decode(&mut &bytes[..]).expect("Core's lock");
    assert_eq!(isdlock_hash(&islock), sha256d(&bytes));
    let sig: [u8; 96] = *islock.sig.as_bytes();
    assert!(matches!(read_lazy_sig(&sig, BlsCtx::BASIC), SigRead::Valid(_)));
    assert_eq!(read_lazy_sig(&sig, BlsCtx::LEGACY), SigRead::Invalid);
    // The eager reader agrees on canonical bytes.
    assert!(matches!(read_eager_sig(&sig, BlsCtx::BASIC), SigRead::Valid(_)));
    assert_eq!(read_eager_sig(&sig, BlsCtx::LEGACY), SigRead::Invalid);
}

#[test]
fn signature_reads() {
    let (basic, legacy) = basic_and_legacy();
    assert_eq!(read_eager_sig(&basic, BlsCtx::BASIC), SigRead::Valid(sig(1, &[0x5a; 32])));
    assert_eq!(read_eager_sig(&legacy, BlsCtx::LEGACY), SigRead::Valid(sig(1, &[0x5a; 32])));
    assert_eq!(read_lazy_sig(&legacy, BlsCtx::LEGACY), SigRead::Valid(sig(1, &[0x5a; 32])));
    for bls in [BlsCtx::BASIC, BlsCtx::LEGACY] {
        assert_eq!(read_eager_sig(&[0; 96], bls), SigRead::Invalid);
        assert_eq!(read_lazy_sig(&[0; 96], bls), SigRead::Invalid);
        // Only the eager reader throws.
        assert_eq!(read_eager_sig(&NEITHER, bls), SigRead::Unreadable);
        assert_eq!(read_lazy_sig(&NEITHER, bls), SigRead::Invalid);
    }
}

#[test]
fn a_recovered_sig_reads_its_193_bytes_and_leaves_the_rest() {
    let (basic, _) = basic_and_legacy();
    let wire = recsig_wire(basic);
    assert_eq!(wire.len(), RecoveredSig::LEN);
    let rec = decode_recsig(&wire);
    assert_eq!(
        rec,
        RecoveredSig {
            llmq_type: LlmqType::from_base(100),
            quorum_hash: qhash(0x22),
            id: [0x33; 32],
            msg_hash: [0x44; 32],
            sig: basic,
        }
    );
    // Any type byte reads.
    let mut odd = wire.clone();
    odd[0] = 0xfe;
    assert_eq!(
        RecoveredSig::decode(&mut &odd[..]).map(|r| r.llmq_type),
        Some(LlmqType::from_base(0xfe))
    );
    // Trailing bytes are left to the caller; a short read is none.
    let longer = [&wire[..], &[9, 9]].concat();
    let mut r = &longer[..];
    assert_eq!(RecoveredSig::decode(&mut r), Some(rec));
    assert_eq!(r, &[9, 9]);
    let mut short = &wire[..RecoveredSig::LEN - 1];
    assert_eq!(RecoveredSig::decode(&mut short), None);
    assert_eq!(short.len(), RecoveredSig::LEN - 1);
}

#[test]
fn the_isdlock_hash_counts_inputs_with_a_compact_size() {
    // 253 inputs: the count takes three bytes.
    let mut is = islock([0; 96]);
    is.inputs = (0..253u32)
        .map(|i| OutPoint {
            hash: TxHash::from([5; 32]),
            index: i,
        })
        .collect();
    let mut wire = vec![1, 0xfd, 253, 0];
    for input in &is.inputs {
        wire.extend_from_slice(input.hash.as_bytes());
        wire.extend_from_slice(&input.index.to_le_bytes());
    }
    wire.extend_from_slice(is.txid.as_bytes());
    wire.extend_from_slice(is.cycle_hash.as_bytes());
    wire.extend_from_slice(&[0; 96]);
    assert_eq!(isdlock_hash(&is), sha256d(&wire));
}
