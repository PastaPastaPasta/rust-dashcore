//! Smoke differential test of the v1 entry points, for every pin, on mainnet and
//! testnet samples captured from the BS2 reference dashd (tests/data, see
//! tests/data/PROVENANCE.md). tests/contexts.rs covers the regtest and devnet contexts
//! and the later reward eras, tests/script.rs the script entry points.
//!
//! Expected values come from dashd (hashes, txids, merkle roots, header `nBits`,
//! subsidies, payments, budgets, deployment heights, and the undo records it wrote
//! to disk) or from dashcore (decoding, sighash, merkle, compact targets, work, and
//! a script-parser recount of legacy sigops), never from hand-written constants or
//! the oracle. The oracle's own chain parameters only select which samples are
//! checked how (era boundaries, script flags).

mod common;

use std::sync::OnceLock;

use common::{Coverage, HeaderRange, Subsidy, Superblock, header_time, hex, load_data, rpc_hash};
use dash_oracle_sys::{
    ARCHIVES_PINNED, Chain, CoinCodec, ErrorKind, Oracle, RewardEra, Verdict, ffi,
};
use dashcore::blockdata::script::Script;
use dashcore::consensus::encode::{VarInt, deserialize, serialize};
use dashcore::hashes::Hash;
use dashcore::pow::{CompactTarget, Target};
use dashcore::sighash::SighashCache;
use dashcore::{Block, Network, Transaction, TxMerkleNode, Txid};
use serde::Deserialize;

#[derive(Deserialize)]
struct Samples {
    deployments: std::collections::BTreeMap<String, Option<i32>>,
    blocks: Vec<SampleBlock>,
    header_ranges: Vec<HeaderRange>,
    subsidy: Vec<Subsidy>,
    superblocks: Vec<Superblock>,
    payments: Vec<Payment>,
}

#[derive(Deserialize)]
struct SampleBlock {
    height: i32,
    hash: String,
    merkle_root: String,
    hex: String,
    txs: Vec<SampleTx>,
    /// the block's CBlockUndo as dashd wrote it to rev*.dat (blocks with inputs)
    undo_hex: Option<String>,
}

#[derive(Deserialize)]
struct SampleTx {
    txid: String,
    instance_hash: String,
    prevouts: Vec<Prevout>,
}

#[derive(Deserialize)]
struct Prevout {
    script_pubkey: String,
    amount: i64,
    height: u32,
    coinbase: bool,
}

#[derive(Deserialize)]
struct Payment {
    height: i32,
    subsidy: i64,
    total_fee: i64,
    mn_amount: i64,
}

struct Net {
    name: &'static str,
    chain: Chain,
    network: Network,
    samples: Samples,
}

fn nets() -> &'static [Net] {
    static NETS: OnceLock<Vec<Net>> = OnceLock::new();
    NETS.get_or_init(|| {
        vec![
            Net {
                name: "mainnet",
                chain: Chain::main(),
                network: Network::Mainnet,
                samples: load_data("mainnet.json"),
            },
            Net {
                name: "testnet",
                chain: Chain::test(),
                network: Network::Testnet,
                samples: load_data("testnet.json"),
            },
        ]
    })
}

/// Run `f` for every pin and network, with a `"<pin> <network>"` label for assertion messages.
fn for_each(mut f: impl FnMut(Oracle, &Net, &str)) {
    for oracle in Oracle::all() {
        for net in nets() {
            f(oracle, net, &format!("{} {}", oracle.pin(), net.name));
        }
    }
}

fn chainparams(oracle: Oracle, net: &Net) -> serde_json::Value {
    serde_json::from_str(&oracle.chainparams_json(&net.chain).unwrap()).unwrap()
}

fn param(params: &serde_json::Value, key: &str) -> i64 {
    params["consensus"][key].as_i64().unwrap_or_else(|| panic!("consensus.{key}"))
}

fn deployment_height(net: &Net, name: &str) -> i32 {
    net.samples.deployments[name].unwrap_or(i32::MAX)
}

fn sample_txs(net: &Net) -> impl Iterator<Item = (&SampleBlock, Transaction, Vec<u8>, &SampleTx)> {
    net.samples.blocks.iter().flat_map(|b| {
        let block: Block = deserialize(&hex(&b.hex)).unwrap();
        block.txdata.into_iter().zip(&b.txs).map(move |(tx, s)| {
            let bytes = serialize(&tx);
            (b, tx, bytes, s)
        })
    })
}

#[test]
fn abi_and_version() {
    for oracle in Oracle::all() {
        assert_eq!(oracle.abi_version(), ffi::ABI_VERSION, "{}", oracle.pin());
        assert_eq!(ffi::ABI_VERSION, 31);
        let version = oracle.core_version();
        assert!(version.starts_with(&format!("{}.", oracle.pin())), "{}: {version}", oracle.pin());
        assert_eq!(oracle.script_error_string(0), "No error");
    }
}

#[test]
fn pins_match_pins_txt() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/pins.txt")).unwrap();
    let listed: Vec<Vec<&str>> = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .filter(|f| !f.is_empty())
        .collect();
    let linked: Vec<&str> = Oracle::all().map(|o| o.pin().name()).collect();
    assert_eq!(listed.iter().map(|f| f[0]).collect::<Vec<_>>(), linked);
    // O1 review B1: build.rs links only archives whose bytes hash to pins.txt.
    if !ARCHIVES_PINNED {
        panic!("built with DASH_ORACLE_ALLOW_UNPINNED=1: not the pinned Core releases");
    }
    for (oracle, fields) in Oracle::all().zip(&listed) {
        let sha = oracle.pin().archive_sha256().expect("an archive is linked");
        assert!(
            fields[3..5].contains(&sha),
            "{}: linked sha256 {sha} is not in pins.txt",
            oracle.pin()
        );
    }
}

#[test]
fn chainparams_match_dashd() {
    let keys = [
        ("bip34", "BIP34Height"),
        ("bip66", "BIP66Height"),
        ("bip65", "BIP65Height"),
        ("bip147", "BIP147Height"),
        ("csv", "CSVHeight"),
        ("dip0001", "DIP0001Height"),
        ("dip0003", "DIP0003Height"),
        ("dip0008", "DIP0008Height"),
        ("dip0020", "DIP0020Height"),
        ("dip0024", "DIP0024Height"),
        ("realloc", "BRRHeight"),
        ("v19", "V19Height"),
        ("v20", "V20Height"),
        ("mn_rr", "MN_RRHeight"),
        ("withdrawals", "WithdrawalsHeight"),
    ];
    for_each(|oracle, net, case| {
        let params = chainparams(oracle, net);
        let genesis = dashcore::blockdata::constants::genesis_block(net.network);
        assert_eq!(params["genesisHash"], genesis.block_hash().to_string(), "{case}");
        for (dashd, core) in keys {
            assert_eq!(
                Some(param(&params, core) as i32),
                net.samples.deployments[dashd],
                "{case} {core}"
            );
        }
    });
}

#[test]
fn tx_decode_matches_dashd_and_dashcore() {
    for_each(|oracle, net, case| {
        let mut n = 0;
        for (_, tx, bytes, s) in sample_txs(net) {
            let d = oracle.tx_decode(&bytes).unwrap();
            assert_eq!(d.consumed, bytes.len());
            assert_eq!(d.reencoded, bytes);
            assert_eq!(d.txid, rpc_hash(&s.txid), "{case} {}", s.txid);
            assert_eq!(d.txid, tx.txid().to_byte_array());
            assert_eq!(d.instance_hash, rpc_hash(&s.instance_hash));
            n += 1;
        }
        assert!(n > 0);
        // Core's deserializer throws on truncated input.
        let (_, _, bytes, _) = sample_txs(net).next().unwrap();
        assert_eq!(
            oracle.tx_decode(&bytes[..bytes.len() - 1]).unwrap_err().kind,
            ErrorKind::Decode
        );
    });
}

#[test]
fn block_and_header_decode_match_dashd_and_dashcore() {
    for_each(|oracle, net, case| {
        for b in &net.samples.blocks {
            let bytes = hex(&b.hex);
            let block: Block = deserialize(&bytes).unwrap();
            let d = oracle.block_decode(&bytes).unwrap();
            let label = format!("{case} {}", b.height);
            assert_eq!(d.consumed, bytes.len(), "{label}");
            assert_eq!(d.reencoded, bytes, "{label}");
            assert_eq!(d.hash, rpc_hash(&b.hash), "{label}");
            assert_eq!(d.hash, block.block_hash().to_byte_array(), "{label}");
            assert_eq!(d.merkle_root, rpc_hash(&b.merkle_root), "{label}");
            assert!(!d.mutated, "{label}");

            let h = oracle.header_decode(&bytes).unwrap();
            assert_eq!(h.consumed, 80);
            assert_eq!(h.reencoded[..], bytes[..80]);
            assert_eq!(h.hash, d.hash);
        }
    });
}

#[test]
fn merkle_root_matches_dashd_and_dashcore() {
    for_each(|oracle, net, case| {
        for b in &net.samples.blocks {
            let leaves: Vec<[u8; 32]> = b.txs.iter().map(|t| rpc_hash(&t.txid)).collect();
            let (root, mutated) = oracle.merkle_root(&leaves).unwrap();
            assert_eq!(root, rpc_hash(&b.merkle_root), "{case} {}", b.height);
            assert!(!mutated);
            let ours: TxMerkleNode = dashcore::merkle_tree::calculate_root(
                leaves.iter().map(|l| Txid::from_byte_array(*l).to_raw_hash()),
            )
            .map(TxMerkleNode::from_raw_hash)
            .unwrap();
            assert_eq!(root, ours.to_byte_array());
        }
        // A duplicated last pair is CVE-2012-2459 mutation.
        let leaves = [[1u8; 32], [2u8; 32], [3u8; 32], [3u8; 32]];
        assert!(oracle.merkle_root(&leaves).unwrap().1);
    });
}

#[test]
fn check_transaction_and_check_block_accept_the_chain() {
    for_each(|oracle, net, case| {
        for (b, mut tx, bytes, _) in sample_txs(net) {
            let label = format!("{case} {}", b.height);
            assert_eq!(oracle.check_transaction(&bytes).unwrap(), Verdict::Accept, "{label}");
            // (special transactions such as quorum commitments have no inputs)
            if !tx.is_coin_base() && !tx.input.is_empty() {
                tx.input.push(tx.input[0].clone());
                let Verdict::Reject(r) = oracle.check_transaction(&serialize(&tx)).unwrap() else {
                    panic!("{label}: duplicate input accepted");
                };
                assert_eq!(r.reason, "bad-txns-inputs-duplicate", "{label}");
            }
        }
        for b in &net.samples.blocks {
            let label = format!("{case} {}", b.height);
            let mut bytes = hex(&b.hex);
            assert_eq!(
                oracle.check_block(&net.chain, &bytes, true, true).unwrap(),
                Verdict::Accept,
                "{label}"
            );
            bytes[36] ^= 1; // merkle root
            let Verdict::Reject(r) = oracle.check_block(&net.chain, &bytes, false, true).unwrap()
            else {
                panic!("{label}: bad merkle root accepted");
            };
            assert_eq!(r.reason, "bad-txnmrklroot", "{label}");
        }
    });
}

/// GetLegacySigOpCount() reimplemented over dashcore's script parser.
fn legacy_sigops(tx: &Transaction) -> u32 {
    use dashcore::blockdata::opcodes::all::*;
    let count = |script: &Script| -> u32 {
        script
            .instructions()
            .map_while(Result::ok)
            .map(|i| match i.opcode() {
                Some(OP_CHECKSIG | OP_CHECKSIGVERIFY) => 1,
                Some(OP_CHECKMULTISIG | OP_CHECKMULTISIGVERIFY) => 20,
                _ => 0,
            })
            .sum()
    };
    tx.input.iter().map(|i| count(&i.script_sig)).sum::<u32>()
        + tx.output.iter().map(|o| count(&o.script_pubkey)).sum::<u32>()
}

#[test]
fn legacy_sigops_match_dashcore_scripts() {
    for_each(|oracle, net, case| {
        for (b, tx, bytes, _) in sample_txs(net) {
            assert_eq!(
                oracle.legacy_sigops(&bytes).unwrap(),
                legacy_sigops(&tx),
                "{case} {}",
                b.height
            );
        }
    });
}

/// GetBlockScriptFlags() for the buried deployments, from the oracle's own parameters.
fn script_flags(params: &serde_json::Value, height: i64) -> u32 {
    let mut flags = 1 << 0; // P2SH, always on in Dash
    for (key, flag) in [
        ("BIP66Height", 1 << 2),
        ("BIP65Height", 1 << 9),
        ("CSVHeight", 1 << 10),
        ("BIP147Height", 1 << 4),
    ] {
        if height >= param(params, key) {
            flags |= flag;
        }
    }
    flags
}

#[test]
fn verify_script_accepts_the_chain_and_rejects_a_bad_signature() {
    for_each(|oracle, net, case| {
        let params = chainparams(oracle, net);
        let mut n = 0;
        for (b, tx, bytes, s) in sample_txs(net) {
            let flags = script_flags(&params, b.height.into());
            for (i, prevout) in s.prevouts.iter().enumerate() {
                let spk = hex(&prevout.script_pubkey);
                let r =
                    oracle.verify_script(&spk, &bytes, i as u32, prevout.amount, flags).unwrap();
                assert!(
                    r.ok,
                    "{case} {} input {i}: {}",
                    b.height,
                    oracle.script_error_string(r.error)
                );
                n += 1;

                let mut bad = tx.clone();
                let mut sig = bad.input[i].script_sig.to_bytes();
                if sig.len() > 10 {
                    sig[10] ^= 0x55;
                    bad.input[i].script_sig = sig.into();
                    let r = oracle
                        .verify_script(&spk, &serialize(&bad), i as u32, prevout.amount, flags)
                        .unwrap();
                    assert!(
                        !r.ok && r.error != 0,
                        "{case} {} input {i}: corrupted scriptSig passed",
                        b.height
                    );
                }
            }
        }
        assert!(n > 0, "{case}: no inputs");
        // This script passes, so VerifyScript() would reach its CLEANSTACK-without-P2SH
        // assert; the shim refuses it (tests/script.rs covers the failing side).
        let (_, _, bytes, _) = sample_txs(net).find(|(_, tx, _, _)| !tx.is_coin_base()).unwrap();
        assert_eq!(
            oracle.verify_script(&[], &bytes, 0, 0, 1 << 8).unwrap_err().kind,
            ErrorKind::Argument
        );
    });
}

#[test]
fn signature_hash_matches_dashcore() {
    for_each(|oracle, net, case| {
        let mut n = 0;
        for (b, tx, bytes, s) in sample_txs(net) {
            let cache = SighashCache::new(&tx);
            for (i, prevout) in s.prevouts.iter().enumerate() {
                let spk = hex(&prevout.script_pubkey);
                for hash_type in [0x01, 0x02, 0x03, 0x81, 0x82, 0x83, 0x00, 0x41] {
                    let ours = cache
                        .legacy_signature_hash(i, Script::from_bytes(&spk), hash_type)
                        .unwrap();
                    let core =
                        oracle.signature_hash(&spk, &bytes, i as u32, hash_type as i32).unwrap();
                    assert_eq!(
                        core,
                        ours.to_byte_array(),
                        "{case} {} input {i} type {hash_type:#x}",
                        b.height
                    );
                    n += 1;
                }
            }
        }
        assert!(n > 0);
    });
}

#[test]
fn coin_codecs_match_dashd_undo_files_and_dashcore() {
    let codec = |oracle: Oracle, op, input: &[u8]| oracle.coin_codec(op, input).unwrap().0;
    for_each(|oracle, net, case| {
        // CompactSize is Bitcoin's VarInt in dashcore.
        for (_, tx, _, _) in sample_txs(net) {
            for (txout, n) in tx.output.iter().zip(1u64..) {
                let x = txout.value.wrapping_mul(n) % 0x0200_0000;
                let enc = codec(oracle, CoinCodec::CompactSizeEncode, &x.to_le_bytes());
                assert_eq!(enc, serialize(&VarInt(x)), "{case}");
                let (dec, consumed) =
                    oracle.coin_codec(CoinCodec::CompactSizeDecode, &enc).unwrap();
                assert_eq!((dec, consumed), (x.to_le_bytes().to_vec(), enc.len()), "{case}");
            }
        }

        // Rebuild each block's CBlockUndo from the prevouts dashd reported, once with the
        // Coin codec and once from the VARINT, CompressAmount and ScriptCompression codecs,
        // and compare both with the bytes dashd wrote to its undo file. Coin is
        // VARINT(height*2+coinbase) | VARINT(CompressAmount(value)) | ScriptCompression;
        // an undo record adds VARINT(0) after the code when height > 0 (TxInUndoFormatter).
        let (mut blocks, mut varints) = (0, 0);
        for b in net.samples.blocks.iter().filter(|b| b.undo_hex.is_some()) {
            let undo = hex(b.undo_hex.as_deref().unwrap());
            let mut from_coins = serialize(&VarInt(b.txs.len() as u64 - 1));
            let mut from_parts = from_coins.clone();
            for t in &b.txs[1..] {
                let mut tx_undo = serialize(&VarInt(t.prevouts.len() as u64));
                let mut tx_parts = tx_undo.clone();
                for p in &t.prevouts {
                    let spk = hex(&p.script_pubkey);
                    let code = (u64::from(p.height) << 1) | u64::from(p.coinbase);
                    let dummy: &[u8] = if p.height > 0 {
                        &[0]
                    } else {
                        &[]
                    };

                    let mut coin_input = p.height.to_le_bytes().to_vec();
                    coin_input.push(p.coinbase.into());
                    coin_input.extend(serialize(&dashcore::TxOut {
                        value: p.amount as u64,
                        script_pubkey: spk.clone().into(),
                    }));
                    let coin = codec(oracle, CoinCodec::CoinEncode, &coin_input);
                    let (decoded, consumed) =
                        oracle.coin_codec(CoinCodec::CoinDecode, &coin).unwrap();
                    assert_eq!((&decoded, consumed), (&coin_input, coin.len()), "{case}");
                    let code_len =
                        codec(oracle, CoinCodec::VarIntEncode, &code.to_le_bytes()).len();
                    tx_undo.extend([&coin[..code_len], dummy, &coin[code_len..]].concat());

                    let compressed =
                        codec(oracle, CoinCodec::AmountCompress, &p.amount.to_le_bytes());
                    assert_eq!(
                        codec(oracle, CoinCodec::AmountDecompress, &compressed),
                        p.amount.to_le_bytes()
                    );
                    let script = codec(oracle, CoinCodec::ScriptCompress, &spk);
                    assert_eq!(codec(oracle, CoinCodec::ScriptDecompress, &script), spk);
                    tx_parts.extend(codec(oracle, CoinCodec::VarIntEncode, &code.to_le_bytes()));
                    tx_parts.extend(dummy);
                    tx_parts.extend(codec(oracle, CoinCodec::VarIntEncode, &compressed));
                    tx_parts.extend(script);
                }
                assert_eq!(codec(oracle, CoinCodec::TxUndoRoundtrip, &tx_undo), tx_undo, "{case}");
                from_coins.extend(tx_undo);
                from_parts.extend(tx_parts);
            }
            assert_eq!(from_coins, undo, "{case} {}: Coin codec vs dashd undo file", b.height);
            assert_eq!(
                from_parts, undo,
                "{case} {}: VARINT/amount/script codecs vs dashd undo file",
                b.height
            );
            assert_eq!(
                codec(oracle, CoinCodec::BlockUndoRoundtrip, &undo),
                undo,
                "{case} {}",
                b.height
            );

            // Read dashd's undo bytes back with the stand-alone decoders and compare every
            // field with what dashd's RPC reported for the spent outputs (O1 review M3:
            // VARINT decoding).
            let mut rest = &undo[..];
            let take = |op, rest: &mut &[u8]| {
                let (out, consumed) = oracle.coin_codec(op, rest).unwrap();
                *rest = &rest[consumed..];
                out
            };
            let u64_of = |b: Vec<u8>| u64::from_le_bytes(b.try_into().unwrap());
            assert_eq!(
                u64_of(take(CoinCodec::CompactSizeDecode, &mut rest)),
                b.txs.len() as u64 - 1
            );
            for t in &b.txs[1..] {
                assert_eq!(
                    u64_of(take(CoinCodec::CompactSizeDecode, &mut rest)),
                    t.prevouts.len() as u64
                );
                for p in &t.prevouts {
                    let code = u64_of(take(CoinCodec::VarIntDecode, &mut rest));
                    assert_eq!(code, (u64::from(p.height) << 1) | u64::from(p.coinbase), "{case}");
                    if p.height > 0 {
                        assert_eq!(u64_of(take(CoinCodec::VarIntDecode, &mut rest)), 0, "{case}");
                    }
                    let compressed = take(CoinCodec::VarIntDecode, &mut rest);
                    assert_eq!(
                        u64_of(codec(oracle, CoinCodec::AmountDecompress, &compressed)),
                        p.amount as u64,
                        "{case}"
                    );
                    assert_eq!(
                        take(CoinCodec::ScriptDecompress, &mut rest),
                        hex(&p.script_pubkey),
                        "{case}"
                    );
                    varints += 1;
                }
            }
            assert!(rest.is_empty(), "{case} {}: undo bytes left over", b.height);
            blocks += 1;
        }
        assert!(blocks > 0 && varints > 0, "{case}: no samples with undo data");
    });
}

/// Core: validation.cpp ConvertBitsToDouble
fn bits_to_double(bits: u32) -> f64 {
    let mut shift = (bits >> 24) & 0xff;
    let mut diff = f64::from(0xffff) / f64::from(bits & 0x00ff_ffff);
    while shift < 29 {
        diff *= 256.0;
        shift += 1;
    }
    while shift > 29 {
        diff /= 256.0;
        shift -= 1;
    }
    diff
}

/// Core requires a header's nBits to equal GetNextWorkRequired(), except on mainnet
/// up to height 68589, where it accepts nBits within 50% of it (Core: validation.cpp
/// ContextualCheckBlockHeader, "DGW pre-fork"). There the chain's BTC retargets and
/// early DGW blocks differ from Core's exact answer, so only the tolerance is checked,
/// except for the KGW era (`kgw`), where every mainnet header equals Core's exact answer.
fn nbits_accepted(net: &Net, height: i32, kgw: bool, core: u32, header: u32) -> bool {
    if net.chain.name == "main" && height <= 68589 && !kgw {
        let (n1, n2) = (bits_to_double(header), bits_to_double(core));
        (n1 - n2).abs() <= n1 * 0.5
    } else {
        core == header
    }
}

#[test]
fn next_work_required_reproduces_every_header() {
    for_each(|oracle, net, case| {
        let params = chainparams(oracle, net);
        let kgw = param(&params, "nPowKGWHeight")..param(&params, "nPowDGWHeight");
        let (timespan, spacing) =
            (param(&params, "nPowTargetTimespan"), param(&params, "nPowTargetSpacing"));
        let interval = timespan / spacing; // DifficultyAdjustmentInterval()
        let min_difficulty = params["consensus"]["fPowAllowMinDifficultyBlocks"].as_bool().unwrap();
        let mut coverage = Coverage::default();
        for range in &net.samples.header_ranges {
            let headers = hex(&range.hex);
            let count = headers.len() / 80;
            let mut checked = 0;
            for i in 1..count {
                // at most the longest lookback (KGW: 4032 blocks) plus slack
                let start = i.saturating_sub(4100);
                let window = &headers[start * 80..i * 80];
                let next: [u8; 80] = headers[i * 80..(i + 1) * 80].try_into().unwrap();
                let height = range.first_height + i as i32;
                let (first, len) =
                    (i64::from(range.first_height) + start as i64, (i - start) as i64);
                // a late block gets the minimum or a relaxed difficulty from the last header alone
                let (last, new) = (header_time(&headers[(i - 1) * 80..i * 80]), header_time(&next));
                let late =
                    new > last + 2 * 60 * 60 || i64::from(new) > i64::from(last) + 4 * spacing;
                // How many headers pow.cpp reads for this block (O1 review M4): the BTC
                // retarget its interval, KGW its PastBlocksMax (or back to height 1), DGW 24.
                // The testnet min-difficulty walk depends on the bits, so it is not predicted.
                let (algo, enough) = if i64::from(height) < kgw.start {
                    if i64::from(height) % interval == 0 {
                        ("BTC", Some(len >= interval))
                    } else {
                        ("BTC", (!min_difficulty).then_some(true))
                    }
                } else if min_difficulty && late {
                    ("min-difficulty", Some(true))
                } else if kgw.contains(&height.into()) {
                    ("KGW", Some(first <= 1 || len >= timespan * 7 / spacing))
                } else {
                    ("DGW", Some(height <= 24 || len >= 24))
                };
                match oracle.next_work_required(&net.chain, window, first as i32, &next) {
                    Ok(bits) => {
                        assert_ne!(
                            enough,
                            Some(false),
                            "{case} height {height}: {len} headers are too few"
                        );
                        let expected = u32::from_le_bytes(next[72..76].try_into().unwrap());
                        coverage.hit(algo);
                        assert!(
                            nbits_accepted(net, height, algo == "KGW", bits, expected),
                            "{case} height {height}: Core {bits:#010x}, header {expected:#010x}"
                        );
                        checked += 1;
                    }
                    // never ErrorKind::Argument: these are all real chain headers
                    Err(e) if e.kind == ErrorKind::Context => {
                        assert_ne!(
                            enough,
                            Some(true),
                            "{case} height {height}: {len} headers suffice: {e}"
                        );
                        coverage.hit("context");
                    }
                    Err(e) => panic!("{case} height {height}: {e}"),
                }
            }
            assert!(checked > 0, "{case} range at {}", range.first_height);
        }
        // every algorithm the samples reach is exercised (testnet has no KGW era)
        if net.chain.name == "main" {
            coverage.require(&[("BTC", 500), ("KGW", 100), ("DGW", 100)]);
        } else {
            coverage.require(&[("BTC", 500), ("DGW", 100)]);
        }
    });
}

#[test]
fn proof_of_work_and_compact_targets_match_dashcore() {
    for_each(|oracle, net, case| {
        let mut all_bits = Vec::new();
        for range in &net.samples.header_ranges {
            for header in hex(&range.hex).chunks(80) {
                let h = oracle.header_decode(header).unwrap();
                let parsed: dashcore::block::Header = deserialize(header).unwrap();
                assert_eq!(h.hash, parsed.block_hash().to_byte_array());
                let bits = parsed.bits.to_consensus();
                assert!(oracle.check_proof_of_work(&net.chain, &h.hash, bits).unwrap(), "{case}");
                assert!(!oracle.check_proof_of_work(&net.chain, &[0xff; 32], bits).unwrap());
                all_bits.push(bits);
            }
        }
        all_bits.sort_unstable();
        all_bits.dedup();
        // sample bits plus a sweep over exponents and mantissas, including sign and overflow cases
        all_bits.extend((0u32..=34).flat_map(|e| {
            [0x01_u32, 0x8000, 0x7f_ffff, 0x80_0000, 0x12_3456].map(move |m| (e << 24) | m)
        }));
        for bits in all_bits {
            let c = oracle.compact_decode(bits).unwrap();
            if !c.negative && !c.overflow {
                let target = Target::from_compact(CompactTarget::from_consensus(bits));
                assert_eq!(c.target, target.to_le_bytes(), "bits {bits:#010x}");
                assert_eq!(
                    oracle.compact_encode(&c.target).unwrap(),
                    target.to_compact_lossy().to_consensus()
                );
            }
            if bits & 0x0080_0000 == 0 && bits >> 24 <= 32 && bits & 0x007f_ffff != 0 {
                let header = dashcore::block::Header {
                    bits: CompactTarget::from_consensus(bits),
                    ..dashcore::blockdata::constants::genesis_block(net.network).header
                };
                assert_eq!(
                    oracle.block_proof(bits).unwrap(),
                    header.work().to_le_bytes(),
                    "bits {bits:#010x}"
                );
            }
        }
    });
}

#[test]
fn subsidy_matches_getblockstats() {
    for_each(|oracle, net, case| {
        let v20 = deployment_height(net, "v20");
        assert!(!net.samples.subsidy.is_empty(), "{case}: no samples");
        for s in &net.samples.subsidy {
            let got = oracle
                .block_subsidy_inner(&net.chain, s.prev_bits, s.height - 1, s.height >= v20)
                .unwrap();
            assert_eq!(got, s.subsidy, "{case} height {}", s.height);
        }
        // Core asserts on heights below nHighSubsidyBlocks outside devnets; the shim refuses them.
        let e = oracle.block_subsidy_inner(&net.chain, 0x1d00ffff, -1, false).unwrap_err();
        assert!(e.kind == ErrorKind::Argument && !e.message.is_empty(), "{case}: {e}");
    });
}

#[test]
fn superblock_subsidy_matches_getsuperblockbudget() {
    for_each(|oracle, net, case| {
        let params = chainparams(oracle, net);
        let cycle = param(&params, "nSuperblockCycle");
        let pow_limit_bits = {
            let limit = rpc_hash(params["consensus"]["powLimit"].as_str().unwrap());
            oracle.compact_encode(&limit).unwrap()
        };
        // Core: governance/superblock.cpp CSuperblock::GetPaymentsLimit
        let bits = if params["consensus"]["fPowAllowMinDifficultyBlocks"].as_bool().unwrap() {
            pow_limit_bits
        } else {
            1
        };
        assert!(!net.samples.superblocks.is_empty(), "{case}: no samples");
        for sb in &net.samples.superblocks {
            let v20 = i64::from(sb.height) >= param(&params, "V20Height");
            let part =
                oracle.superblock_subsidy_inner(&net.chain, bits, sb.height - 1, v20).unwrap();
            assert_eq!(part * cycle, sb.budget, "{case} height {}", sb.height);
            // getsuperblockbudget is CSuperblock::GetPaymentsLimit() itself
            assert_eq!(
                oracle.superblock_payments_limit(&net.chain, sb.height).unwrap(),
                sb.budget,
                "{case} height {}",
                sb.height
            );
        }
    });
}

#[test]
fn masternode_payment_matches_dashd() {
    let mut checked = std::collections::HashMap::new();
    for_each(|oracle, net, case| {
        let (v20, mn_rr) = (deployment_height(net, "v20"), deployment_height(net, "mn_rr"));
        for p in &net.samples.payments {
            let era = if p.height >= mn_rr {
                RewardEra::EvoReward
            } else if p.height >= v20 {
                RewardEra::CreditPool
            } else {
                RewardEra::Classic
            };
            // Core: masternode/payments.cpp CMNPaymentsProcessor::GetMasternodeTxOuts
            let mut reward =
                oracle.mn_payment(&net.chain, p.height, p.subsidy + p.total_fee, era).unwrap();
            if era == RewardEra::EvoReward {
                let subsidy_reward =
                    oracle.mn_payment(&net.chain, p.height, p.subsidy, era).unwrap();
                reward -= oracle.platform_share(subsidy_reward).unwrap();
            }
            assert_eq!(reward, p.mn_amount, "{case} height {}", p.height);
            *checked.entry(oracle.pin()).or_insert(0) += 1;
        }
        // PlatformShare() overflows or asserts on this; the shim refuses it.
        assert_eq!(oracle.platform_share(i64::MAX).unwrap_err().kind, ErrorKind::Argument);
    });
    for oracle in Oracle::all() {
        assert!(
            checked.get(&oracle.pin()).is_some_and(|&n| n > 0),
            "{}: no payment samples",
            oracle.pin()
        );
    }
}

#[test]
fn chain_switches_are_isolated_per_pin() {
    // Interleave chains on every linked pin from several threads; every answer must
    // belong to the chain that was asked for.
    let expected: Vec<(Oracle, &Net, String)> = Oracle::all()
        .flat_map(|o| {
            nets()
                .iter()
                .map(move |n| (o, n, chainparams(o, n)["networkID"].as_str().unwrap().to_owned()))
        })
        .collect();
    std::thread::scope(|scope| {
        for t in 0..4 {
            let expected = &expected;
            scope.spawn(move || {
                for i in 0..50 {
                    let (oracle, net, id) = &expected[(i + t) % expected.len()];
                    let params = chainparams(*oracle, net);
                    assert_eq!(params["networkID"].as_str().unwrap(), id);
                }
            });
        }
    });
}

#[derive(Deserialize)]
struct DifficultySamples {
    samples: Vec<DifficultySample>,
}

#[derive(Deserialize)]
struct DifficultySample {
    network: String,
    height: i32,
    bits: u32,
    /// as dashd printed it: 16 significant digits
    difficulty: String,
}

/// A decimal number as (significant digits, position of the decimal point), so that
/// "320", "3.2e2" and "3.200e+02" compare equal.
fn decimal_digits(s: &str) -> (String, i32) {
    let (mantissa, exp) = s.split_once(['e', 'E']).unwrap_or((s, "0"));
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{int}{frac}");
    let leading = digits.len() - digits.trim_start_matches('0').len();
    let point = int.len() as i32 + exp.parse::<i32>().unwrap() - leading as i32;
    (digits.trim_matches('0').to_string(), point)
}

/// ConvertBitsToDouble() (O1b, for CS2) against dashd's getblockheader difficulty, which
/// rpc/blockchain.cpp computes with its own copy of the formula and prints with 16
/// significant digits.
#[test]
fn convert_bits_to_double_matches_dashd_difficulty() {
    let data: DifficultySamples = load_data("difficulty.json");
    assert!(data.samples.len() > 1000);
    for oracle in Oracle::all() {
        for s in &data.samples {
            let got = oracle.convert_bits_to_double(s.bits).unwrap();
            // dashd prints 16 significant digits (%.16g); compare the digits exactly
            assert_eq!(
                decimal_digits(&format!("{got:.15e}")),
                decimal_digits(&s.difficulty),
                "{} {} height {} bits {:#010x}: {got:e} vs dashd {}",
                oracle.pin(),
                s.network,
                s.height,
                s.bits,
                s.difficulty
            );
        }
    }
}
