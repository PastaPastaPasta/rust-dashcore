use std::sync::atomic::{AtomicI64, Ordering};

use dash_clock::NodeSeconds;
use dash_params::{ChainOption, ChainParamsBuilder, NetworkKind, Release};
use dash_types::codec::BaseCodec;

use super::*;

/// Core's functional-test spork key's WIF (`test_framework.py:1514`), regtest's spork address's
/// key.
const FUNCTIONAL_TEST_WIF: &str = "cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";

/// [`FUNCTIONAL_TEST_WIF`]'s secret.
const REGTEST_KEY: [u8; 32] = [
    0x2b, 0xff, 0xcd, 0x23, 0x3d, 0x06, 0x6e, 0xee, 0xcf, 0x77, 0x05, 0xbd, 0x37, 0x95, 0x8d, 0xdf,
    0xb1, 0xc0, 0xe6, 0xf6, 0x09, 0x4f, 0xd0, 0xb5, 0xcd, 0xaf, 0x80, 0x42, 0xb5, 0xcd, 0x3f, 0xf0,
];

struct FixedTime(pub AtomicI64);

impl AdjustedTime for FixedTime {
    fn now(&self) -> NodeSeconds {
        NodeSeconds(self.0.load(Ordering::SeqCst))
    }
}

fn params(net: NetworkKind) -> ChainParams {
    ChainParamsBuilder::new(net, Release::V24_0).build().unwrap()
}

fn manager(net: NetworkKind, now: i64) -> (SporkManager, Arc<FixedTime>) {
    let time = Arc::new(FixedTime(AtomicI64::new(now)));
    (SporkManager::new(&params(net), Arc::clone(&time) as _), time)
}

fn key() -> SecretKey {
    SecretKey::from_secret_bytes(REGTEST_KEY).unwrap()
}

#[test]
fn the_functional_test_key_signs_for_regtest() {
    let (m, _) = manager(NetworkKind::Regtest, 1000);
    assert_eq!(m.form(), SigForm::Message);
    assert_eq!(m.set_signer(key(), false), Err(SignerError::NotTheSporkKey));
    assert_eq!(
        m.set_signer(SecretKey::from_secret_bytes([9; 32]).unwrap(), true),
        Err(SignerError::NotTheSporkKey)
    );
    m.set_signer(key(), true).unwrap();
    let s19 = SporkId::SPORK_19_CHAINLOCKS_ENABLED;
    assert!(!m.chainlocks_enabled());
    let hash = m.update_spork(s19, 0).unwrap();
    assert!(m.chainlocks_enabled());
    let msg = m.spork_by_hash(&hash).unwrap();
    assert_eq!(msg.time_signed, 1000);
    assert_eq!(m.is_valid_spork(&msg), Ok(()));
    assert_eq!(m.active_sporks(), [msg]);
}

/// `-sporkkey`: Core's functional-test WIF, refused uncompressed, under another network's
/// version byte, or garbled.
#[test]
fn the_spork_key_is_a_wif() {
    use dash_pkc::ecdsa::{Compression, EcdsaSkBytes};
    let (m, _) = manager(NetworkKind::Regtest, 1000);
    let wif = |compressed: bool, prefix| {
        let sk = EcdsaSkBytes::from_bytes(REGTEST_KEY, Compression::from(compressed));
        sk.to_wif(prefix).unwrap().to_string()
    };
    assert_eq!(wif(true, 239), FUNCTIONAL_TEST_WIF);
    assert_eq!(m.set_priv_key(&wif(false, 239)), Err(SignerError::NotTheSporkKey));
    assert_eq!(m.set_priv_key(&wif(true, 204)), Err(SignerError::Parse));
    let garbled = FUNCTIONAL_TEST_WIF.replace('Q', "q");
    assert_eq!(m.set_priv_key(&garbled), Err(SignerError::Parse));
    assert_eq!(m.update_spork(SporkId::SPORK_2_INSTANTSEND_ENABLED, 0), None);
    m.set_priv_key(FUNCTIONAL_TEST_WIF).unwrap();
    assert!(m.update_spork(SporkId::SPORK_2_INSTANTSEND_ENABLED, 0).is_some());
}

#[test]
fn testnet_signs_the_hash() {
    let (m, _) = manager(NetworkKind::Testnet, 0);
    assert_eq!(m.form(), SigForm::Hash);
    assert!(m.key_id().is_some());
}

/// O17: mainnet hard-codes every spork whatever the store holds.
#[test]
fn mainnet_is_hardened() {
    let (m, _) = manager(NetworkKind::Mainnet, 1000);
    for d in spork::SPORK_DEFS {
        let v = if d.id == SporkId::SPORK_21_QUORUM_ALL_CONNECTED {
            1
        } else {
            0
        };
        assert_eq!(m.value(d.id), v, "{}", d.name);
        assert!(m.is_active(d.id));
    }
    assert!(m.chainlocks_enabled());
    assert_eq!(m.value(SporkId(1)), 0);
}

#[test]
fn activation_follows_the_adjusted_time() {
    let (m, time) = manager(NetworkKind::Regtest, 1000);
    m.set_signer(key(), true).unwrap();
    m.update_spork(SporkId::SPORK_19_CHAINLOCKS_ENABLED, 1500).unwrap();
    assert!(!m.chainlocks_enabled());
    time.0.store(1500, Ordering::SeqCst);
    assert!(!m.chainlocks_enabled());
    time.0.store(1501, Ordering::SeqCst);
    assert!(m.chainlocks_enabled());
}

#[test]
fn the_store_file_round_trips_in_core_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(file::FILE_NAME);
    let p = params(NetworkKind::Regtest);
    let (m, time) = manager(NetworkKind::Regtest, 1000);
    m.set_signer(key(), true).unwrap();
    for (i, d) in spork::SPORK_DEFS.iter().enumerate() {
        time.0.store(1000 + i as i64, Ordering::SeqCst);
        m.update_spork(d.id, 0).unwrap();
    }
    m.update_spork(SporkId::SPORK_2_INSTANTSEND_ENABLED, 7).unwrap();
    assert!(matches!(m.load(&path, p.message_start), Ok(())), "a missing file recreates");
    assert!(m.store(&path, p.message_start).unwrap());

    let (n, _) = manager(NetworkKind::Regtest, 1000);
    n.load(&path, p.message_start).unwrap();
    // Core writes the iteration order and reads it back to the front: reversed.
    let mut expected = m.active_sporks();
    expected.reverse();
    assert_eq!(n.active_sporks(), expected);
    assert_eq!(n.value(SporkId::SPORK_2_INSTANTSEND_ENABLED), 7);
    // The replaced SPORK_2 stays servable.
    assert_eq!(n.state.read().store.all().count(), 6);

    // Another network's file stops the node and is left alone.
    let other = params(NetworkKind::Testnet).message_start;
    let (t, _) = manager(NetworkKind::Testnet, 0);
    assert!(matches!(t.load(&path, other), Err(LoadError::Network)));
    let before = std::fs::read(&path).unwrap();
    assert!(!t.store(&path, other).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), before);

    // A corrupted byte stops the node; a well-hashed bad body recreates.
    let mut bad = before.clone();
    bad[20] ^= 1;
    std::fs::write(&path, &bad).unwrap();
    assert!(matches!(n.load(&path, p.message_start), Err(LoadError::Corrupt)));
    std::fs::write(&path, &before[..31]).unwrap();
    let short = n.load(&path, p.message_start);
    assert!(matches!(short, Err(LoadError::Short)), "{short:?}");
    let mut body = before[..before.len() - 32 - 1].to_vec();
    body.extend_from_slice(&dash_consensus::hash::sha256d(&body));
    std::fs::write(&path, &body).unwrap();
    let (r, _) = manager(NetworkKind::Regtest, 1000);
    assert!(matches!(r.load(&path, p.message_start), Ok(())));
    assert!(r.active_sporks().is_empty());
}

/// `CoreRead`: a header cut short is `IncorrectFormat` (recreate), and a file that cannot be
/// opened is `FileError` (recreate, as if missing).
#[test]
fn a_short_header_or_an_unopenable_file_recreates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(file::FILE_NAME);
    let p = params(NetworkKind::Regtest);
    let (m, _) = manager(NetworkKind::Regtest, 1000);
    // An empty body with its checksum.
    std::fs::write(&path, dash_consensus::hash::sha256d(&[])).unwrap();
    assert!(matches!(m.load(&path, p.message_start), Ok(())));
    // A header that stops inside the message start.
    let mut body = vec![13];
    body.extend_from_slice(b"rdashd-sporks\xfc\xc1");
    body.extend_from_slice(&dash_consensus::hash::sha256d(&body));
    std::fs::write(&path, &body).unwrap();
    assert!(matches!(m.load(&path, p.message_start), Ok(())));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root opens anything; the check holds only where the open fails.
        if std::fs::File::open(&path).is_err() {
            assert!(matches!(m.load(&path, p.message_start), Ok(())));
        }
    }
}

/// A spork cut short in `sporks.dat` is Core's end of data: the file is recreated, and the
/// `CoreRead` line is dashd's for a stream that ran out (SP1 D17).
#[test]
fn a_cut_spork_is_cores_end_of_data() {
    let p = params(NetworkKind::Regtest);
    let (_, spork) = live_sporks().remove(0);
    let whole = dash_evo::spork::core_bytes(&spork);
    let mut body = vec![13];
    body.extend_from_slice(b"rdashd-sporks");
    body.extend_from_slice(&p.message_start);
    body.push(12);
    body.extend_from_slice(b"SporkStore-1");
    body.push(1);
    body.extend_from_slice(&whole[..whole.len() - 1]);
    body.extend_from_slice(&dash_consensus::hash::sha256d(&body));
    let err = file::decode(&body, p.message_start).map(drop).unwrap_err();
    assert!(err.recreates(), "{err:?}");
    assert_eq!(
        err.core_read_error().as_deref(),
        Some(
            "CoreRead: Deserialize or I/O error - DataStream::read(): end of data: iostream error"
        )
    );
}

/// `LoadCache` drops what the current key did not sign (`-sporkaddr` changed between runs).
#[test]
fn loading_checks_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(file::FILE_NAME);
    let p = params(NetworkKind::Regtest);
    let (m, _) = manager(NetworkKind::Regtest, 1000);
    m.set_signer(key(), true).unwrap();
    m.update_spork(SporkId::SPORK_19_CHAINLOCKS_ENABLED, 0).unwrap();
    m.store(&path, p.message_start).unwrap();
    // The P2PKH address of HASH160 bytes 1..=20 under regtest's prefix 140.
    let other = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .arg(ChainOption::SporkAddr, "yLQmwB9hninHD8Ceh2UAqLFWCzirppNLik")
        .build()
        .unwrap();
    let n = SporkManager::new(&other, Arc::new(FixedTime(AtomicI64::new(0))));
    n.load(&path, p.message_start).unwrap();
    assert!(n.active_sporks().is_empty());
}

/// The live sporks (`tests/fixtures/live/sporks.txt`): what the networks' signers signed, mainnet
/// in the message form and testnet in the hash form.
fn live_sporks() -> Vec<(NetworkKind, Spork)> {
    include_str!("../../tests/fixtures/live/sporks.txt")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            let net = match f[0] {
                "mainnet" => NetworkKind::Mainnet,
                "testnet" => NetworkKind::Testnet,
                other => panic!("unknown network {other}"),
            };
            let bytes: Vec<u8> = (0..f[2].len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&f[2][i..i + 2], 16).unwrap())
                .collect();
            (net, Spork::decode(&mut &bytes[..]).unwrap())
        })
        .collect()
}

#[test]
fn live_sporks_verify_under_their_network_only() {
    let live = live_sporks();
    assert!(live.iter().any(|(n, _)| *n == NetworkKind::Mainnet));
    assert!(live.iter().any(|(n, _)| *n == NetworkKind::Testnet));
    // Well after every live spork was signed, so none is in the future.
    let now = 2_000_000_000;
    for (net, msg) in &live {
        let other = if *net == NetworkKind::Mainnet {
            NetworkKind::Testnet
        } else {
            NetworkKind::Mainnet
        };
        let (m, _) = manager(*net, now);
        let (o, _) = manager(other, now);
        assert_eq!(m.is_valid_spork(msg), Ok(()), "{net:?} spork {}", msg.spork_id);
        assert_eq!(o.is_valid_spork(msg), Err(InvalidSpork::InvalidSignature));
        // An aliased header byte still verifies; any other change does not.
        let mut alias = msg.clone();
        alias.sig[0] += 8;
        assert_eq!(m.is_valid_spork(&alias), Ok(()));
        let mut value = msg.clone();
        value.value ^= 1;
        assert_eq!(m.is_valid_spork(&value), Err(InvalidSpork::InvalidSignature));
        let mut r = msg.clone();
        r.sig[10] ^= 1;
        assert_eq!(m.is_valid_spork(&r), Err(InvalidSpork::InvalidSignature));
        // Too far in the future at the time it was signed minus Core's two hours.
        let (early, _) = manager(*net, msg.time_signed - spork::MAX_TIME_AHEAD - 1);
        assert_eq!(early.is_valid_spork(msg), Err(InvalidSpork::TooFarInFuture));
        assert_eq!(m.process_spork(msg.clone(), None), Processed::New);
    }
}
