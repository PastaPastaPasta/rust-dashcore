//! BL-648 T1: dashd v24.0.0's recorded V19-window chains (`tests/data/v19-window`, PROVENANCE.md)
//! over the node's connector, with V19 above height 1 and DIP0003 active.
//!
//! - R1: V19 at 212, ChainLocks off, ProTxs of both versions around it, and a side branch S off
//!   208 that crosses V19 at its own 211 (B, the last block processed under legacy).
//! - R2: V20 and MN_RR at 100, V19 at 236, ChainLocks on. Every CbTx from 100 to 236 carries a
//!   null ChainLock, as Core's miner writes it (`node/miner.cpp:117-124` at v24.0.0), and 237's
//!   carries the basic one on 236. `switch-refused.txt` is 236 carrying the legacy ChainLock on
//!   234 at difference 1, which dashd refuses with `bad-cbtx-cldiff`.
//! - R3 (BL-839): V19 at 150, ChainLocks and DKG off. A legacy masternode's ProUpServTx v1 at 150
//!   and another's ProUpRevTx v1 at 151, each signed under the basic scheme in the legacy
//!   encoding. `serv-refused.txt` and `rev-refused.txt` are those blocks carrying the same payloads
//!   signed under legacy (as dashd's RPC writes v1 payloads), which dashd refuses with
//!   `bad-protx-sig`.

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::BlsCtx;
use dash_dml::MnList;
use dash_dml::netinfo::{is_empty, primary};
use dash_p2p_core::ClSig;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};
use dash_pkc::bls::BlsGlobalSig;
use serde_json::{Value, json};

use super::evo_chain::{Kept, decode, linked, node_config, recorded_config_on};
use super::evo_chain_cl::{Counted, submit_verdict};
use super::locks::{cl_rig, intake, record, settle};
use super::support::{Mined, Rig, RigConfig, install_accept_log, lines_on};
use crate::chain::ChainLockSig;
use crate::connect::NodeHelper;
use crate::evo::cl::{AttemptClVerifier, ClPass};
use crate::evo::gates::GateCounts;

pub(super) const R1_CHAIN: &str = include_str!("../../../tests/data/v19-window/r1/chain.txt");
const R1_FORK: &str = include_str!("../../../tests/data/v19-window/r1/fork.txt");
pub(super) const R1_SUMMARY: &str = include_str!("../../../tests/data/v19-window/r1/summary.json");
pub(super) const R2_CHAIN: &str = include_str!("../../../tests/data/v19-window/r2/chain.txt");
const R2_CLSIGS: &str = include_str!("../../../tests/data/v19-window/r2/clsigs.jsonl");
const R2_REFUSED: &str = include_str!("../../../tests/data/v19-window/r2/switch-refused.txt");
pub(super) const R2_SUMMARY: &str = include_str!("../../../tests/data/v19-window/r2/summary.json");
pub(super) const R3_CHAIN: &str = include_str!("../../../tests/data/v19-window/r3/chain.txt");
const R3_SERV_REFUSED: &str = include_str!("../../../tests/data/v19-window/r3/serv-refused.txt");
const R3_REV_REFUSED: &str = include_str!("../../../tests/data/v19-window/r3/rev-refused.txt");
pub(super) const R3_SUMMARY: &str = include_str!("../../../tests/data/v19-window/r3/summary.json");

/// R1's fork point P: S's first block is P + 1.
const R1_P: u32 = 208;
/// R1's M tip when S was mined.
const R1_M_AT_FORK: u32 = 215;

/// `ProcessSpecialTxsInBlock`'s switch to basic at B (`evo/specialtxman.cpp:1043-1047`).
const PS0: &str = "CSpecialTxProcessor::ProcessSpecialTxsInBlock -- bls_legacy_scheme=0";
/// `ConnectTip`/`DisconnectTip`'s guard entering legacy (`validation.cpp:154-161`).
const EN1: &str = "ScopedBLSLegacyScheme: entered bls_legacy_scheme=1";

pub(super) fn summary(text: &str) -> Value {
    serde_json::from_str(text).expect("summary.json")
}

/// Regtest at v24 with the recording's `chain_args`.
pub(super) fn params(s: &Value) -> Arc<ChainSetup> {
    let mut builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0);
    for arg in s["chain_args"].as_array().expect("chain_args") {
        let arg = arg.as_str().expect("an arg");
        let (name, value) = arg.split_once('=').unwrap_or((arg, ""));
        let option = ChainOption::from_name(name).unwrap_or_else(|| panic!("{name}"));
        builder = builder.arg(option, value);
    }
    Arc::new(builder.build().expect("regtest"))
}

pub(super) fn last_time(blocks: &[Arc<dash_consensus::Block>]) -> i64 {
    i64::from(blocks.last().expect("blocks").header().raw().time())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// Connects `blocks` in order, each accepted.
fn connect(rig: &Rig, blocks: &[Mined]) {
    for m in blocks {
        rig.accept(m);
    }
}

fn tip_list(rig: &Rig) -> Arc<MnList> {
    rig.v.query(|q| q.evo_view().tip_list().expect("reads")).recv().expect("V serves")
}

pub(super) fn scheme(rig: &Rig) -> BlsCtx {
    *rig.v.view().bls()
}

/// `(version, pubKeyOperator, PoSeBanHeight, revocationReason)` by proTxHash, as `protx info`
/// prints them.
type States = BTreeMap<String, (i64, String, i64, i64)>;

fn ours(list: &MnList) -> States {
    list.iter()
        .map(|dmn| {
            let cold = &dmn.cold;
            let key = hex(&cold.pub_key_operator.to_bytes_as(cold.key_scheme()));
            let state = (
                i64::from(cold.version),
                key,
                i64::from(dmn.hot.banned_height()),
                i64::from(cold.revocation_reason),
            );
            (dmn.pro_tx_hash.to_string(), state)
        })
        .collect()
}

fn recorded(s: &Value, height: u32) -> States {
    let at = s["state_versions"][height.to_string()].as_object().expect("states at the height");
    at.iter()
        .map(|(hash, st)| {
            let state = (
                st["version"].as_i64().expect("version"),
                st["pubKeyOperator"].as_str().expect("key").to_owned(),
                st["PoSeBanHeight"].as_i64().expect("ban"),
                st["revocationReason"].as_i64().expect("reason"),
            );
            (hash.clone(), state)
        })
        .collect()
}

/// R1 on a rig over the node's connector: M (1-259) and S (209-214).
struct R1 {
    rig: Rig,
    kept: Kept,
    m: Vec<Mined>,
    s: Vec<Mined>,
    summary: Value,
}

fn r1() -> R1 {
    let summary = summary(R1_SUMMARY);
    let raw = decode(R1_CHAIN);
    let (cfg, kept) =
        recorded_config_on(RigConfig::default(), params(&summary), last_time(&raw), None);
    let rig = Rig::with(cfg);
    let m = linked(&rig.genesis, raw);
    let s = linked(&m[R1_P as usize - 1], decode(R1_FORK));
    assert_eq!((m.len(), s.len()), (259, 6), "R1's shape");
    assert_eq!(summary["v19_height"], 212);
    R1 {
        rig,
        kept,
        m,
        s,
        summary,
    }
}

/// R1's M to its tip on the node's connector: every block connects, so H4's
/// `merkleRootMNList` and `merkleRootQuorums` are Core's, with legacy commitments (version 1) at
/// 179 and 203 and basic ones (3) at 227 and 251 (`llmq/commitment.cpp:130-137`), and ProTxs of
/// both versions around V19 (`stm:1172-1176`). Each masternode's state version, operator key,
/// ban height and revocation reason at 211, 212, 214 and 259 are dashd's (`protx info`).
// Core: evo/specialtxman.cpp:1043-1047, :1172-1176; llmq/commitment.cpp:130-137 at v24.0.0
#[test]
fn r1_connects_with_cores_roots() {
    let r = r1();
    let mut from = 0;
    for h in [211, 212, 214, 259] {
        connect(&r.rig, &r.m[from..h as usize]);
        from = h as usize;
        assert_eq!(ours(&tip_list(&r.rig)), recorded(&r.summary, h), "the list at {h}");
    }
    let versions = |h: u32| -> Vec<i64> {
        let mut v: Vec<_> = recorded(&r.summary, h).into_values().map(|s| s.0).collect();
        v.sort_unstable();
        v
    };
    assert_eq!(versions(211), [1, 1, 1, 1], "legacy states below V19");
    assert!(versions(259).contains(&2), "a basic state after V19");
    assert_eq!(scheme(&r.rig), BlsCtx::BASIC);
    r.kept.assert_no_abort(&r.rig);
}

/// The scheme lines V's thread logged since `mark`, which moves to the end.
fn window(v: std::thread::ThreadId, mark: &mut usize) -> Vec<String> {
    let lines = lines_on(v);
    let new = lines[*mark..]
        .iter()
        .filter(|l| {
            (l.starts_with("ScopedBLSLegacyScheme:") || l.starts_with("CSpecialTxProcessor::"))
                && l.contains("bls_legacy_scheme=")
        })
        .cloned()
        .collect();
    *mark = lines.len();
    new
}

/// R1's reorg across V19: M to 215, `invalidateblock 209`, S to 214, `reconsiderblock 209`, M to
/// 259. Each step's scheme lines are dashd's (the probe in PROVENANCE.md): PS0 at M's B, EN1 at
/// its disconnect, PS0 at S's B, EN1 at S's B's disconnect then PS0 at M's B again, and none
/// after; no `UndoSpecialTxsInBlock` switch and no revert anywhere. The published scheme is
/// legacy at 208, basic on S's 214 and at M's tip.
// Core: validation.cpp:154-161, :3283-3284, :3331; evo/specialtxman.cpp:1043-1047, :1065-1073
#[test]
fn r1_reorg_across_v19_logs_cores_scheme_lines() {
    install_accept_log();
    let r = r1();
    let v = r.rig.v_thread();
    connect(&r.rig, &r.m[..R1_P as usize]);
    assert_eq!(scheme(&r.rig), BlsCtx::LEGACY, "at {R1_P}");
    let mut mark = lines_on(v).len();
    let mut windows = Vec::new();

    connect(&r.rig, &r.m[R1_P as usize..R1_M_AT_FORK as usize]);
    windows.push(("M to 215", window(v, &mut mark)));
    let cut = r.m[R1_P as usize].hash();
    assert_eq!(r.rig.v.invalidate_block(cut).recv(), Ok(Ok(())));
    assert_eq!(r.rig.v.view().snap.tip.tip_hash, r.m[R1_P as usize - 1].hash());
    windows.push(("invalidateblock 209", window(v, &mut mark)));
    connect(&r.rig, &r.s);
    assert_eq!(scheme(&r.rig), BlsCtx::BASIC, "on S's 214");
    windows.push(("S", window(v, &mut mark)));
    assert_eq!(r.rig.v.reconsider_block(cut, false).recv(), Ok(Ok(())));
    assert_eq!(r.rig.v.view().snap.tip.tip_hash, r.m[R1_M_AT_FORK as usize - 1].hash());
    windows.push(("reconsiderblock 209", window(v, &mut mark)));
    connect(&r.rig, &r.m[R1_M_AT_FORK as usize..]);
    windows.push(("M to 259", window(v, &mut mark)));
    assert_eq!(scheme(&r.rig), BlsCtx::BASIC, "at M's tip");

    let want: [(&str, &[&str]); 5] = [
        ("M to 215", &[PS0]),
        ("invalidateblock 209", &[EN1]),
        ("S", &[PS0]),
        ("reconsiderblock 209", &[EN1, PS0]),
        ("M to 259", &[]),
    ];
    let want: Vec<(&str, Vec<String>)> =
        want.iter().map(|(s, l)| (*s, l.iter().map(|l| (*l).to_owned()).collect())).collect();
    assert_eq!(windows, want);
    r.kept.assert_no_abort(&r.rig);
}

/// One of R2's recorded ChainLocks.
pub(super) struct Lock {
    pub(super) height: u32,
    pub(super) sig: [u8; 96],
    pub(super) arrival: u32,
}

/// `clsigs.jsonl`'s locks, in the order they reach V: by arrival tip, then height. Each wire
/// payload names its height and block on the chain.
pub(super) fn locks(m: &[Mined]) -> Vec<Lock> {
    let mut locks: Vec<Lock> = R2_CLSIGS
        .lines()
        .map(|line| {
            let l: Value = serde_json::from_str(line).expect("a lock");
            let wire = unhex(l["wire"].as_str().expect("wire"));
            assert_eq!(wire.len(), 132, "height, hash, signature");
            let height = u32::from_le_bytes(wire[..4].try_into().expect("four bytes"));
            assert_eq!(l["height"], height);
            assert_eq!(m[height as usize - 1].hash().as_bytes()[..], wire[4..36], "{height}");
            Lock {
                height,
                sig: wire[36..].try_into().expect("96 bytes"),
                arrival: u32::try_from(l["arrival_tip"].as_u64().expect("arrival")).expect("tip"),
            }
        })
        .collect();
    locks.sort_by_key(|l| (l.arrival, l.height));
    locks
}

/// `m`'s CbTx best ChainLock, `(difference, signature)`, from version 3; the payload is
/// version(2) height(4) roots(64) diff(1, below 253) sig(96) balance(8).
pub(super) fn cbtx_cl(m: &Mined) -> Option<(u32, [u8; 96])> {
    let payload = m.block.txs()[0].to_mutable().extra_payload().clone();
    if payload.len() < 2 || u16::from_le_bytes([payload[0], payload[1]]) < 3 {
        return None;
    }
    assert!(payload[70] < 253, "a one-byte difference at {}", m.height);
    Some((u32::from(payload[70]), payload[71..167].try_into().expect("96 bytes")))
}

/// R2 to its tip, `with_locks`: with each recorded ChainLock given to V at its arrival tip (`ClIntake`,
/// then `RecordChainLock`). Every block connects. Each CbTx takes the path Core's rule gives under
/// the best ChainLock V holds then: null below 237, then shortcut 1 (`stm:152-158`), shortcut 2
/// (`:187-193`) or a verify (`:202-205`), each check valid. No verify happens below 237, as no
/// CbTx there carries a ChainLock, so none is made under the legacy scheme. Before the
/// ChainLocks first seen at 235, `switch-refused.txt` is `bad-cbtx-cldiff`, dashd's answer with
/// and without the one on 234 as its best: its legacy bytes are no basic signature at 236, so it
/// is a null signature at difference 1 (`stm:209-211`). That holds for these bytes only: a legacy
/// signature that also reads as a basic point would reach the verify (`:202-205`).
// Core: evo/specialtxman.cpp:139-215 CheckCbTxBestChainlock; node/miner.cpp:117-124 at v24.0.0
fn r2(with_locks: bool) {
    let s = summary(R2_SUMMARY);
    let (n2, v20) = (236_u32, 100_u32);
    assert_eq!((s["v19_height"].as_u64(), s["v20_height"].as_u64()), (Some(236), Some(100)));
    let raw = decode(R2_CHAIN);
    let p = params(&s);
    let after_genesis = last_time(&raw) - i64::from(Mined::genesis(&p).time()) + 86_400;
    let (verifier, census) = AttemptClVerifier::with_census();
    let counts = Arc::new(GateCounts::default());
    let (cfg, kept) = node_config(
        p,
        after_genesis,
        RigConfig::default(),
        Counted(counts.clone()),
        Box::new(verifier),
        Box::new(NodeHelper),
        |b| b,
    );
    let (rig, _) = cl_rig(cfg);
    let m = linked(&rig.genesis, raw);
    assert_eq!(m.len(), 290);
    let [refused] = <[Mined; 1]>::try_from(linked(&m[n2 as usize - 2], decode(R2_REFUSED)))
        .ok()
        .expect("one block");
    let locks = locks(&m);
    assert!(locks.iter().any(|l| (l.height, l.arrival) == (n2 - 2, n2 - 2)), "234's at 234");

    let mut best: Option<(u32, [u8; 96])> = None;
    let (mut shortcuts, mut checks) = (Vec::new(), Vec::new());
    let lock_234 = locks.iter().find(|l| l.height == n2 - 2).expect("the lock on 234").sig;
    let decoded = BlsGlobalSig::decode(&lock_234).expect("96 bytes");
    let valid = [BlsCtx::LEGACY, BlsCtx::BASIC].map(|ctx| decoded.get(ctx).is_valid());
    assert_eq!(valid, [true, false], "234's signature: legacy, and no basic point");
    let locks = if with_locks {
        locks
    } else {
        Vec::new()
    };
    let mut fed = locks.iter().peekable();
    let mut refused_verdict = None;
    for b in &m {
        let h = b.height;
        if let Some((diff, sig)) = cbtx_cl(b) {
            let is_best = |height: u32| best.is_some_and(|(bh, bs)| bh == height && bs == sig);
            if sig == [0; 96] {
                assert_eq!(diff, 0, "{h}");
            } else if diff == 0 && is_best(h - 1) {
                shortcuts.push((h, ClPass::BestAtParent));
            } else if is_best(h - 1 - diff) {
                shortcuts.push((h, ClPass::OutdatedBest));
            } else {
                checks.push((h, h - 1 - diff, Some(true)));
            }
            assert_eq!(sig == [0; 96], (v20..=n2).contains(&h), "{h}: Core's miner");
        } else {
            assert!(h < v20, "{h}: CbTx v3 from V20");
        }
        rig.accept(b);
        if h == n2 - 1 {
            let want = with_locks.then_some(n2 - 2);
            assert_eq!(best.map(|(bh, _)| bh), want, "the best at {h}");
            refused_verdict = Some(submit_verdict(&rig, &refused));
        }
        while let Some(lock) = fed.next_if(|l| l.arrival == h) {
            intake(&rig);
            let read = scheme(&rig);
            let height = i32::try_from(lock.height).expect("a height");
            let hash = m[lock.height as usize - 1].hash();
            let wire = ClSig::new(height, hash, lock.sig.into());
            let sig = Arc::new(ChainLockSig::from_wire(&wire, read).expect("archive sig"));
            let moves = best.is_none_or(|(bh, _)| lock.height > bh);
            assert_eq!(record(&rig, &sig), moves, "the lock on {} at {h}", lock.height);
            if moves {
                best = Some((lock.height, lock.sig));
            }
            settle(&rig);
        }
    }
    assert!(fed.next().is_none(), "every lock fed");
    assert_eq!(refused_verdict.as_deref(), Some("bad-cbtx-cldiff"), "switch-refused.txt");
    // 237 to 290 carry a ChainLock: fed, each is V's best or an outdated best; unfed, each is
    // verified, under basic.
    let (taken, verified) = if with_locks {
        (54, 0)
    } else {
        (0, 54)
    };
    assert_eq!((shortcuts.len(), checks.len()), (taken, verified), "{shortcuts:?} {checks:?}");
    if with_locks {
        assert_eq!(shortcuts.first(), Some(&(n2 + 1, ClPass::BestAtParent)), "237 takes 236's");
    } else {
        assert_eq!(checks.first(), Some(&(n2 + 1, n2, Some(true))), "237 verifies 236's");
    }
    assert_eq!(counts.root_checks().cl_shortcuts, shortcuts);
    assert_eq!(*census.checks.lock(), checks);
    assert!(checks.iter().all(|&(h, _, _)| h > n2), "no verify below V19: {checks:?}");
    let best_height = rig.v.view().best_chainlock().map(|b| b.height);
    assert_eq!(best_height, with_locks.then_some(290));
    kept.assert_no_abort(&rig);
}

#[test]
fn r2_cbtx_chainlocks_take_cores_path_with_the_recorded_chainlocks() {
    r2(true);
}

#[test]
fn r2_cbtx_chainlocks_verify_under_basic_without_chainlocks() {
    r2(false);
}

/// Each masternode's `addresses` as `protx info` prints a legacy state's: its primary service as
/// `core_p2p`, none once revoked.
fn addresses(list: &MnList) -> BTreeMap<String, Value> {
    list.iter()
        .map(|dmn| {
            let net_info = &dmn.cold.net_info;
            let at = if is_empty(net_info) {
                json!({})
            } else {
                json!({"core_p2p": [primary(net_info).to_string()]})
            };
            (dmn.pro_tx_hash.to_string(), at)
        })
        .collect()
}

fn recorded_addresses(s: &Value, height: u32) -> BTreeMap<String, Value> {
    let at = s["state_versions"][height.to_string()].as_object().expect("states at the height");
    at.iter().map(|(hash, st)| (hash.clone(), st["addresses"].clone())).collect()
}

/// The version of `m`'s one ProTx (types 1-4), its payload's first two bytes.
fn protx_version(m: &Mined) -> u16 {
    let payloads: Vec<_> = m.block.txs()[1..]
        .iter()
        .map(|tx| tx.to_mutable().extra_payload().clone())
        .filter(|p| !p.is_empty())
        .collect();
    let [payload] = <[Vec<u8>; 1]>::try_from(payloads).expect("one ProTx");
    u16::from_le_bytes([payload[0], payload[1]])
}

/// R3: a ProUpServTx v1 at 150 (V19's first block) and a ProUpRevTx v1 at 151, each of a
/// legacy-state masternode and signed under the basic scheme in v1's legacy byte encoding. Core
/// reads a v1 payload's signature in the legacy encoding (`providertx.h:307-310`, `:425-428`) and
/// verifies it under the block's scheme, not the payload's version (`CheckHashSig`,
/// `specialtxman.cpp:1148-1155`, `:1482`, `:1617`; `bls.cpp:323-326`; `validation.cpp:2501`); the
/// version cap is a maximum (`specialtxman.cpp:1172-1176`). Every block connects, and the list at
/// 149, 150, 151 and 160 is dashd's (`protx info`: version, operator key, ban, revocation reason,
/// addresses): A keeps its legacy state and key with its new service, C is revoked at 151 with
/// reason 2. The same payloads signed under legacy, as dashd's RPC signs a v1 payload
/// (`providertx_service.cpp:351`), are `bad-protx-sig` at 150 and 151: `serv-refused.txt` on 149
/// and `rev-refused.txt` on 150, dashd's answers.
// Core: evo/specialtxman.cpp:1148-1182, :1482, :1617; bls/bls.cpp:323-326; evo/providertx.h:307-310,
// :425-428 at v24.0.0
#[test]
fn r3_v1_protxs_above_v19_connect_with_cores_state() {
    let s = summary(R3_SUMMARY);
    let (n, tip) = (150_u32, 160_u32);
    assert_eq!(s["v19_height"], n);
    let raw = decode(R3_CHAIN);
    let (cfg, kept) = recorded_config_on(RigConfig::default(), params(&s), last_time(&raw), None);
    let rig = Rig::with(cfg);
    let m = linked(&rig.genesis, raw);
    assert_eq!(m.len(), tip as usize, "R3's shape");
    let one = |text: &str, parent: u32| -> Mined {
        let [b] = <[Mined; 1]>::try_from(linked(&m[parent as usize - 1], decode(text)))
            .ok()
            .expect("one block");
        b
    };
    let (serv, rev) = (one(R3_SERV_REFUSED, n - 1), one(R3_REV_REFUSED, n));
    for (event, h) in [("upserv_a", n), ("revoke_c", n + 1)] {
        assert_eq!(s["events"][event]["height"], h);
        assert_eq!(protx_version(&m[h as usize - 1]), 1, "{event} at {h}");
    }
    assert_eq!((protx_version(&serv), protx_version(&rev)), (1, 1));

    let check = |h: u32| {
        let list = tip_list(&rig);
        assert_eq!(ours(&list), recorded(&s, h), "the list at {h}");
        assert_eq!(addresses(&list), recorded_addresses(&s, h), "addresses at {h}");
    };
    connect(&rig, &m[..n as usize - 1]);
    check(n - 1);
    assert_eq!(submit_verdict(&rig, &serv), "bad-protx-sig", "serv-refused.txt");
    connect(&rig, &m[n as usize - 1..n as usize]);
    check(n);
    assert_eq!(submit_verdict(&rig, &rev), "bad-protx-sig", "rev-refused.txt");
    connect(&rig, &m[n as usize..n as usize + 1]);
    check(n + 1);
    connect(&rig, &m[n as usize + 1..]);
    check(tip);
    assert_eq!(rig.v.view().snap.tip.tip_hash, m[tip as usize - 1].hash());
    assert_eq!(scheme(&rig), BlsCtx::BASIC);
    kept.assert_no_abort(&rig);
}
