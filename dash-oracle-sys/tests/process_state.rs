//! A peer session leaves the process as a fresh one has it (Sol r3, O1c D4): every other entry
//! point gives the same output, byte for byte, before any session and after sessions that
//! start, are refused and throw; and a later session runs as the first did.
//!
//! The sweep runs in a child process, so its first round sees a fresh process.

mod common;

use common::{coins, extra_payload, headers, hex, rpc_hash};
use dash_oracle_sys::{
    AddrManConfig, AddrOp, Address, BlsKind, BlsMode, Chain, Checkpoints, CoinCodec, CoinsView,
    ConnType, Context, DEPLOYMENT_V24, ErrorKind, ListEncoding, ListOp, MnList, NetAddr, Oracle,
    P2pStream, QuorumMembers, RewardEra, Service, SessionConfig, SpentOutput, ThresholdState,
    UniqueProperty,
};
use dashcore::consensus::deserialize;
use dashcore::hashes::{Hash, sha256d};
use serde_json::Value;

/// Regtest genesis time (chainparams.cpp `CreateGenesisBlock(1417713337, ...)`).
const GENESIS_TIME: i64 = 1_417_713_337;
/// The session clock: a day after genesis.
const T: i64 = GENESIS_TIME + 86_400;
/// Regtest's genesis block hash (RPC order).
const REGTEST_GENESIS: &str = "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e";
/// SCRIPT_VERIFY_P2SH | SCRIPT_VERIFY_STRICTENC | SCRIPT_VERIFY_DERSIG.
const FLAGS: u32 = 1 | 2 | 4;

/// Options away from every default a session's node sets process-wide state from.
const NON_DEFAULT_OPTIONS: &[&str] = &[
    "-allowprivatenet=1",
    "-prune=1",
    "-txindex=0",
    "-disablegovernance=1",
    "-bytespersigop=50",
    "-timeout=1234",
    "-peertimeout=7",
    "-blockfilterindex=basic",
    "-peerbloomfilters=1",
    "-v2transport=1",
    "-maxsigcachesize=8",
    "-debug=all",
    "-loglevel=trace",
];

/// Logging options AppInitParameterInteraction refuses, with the reason it gives
/// (init/common.cpp SetLoggingLevel, SetLoggingCategories).
const INVALID_LOGGING_OPTIONS: [(&str, &str); 3] = [
    (
        "-loglevel=not_a_level",
        "Unsupported global logging level -loglevel=not_a_level. Valid values: info, debug, trace.",
    ),
    (
        "-loglevel=net:bogus",
        "Unsupported category-specific logging level -loglevel=net:bogus. Expected \
         -loglevel=<category>:<loglevel>. Valid categories: addrman, bench, blockstorage, \
         chainlocks, cmpctblock, coindb, coinjoin, creditpool, ehf, estimatefee, gobject, http, \
         i2p, instantsend, ipc, leveldb, libevent, llmq, llmq-dkg, llmq-sigs, mempool, mempoolrej, \
         mnpayments, mnsync, net, netconn, proxy, prune, qt, rand, reindex, rpc, scan, \
         selectcoins, spork, tor, txreconciliation, validation, walletdb, zmq. Valid loglevels: \
         info, debug, trace.",
    ),
    ("-debug=bogus", "Unsupported logging category -debug=bogus."),
];

#[test]
fn sessions_leave_the_process_as_a_fresh_one() {
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sessions_leave_the_process_child", "--nocapture", "--test-threads=1"])
        .env("O1C_PROCESS_STATE_CHILD", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&child.stdout);
    assert!(child.status.success(), "{stdout}{}", String::from_utf8_lossy(&child.stderr));
    assert!(stdout.contains("1 passed"), "{stdout}");
}

#[test]
fn sessions_leave_the_process_child() {
    if std::env::var_os("O1C_PROCESS_STATE_CHILD").is_none() {
        return;
    }
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let fresh = sweep(&oracle);
        let first = plain_session(&oracle);
        let check = |after: &str| {
            let now = sweep(&oracle);
            assert_eq!(now.len(), fresh.len(), "{pin}: after {after}");
            for (a, b) in fresh.iter().zip(&now) {
                assert_eq!(a, b, "{pin}: after {after}");
            }
        };
        check("a session");
        let busy = busy_session(&oracle);
        check("a session with non-default options, blocks and a peer");
        // A fixture block that does not become the tip: the start fails once the node exists.
        let refused = SessionConfig {
            blocks: vec![[vec![0; 80], vec![0]].concat()],
            ..config(NON_DEFAULT_OPTIONS, Vec::new())
        };
        assert!(oracle.peer_session_new(&refused).unwrap().is_none(), "{pin}");
        check("a refused start");
        // Starts that throw: in BasicTestingSetup's argument parsing, in its parameter
        // interaction (an option dashd refuses at init), later in its constructor (after logging,
        // the mock time and the caches), and in ChainTestingSetup's.
        // Each with (part of) Core's message, so a start that fails elsewhere does not count.
        for (option, stage, message) in [
            ("-nosuchoption=1", "parsing", "Invalid parameter -nosuchoption"),
            (
                "-prune=-1",
                "AppInitParameterInteraction",
                "Prune cannot be configured with a negative value",
            ),
            ("-statshost=tcp://127.0.0.1", "BasicTestingSetup", "Unsupported URL scheme"),
            ("-minimumchainwork=xyz", "ChainTestingSetup", "minimum chain work value"),
        ] {
            let options = [NON_DEFAULT_OPTIONS, &[option]].concat();
            let thrown =
                oracle.peer_session_new(&config(&options, Vec::new())).map(|s| s.is_some());
            assert!(
                matches!(thrown, Err(ref e) if e.kind == ErrorKind::Exception && e.message.contains(message)),
                "{pin}: {stage}: {thrown:?}"
            );
            check(&format!("a start that threw in {stage}"));
        }
        // Logging options init refuses (Sol r4 R4-01), each with Core's whole reason, and a
        // session with valid options runs as the first did after each.
        for (option, reason) in INVALID_LOGGING_OPTIONS {
            let options = [NON_DEFAULT_OPTIONS, &[option]].concat();
            let refused =
                oracle.peer_session_new(&config(&options, Vec::new())).map(|s| s.is_some());
            assert!(
                matches!(refused, Err(ref e) if e.kind == ErrorKind::Exception && e.message == reason),
                "{pin}: {option}: {refused:?}"
            );
            check(&format!("a start init refused for {option}"));
            assert!(plain_session(&oracle) == first, "{pin}: a session after {option}");
        }
        // Later sessions start as the first ones did, so option-dependent state left by the busy
        // session (filter types, local services, init statics) shows here too.
        assert!(plain_session(&oracle) == first, "{pin}: the plain session's transcript changed");
        assert!(busy_session(&oracle) == busy, "{pin}: the busy session's transcript changed");
    }
}

/// An `LC_ALL` no locale exists for: a session's SetupEnvironment() replaces it with C.UTF-8.
#[cfg(target_os = "linux")]
const INVALID_LOCALE: &str = "xx_XX.o1c-invalid";

/// The calling thread's OS name, umask and `LC_ALL`, which a session's node changes (Sol r4
/// R4-02): free puts the caller's back. Core's internal thread name (util::ThreadRename) has no
/// accessor in oracle.h; only the log lines a session hashes show it.
#[cfg(target_os = "linux")]
#[test]
fn sessions_restore_umask_locale_and_thread_name() {
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sessions_restore_umask_locale_and_thread_name_child"])
        .args(["--nocapture", "--test-threads=1"])
        .env("O1C_PROCESS_GLOBALS_CHILD", "1")
        .env("LC_ALL", INVALID_LOCALE)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&child.stdout);
    assert!(child.status.success(), "{stdout}{}", String::from_utf8_lossy(&child.stderr));
    assert!(stdout.contains("1 passed"), "{stdout}");
}

#[cfg(target_os = "linux")]
#[test]
fn sessions_restore_umask_locale_and_thread_name_child() {
    if std::env::var_os("O1C_PROCESS_GLOBALS_CHILD").is_none() {
        return;
    }
    unsafe extern "C" {
        fn umask(mask: u32) -> u32;
    }
    let observe = || {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let mask = status.lines().find_map(|l| l.strip_prefix("Umask:")).unwrap().trim();
        let comm = std::fs::read_to_string("/proc/thread-self/comm").unwrap();
        (
            u32::from_str_radix(mask, 8).unwrap(),
            std::env::var("LC_ALL").ok(),
            comm.trim_end().to_owned(),
        )
    };
    unsafe { umask(0o027) };
    std::fs::write("/proc/thread-self/comm", "o1c-caller").unwrap();
    let before = observe();
    assert_eq!(before, (0o027, Some(INVALID_LOCALE.to_owned()), "o1c-caller".to_owned()));
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let session =
            oracle.peer_session_new(&config(&[], Vec::new())).unwrap().expect("a session");
        let during = (0o077, Some("C.UTF-8".to_owned()), "d-test".to_owned());
        assert_eq!(observe(), during, "{pin}: while the session lives");
        drop(session);
        assert_eq!(observe(), before, "{pin}: after the session");
    }
}

fn config(options: &[&str], blocks: Vec<Vec<u8>>) -> SessionConfig {
    SessionConfig {
        chain: Chain::new("regtest", options),
        our_services: 1 | 4,
        mocktime: T,
        rng_seed: 5,
        blocks,
        min_chain_work: None,
        max_tip_age: None,
        check_block_index: true,
    }
}

/// A session with default options: a handshake and ten unconnecting headers (the peer
/// misbehaves), every outcome raw.
fn plain_session(oracle: &Oracle) -> Vec<Vec<u8>> {
    peer_script(oracle, &config(&[], Vec::new()))
}

/// A session with [`NON_DEFAULT_OPTIONS`] on sixteen fixture blocks running [`peer_script`].
fn busy_session(oracle: &Oracle) -> Vec<Vec<u8>> {
    let blocks = mine(rpc_hash(REGTEST_GENESIS), 1, 16);
    peer_script(oracle, &config(NON_DEFAULT_OPTIONS, blocks))
}

fn peer_script(oracle: &Oracle, config: &SessionConfig) -> Vec<Vec<u8>> {
    let mut s = oracle.peer_session_new(config).unwrap().expect("a session");
    let peer = s
        .add_peer(ConnType::OutboundFullRelay, &Service::new(NetAddr::ipv4([1, 2, 3, 4]), 19899))
        .unwrap();
    let mut out = vec![s.outcome_raw(peer).unwrap(), s.outcome_raw(-1).unwrap()];
    s.deliver(peer, "version", &version_payload(), &[]).unwrap();
    s.deliver(peer, "verack", &[], &[]).unwrap();
    for _ in 0..4 {
        s.send_pass(T).unwrap();
        out.extend([s.outcome_raw(peer).unwrap(), s.outcome_raw(-1).unwrap()]);
    }
    let unconnecting = mine([0x77; 32], 1, 1);
    let headers = [&[1][..], &unconnecting[0][..80], &[0]].concat();
    for _ in 0..10 {
        s.deliver(peer, "headers", &headers, &[]).unwrap();
        s.send_pass(T).unwrap();
        out.extend([s.outcome_raw(peer).unwrap(), s.outcome_raw(-1).unwrap()]);
    }
    out
}

/// A `version` payload from an outbound peer at `PROTOCOL_VERSION` with NODE_NETWORK | NODE_BLOOM.
fn version_payload() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend(70242i32.to_le_bytes());
    p.extend(5u64.to_le_bytes());
    p.extend(T.to_le_bytes());
    for _ in 0..2 {
        p.extend([0u8; 18]);
        p.extend([0xff, 0xff, 127, 0, 0, 1]);
        p.extend(19899u16.to_be_bytes());
    }
    p.extend(7u64.to_le_bytes());
    p.push(5);
    p.extend(b"/o1c/");
    p.extend(0i32.to_le_bytes());
    p.push(1);
    p
}

/// `n` regtest blocks on `prev` (a coinbase paying nothing to OP_TRUE each).
fn mine(mut prev: [u8; 32], first_height: u8, n: u8) -> Vec<Vec<u8>> {
    let mut blocks = Vec::new();
    for height in first_height..first_height + n {
        let mut tx = vec![1, 0, 0, 0, 1];
        tx.extend([0; 32]);
        tx.extend([0xff; 4]);
        tx.extend([2, 0x50 + height, 0x00]);
        tx.extend([0xff; 4]);
        tx.push(1);
        tx.extend(0u64.to_le_bytes());
        tx.extend([1, 0x51]);
        tx.extend([0; 4]);
        let mut header = Vec::new();
        header.extend(0x2000_0000i32.to_le_bytes());
        header.extend(prev);
        header.extend(sha256d::Hash::hash(&tx).to_byte_array());
        header.extend((GENESIS_TIME as u32 + 150 * u32::from(height)).to_le_bytes());
        header.extend(0x207f_ffffu32.to_le_bytes());
        prev = (0u32..)
            .find_map(|nonce| {
                let h = [&header[..], &nonce.to_le_bytes()].concat();
                let hash = deserialize::<dashcore::block::Header>(&h)
                    .unwrap()
                    .block_hash()
                    .to_byte_array();
                (hash[31] < 0x7f).then(|| {
                    header = h;
                    hash
                })
            })
            .unwrap();
        blocks.push([&header[..], &[1], &tx].concat());
    }
    blocks
}

/// Every entry point but the sessions' on fixed inputs, each output (or error) as text. The
/// calls that select no chain come first, so they run on the state the previous round left.
fn sweep(oracle: &Oracle) -> Vec<String> {
    let mut out = Vec::new();
    let mut put =
        |name: &str, result: &dyn std::fmt::Debug| out.push(format!("{name}: {result:?}"));

    let fixture: Value = common::load_data("contextual-mainnet.json");
    let sample = &fixture["dip3_block"];
    let block = hex(sample["block"].as_str().unwrap());
    let header: [u8; 80] = hex(sample["header"].as_str().unwrap()).try_into().unwrap();
    let tx = hex(sample["normal_tx"]["hex"].as_str().unwrap());
    let spent_coins = coins(&sample["normal_tx"]["coins"]);
    let spent: Vec<SpentOutput> = spent_coins
        .iter()
        .map(|c| SpentOutput {
            amount: c.amount,
            script_pubkey: &c.script_pubkey,
        })
        .collect();
    let wrong_script = [0x51u8];
    let wrong: Vec<SpentOutput> = spent
        .iter()
        .map(|s| SpentOutput {
            amount: s.amount,
            script_pubkey: &wrong_script,
        })
        .collect();
    let special: Vec<Vec<u8>> = sample["special_txs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| hex(t["hex"].as_str().unwrap()))
        .collect();
    let script_sig = script_sig_of(&tx);
    let sig = script_sig[1..1 + script_sig[0] as usize].to_vec();
    let pubkey = {
        let rest = &script_sig[1 + script_sig[0] as usize..];
        rest[1..1 + rest[0] as usize].to_vec()
    };
    let snapshots: Vec<Value> = common::transcribed("evo_snapshot_tests.json");
    let snapshot = |kind: &str, case: &str, field: &str| -> Vec<u8> {
        let r = snapshots.iter().find(|r| r["kind"] == kind && r["case"] == case).unwrap();
        hex(r[field].as_str().unwrap())
    };
    let base_list = snapshot("list", "diff_chain_base", "canonical");
    let dmns = test_mns(&base_list);

    // No chain.
    put("abi_version", &oracle.abi_version());
    put("core_version", &oracle.core_version());
    put("script_error_string", &oracle.script_error_string(2));
    put("tx_decode", &oracle.tx_decode(&tx));
    put("block_decode", &oracle.block_decode(&block));
    put("header_decode", &oracle.header_decode(&header));
    put("merkle_root", &oracle.merkle_root(&[[1; 32], [2; 32], [3; 32]]));
    for (op, input) in [
        (CoinCodec::CompactSizeDecode, vec![0xfd, 0x00, 0x01]),
        (CoinCodec::VarIntEncode, 300u64.to_le_bytes().to_vec()),
        (CoinCodec::AmountCompress, 50_000_000u64.to_le_bytes().to_vec()),
        (CoinCodec::ScriptCompress, spent_coins[0].script_pubkey.clone()),
    ] {
        put("coin_codec", &oracle.coin_codec(op, &input));
    }
    put("check_transaction", &oracle.check_transaction(&tx));
    put("legacy_sigops", &oracle.legacy_sigops(&tx));
    let spk = &spent_coins[0].script_pubkey;
    put("verify_script", &oracle.verify_script(spk, &tx, 0, spent_coins[0].amount, FLAGS));
    put("signature_hash", &oracle.signature_hash(spk, &tx, 0, 1));
    put("compact_decode", &oracle.compact_decode(0x1b04_04cb));
    put("compact_encode", &oracle.compact_encode(&[0x11; 32]));
    put("block_proof", &oracle.block_proof(0x1b04_04cb));
    put("convert_bits_to_double", &oracle.convert_bits_to_double(0x1b04_04cb));
    put("platform_share", &oracle.platform_share(1_000_000));
    put("split_amount_by_shares", &oracle.split_amount_by_shares(100, &[1, 2, 3]));
    put("eval_script", &oracle.eval_script(&[0x51, 0x52, 0x93], &[], FLAGS));
    let msg = [7; 32];
    put("pubkey_verify", &oracle.pubkey_verify(&pubkey, &msg, &sig[..sig.len() - 1]));
    put("check_low_s", &oracle.check_low_s(&sig[..sig.len() - 1]));
    put("parse_der_lax", &oracle.parse_der_lax(&sig[..sig.len() - 1]));
    put("check_signature_encoding", &oracle.check_signature_encoding(&sig, FLAGS));
    put("check_minimal_push", &oracle.check_minimal_push(&[1], 1));
    put("scriptnum_decode", &oracle.scriptnum_decode(&[0x80, 0x00], true, 4));
    put("scriptnum_encode", &oracle.scriptnum_encode(-1000));
    put("scriptnum_is_minimally_encoded", &oracle.scriptnum_is_minimally_encoded(&[1, 0], 4));
    put("scriptnum_minimally_encode", &oracle.scriptnum_minimally_encode(&[1, 0, 0]));
    put("find_and_delete", &oracle.find_and_delete(&[0x51, 0x01, 0x51, 0x51], &[0x51]));
    put("script_sigops", &oracle.script_sigops(spk, true));
    put("script_sigops_with_sig", &oracle.script_sigops_with_sig(spk, &script_sig));
    put("transaction_sigops", &oracle.transaction_sigops(&tx, &spent, FLAGS));
    put("check_input_scripts", &oracle.check_input_scripts(&tx, &spent, FLAGS));
    put("check_input_scripts wrong coins", &oracle.check_input_scripts(&tx, &wrong, FLAGS));
    put("check_tx_inputs", &oracle.check_tx_inputs(&tx, &spent_coins, 1_028_200));
    let g1 = hex(
        "97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb",
    );
    for legacy in [true, false] {
        put("bls_decode", &oracle.bls_decode(BlsKind::PublicKey, BlsMode::Eager, legacy, &g1));
        put(
            "bls_decode lazy",
            &oracle.bls_decode(BlsKind::Signature, BlsMode::Lazy, legacy, &[0; 96]),
        );
        put(
            "bls_verify",
            &oracle.bls_verify(legacy, &g1.clone().try_into().unwrap(), &[0; 96], &msg),
        );
    }
    put("payload_decode", &oracle.payload_decode(1, true, &extra_payload(&special[0])));
    let stream = P2pStream {
        version: 70242,
        bls_legacy: false,
        qdata_max_vvec: 0,
        qdata_max_contrib: 0,
    };
    put("p2p_decode", &oracle.p2p_decode("qgetdata", &[0; 67], &stream));
    put("unique_property_hash", &oracle.unique_property_hash(UniqueProperty::Collateral, &[3; 36]));
    let (old, new) = (
        snapshot("state_diff", "evo_extaddr_address_change", "old"),
        snapshot("state_diff", "evo_extaddr_address_change", "new"),
    );
    put("state_diff", &oracle.state_diff(&old, &new));
    put("sml_entry_hash", &oracle.sml_entry_hash(&dmns[0]));
    put("addrman_chance", &oracle.addrman_chance(3600, 2));
    put("netgroup", &oracle.netgroup(&NetAddr::ipv4([10, 1, 2, 3]), &[]));
    let addr = Service::new(NetAddr::ipv4([10, 1, 2, 3]), 9999);
    put(
        "addrman_bucket",
        &oracle.addrman_bucket(&[9; 32], &addr, &NetAddr::ipv4([5, 6, 7, 8]), &[]),
    );

    // On mainnet.
    let main = Chain::main();
    let first_height = sample["first_height"].as_i64().unwrap() as i32;
    let ctx_headers = headers(sample["headers"].as_str().unwrap());
    let ctx = Context::new(
        first_height,
        ctx_headers.clone(),
        true,
        vec![(DEPLOYMENT_V24, ThresholdState::Defined)],
        sample["adjusted_time"].as_i64().unwrap() + 60,
        Checkpoints::Enabled {
            best_header_height: first_height + ctx_headers.len() as i32 - 1,
        },
        CoinsView::AtTip(coins(&sample["coins"])),
        MnList::Supplied(hex(sample["prev_list"].as_str().unwrap())),
        vec![],
    );
    let tip = first_height + ctx_headers.len() as i32 - 1;
    put("chainparams_json", &oracle.chainparams_json(&main));
    put("check_block", &oracle.check_block(&main, &block, false, true));
    put("check_block pow", &oracle.check_block(&main, &block, true, true));
    let header_bytes: Vec<u8> = ctx_headers.concat();
    put(
        "next_work_required",
        &oracle.next_work_required(&main, &header_bytes, first_height, &header),
    );
    put("check_proof_of_work", &oracle.check_proof_of_work(&main, &[0; 32], 0x1b04_04cb));
    put("block_subsidy_inner", &oracle.block_subsidy_inner(&main, 0x1b04_04cb, tip, false));
    put(
        "superblock_subsidy_inner",
        &oracle.superblock_subsidy_inner(&main, 0x1b04_04cb, tip, false),
    );
    put("mn_payment", &oracle.mn_payment(&main, tip + 1, 500_000_000, RewardEra::Classic));
    put("superblock_payments_limit", &oracle.superblock_payments_limit(&main, 1_036_800));
    for raw in &special {
        put("check_special_tx", &oracle.check_special_tx(&main, raw, &ctx, true));
        put("special_tx_sign_hash", &oracle.special_tx_sign_hash(&main, raw));
    }
    put("contextual_check_header", &oracle.contextual_check_header(&main, &header, &ctx));
    put("contextual_check_block", &oracle.contextual_check_block(&main, &block, &ctx));
    put("contextual_check_tx", &oracle.contextual_check_tx(&main, &tx, &ctx));
    let heights = vec![tip + 1; spent_coins.len()];
    put("sequence_locks", &oracle.sequence_locks(&main, &tx, &ctx, &heights, 1));
    let applied = oracle.dml_apply(&main, &block, &ctx);
    put("dml_apply", &applied);
    put("dml_block", &oracle.dml_block(&main, &block, &ctx, true, false));
    let prev_list = hex(sample["prev_list"].as_str().unwrap());
    let new_list = applied.unwrap().new_list.unwrap();
    put("dml_listdiff_json", &oracle.dml_listdiff_json(&main, &prev_list, &new_list));
    put("mnlist_canonical", &oracle.mnlist_canonical(&main, ListEncoding::Disk, &new_list));
    let diff = oracle.mnlist_diff(&main, ListEncoding::Disk, &prev_list, &new_list);
    put("mnlist_diff", &diff);
    let diff = diff.unwrap();
    put(
        "mnlist_apply_diff",
        &oracle.mnlist_apply_diff(
            &main,
            ListEncoding::Disk,
            &prev_list,
            ListEncoding::Disk,
            &diff.disk,
            &oracle.block_decode(&block).unwrap().hash,
            tip + 1,
        ),
    );
    put("mnlist_payee", &oracle.mnlist_payee(&main, ListEncoding::Disk, &new_list, tip + 1));
    put(
        "mnlist_projected",
        &oracle.mnlist_projected(&main, ListEncoding::Disk, &new_list, tip + 1, 3),
    );
    put("sml_merkle_root", &oracle.sml_merkle_root(&main, ListEncoding::Disk, &new_list));
    put(
        "mnlist_canonical base",
        &oracle.mnlist_canonical(&main, ListEncoding::Canonical, &base_list),
    );
    let ops: Vec<ListOp> = dmns
        .iter()
        .map(|dmn| ListOp::Add {
            dmn: dmn.clone(),
            bump_total_count: true,
        })
        .collect();
    put("mnlist_core_order", &oracle.mnlist_core_order(&main, &ops));
    // A debug-level LLMQ reason Core logs (Sol r2 F1).
    let commitment = &fixture["commitment"];
    let qc = hex(commitment["commitment"].as_str().unwrap());
    let members: Vec<Vec<u8>> = commitment["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| hex(m.as_str().unwrap()))
        .collect();
    let qc_first = commitment["first_height"].as_i64().unwrap() as i32;
    let qc_headers = headers(commitment["headers"].as_str().unwrap());
    for n in [members.len(), 1] {
        let qc_ctx = Context::new(
            qc_first,
            qc_headers.clone(),
            true,
            vec![(DEPLOYMENT_V24, ThresholdState::Defined)],
            0,
            Checkpoints::Enabled {
                best_header_height: qc_first + qc_headers.len() as i32 - 1,
            },
            CoinsView::Unchecked(vec![]),
            MnList::Absent,
            vec![QuorumMembers {
                llmq_type: commitment["llmq_type"].as_u64().unwrap() as u8,
                quorum_hash: hex(commitment["quorum_hash"].as_str().unwrap()).try_into().unwrap(),
                members: members[..n].to_vec(),
            }],
        );
        put("verify_commitment", &oracle.verify_commitment(&main, &qc, &qc_ctx, true));
    }

    // On regtest: an addrman.
    let config = AddrManConfig::default();
    match oracle.addrman_new(&config) {
        Ok(Some(mut addrman)) => {
            let add = AddrOp::Add {
                addrs: vec![Address {
                    time: T as u32,
                    services: 1,
                    service: addr,
                }],
                source: NetAddr::ipv4([5, 6, 7, 8]),
                time_penalty: 0,
            };
            put("addrman op", &addrman.op_raw(&add.encode(), T));
            put("addrman canonical", &addrman.canonical_raw());
        }
        other => put("addrman_new", &other.map(|a| a.is_some())),
    }
    out
}

/// The scriptSig of `tx`'s first input (a version-1 transaction).
fn script_sig_of(tx: &[u8]) -> Vec<u8> {
    let start = 4 + 1 + 36;
    let len = tx[start] as usize;
    tx[start + 1..start + 1 + len].to_vec()
}

/// The masternodes of one of Core's test lists (each proTxHash is 8 bytes 0xa5, a suffix and
/// zeros), as `CDeterministicMN` bytes (tests/mnlist.rs `test_mns`).
fn test_mns(canonical: &[u8]) -> Vec<Vec<u8>> {
    let starts: Vec<usize> = (41..canonical.len().saturating_sub(31))
        .filter(|&i| canonical[i..i + 8] == [0xa5; 8] && canonical[i + 9..i + 32] == [0; 23])
        .collect();
    assert_eq!(starts.len(), canonical[40] as usize, "one proTxHash per masternode");
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            canonical[start..starts.get(n + 1).copied().unwrap_or(canonical.len())].to_vec()
        })
        .collect()
}
