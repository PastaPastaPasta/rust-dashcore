//! Differential test for Core's `test/functional/p2p_permissions.py` against dashd v24.0.2.
//!
//! Each case runs identically on dashd v24.0.2 and rdashd on loopback (`127.0.0.1`) with
//! `-connect=0`, and asserts equal observed results:
//! - `checkpermission` rows (`p2p_permissions.py:36-95`), including `-whitebind`, legacy
//!   `-whitelistrelay`/`-whitelistforcerelay` interactions, `all@`, and the `out` direction loop;
//! - onion inbound isolation (`p2p_permissions.py:97-103`);
//! - the five init errors (`p2p_permissions.py:105-109`);
//! - `check_tx_relay` (`p2p_permissions.py:111-154`) on a clean regtest chain mined via RPC.
//!
//! Cannot be skipped (`common::net::Binary::dashd` -> `common::require_dashd`).
#![cfg(unix)]

mod common;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use common::mine::parse_hex;
use common::net::{
    ADDRESS_BCRT1_P2SH_OP_TRUE, Binary, Json, Kind, Node, TIME_GENESIS_BLOCK, as_array, as_i64,
    as_str, connect, field, json_str, scaled, wait_for,
};
use common::peer::{Peer, hash_hex};
use common::{RetainedHome, free_port, output, private_dir, regtest_on, stderr};
use dash_consensus::primitives::decode_tx;
use dash_primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_types::codec::BaseCodec as _;

/// Every case this test compares, in run order.
const CASES: [&str; 20] = [
    "whitelist_default",
    "whitelist_empty_with_forcerelay",
    "whitelist_no_relay",
    "whitelist_forcerelay_legacy",
    "whitebind_merge_with_whitelist",
    "whitelist_explicit_ignores_whitelistrelay",
    "whitelist_explicit_ignores_whitelistforcerelay",
    "whitelist_noban_only",
    "whitelist_all",
    "whitelist_out_applies_to_manual",
    "whitelist_in_does_not_apply_to_out",
    "onion_inbound_no_whitelist",
    "init_err_only_direction",
    "init_err_invalid_permission",
    "init_err_invalid_netmask",
    "init_err_whitebind_resolve",
    "init_err_whitebind_listen_zero",
    "tx_relay_forcerelay",
    "tx_relay_conflict_first",
    "tx_relay_conflict_second",
];

/// Cases that currently differ from dashd v24.0.2. Each listed case is asserted to still differ so
/// the list can only shrink as gaps are fixed.
const KNOWN_GAPS: &[&str] = &[];

#[test]
fn known_gaps_name_cases() {
    for gap in KNOWN_GAPS {
        assert!(CASES.contains(gap), "KNOWN_GAPS entry {gap:?} names no case");
    }
}

/// How a node binds its listening sockets.
#[derive(Clone, Copy, Debug)]
enum BindMode {
    /// Plain `-bind=127.0.0.1:<p2p_port>`.
    Plain,
    /// Plain `-bind=127.0.0.1:<p2p_port>` plus `-bind=127.0.0.1:<onion_port>=onion`.
    PlainWithOnion,
    /// `-whitebind=<perms>@127.0.0.1:<p2p_port>` plus `-bind=127.0.0.1:<onion_port>=onion` (no
    /// plain `-bind`, as in `p2p_permissions.py:62-69`).
    WhitebindWithOnion(&'static str),
}

/// Node `index` of `binary` under `home`, without the framework's `-blocksonly=1`.
fn perm_node(binary: &Binary, home: &RetainedHome, index: usize) -> Node {
    let mut node = Node::new(binary, home, index, &[]);
    node.blocksonly = false;
    node
}

/// Starts `node` with `bind`, `-mocktime` when given, and `extra` for this start only.
fn start(node: &mut Node, bind: BindMode, mocktime: Option<u64>, extra: &[&str]) {
    let onion = matches!(bind, BindMode::PlainWithOnion | BindMode::WhitebindWithOnion(_));
    node.onion_port = if onion {
        Some(node.onion_port.unwrap_or_else(free_port))
    } else {
        None
    };
    node.whitebind = match bind {
        BindMode::WhitebindWithOnion(perms) => Some(perms.to_owned()),
        BindMode::Plain | BindMode::PlainWithOnion => None,
    };
    let extra: Vec<String> = extra.iter().map(|s| (*s).to_owned()).collect();
    node.start_with(mocktime, &extra);
}

fn raw_mempool(node: &Node) -> Vec<String> {
    as_array(&node.call("getrawmempool", "[]")).iter().map(|v| as_str(v).to_owned()).collect()
}

fn in_mempool(node: &Node, txid: &str) -> bool {
    raw_mempool(node).iter().any(|t| t == txid)
}

fn set_mocktime(nodes: &[&Node], t: u64) {
    for node in nodes {
        assert!(node.call("setmocktime", &format!("[{t}]")).is_null());
    }
}

/// Polls `done` until it holds or [`scaled`] `base_secs` pass; whether it held.
fn holds_within(base_secs: u64, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + scaled(base_secs);
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A peer's getpeerinfo `permissions`, in the node's order (Core `NetPermissions::ToStrings`,
/// `src/net_permissions.cpp:90-101`), with no duplicates.
fn peer_permissions(peer: &Json) -> Vec<String> {
    let perms: Vec<String> =
        as_array(field(peer, "permissions")).iter().map(|v| as_str(v).to_owned()).collect();
    let set: BTreeSet<&String> = perms.iter().collect();
    assert_eq!(set.len(), perms.len(), "duplicate permissions in getpeerinfo: {perms:?}");
    perms
}

fn perm_set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

fn perm_vec(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

fn record_case<T: PartialEq + std::fmt::Debug>(
    case: &str,
    src_line: &str,
    core: &T,
    ours: &T,
    failures: &mut Vec<String>,
) {
    assert!(CASES.contains(&case), "{case} is missing from CASES");
    let equal = ours == core;
    let in_gaps = KNOWN_GAPS.contains(&case);
    eprintln!(
        "CASE {case} ({src_line}): {} | dashd={core:?} | rdashd={ours:?}",
        if equal {
            "MATCH"
        } else {
            "DIFFER"
        }
    );
    match (equal, in_gaps) {
        (true, false) | (false, true) => {}
        (true, true) => failures.push(format!(
            "{case} ({src_line}): listed in KNOWN_GAPS but now matches dashd ({core:?}) — remove it"
        )),
        (false, false) => failures
            .push(format!("{case} ({src_line}): DIFFER\n  dashd:  {core:?}\n  rdashd: {ours:?}")),
    }
}

/// Runs the 11 `checkpermission` rows (`p2p_permissions.py:36-95`) on one binary and returns
/// `(case_name, p2p_permissions.py:line, observed_permissions)` in order.
// Core: test/functional/p2p_permissions.py:36-95
fn run_checkpermission_rows(binary: &Binary) -> Vec<(&'static str, &'static str, Vec<String>)> {
    let prefix = match binary.kind {
        Kind::Dashd => "p2p-perm-dashd-",
        Kind::Rdashd => "p2p-perm-rdashd-",
    };
    let home = RetainedHome::new(prefix);
    let mut node0 = perm_node(binary, &home, 0);
    let mut node1 = perm_node(binary, &home, 1);

    let inbound_cases: [(&str, &str, BindMode, &[&str]); 9] = [
        (
            "whitelist_default",
            "p2p_permissions.py:36-40",
            BindMode::Plain,
            &["-whitelist=127.0.0.1"],
        ),
        (
            "whitelist_empty_with_forcerelay",
            "p2p_permissions.py:42-45",
            BindMode::Plain,
            &["-whitelist=@127.0.0.1", "-whitelistforcerelay=1"],
        ),
        (
            "whitelist_no_relay",
            "p2p_permissions.py:47-50",
            BindMode::Plain,
            &["-whitelist=127.0.0.1", "-whitelistrelay=0"],
        ),
        (
            "whitelist_forcerelay_legacy",
            "p2p_permissions.py:52-57",
            BindMode::Plain,
            &["-whitelist=127.0.0.1", "-whitelistforcerelay"],
        ),
        (
            "whitebind_merge_with_whitelist",
            "p2p_permissions.py:60-69",
            BindMode::WhitebindWithOnion("bloomfilter,forcerelay"),
            &["-whitelist=noban@127.0.0.1"],
        ),
        (
            "whitelist_explicit_ignores_whitelistrelay",
            "p2p_permissions.py:71-74",
            BindMode::Plain,
            &["-whitelist=noban,mempool@127.0.0.1", "-whitelistrelay"],
        ),
        (
            "whitelist_explicit_ignores_whitelistforcerelay",
            "p2p_permissions.py:76-79",
            BindMode::Plain,
            &["-whitelist=noban,mempool@127.0.0.1", "-whitelistforcerelay"],
        ),
        (
            "whitelist_noban_only",
            "p2p_permissions.py:81-84",
            BindMode::Plain,
            &["-whitelist=noban@127.0.0.1"],
        ),
        (
            "whitelist_all",
            "p2p_permissions.py:86-89",
            BindMode::Plain,
            &["-whitelist=all@127.0.0.1"],
        ),
    ];

    let mut results = Vec::new();
    start(&mut node0, BindMode::Plain, None, &[]);
    for (name, line, bind, extra) in inbound_cases {
        start(&mut node1, bind, None, extra);
        connect(&node0, &node1);
        let peers = node1.peers();
        assert_eq!(peers.len(), 1, "{:?} {name}: expected 1 peer on node 1", binary.kind);
        results.push((name, line, peer_permissions(&peers[0])));
        node1.stop();
        wait_for(&format!("{:?} node0 drops node1 after {name}", binary.kind), 60, || {
            node0.peers().is_empty()
        });
    }
    node0.stop();

    // Outgoing whitelist direction loop (p2p_permissions.py:91-95).
    let outbound_cases: [(&str, &str, &[&str]); 2] = [
        (
            "whitelist_out_applies_to_manual",
            "p2p_permissions.py:91-95(out)",
            &["-whitelist=noban,out@127.0.0.1"],
        ),
        (
            "whitelist_in_does_not_apply_to_out",
            "p2p_permissions.py:91-95(in)",
            &["-whitelist=noban@127.0.0.1"],
        ),
    ];
    start(&mut node1, BindMode::Plain, None, &[]);
    for (name, line, extra) in outbound_cases {
        start(&mut node0, BindMode::Plain, None, extra);
        connect(&node0, &node1);
        let peers = node0.peers();
        assert_eq!(peers.len(), 1, "{:?} {name}: expected 1 peer on node 0", binary.kind);
        results.push((name, line, peer_permissions(&peers[0])));
        node0.stop();
        wait_for(&format!("{:?} node1 drops node0 after {name}", binary.kind), 60, || {
            node1.peers().is_empty()
        });
    }
    node1.stop();
    results
}

#[test]
fn checkpermission_rows_match_dashd_v24() {
    let dashd = Binary::dashd();
    let rdashd = Binary::rdashd();
    let core = run_checkpermission_rows(&dashd);
    let ours = run_checkpermission_rows(&rdashd);

    // `true` where the FT compares the list in order (p2p_permissions.py:95); elsewhere
    // `checkpermission` compares length and membership (p2p_permissions.py:156-163).
    let expected: [(&str, &[&str], bool); 11] = [
        ("whitelist_default", &["relay", "noban", "mempool", "download"], false),
        ("whitelist_empty_with_forcerelay", &[], false),
        ("whitelist_no_relay", &["noban", "mempool", "download"], false),
        (
            "whitelist_forcerelay_legacy",
            &["forcerelay", "relay", "noban", "mempool", "download"],
            false,
        ),
        (
            "whitebind_merge_with_whitelist",
            &["noban", "bloomfilter", "forcerelay", "relay", "download"],
            false,
        ),
        ("whitelist_explicit_ignores_whitelistrelay", &["noban", "mempool", "download"], false),
        (
            "whitelist_explicit_ignores_whitelistforcerelay",
            &["noban", "mempool", "download"],
            false,
        ),
        ("whitelist_noban_only", &["noban", "download"], false),
        (
            "whitelist_all",
            &["forcerelay", "noban", "mempool", "bloomfilter", "relay", "download", "addr"],
            false,
        ),
        ("whitelist_out_applies_to_manual", &["noban", "download"], true),
        ("whitelist_in_does_not_apply_to_out", &[], true),
    ];

    let mut failures = Vec::new();
    for (((name, line, core_perms), (_, _, our_perms)), (exp_name, exp_perms, ordered)) in
        core.iter().zip(&ours).zip(&expected)
    {
        assert_eq!(name, exp_name);
        if *ordered {
            assert_eq!(
                *core_perms,
                perm_vec(exp_perms),
                "dashd v24 oracle deviated from p2p_permissions.py on {name}"
            );
        } else {
            assert_eq!(
                core_perms.iter().cloned().collect::<BTreeSet<_>>(),
                perm_set(exp_perms),
                "dashd v24 oracle deviated from p2p_permissions.py on {name}"
            );
        }
        record_case(name, line, core_perms, our_perms, &mut failures);
    }
    assert!(failures.is_empty(), "checkpermission mismatches:\n{}", failures.join("\n"));
}

/// Runs the onion inbound test (`p2p_permissions.py:97-103`) on one binary and returns sorted
/// `(network, permissions)` pairs for the two inbound peers.
// Core: test/functional/p2p_permissions.py:97-103
fn run_onion_inbound(binary: &Binary) -> Vec<(String, Vec<String>)> {
    let prefix = match binary.kind {
        Kind::Dashd => "p2p-perm-onion-dashd-",
        Kind::Rdashd => "p2p-perm-onion-rdashd-",
    };
    let home = RetainedHome::new(prefix);
    let mut node1 = perm_node(binary, &home, 1);
    start(&mut node1, BindMode::PlainWithOnion, None, &["-whitelist=noban@127.0.0.1"]);
    let onion_port = node1.onion_port.expect("onion port");

    let _clear = Peer::dial(node1.p2p_port);
    let _onion = Peer::dial(onion_port);
    wait_for(&format!("{:?} node1 lists both inbound peers", binary.kind), 60, || {
        node1.peers().len() == 2
    });
    let mut per_peer: Vec<(String, Vec<String>)> = node1
        .peers()
        .iter()
        .map(|p| (as_str(field(p, "network")).to_owned(), peer_permissions(p)))
        .collect();
    per_peer.sort();
    node1.stop();
    per_peer
}

#[test]
fn onion_inbound_no_whitelist_matches_dashd_v24() {
    let core = run_onion_inbound(&Binary::dashd());
    let ours = run_onion_inbound(&Binary::rdashd());

    let expected = vec![
        ("not_publicly_routable".to_owned(), perm_vec(&["noban", "download"])),
        ("onion".to_owned(), perm_vec(&[])),
    ];
    assert_eq!(core, expected, "dashd v24 oracle deviated from p2p_permissions.py:97-103");

    let mut failures = Vec::new();
    record_case(
        "onion_inbound_no_whitelist",
        "p2p_permissions.py:97-103",
        &core,
        &ours,
        &mut failures,
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[derive(Debug, PartialEq, Eq)]
struct InitErrorObs {
    code: Option<i32>,
    stderr: String,
}

fn run_init_error(binary: &Binary, extra: &[&str], with_plain_bind: bool) -> InitErrorObs {
    let home = tempfile::tempdir().unwrap();
    let datadir = home.path().join("dd");
    private_dir(&datadir);
    let mut args: Vec<String> = vec![
        "-connect=0".into(),
        "-discover=0".into(),
        "-dnsseed=0".into(),
        "-fixedseeds=0".into(),
        "-listenonion=0".into(),
        "-upnp=0".into(),
        "-natpmp=0".into(),
        "-disablewallet".into(),
        "-printtoconsole=0".into(),
        format!("-rpcport={}", free_port()),
    ];
    if with_plain_bind {
        args.push(format!("-bind=127.0.0.1:{}", free_port()));
    }
    args.extend(extra.iter().map(|s| (*s).to_owned()));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = output(regtest_on(&binary.path, home.path(), &datadir, &refs));
    InitErrorObs {
        code: out.status.code(),
        stderr: stderr(&out),
    }
}

#[test]
fn init_errors_match_dashd_v24() {
    let dashd = Binary::dashd();
    let rdashd = Binary::rdashd();
    // Core: test/functional/p2p_permissions.py:105-109
    let cases: [(&str, &str, &[&str], bool, &str); 5] = [
        (
            "init_err_only_direction",
            "p2p_permissions.py:105",
            &["-whitelist=in,out@127.0.0.1"],
            true,
            "Only direction was set, no permissions",
        ),
        (
            "init_err_invalid_permission",
            "p2p_permissions.py:106",
            &["-whitelist=oopsie@127.0.0.1"],
            true,
            "Invalid P2P permission",
        ),
        (
            "init_err_invalid_netmask",
            "p2p_permissions.py:107",
            &["-whitelist=noban@127.0.0.1:230"],
            true,
            "Invalid netmask specified in",
        ),
        (
            "init_err_whitebind_resolve",
            "p2p_permissions.py:108",
            &["-whitebind=noban@127.0.0.1/10"],
            true,
            "Cannot resolve -whitebind address",
        ),
        (
            "init_err_whitebind_listen_zero",
            "p2p_permissions.py:109",
            &["-whitebind=noban@127.0.0.1", "-bind=127.0.0.1", "-listen=0"],
            false,
            "Cannot set -bind or -whitebind together with -listen=0",
        ),
    ];

    let mut failures = Vec::new();
    for (name, line, args, with_plain_bind, pattern) in cases {
        let core = run_init_error(&dashd, args, with_plain_bind);
        assert_ne!(core.code, Some(0), "dashd did not fail init on {name}");
        assert!(
            core.stderr.contains(pattern),
            "dashd stderr for {name} lacked {pattern:?}: {:?}",
            core.stderr
        );
        let ours = run_init_error(&rdashd, args, with_plain_bind);
        record_case(name, line, &core, &ours, &mut failures);
    }
    assert!(failures.is_empty(), "init error mismatches:\n{}", failures.join("\n"));
}

/// Builds a version-2 transaction spending a P2SH(`OP_TRUE`) output with `SEQUENCE_FINAL`.
fn build_p2sh_op_true_spend(
    prev_txid: TxHash,
    vout: u32,
    value: i64,
    spk: Vec<u8>,
) -> (Vec<u8>, String) {
    let tx = Transaction::new(
        2,
        TxType::new(0),
        vec![TxIn::new(OutPoint::new(prev_txid, vout), vec![0x01, 0x51].into(), u32::MAX)],
        vec![TxOut::from_duffs(value, spk.into())],
        0,
        vec![],
    );
    let mut bytes = Vec::new();
    tx.encode(&mut bytes);
    let txid_hex = hash_hex(&common::mine::txid(&bytes));
    (bytes, txid_hex)
}

/// A log line with its leading timestamp and any `(mocktime: <n>) ` stamp stripped, and whether
/// the stamp was there (Core `src/logging.cpp:401-403`).
#[derive(Debug, PartialEq, Eq)]
struct LogLine {
    text: String,
    mocktime_stamped: bool,
}

/// Finds the first line in `log` containing `needle` and replaces `txid` with `placeholder`.
fn normalized_log_line(log: &str, needle: &str, txid: &str, placeholder: &str) -> Option<LogLine> {
    log.lines().find(|l| l.contains(needle)).map(|line| {
        let text = line.split_once(' ').map_or(line, |(_, rest)| rest);
        let stripped =
            text.strip_prefix("(mocktime: ").and_then(|rest| rest.split_once(") ")).map(|(_, r)| r);
        LogLine {
            text: stripped.unwrap_or(text).replace(txid, placeholder),
            mocktime_stamped: stripped.is_some(),
        }
    })
}

/// Polls `node`'s log window from `from` for a short grace period until `needle` appears.
fn wait_log_line(
    node: &Node,
    from: u64,
    needle: &str,
    txid: &str,
    placeholder: &str,
) -> Option<LogLine> {
    let mut found = None;
    holds_within(5, || {
        found = normalized_log_line(&node.log_since(from), needle, txid, placeholder);
        found.is_some()
    });
    found
}

#[derive(Debug, PartialEq, Eq)]
struct ForceRelayObs {
    accepted_on_node1: bool,
    present_on_node0_before_resend: bool,
    present_on_node0_after_trickle: bool,
    relayed_to_node0: bool,
    force_relay_log: Option<String>,
    force_relay_log_mocktime_stamped: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct ConflictFirstObs {
    in_node1_mempool: bool,
    in_node0_mempool: bool,
    reject_log: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct ConflictSecondObs {
    in_node1_mempool: bool,
    in_node0_mempool: bool,
    not_relaying_log: Option<String>,
}

/// Runs `check_tx_relay` (`p2p_permissions.py:111-154`) on one binary on a clean regtest chain.
// Core: test/functional/p2p_permissions.py:111-154
fn run_tx_relay(binary: &Binary) -> (ForceRelayObs, ConflictFirstObs, ConflictSecondObs) {
    let prefix = match binary.kind {
        Kind::Dashd => "p2p-perm-txrelay-dashd-",
        Kind::Rdashd => "p2p-perm-txrelay-rdashd-",
    };
    let home = RetainedHome::new(prefix);
    let mut node0 = perm_node(binary, &home, 0);
    let mut node1 = perm_node(binary, &home, 1);

    let mut mocktime = TIME_GENESIS_BLOCK + 101 * 156;
    start(&mut node0, BindMode::Plain, Some(mocktime), &[]);
    start(&mut node1, BindMode::Plain, Some(mocktime), &["-whitelist=forcerelay@127.0.0.1"]);

    // Mine 101 blocks to P2SH(OP_TRUE) on node 0 and submit them to node 1 so both nodes have
    // the same matured coinbase and are out of IBD without connecting yet.
    let hashes: Vec<String> = as_array(
        &node0
            .call("generatetoaddress", &format!("[101,{}]", json_str(ADDRESS_BCRT1_P2SH_OP_TRUE))),
    )
    .iter()
    .map(|v| as_str(v).to_owned())
    .collect();
    assert_eq!(hashes.len(), 101);
    for hash in &hashes {
        let raw = as_str(&node0.call("getblock", &format!("[{},0]", json_str(hash)))).to_owned();
        assert!(node1.call("submitblock", &format!("[{}]", json_str(&raw))).is_null());
    }
    for n in [&node0, &node1] {
        let info = n.call("getblockchaininfo", "[]");
        assert_eq!(as_i64(field(&info, "blocks")), 101);
        assert!(field(&info, "initialblockdownload").is_false());
    }

    // Decode block 1's coinbase output to build a standard self-transfer and a conflicting spend.
    let block1_hex =
        as_str(&node0.call("getblock", &format!("[{},0]", json_str(&hashes[0])))).to_owned();
    let block1_bytes = parse_hex(&block1_hex);
    assert_eq!(block1_bytes[80], 1, "block 1 has only the coinbase transaction");
    let coinbase = decode_tx(&mut &block1_bytes[81..]).expect("coinbase tx").into_value();
    let cb_txid = coinbase.txid();
    let out0 = &coinbase.outputs()[0];
    let spend_value = out0.value() - 10_000;
    let spk = out0.script_pubkey().as_bytes().to_vec();

    let (tx1_bytes, tx1_id) = build_p2sh_op_true_spend(cb_txid, 0, spend_value, spk.clone());
    let (tx2_bytes, tx2_id) = build_p2sh_op_true_spend(cb_txid, 0, spend_value + 1, spk);

    // Connect a raw P2P peer to node 1 (peer=0, whitelisted with forcerelay).
    let mut peer = Peer::dial(node1.p2p_port);
    peer.send("tx", tx1_bytes.clone());
    peer.sync_with_ping();
    let accepted_on_node1 = holds_within(60, || in_mempool(&node1, &tx1_id));

    // Connect node 1 -> node 0; node 0 should not have tx1 yet.
    connect(&node1, &node0);
    let present_on_node0_before_resend = in_mempool(&node0, &tx1_id);

    // Several trickle intervals pass with no resend: node 1 must not announce its existing
    // mempool to the new peer, so a later relay comes from the force-relay path.
    // Core: src/net_processing.cpp:187-192, 6617-6625
    for _ in 0..5 {
        mocktime += 5;
        set_mocktime(&[&node0, &node1], mocktime);
        peer.answer();
        std::thread::sleep(Duration::from_millis(200));
    }
    peer.sync_with_ping();
    let present_on_node0_after_trickle = in_mempool(&node0, &tx1_id);

    // Resend tx1 from the forcerelay peer; node 1 must log and relay it to node 0.
    let log_from_force = node1.log_len();
    peer.send("tx", tx1_bytes);
    peer.sync_with_ping();

    let relay_deadline = Instant::now() + scaled(45);
    let mut relayed_to_node0 = false;
    while Instant::now() < relay_deadline {
        if in_mempool(&node0, &tx1_id) {
            relayed_to_node0 = true;
            break;
        }
        mocktime += 2;
        set_mocktime(&[&node0, &node1], mocktime);
        peer.answer();
        std::thread::sleep(Duration::from_millis(200));
    }
    let force_relay_log =
        wait_log_line(&node1, log_from_force, "Force relaying tx ", &tx1_id, "<txid>");
    let force_obs = ForceRelayObs {
        accepted_on_node1,
        present_on_node0_before_resend,
        present_on_node0_after_trickle,
        relayed_to_node0,
        force_relay_log_mocktime_stamped: force_relay_log
            .as_ref()
            .is_some_and(|l| l.mocktime_stamped),
        force_relay_log: force_relay_log.map(|l| l.text),
    };

    // First send of conflicting tx2: rejected by ATMP as txn-mempool-conflict.
    let log_from_first = node1.log_len();
    peer.send("tx", tx2_bytes.clone());
    peer.sync_with_ping();
    let reject_log = wait_log_line(&node1, log_from_first, "was not accepted:", &tx2_id, "<txid2>");
    let conflict_first_obs = ConflictFirstObs {
        in_node1_mempool: in_mempool(&node1, &tx2_id),
        in_node0_mempool: in_mempool(&node0, &tx2_id),
        reject_log: reject_log.map(|l| l.text),
    };

    // Second send of conflicting tx2: caught by m_recent_rejects -> Not relaying non-mempool tx.
    let log_from_second = node1.log_len();
    peer.send("tx", tx2_bytes);
    peer.sync_with_ping();
    let not_relaying_log = wait_log_line(
        &node1,
        log_from_second,
        "Not relaying non-mempool transaction ",
        &tx2_id,
        "<txid2>",
    );
    let conflict_second_obs = ConflictSecondObs {
        in_node1_mempool: in_mempool(&node1, &tx2_id),
        in_node0_mempool: in_mempool(&node0, &tx2_id),
        not_relaying_log: not_relaying_log.map(|l| l.text),
    };

    drop(peer);
    node1.stop();
    node0.stop();
    (force_obs, conflict_first_obs, conflict_second_obs)
}

#[test]
fn tx_relay_forcerelay_matches_dashd_v24() {
    let (core_force, core_first, core_second) = run_tx_relay(&Binary::dashd());
    assert_eq!(
        core_force,
        ForceRelayObs {
            accepted_on_node1: true,
            present_on_node0_before_resend: false,
            present_on_node0_after_trickle: false,
            relayed_to_node0: true,
            force_relay_log: Some("Force relaying tx <txid> from peer=0".to_owned()),
            force_relay_log_mocktime_stamped: true,
        },
        "dashd v24 oracle deviated from p2p_permissions.py:111-135"
    );
    assert_eq!(
        core_first,
        ConflictFirstObs {
            in_node1_mempool: false,
            in_node0_mempool: false,
            reject_log: Some(
                "[mempoolrej] <txid2> from peer=0 was not accepted: txn-mempool-conflict"
                    .to_owned()
            ),
        },
        "dashd v24 oracle deviated from p2p_permissions.py:137-147"
    );
    assert_eq!(
        core_second,
        ConflictSecondObs {
            in_node1_mempool: false,
            in_node0_mempool: false,
            not_relaying_log: Some(
                "Not relaying non-mempool transaction <txid2> from forcerelay peer=0".to_owned()
            ),
        },
        "dashd v24 oracle deviated from p2p_permissions.py:149-154"
    );

    let (our_force, our_first, our_second) = run_tx_relay(&Binary::rdashd());
    let mut failures = Vec::new();
    record_case(
        "tx_relay_forcerelay",
        "p2p_permissions.py:111-135",
        &core_force,
        &our_force,
        &mut failures,
    );
    record_case(
        "tx_relay_conflict_first",
        "p2p_permissions.py:137-147",
        &core_first,
        &our_first,
        &mut failures,
    );
    record_case(
        "tx_relay_conflict_second",
        "p2p_permissions.py:149-154",
        &core_second,
        &our_second,
        &mut failures,
    );
    assert!(failures.is_empty(), "check_tx_relay mismatches:\n{}", failures.join("\n"));
}
