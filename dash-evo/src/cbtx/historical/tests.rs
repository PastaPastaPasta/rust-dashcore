use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_consensus::primitives::{Header, Tx, TxRef, decode_tx_exact};
use dash_consensus::{Block, BlockId};
use dash_pkc::bls::{BlsCtx, BlsGlobalSig, BlsScIetf, BlsSchemeKind, BlsSecretKey};
use dash_primitives::{BlockHash, BlockHeader, TxType};

use super::*;

/// `feature_llmq_chainlocks`' tip, V20 height and carriers (the run rpc_diff replays): each
/// carrier from 309 on and the height its coinbase certifies; 200 to 308 carry none.
const TIP: i32 = 352;
const V20: i32 = 200;
const CERTIFIED: &[(std::ops::RangeInclusive<i32>, i32)] = &[
    (309..=329, 308),
    (330..=336, 329),
    (337..=337, 336),
    (338..=338, 337),
    (339..=339, 338),
    (340..=340, 339),
    (341..=343, 340),
    (344..=347, 343),
    (348..=349, 346),
    (350..=350, 349),
    (351..=351, 350),
    (352..=352, 351),
];

fn hash(height: i32) -> BlockHash {
    let mut b = [0; 32];
    b[..4].copy_from_slice(&height.to_le_bytes());
    b[31] = 0xa5;
    BlockHash::from(b)
}

fn certified(carrier: i32) -> Option<i32> {
    CERTIFIED.iter().find(|(range, _)| range.contains(&carrier)).map(|(_, h)| *h)
}

fn entry_certifying(carrier: i32, signed: Option<i32>) -> CarrierEntry {
    match signed {
        Some(h) => CarrierEntry {
            height_diff: u32::try_from(carrier - h - 1).unwrap(),
            signature: [u8::try_from(carrier % 200).unwrap() | 1; 96],
        },
        None => CarrierEntry {
            height_diff: 0,
            signature: [0; 96],
        },
    }
}

/// A chain to `tip` whose carriers answer `entries` (a null certificate where it has none), or
/// the error in `failing`; it records each load.
struct Fake {
    tip: i32,
    entries: BTreeMap<i32, CarrierEntry>,
    failing: BTreeMap<i32, HistoricalClError>,
    missing: BTreeSet<i32>,
    interrupted: bool,
    loads: Vec<i32>,
}

impl Fake {
    fn chainlocks() -> Fake {
        let entries = (V20..=TIP).map(|c| (c, entry_certifying(c, certified(c)))).collect();
        Fake::with(TIP, entries)
    }

    fn with(tip: i32, entries: BTreeMap<i32, CarrierEntry>) -> Fake {
        Fake {
            tip,
            entries,
            failing: BTreeMap::new(),
            missing: BTreeSet::new(),
            interrupted: false,
            loads: Vec::new(),
        }
    }
}

impl CarrierSource for &mut Fake {
    fn ancestor(&self, height: i32) -> Option<Carrier> {
        ((0..=self.tip).contains(&height) && !self.missing.contains(&height)).then(|| Carrier {
            height,
            hash: hash(height),
            id: BlockId(u32::try_from(height).unwrap()),
        })
    }

    fn load(&mut self, carrier: &Carrier) -> Result<CarrierEntry, HistoricalClError> {
        self.loads.push(carrier.height);
        if let Some(e) = self.failing.get(&carrier.height) {
            return Err(*e);
        }
        Ok(self.entries.get(&carrier.height).copied().unwrap_or(entry_certifying(0, None)))
    }

    fn interrupted(&self) -> bool {
        self.interrupted
    }
}

fn find(fake: &mut Fake, height: i32) -> Result<Option<HistoricalCl>, HistoricalClError> {
    let tip = fake.tip;
    CoinbaseClReader::new(tip, V20, fake).find(height, height)
}

/// Every height of the run: found exactly where the first carrier above it certifies it, with
/// that carrier, the signed block's hash and the carrier's bytes; a skipped height, the tip and
/// past it are not found.
#[test]
fn the_chainlock_runs_carriers_answer_every_height() {
    let mut found = BTreeMap::new();
    for height in -1..=TIP + 1 {
        let mut fake = Fake::chainlocks();
        if let Some(cl) = find(&mut fake, height).unwrap() {
            assert_eq!((cl.height, cl.block_hash), (height, hash(height)));
            assert_eq!(cl.signature, entry_certifying(cl.carrier.height, Some(height)).signature);
            assert_eq!(cl.carrier.hash, hash(cl.carrier.height));
            found.insert(height, cl.carrier.height);
        }
    }
    let want = [
        (308, 309),
        (329, 330),
        (336, 337),
        (337, 338),
        (338, 339),
        (339, 340),
        (340, 341),
        (343, 344),
        (346, 348),
        (349, 350),
        (350, 351),
        (351, 352),
    ];
    assert_eq!(found, BTreeMap::from(want));
    let mut fake = Fake::chainlocks();
    assert_eq!(CoinbaseClReader::new(TIP, V20, &mut fake).find(i32::MAX, i32::MAX), Ok(None));
}

/// Core's probes for 200: doubling steps from the first carrier, then the bisection.
#[test]
fn the_search_reads_cores_probes_in_order() {
    let mut fake = Fake::chainlocks();
    assert_eq!(find(&mut fake, 200), Ok(None));
    let probes = [201, 202, 204, 208, 216, 232, 264, 328, 296, 312, 304, 308, 310, 309];
    assert_eq!(fake.loads, probes);
    let mut fake = Fake::chainlocks();
    assert!(find(&mut fake, 346).unwrap().is_some());
    assert_eq!(fake.loads, [347, 348]);

    // A carrier Core never reads can fail; one it reads fails the lookup with its error.
    for carrier in V20..=TIP {
        let mut fake = Fake::chainlocks();
        fake.failing.insert(carrier, HistoricalClError::Unavailable);
        let got = find(&mut fake, 200);
        if probes.contains(&carrier) {
            assert_eq!(got, Err(HistoricalClError::Unavailable), "{carrier}");
        } else {
            assert_eq!(got, Ok(None), "{carrier}");
        }
    }
}

/// Below V20 no carrier is read, though its coinbase would throw.
#[test]
fn carriers_below_v20_are_never_read() {
    let mut fake = Fake::chainlocks();
    for c in 0..V20 {
        fake.failing.insert(c, HistoricalClError::InvalidCoinbase);
    }
    for height in [0, 5, 150, 198, 199] {
        assert_eq!(find(&mut fake, height), Ok(None), "{height}");
    }
    assert!(fake.loads.iter().all(|h| *h >= V20), "{:?}", fake.loads);
    let mut reader = CoinbaseClReader::new(TIP, V20, &mut fake);
    assert_eq!(reader.read(150), Ok(None));
    assert_eq!(reader.read(-1), Ok(None));
}

#[test]
fn the_range_checks_come_before_any_read() {
    let mut fake = Fake::chainlocks();
    let mut reader = CoinbaseClReader::new(TIP, V20, &mut fake);
    assert_eq!(reader.find(-1, 5), Ok(None));
    assert_eq!(reader.find(6, 5), Ok(None));
    assert_eq!(reader.find(TIP, TIP), Ok(None));
    assert!(fake.loads.is_empty());

    // The first carrier above the tip: a tip below V20.
    let mut short = Fake::with(V20 - 1, BTreeMap::new());
    assert_eq!(CoinbaseClReader::new(V20 - 1, V20, &mut short).find(0, 0), Ok(None));
    assert!(short.loads.is_empty());

    // The only carrier is the tip.
    let entries = BTreeMap::from([(309, entry_certifying(309, Some(308)))]);
    let mut at_tip = Fake::with(309, entries);
    let cl = CoinbaseClReader::new(309, V20, &mut at_tip).find(308, 308).unwrap().unwrap();
    assert_eq!((cl.height, cl.carrier.height), (308, 309));
    assert_eq!(at_tip.loads, [309]);
}

/// `Read`'s order: the ancestor and V20 checks, the request's own answers, the shutdown flag,
/// then the budget; a repeated height counts once.
#[test]
fn read_checks_in_cores_order() {
    let mut fake = Fake::chainlocks();
    fake.interrupted = true;
    let mut reader = CoinbaseClReader::new(TIP, V20, &mut fake);
    assert_eq!(reader.read(TIP + 1), Ok(None), "no ancestor before the flag");
    assert_eq!(reader.read(V20 - 1), Ok(None), "below V20 before the flag");
    assert_eq!(reader.read(309), Err(HistoricalClError::Interrupted));

    let mut fake = Fake::chainlocks();
    let mut reader = CoinbaseClReader::new(TIP, V20, &mut fake);
    let first = reader.read(309).unwrap();
    reader.source.interrupted = true;
    assert_eq!(reader.read(309), Ok(first), "a height already read");
    assert_eq!(reader.read(310), Err(HistoricalClError::Interrupted));
    assert_eq!(fake.loads, [309]);

    let tip = 20_000;
    let mut wide = Fake::with(tip, BTreeMap::new());
    let mut reader = CoinbaseClReader::new(tip, 1, &mut wide);
    for h in 1..=i32::try_from(MAX_HISTORICAL_CARRIER_LOOKUPS).unwrap() {
        assert_eq!(reader.read(h), Ok(None));
    }
    assert_eq!(reader.read(1), Ok(None), "a repeat does not count");
    let next = i32::try_from(MAX_HISTORICAL_CARRIER_LOOKUPS).unwrap() + 1;
    reader.source.interrupted = true;
    assert_eq!(reader.read(next), Err(HistoricalClError::Interrupted), "the flag first");
    reader.source.interrupted = false;
    assert_eq!(reader.read(next), Err(HistoricalClError::BudgetExhausted));
}

/// A source without the signed block's ancestor: the defensive branch's text.
#[test]
fn a_signed_height_without_an_ancestor_is_cores_error() {
    let mut fake = Fake::chainlocks();
    fake.missing.insert(308);
    assert_eq!(find(&mut fake, 308), Err(HistoricalClError::InvalidBlockHeight));
    assert_eq!(
        HistoricalClError::InvalidBlockHeight.to_string(),
        "Invalid historical ChainLock block height"
    );
}

fn fixtures() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../tests/data/cbtx_testnet.json")).unwrap()
}

fn fixture_tx(height: u64, key: &str) -> TxRef {
    let blocks = fixtures()["blocks"].as_array().unwrap().clone();
    let block = blocks.iter().find(|b| b["height"] == height).unwrap();
    let hex = block[key].as_str().unwrap();
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect();
    Arc::new(decode_tx_exact(&bytes).unwrap().into_value())
}

/// Testnet's v3 coinbase at 1,569,000 (difference 0, a basic signature), with `edit` applied.
fn coinbase(edit: impl FnOnce(&mut dash_primitives::Transaction)) -> TxRef {
    let mut raw = fixture_tx(1_569_000, "coinbase_hex").to_mutable();
    edit(&mut raw);
    Arc::new(Tx::new(raw))
}

/// A block of `txs` whose header commits to them.
fn block_of(txs: Vec<TxRef>) -> Block {
    let root = Block::new(Header::new(BlockHeader::default()), txs.clone()).merkle().0;
    let header = BlockHeader {
        merkle_root: root,
        ..BlockHeader::default()
    };
    Block::new(Header::new(header), txs)
}

const H: i32 = 1_569_000;
/// The v3 payload's diff and signature bytes: version(2) height(4) roots(64) diff(1) sig(96).
const DIFF: usize = 70;
const SIG: std::ops::Range<usize> = 71..167;

#[test]
fn the_loader_checks_in_cores_order() {
    use HistoricalClError::*;

    assert_eq!(load_carrier(None, H), Err(Unavailable));
    assert_eq!(load_carrier(Some(&block_of(Vec::new())), H), Err(Unavailable));
    let corrupt = Block::new(Header::new(BlockHeader::default()), vec![coinbase(|_| {})]);
    assert_eq!(load_carrier(Some(&corrupt), H), Err(Corrupted));
    // The merkle root before the coinbase: a corrupt block whose first transaction is no
    // coinbase is still corrupt.
    let second = fixture_tx(1_569_000, "second_tx_hex");
    let corrupt = Block::new(Header::new(BlockHeader::default()), vec![second]);
    assert_eq!(load_carrier(Some(&corrupt), H), Err(Corrupted));

    let second = fixture_tx(1_569_000, "second_tx_hex");
    let v2 = fixture_tx(900_000, "coinbase_hex");
    for (why, tx, height) in [
        ("not a coinbase", second, H),
        ("not a CbTx", coinbase(|t| t.tx_type = TxType::ProviderUpdateRevoke), H),
        ("trailing payload", coinbase(|t| t.extra_payload.push(0)), H),
        ("version 2", v2, 900_000),
        ("another height", coinbase(|_| {}), H + 1),
    ] {
        assert_eq!(load_carrier(Some(&block_of(vec![tx])), height), Err(InvalidCoinbase), "{why}");
    }

    // Height 1 with difference 1 and a null certificate: the difference is checked first.
    let low = coinbase(|t| {
        t.extra_payload[2..6].copy_from_slice(&1i32.to_le_bytes());
        t.extra_payload[DIFF] = 1;
        t.extra_payload[SIG].fill(0);
    });
    assert_eq!(load_carrier(Some(&block_of(vec![low])), 1), Err(InvalidDiff));
    // The coinbase's height before the difference: at another carrier height, with a difference
    // above that height too, it is InvalidCoinbase.
    let both = coinbase(|t| {
        t.extra_payload[2..6].copy_from_slice(&1i32.to_le_bytes());
        t.extra_payload[DIFF] = 5;
    });
    assert_eq!(load_carrier(Some(&block_of(vec![both])), 2), Err(InvalidCoinbase));

    let real = load_carrier(Some(&block_of(vec![coinbase(|_| {})])), H).unwrap();
    let raw = fixture_tx(1_569_000, "coinbase_hex");
    assert!(real.has_chainlock());
    assert_eq!(real.height_diff, 0);
    assert_eq!(&real.signature[..], &raw.to_mutable().extra_payload[SIG]);

    let null = coinbase(|t| t.extra_payload[SIG].fill(0));
    let null = load_carrier(Some(&block_of(vec![null])), H).unwrap();
    assert!(!null.has_chainlock());
}

/// A key's signature in its basic and legacy encodings: each is canonical in its own scheme
/// only.
fn both_encodings() -> ([u8; 96], [u8; 96]) {
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[7; 32]).expect("a key");
    let basic = key.sign_message(b"bl-689a").to_bytes();
    let legacy =
        BlsGlobalSig::decode(&basic).unwrap().get(BlsCtx::BASIC).to_bytes(BlsSchemeKind::Legacy);
    (basic, legacy)
}

/// Presence is the raw bytes, whatever the scheme; `Signed()` decodes under the flag given.
#[test]
fn presence_is_the_bytes_and_the_signature_takes_the_flag() {
    let (basic, legacy) = both_encodings();
    let cl = |signature| HistoricalCl {
        height: 1,
        block_hash: hash(1),
        carrier: Carrier {
            height: 2,
            hash: hash(2),
            id: BlockId(2),
        },
        signature,
    };
    assert_eq!(cl(basic).signed(BlsCtx::BASIC), Ok(basic));
    assert_eq!(cl(legacy).signed(BlsCtx::LEGACY), Ok(legacy));
    assert_eq!(cl(basic).signed(BlsCtx::LEGACY), Err(HistoricalClError::InvalidSignature));
    assert_eq!(cl(legacy).signed(BlsCtx::BASIC), Err(HistoricalClError::InvalidSignature));
    assert_eq!(
        HistoricalClError::InvalidSignature.to_string(),
        "Invalid historical ChainLock signature"
    );
    assert_eq!(cl([0xff; 96]).signed(BlsCtx::BASIC), Err(HistoricalClError::Malleable));

    // A legacy encoding in the coinbase is a certificate, read without the flag.
    let carried = coinbase(|t| t.extra_payload[SIG].copy_from_slice(&legacy));
    let entry = load_carrier(Some(&block_of(vec![carried])), H).unwrap();
    assert!(entry.has_chainlock());
    assert_eq!(entry.signature, legacy);
}
