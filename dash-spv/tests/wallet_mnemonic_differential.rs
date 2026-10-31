//! WL1 regtest differential 1 (design §6.2): key-wallet's Dash Core mnemonic mode against a
//! live regtest dashd's `upgradetohd`.
//!
//! For every (phrase, passphrase) pair of the corpus:
//! - on a blank descriptor wallet, `upgradetohd` then `listdescriptors`: dashd's
//!   `deriveaddresses` over the BIP44 external, BIP44 internal and DIP9 CoinJoin external
//!   descriptors equals key-wallet's BIP44 account 0 receive and change addresses and CoinJoin
//!   account 0 receive addresses (Core has no internal CoinJoin descriptor);
//! - on a blank legacy wallet, the same pair: `getnewaddress` and `getrawchangeaddress` equal the
//!   BIP44 receive and change addresses, and a passphrase over
//!   `CORE_LEGACY_MAX_PASSPHRASE_BYTES` gets Core's length error instead.
//!
//! The corpus interleaves the desktop quirk corpus' 131 weak-checksum phrases (Core accepts,
//! strict BIP39 refuses; taken round-robin over word counts), the 24 Trezor phrases and phrases
//! dashd generates (200 in all), with 12 passphrase shapes. It starts with one weak phrase per
//! word count (12 to 24), the first 24-word Trezor phrase (over 128 bytes, so PBKDF2 hashes the
//! key), a dashd-generated phrase and a 12-word Trezor phrase, so the default size covers them. Phrases Core refuses must be refused
//! by both dashd and key-wallet: every malformed-phrase case of the corpus, plus wrong-checksum
//! cases round-robin over word counts.
//!
//! Ignored by default. Run after `eval $(python3 contrib/setup-dashd.py)`:
//!
//! ```text
//! cargo test -p dash-spv --test wallet_mnemonic_differential -- --ignored --nocapture
//! ```
//!
//! Size (the defaults are gate-sized; the full corpus is `WL1_DIFF_PHRASES=200
//! WL1_DIFF_ADDRESSES=1000 WL1_DIFF_REJECTS=all`):
//! - `WL1_DIFF_PHRASES`: phrases (default 8, 1 to 200), each with all 12 passphrases;
//! - `WL1_DIFF_ADDRESSES`: addresses compared per chain (default 100);
//! - `WL1_DIFF_REJECTS`: Core-refused phrases checked (default 24, at least the 14 malformed
//!   ones, or `all` for 374);
//! - `WL1_DIFF_JOBS`: parallel workers, each with its own wallets (default 4, at most 64).
//!
//! A legacy wallet syncs its database for every key it issues, so on a host with slow fsync run
//! dashd under `eatmydata` (point `DASHD_PATH` at a wrapper, as the gate does): the gate-sized
//! run then takes about a minute instead of most of an hour.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use dash_spv::test_utils::{DashdTestContext, TestChain};
use dashcore_rpc::jsonrpc;
use dashcore_rpc::jsonrpc::http::simple_http::SimpleHttpTransport;
use dashcore_rpc::{Client, RpcApi};
use key_wallet::account::ECDSAAddressDerivation;
use key_wallet::mnemonic::{
    core_check, DashCoreMnemonic, MnemonicCompat, CORE_LEGACY_MAX_PASSPHRASE_BYTES,
};
use key_wallet::wallet::initialization::WalletAccountCreationOptions;
use key_wallet::{Network, Passphrase, Wallet};
use serde_json::{json, Value};

/// Core's refusal of a legacy passphrase over the limit (`hdchain.cpp`, `CHDChain::SetMnemonic`).
const LEGACY_TOO_LONG: &str = "Mnemonic passphrase is too long, must be at most";

/// The 12 passphrase shapes: empty, ASCII, a trailing NUL (`wallet_upgradetohd.py`), NFC and NFD
/// non-ASCII, characters NFKD folds, 4-byte UTF-8, and the lengths around Core's 256-byte salt
/// cut and the legacy limit (248, 249, 256, 300 bytes, and a cut inside a two-byte character).
fn passphrases() -> Vec<(&'static str, Vec<u8>)> {
    let p = |n: usize| vec![b'p'; n];
    let mut straddle = p(247);
    straddle.extend_from_slice("\u{e9}".as_bytes());
    vec![
        ("empty", Vec::new()),
        ("ASCII", b"TREZOR".to_vec()),
        ("trailing NUL", b"custom-passphrase\0".to_vec()),
        ("NFC e-acute", "caf\u{e9}".as_bytes().to_vec()), // spellchecker:disable
        ("NFD e + combining acute", "cafe\u{301}".as_bytes().to_vec()),
        ("fullwidth and ligature (NFKD folds)", "\u{ff21}\u{ff22}\u{fb01}".as_bytes().to_vec()),
        (
            "Cyrillic and 4-byte emoji",
            "\u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c}\u{1f600}".as_bytes().to_vec(),
        ),
        ("248 bytes (salt 256)", p(248)),
        ("249 bytes (salt cut)", p(249)),
        ("256 bytes (legacy limit)", p(256)),
        ("300 bytes (over the legacy limit)", p(300)),
        ("249 bytes, cut inside a character", straddle),
    ]
}

fn env_usize(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(v) if v == "all" => usize::MAX,
        Ok(v) => v.parse().unwrap_or_else(|_| panic!("{name}={v} is not a number")),
        Err(_) => default,
    }
}

fn quirks() -> Value {
    let path =
        concat!(env!("CARGO_MANIFEST_DIR"), "/../key-wallet/tests/data/core_bip39_quirks.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// RPC access to the node: one client per wallet URL, cookie auth.
struct Node {
    rpc_port: u16,
    cookie: String,
    wallets_dir: PathBuf,
}

impl Node {
    /// A client without the transport's 15 s timeout: an `upgradetohd` can take longer on a
    /// loaded host, and the transport resends a request it gave up on (which then fails as
    /// "already upgraded").
    fn client(&self, wallet: Option<&str>) -> Client {
        let url = match wallet {
            Some(name) => format!("http://127.0.0.1:{}/wallet/{name}", self.rpc_port),
            None => format!("http://127.0.0.1:{}", self.rpc_port),
        };
        let transport = SimpleHttpTransport::builder()
            .url(&url)
            .unwrap()
            .timeout(Duration::from_secs(1800))
            .cookie_auth(&self.cookie)
            .build();
        Client::from_jsonrpc(jsonrpc::Client::with_transport(transport))
    }

    fn create_blank(&self, name: &str, descriptors: bool) -> Client {
        let args = [
            json!(name),
            json!(false), // disable_private_keys
            json!(true),  // blank
            json!(""),    // passphrase
            json!(false), // avoid_reuse
            json!(descriptors),
            json!(false), // load_on_startup
        ];
        let _: Value = self.client(None).call("createwallet", &args).unwrap();
        self.client(Some(name))
    }

    fn drop_wallet(&self, name: &str) {
        let _: Value = self.client(None).call("unloadwallet", &[json!(name)]).unwrap();
        for dir in [self.wallets_dir.join(name), self.wallets_dir.join("wallets").join(name)] {
            if dir.exists() {
                std::fs::remove_dir_all(&dir).unwrap();
            }
        }
    }
}

/// `upgradetohd mnemonic passphrase "" false`; the error text on failure.
fn upgradetohd(wallet: &Client, phrase: &str, passphrase: &[u8]) -> Result<(), String> {
    // JSON carries text only; every passphrase in the corpus is UTF-8.
    let passphrase = std::str::from_utf8(passphrase).unwrap();
    wallet
        .call::<Value>("upgradetohd", &[json!(phrase), json!(passphrase), json!(""), json!(false)])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// key-wallet's addresses for a pair: BIP44 account 0 receive and change, CoinJoin account 0
/// receive, and the two account xpubs.
struct Ours {
    receive: Vec<String>,
    change: Vec<String>,
    coinjoin: Vec<String>,
    bip44_xpub: String,
    coinjoin_xpub: String,
}

fn ours(phrase: &str, passphrase: &[u8], count: u32) -> Ours {
    let options = WalletAccountCreationOptions::SpecificAccounts(
        [0].into(),
        BTreeSet::new(),
        [0].into(),
        BTreeSet::new(),
        BTreeSet::new(),
        None,
    );
    let wallet = Wallet::from_phrase(
        phrase,
        Passphrase::from_slice(passphrase),
        MnemonicCompat::DashCore,
        Network::Regtest,
        options,
    )
    .unwrap();
    let strings = |v: Vec<dashcore::Address>| v.into_iter().map(|a| a.to_string()).collect();
    let bip44 = &wallet.accounts.standard_bip44_accounts[&0];
    let coinjoin = &wallet.accounts.coinjoin_accounts[&0];
    Ours {
        receive: strings(bip44.derive_receive_addresses(0, count).unwrap()),
        change: strings(bip44.derive_change_addresses(0, count).unwrap()),
        coinjoin: strings(coinjoin.derive_receive_addresses(0, count).unwrap()),
        bip44_xpub: bip44.extended_public_key().to_string(),
        coinjoin_xpub: coinjoin.extended_public_key().to_string(),
    }
}

/// The xpub inside `pkh([fingerprint/path]xpub/chain/*)#checksum`.
fn descriptor_xpub(desc: &str) -> &str {
    let key = desc.split(']').nth(1).unwrap();
    key.split('/').next().unwrap()
}

/// Descriptor wallet: the three descriptors and 1..=`count` addresses of each.
fn check_descriptor(
    node: &Node,
    name: &str,
    phrase: &str,
    passphrase: &[u8],
    ours: &Ours,
    count: u32,
) {
    let wallet = node.create_blank(name, true);
    upgradetohd(&wallet, phrase, passphrase).unwrap_or_else(|e| panic!("{name}: {e}"));
    let listed: Value = wallet.call("listdescriptors", &[]).unwrap();
    let descriptors = listed["descriptors"].as_array().unwrap();
    assert_eq!(descriptors.len(), 3, "{name}: {listed}");
    let find = |pred: &dyn Fn(&Value) -> bool| -> String {
        let hits: Vec<_> = descriptors.iter().filter(|d| pred(d)).collect();
        assert_eq!(hits.len(), 1, "{name}: {listed}");
        hits[0]["desc"].as_str().unwrap().to_owned()
    };
    let external = find(&|d| d["active"] == true && d["internal"] == false);
    let internal = find(&|d| d["active"] == true && d["internal"] == true);
    let coinjoin = find(&|d| d["coinjoin"] == true);
    // v23 writes hardened steps as `'`, v24 as `h`.
    let path = |desc: &str, origin: &str, chain: &str| {
        let desc = desc.replace('\'', "h");
        assert!(desc.contains(origin) && desc.contains(chain), "{name}: {desc}");
    };
    path(&external, "/44h/1h/0h]", "/0/*)");
    path(&internal, "/44h/1h/0h]", "/1/*)");
    path(&coinjoin, "/9h/1h/4h/0h]", "/0/*)");
    assert_eq!(descriptor_xpub(&external), ours.bip44_xpub, "{name}");
    assert_eq!(descriptor_xpub(&coinjoin), ours.coinjoin_xpub, "{name}");

    let rpc = node.client(None);
    let derive = |desc: &str| -> Vec<String> {
        let range = json!([0, count - 1]);
        let v: Value = rpc.call("deriveaddresses", &[json!(desc), range]).unwrap();
        v.as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_owned()).collect()
    };
    assert_eq!(derive(&external), ours.receive, "{name}: BIP44 external");
    assert_eq!(derive(&internal), ours.change, "{name}: BIP44 internal");
    assert_eq!(derive(&coinjoin), ours.coinjoin, "{name}: CoinJoin external");
    node.drop_wallet(name);
}

/// Legacy wallet: issued receive and change addresses, or Core's length error. Returns whether
/// dashd refused the passphrase as too long.
fn check_legacy(
    node: &Node,
    name: &str,
    phrase: &str,
    passphrase: &[u8],
    ours: &Ours,
    count: u32,
) -> bool {
    let wallet = node.create_blank(name, false);
    let result = upgradetohd(&wallet, phrase, passphrase);
    let refused = passphrase.len() > CORE_LEGACY_MAX_PASSPHRASE_BYTES;
    if refused {
        let err = result.expect_err(name);
        let expected = format!("{LEGACY_TOO_LONG} {CORE_LEGACY_MAX_PASSPHRASE_BYTES} characters");
        assert!(err.contains(&expected), "{name}: {err}");
        assert!(!is_hd(&wallet), "{name}: a refused upgrade left an HD chain");
    } else {
        result.unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(is_hd(&wallet), "{name}: no HD chain after upgradetohd");
        for i in 0..count as usize {
            let receive: String = wallet.call("getnewaddress", &[]).unwrap();
            assert_eq!(receive, ours.receive[i], "{name}: receive {i}");
            let change: String = wallet.call("getrawchangeaddress", &[]).unwrap();
            assert_eq!(change, ours.change[i], "{name}: change {i}");
        }
    }
    node.drop_wallet(name);
    refused
}

/// Whether a legacy wallet has an HD chain (`getwalletinfo` reports `hdchainid`).
fn is_hd(wallet: &Client) -> bool {
    let info: Value = wallet.call("getwalletinfo", &[]).unwrap();
    info.get("hdchainid").is_some()
}

/// Phrases dashd generates (`upgradetohd` without a phrase), read back with
/// `listdescriptors true`.
fn core_generated(node: &Node, count: usize) -> Vec<String> {
    (0..count)
        .map(|i| {
            let name = format!("wl1-gen-{i}");
            let wallet = node.create_blank(&name, true);
            let _: Value = wallet.call("upgradetohd", &[]).unwrap();
            let listed: Value = wallet.call("listdescriptors", &[json!(true)]).unwrap();
            let phrase = listed["descriptors"][0]["mnemonic"].as_str().unwrap().to_owned();
            node.drop_wallet(&name);
            phrase
        })
        .collect()
}

/// `(case, phrase)` of the quirk corpus' `check` cases that `keep` selects.
fn check_cases(corpus: &Value, keep: impl Fn(&Value) -> bool) -> Vec<(String, String)> {
    corpus["check"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| keep(c))
        .map(|c| {
            (c["case"].as_str().unwrap().to_owned(), c["mnemonic"].as_str().unwrap().to_owned())
        })
        .collect()
}

/// Reorders `cases` round-robin over word counts, so that any prefix covers every length.
fn by_word_count(cases: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut groups: BTreeMap<usize, VecDeque<(String, String)>> = BTreeMap::new();
    for case in cases {
        groups.entry(case.1.split(' ').count()).or_default().push_back(case);
    }
    let mut out = Vec::new();
    while groups.values().any(|g| !g.is_empty()) {
        out.extend(groups.values_mut().filter_map(VecDeque::pop_front));
    }
    out
}

/// `head` first, then one item per source in turn (skipping those in `head`) until every
/// source is used up; returns `(source, index)` pairs. Within each source the indices stay in
/// increasing order when `head` takes a source's items from its start.
fn interleave(lengths: &[usize], head: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let longest = lengths.iter().copied().max().unwrap_or(0);
    let rest = (0..longest)
        .flat_map(|i| {
            lengths.iter().enumerate().filter(move |(_, &n)| i < n).map(move |(s, _)| (s, i))
        })
        .filter(|item| !head.contains(item));
    head.iter().copied().chain(rest).collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "regtest differential (WL1): needs dashd from contrib/setup-dashd.py; run with --ignored"]
async fn dash_core_mnemonics_match_dashd() {
    let phrases_wanted = env_usize("WL1_DIFF_PHRASES", 8).clamp(1, 200);
    let count = u32::try_from(env_usize("WL1_DIFF_ADDRESSES", 100).clamp(1, 1000)).unwrap();
    let rejects_wanted = env_usize("WL1_DIFF_REJECTS", 24);
    let jobs = env_usize("WL1_DIFF_JOBS", 4).clamp(1, 64);

    let args = vec![
        format!("-rpcthreads={}", jobs + 2),
        "-rpcworkqueue=256".to_string(),
        "-debug=0".to_string(),
        "-createwalletbackups=0".to_string(),
        // Addresses do not depend on the keypool size; a small pool keeps upgradetohd cheap
        // (getnewaddress then derives one key per call).
        "-keypool=1".to_string(),
    ];
    // Opt-in test: a skip would be a silent pass.
    let ctx = DashdTestContext::new_with_args(TestChain::Minimal, args)
        .await
        .expect("SKIP_DASHD_TESTS is set, but the WL1 differential was requested");
    let datadir: &Path = ctx.node.datadir();
    let node = Node {
        rpc_port: ctx.node.rpc_port(),
        cookie: std::fs::read_to_string(datadir.join("regtest/.cookie")).unwrap(),
        wallets_dir: datadir.join("regtest"),
    };
    let version: Value = node.client(None).call("getnetworkinfo", &[]).unwrap();
    eprintln!("dashd {}", version["subversion"]);

    // The corpus: weak-checksum phrases (round-robin over word counts), Trezor phrases and
    // dashd-generated phrases, interleaved so that a gate-sized prefix has all three.
    let corpus = quirks();
    let weak = by_word_count(check_cases(&corpus, |c| {
        c["core_check"] == true && c["strict_check"] == false
    }));
    let trezor: Vec<(String, String)> = corpus["trezor"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, c)| (format!("trezor {i}"), c["mnemonic"].as_str().unwrap().to_owned()))
        .collect();
    assert_eq!((weak.len(), trezor.len()), (131, 24));
    for (case, phrase) in &weak {
        let parsed = DashCoreMnemonic::parse(phrase.as_bytes()).unwrap();
        assert!(!parsed.is_strict_bip39(), "{case}: labelled weak, but strict BIP39 accepts it");
    }
    // `by_word_count` puts one phrase of each word count first.
    let word_counts: Vec<usize> = weak[..5].iter().map(|(_, p)| p.split(' ').count()).collect();
    assert_eq!(word_counts, [12, 15, 18, 21, 24]);
    let trezor_24 = trezor.iter().position(|(_, p)| p.split(' ').count() == 24).unwrap();
    assert!(trezor[trezor_24].1.len() > 128, "a phrase longer than one SHA-512 block");
    assert_eq!(trezor[0].1.split(' ').count(), 12);
    let head = [(0, 0), (0, 1), (0, 2), (0, 3), (0, 4), (1, trezor_24), (2, 0), (1, 0)];
    let mut plan = interleave(&[weak.len(), trezor.len(), 200 - weak.len() - trezor.len()], &head);
    assert_eq!(plan.len(), 200);
    plan.truncate(phrases_wanted);
    let generated: Vec<(String, String)> =
        core_generated(&node, plan.iter().filter(|(s, _)| *s == 2).count())
            .into_iter()
            .enumerate()
            .map(|(i, p)| (format!("dashd-generated {i}"), p))
            .collect();
    for (case, phrase) in &generated {
        let parsed = DashCoreMnemonic::parse(phrase.as_bytes()).unwrap();
        assert!(parsed.is_strict_bip39(), "{case}");
    }
    let sources = [&weak, &trezor, &generated];
    let phrases: Vec<&(String, String)> = plan.iter().map(|&(s, i)| &sources[s][i]).collect();

    let shapes = passphrases();
    assert_eq!(shapes.len(), 12);
    let pairs: Vec<(usize, usize)> =
        (0..phrases.len()).flat_map(|p| (0..shapes.len()).map(move |s| (p, s))).collect();
    eprintln!(
        "{} phrases x {} passphrases = {} pairs, {count} addresses per chain, {jobs} jobs",
        phrases.len(),
        shapes.len(),
        pairs.len()
    );

    let start = Instant::now();
    let next = AtomicUsize::new(0);
    let legacy_refusals = AtomicUsize::new(0);
    // Each worker returns the labels of the pairs that differed. The workers block, so the
    // runtime is told to move its other tasks off this thread.
    let failures: Vec<String> = tokio::task::block_in_place(|| {
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..jobs)
                .map(|_| {
                    let (node, phrases, shapes, pairs, next, refusals) =
                        (&node, &phrases, &shapes, &pairs, &next, &legacy_refusals);
                    scope.spawn(move || {
                        let mut failures = Vec::new();
                        loop {
                            let k = next.fetch_add(1, Ordering::Relaxed);
                            let Some(&(p, s)) = pairs.get(k) else {
                                break failures;
                            };
                            let ((case, phrase), (shape, passphrase)) = (phrases[p], &shapes[s]);
                            let run = std::panic::catch_unwind(|| {
                                assert!(core_check(phrase.as_bytes()));
                                let ours = ours(phrase, passphrase, count);
                                let name = format!("wl1-d-{k}");
                                check_descriptor(node, &name, phrase, passphrase, &ours, count);
                                let name = format!("wl1-l-{k}");
                                check_legacy(node, &name, phrase, passphrase, &ours, count)
                            });
                            match run {
                                Ok(true) => {
                                    refusals.fetch_add(1, Ordering::Relaxed);
                                }
                                Ok(false) => {}
                                Err(_) => failures.push(format!("{case} / {shape}")),
                            }
                            let done = k + 1;
                            if done % 24 == 0 {
                                eprintln!("{done}/{} pairs, {:?}", pairs.len(), start.elapsed());
                            }
                        }
                    })
                })
                .collect();
            workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
        })
    });
    assert!(failures.is_empty(), "{} pairs differ: {failures:#?}", failures.len());
    // One shape is over the legacy limit, so dashd refused exactly one legacy upgrade per phrase.
    assert_eq!(legacy_refusals.load(Ordering::Relaxed), phrases.len());
    eprintln!("{} pairs agree in {:?}", pairs.len(), start.elapsed());

    // Phrases Core refuses: dashd's upgradetohd refuses them on both wallet kinds, and so does
    // key-wallet's DashCore mode. Every malformed-phrase case is checked; the wrong-checksum
    // cases fill the rest of the budget, round-robin over word counts. The empty phrase is left
    // out: it asks upgradetohd to generate one.
    let refused_cases = check_cases(&corpus, |c| c["core_check"] == false && c["mnemonic"] != "");
    let (checksum, malformed): (Vec<_>, Vec<_>) =
        refused_cases.into_iter().partition(|(case, _)| case.contains("checksum"));
    assert_eq!((checksum.len(), malformed.len()), (360, 14));
    let budget = rejects_wanted.saturating_sub(malformed.len());
    let refused: Vec<(String, String)> =
        malformed.into_iter().chain(by_word_count(checksum).into_iter().take(budget)).collect();
    let descriptor = node.create_blank("wl1-refuse-d", true);
    let legacy = node.create_blank("wl1-refuse-l", false);
    for (case, phrase) in &refused {
        assert!(!core_check(phrase.as_bytes()), "{case}");
        let ours = Wallet::from_phrase(
            phrase,
            Passphrase::new(Vec::new()),
            MnemonicCompat::DashCore,
            Network::Regtest,
            WalletAccountCreationOptions::None,
        );
        assert!(ours.is_err(), "{case}: DashCore mode accepted a phrase Core refuses");
        for wallet in [&descriptor, &legacy] {
            let err = upgradetohd(wallet, phrase, b"")
                .expect_err(&format!("{case}: dashd accepted a refused phrase"));
            assert!(err.contains("invalid mnemonic"), "{case}: {err}");
        }
    }
    assert!(!is_hd(&legacy), "a refused phrase gave the legacy wallet an HD chain");
    node.drop_wallet("wl1-refuse-d");
    node.drop_wallet("wl1-refuse-l");
    eprintln!("{} refused phrases refused by both", refused.len());
}
