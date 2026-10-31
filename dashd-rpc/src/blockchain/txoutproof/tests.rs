//! `gettxoutproof`'s argument checks and errors, and `verifytxoutproof`'s decoding,
//! `ExtractMatches` and chain check, over a fake chainman.

use dash_consensus::hash::sha256d;

use super::super::tests::{HASH_7, call_method, facts, method_err, reply, rp1b_1c_server};
use super::TxOutProofError;
use crate::UniValue;
use crate::protocol::{
    RPC_DATABASE_ERROR, RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER,
    RPC_MISC_ERROR, RPC_TYPE_ERROR,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A txid whose internal bytes are `b` repeated; `GetHex` reverses them, so it prints the same.
fn txid(b: u8) -> String {
    format!("{b:02x}").repeat(32)
}

/// `gettxoutproof`: the txids, in `std::set<uint256>` order (by internal bytes), then the
/// chainman, then the block hash; the proof's hex.
// Core: rpc/txoutproof.cpp:56-123
#[test]
fn gettxoutproof_checks_txids_then_the_chainman_then_the_block() {
    let (server, live) = rp1b_1c_server(true);
    let err = |params: &str| method_err(&server, "gettxoutproof", params);
    assert_eq!(err("[[]]"), reply(RPC_INVALID_PARAMETER, "Parameter 'txids' cannot be empty"));
    // `RPCHelpMan`'s argument check.
    let (code, message) = err(r#"["ab"]"#);
    assert_eq!(code, RPC_TYPE_ERROR);
    assert!(message.contains("Wrong type passed"), "{message}");
    assert_eq!(
        err(r#"[[5]]"#),
        reply(RPC_TYPE_ERROR, "JSON value of type number is not of expected type string")
    );
    assert_eq!(
        err(r#"[["00"]]"#),
        reply(RPC_INVALID_PARAMETER, "txid must be of length 64 (not 2, for '00')")
    );
    // The duplicate is printed as given, here in upper case.
    let (a, upper_a) = (txid(0xab), txid(0xab).to_uppercase());
    assert_eq!(
        err(&format!(r#"[["{a}", "{}", "{upper_a}"]]"#, txid(1))),
        reply(RPC_INVALID_PARAMETER, &format!("Invalid parameter, duplicated txid: {upper_a}"))
    );
    assert_eq!(
        err(&format!(r#"[["{a}"], "00"]"#)),
        reply(RPC_INVALID_PARAMETER, "blockhash must be of length 64 (not 2, for '00')")
    );
    assert!(live.lock().unwrap().calls.is_empty());
    // The txids are checked before the chainman is ensured, and the block hash after it.
    let (unloaded, _) = rp1b_1c_server(false);
    let unloaded_err = |params: &str| method_err(&unloaded, "gettxoutproof", params);
    assert_eq!(
        unloaded_err("[[]]"),
        reply(RPC_INVALID_PARAMETER, "Parameter 'txids' cannot be empty")
    );
    assert_eq!(
        unloaded_err(&format!(r#"[["{a}", "{a}"]]"#)),
        reply(RPC_INVALID_PARAMETER, &format!("Invalid parameter, duplicated txid: {a}"))
    );
    assert_eq!(
        unloaded_err(&format!(r#"[["{a}"], "00"]"#)),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );

    // Sorted by internal bytes: 0x01.. < 0x02ff.. < 0xab..; a txid's first internal byte is its
    // hex's last.
    let mut mixed = [0x02u8; 32];
    mixed[31] = 0xff;
    mixed.reverse();
    let mixed = hex(&mixed);
    live.lock().unwrap().proof = Some(Ok(vec![0xde, 0xad]));
    let arg = format!(r#"[["{a}", "{mixed}", "{}"]]"#, txid(1));
    assert_eq!(call_method(&server, "gettxoutproof", &arg).unwrap(), UniValue::from("dead"));
    let arg = format!(r#"[["{a}"], "{HASH_7}"]"#);
    assert_eq!(call_method(&server, "gettxoutproof", &arg).unwrap(), UniValue::from("dead"));
    assert_eq!(
        live.lock().unwrap().calls,
        [format!("proof none {},{mixed},{a}", txid(1)), format!("proof {HASH_7} {a}"),]
    );

    for (e, code, message) in [
        (TxOutProofError::BlockNotFound, RPC_INVALID_ADDRESS_OR_KEY, "Block not found"),
        (
            TxOutProofError::NotYetInBlock,
            RPC_INVALID_ADDRESS_OR_KEY,
            "Transaction not yet in block",
        ),
        (TxOutProofError::Unreadable, RPC_INTERNAL_ERROR, "Can't read block from disk"),
        (
            TxOutProofError::NotAllFound,
            RPC_INVALID_ADDRESS_OR_KEY,
            "Not all transactions found in specified or retrieved block",
        ),
        (TxOutProofError::Aborted, RPC_DATABASE_ERROR, "Error: A fatal internal error occurred"),
    ] {
        live.lock().unwrap().proof = Some(Err(e));
        assert_eq!(err(&format!(r#"[["{a}"]]"#)), reply(code, message));
    }
}

/// A `CMerkleBlock` over `facts(1)`'s header with `root` as its merkle root.
fn proof(
    root: [u8; 32],
    n: u32,
    hashes: &[[u8; 32]],
    flags: &[u8],
) -> (String, super::super::HeaderFacts) {
    let mut f = facts(1);
    f.header.merkle_root = dash_primitives::MerkleRoot::from(root);
    let header = dash_consensus::CoreSerialize::core_serialize(&f.header);
    f.hash = dash_consensus::Header::new(f.header).hash();
    let mut bytes = header;
    bytes.extend_from_slice(&n.to_le_bytes());
    bytes.push(hashes.len() as u8);
    for h in hashes {
        bytes.extend_from_slice(h);
    }
    bytes.push(flags.len() as u8);
    bytes.extend_from_slice(flags);
    (hex(&bytes), f)
}

fn pair(a: [u8; 32], b: [u8; 32]) -> [u8; 32] {
    let mut both = [0; 64];
    both[..32].copy_from_slice(&a);
    both[32..].copy_from_slice(&b);
    sha256d(&both)
}

/// The matched txids of a proof of the block's root and transaction count; `[]` for one that
/// proves another root or count, or breaks one of `ExtractMatches`' checks.
// Core: rpc/txoutproof.cpp:146-174; merkleblock.cpp:112-148, :166-197
#[test]
fn verifytxoutproof_extracts_the_matches_of_a_proof_in_the_active_chain() {
    let (server, live) = rp1b_1c_server(true);
    let verify = |proof: &str| call_method(&server, "verifytxoutproof", &format!(r#"["{proof}"]"#));
    let (a, b) = ([0xaa; 32], [0xbb; 32]);
    let root = pair(a, b);
    // Bits: the root (a parent of a match), `a` (matched), `b` (not).
    let (good, mut f) = proof(root, 2, &[a, b], &[0b011]);
    f.n_tx = 2;
    live.lock().unwrap().facts = Some(f.clone());
    assert_eq!(verify(&good).unwrap().write(), format!(r#"["{}"]"#, txid(0xaa)));
    assert_eq!(live.lock().unwrap().calls, [format!("header {}", f.hash)]);
    // Bytes after the proof are ignored.
    assert_eq!(verify(&format!("{good}00ff")).unwrap().write(), format!(r#"["{}"]"#, txid(0xaa)));
    // Both matched.
    let (both, _) = proof(root, 2, &[a, b], &[0b111]);
    assert_eq!(verify(&both).unwrap().write(), format!(r#"["{}","{}"]"#, txid(0xaa), txid(0xbb)));
    // A block with another transaction count proves nothing.
    live.lock().unwrap().facts.as_mut().unwrap().n_tx = 3;
    assert_eq!(verify(&good).unwrap().write(), "[]");
    live.lock().unwrap().facts = Some(f.clone());

    let empty = |p: &str| assert_eq!(verify(p).unwrap().write(), "[]", "{p}");
    // Another root, and each of `ExtractMatches`' checks; none reaches the chainman.
    live.lock().unwrap().calls.clear();
    empty(&proof(pair(b, a), 2, &[a, b], &[0b011]).0);
    empty(&proof(root, 0, &[a, b], &[0b011]).0);
    empty(&proof(root, 1, &[a, b], &[0b011]).0);
    // Fewer bits than hashes (`vBits.size() < vHash.size()`: nine hashes, eight bits), then a
    // traversal that runs out of hashes.
    empty(&proof(root, 9, &[a, b, a, b, a, b, a, b, a], &[0b011]).0);
    empty(&proof(root, 2, &[a], &[0b011]).0);
    // A byte of bits left unused.
    empty(&proof(root, 2, &[a, b], &[0b011, 0]).0);
    // A hash left unused: one bit (no match below the root) and the root's hash, which is the
    // header's root, then a second hash the traversal never reads.
    empty(&proof(a, 4, &[a, b], &[0]).0);
    // Equal branches: a tree over two equal txids.
    empty(&proof(pair(a, a), 2, &[a, a], &[0b011]).0);
    assert!(live.lock().unwrap().calls.is_empty());

    // A proof of a block not in the active chain.
    let not_in_chain = reply(RPC_INVALID_ADDRESS_OR_KEY, "Block not found in chain");
    let err = |p: &str| method_err(&server, "verifytxoutproof", &format!(r#"["{p}"]"#));
    for off_chain in [true, false] {
        let mut off = f.clone();
        if off_chain {
            off.confirmations = -1;
        } else {
            off.n_tx = 0;
        }
        live.lock().unwrap().facts = Some(off);
        assert_eq!(err(&good), not_in_chain);
    }
    live.lock().unwrap().facts = None;
    assert_eq!(err(&good), not_in_chain);
    let (unloaded, _) = rp1b_1c_server(false);
    assert_eq!(
        method_err(&unloaded, "verifytxoutproof", &format!(r#"["{good}"]"#)),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
    // A proof of another root answers before the chainman is ensured.
    let other = proof(pair(b, a), 2, &[a, b], &[0b011]).0;
    assert_eq!(
        call_method(&unloaded, "verifytxoutproof", &format!(r#"["{other}"]"#)).unwrap().write(),
        "[]"
    );
}

/// `ParseHexV`'s errors, then the stream's.
// Core: rpc/util.cpp:100-108; streams.h DataStream::read
#[test]
fn verifytxoutproof_rejects_bad_hex_and_short_proofs() {
    let (server, _) = rp1b_1c_server(false);
    let err = |params: &str| method_err(&server, "verifytxoutproof", params);
    let not_hex = |s: &str| {
        reply(RPC_INVALID_PARAMETER, &format!("proof must be hexadecimal string (not '{s}')"))
    };
    assert_eq!(err(r#"[""]"#), not_hex(""));
    assert_eq!(err(r#"["abc"]"#), not_hex("abc"));
    assert_eq!(err(r#"["zz"]"#), not_hex("zz"));
    assert_eq!(err(r#"[" a"]"#), not_hex(" a"));
    assert_eq!(err("[5]").0, RPC_TYPE_ERROR);
    let eod = reply(RPC_MISC_ERROR, "DataStream::read(): end of data: iostream error");
    let (full, _) = proof([0xaa; 32], 1, &[[0xaa; 32]], &[1]);
    for cut in [2, 160, 166, 168, 170, full.len() - 2] {
        assert_eq!(err(&format!(r#"["{}"]"#, &full[..cut])), eod, "{cut}");
    }
    // A hash count above `MAX_SIZE`, and a non-canonical one.
    let header = &full[..168];
    assert_eq!(
        err(&format!(r#"["{header}fe00000003"]"#)),
        reply(RPC_MISC_ERROR, "ReadCompactSize(): size too large: iostream error")
    );
    assert_eq!(
        err(&format!(r#"["{header}fd0100"]"#)),
        reply(RPC_MISC_ERROR, "non-canonical ReadCompactSize(): iostream error")
    );
    // The same for the flag-byte count, after one hash.
    let hashed = &full[..full.len() - 4];
    assert_eq!(
        err(&format!(r#"["{hashed}fe00000003"]"#)),
        reply(RPC_MISC_ERROR, "ReadCompactSize(): size too large: iostream error")
    );
    assert_eq!(
        err(&format!(r#"["{hashed}fd0100"]"#)),
        reply(RPC_MISC_ERROR, "non-canonical ReadCompactSize(): iostream error")
    );
}

/// `ExtractMatches`' bound on `nTransactions`, `MaxBlockSize() / 60` = 33333, on a tree that
/// otherwise extracts: one hash, the header's root, with no match below it. Above the bound the
/// root is null and the reply `[]`; at it the tree proves the root and the block is looked up.
/// No transaction count is the null root too. (Adapted from the r1 Opus probe.)
// Core: merkleblock.cpp:168-172; consensus/consensus.h:11-15
#[test]
fn verifytxoutproof_bounds_the_transaction_count_at_max_block_size_over_60() {
    let (server, live) = rp1b_1c_server(true);
    live.lock().unwrap().facts = None;
    let h = [0xcc; 32];
    let call = |n: u32| {
        let arg = format!(r#"["{}"]"#, proof(h, n, &[h], &[0]).0);
        call_method(&server, "verifytxoutproof", &arg).map(|v| v.write())
    };
    assert_eq!(call(0).unwrap(), "[]", "n = 0");
    assert_eq!(call(33_334).unwrap(), "[]", "n = 33334");
    assert!(live.lock().unwrap().calls.is_empty(), "no lookup for a null root");
    let e = call(33_333).unwrap_err();
    assert_eq!(
        (e.code(), e.to_object().write()),
        reply(RPC_INVALID_ADDRESS_OR_KEY, "Block not found in chain"),
        "n = 33333"
    );
}

/// A failed extraction is the null root, which a header whose merkle root is null matches, as
/// Core compares them: the proof goes on to the block lookup.
// Core: rpc/txoutproof.cpp:157-163
#[test]
fn verifytxoutproof_compares_a_failed_extraction_with_a_null_root() {
    let (server, live) = rp1b_1c_server(true);
    let (broken, f) = proof([0; 32], 0, &[], &[]);
    assert_eq!(
        method_err(&server, "verifytxoutproof", &format!(r#"["{broken}"]"#)),
        reply(RPC_INVALID_ADDRESS_OR_KEY, "Block not found in chain")
    );
    assert_eq!(live.lock().unwrap().calls, [format!("header {}", f.hash)]);
}

/// The flag bits are held packed, as Core's `std::vector<bool>`: a proof's flag bytes are
/// stored as they came, eight bits a byte, whatever their number.
// Core: merkleblock.h:61-71 (vBits), merkleblock.cpp:21-28 BytesToBits
#[test]
fn a_proofs_flag_bits_are_stored_packed_as_cores() {
    let flags: Vec<u8> = (0..=255).cycle().take(1 << 20).collect();
    let (hex_proof, _) = proof([0; 32], 0, &[], &[]);
    let mut bytes = crate::request::parse_hex(&hex_proof).unwrap();
    bytes.truncate(bytes.len() - 1);
    bytes.extend_from_slice(&[0xfe, 0x00, 0x00, 0x10, 0x00]);
    bytes.extend_from_slice(&flags);
    let read = super::read_proof(&bytes).unwrap();
    assert_eq!(read.flags.len(), 1 << 20, "one byte per eight bits");
    assert_eq!(read.flags, flags);
    assert_eq!(read.bit_count(), 8 << 20);
    // `BytesToBits`: bit `p % 8` of byte `p / 8`.
    let bits: Vec<bool> = (0..16).map(|p| read.bit(8 * 0x35 + p).unwrap()).collect();
    let want: Vec<bool> =
        [0x35u8, 0x36].iter().flat_map(|b| (0..8).map(move |i| b >> i & 1 == 1)).collect();
    assert_eq!(bits, want);
    assert_eq!(read.bit(8 << 20), None);
}
