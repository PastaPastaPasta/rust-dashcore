//! BL-425 (SP1c-1): the chainstate's `ResetPlatformBan` effects reach the node's masternode meta
//! store through dashd-node's applier (`MetaPlatformBans`), and the store they leave is Core's.
//! Each script runs through Core's `CMasternodeMetaMan` (`oracle_mncache_step`, E3) and through
//! SP1's `MasternodeMetaMan`, whose resets go through the applier only, as a connect's do: the
//! answers Core gives after them and the store's `mncache.dat` body must match. The applier
//! drops `ResetPlatformBan`'s result, as `CDeterministicMNManager::ProcessBlock` only logs it.
//! Core's answer to each reset is not compared: the answers after it and the store show it.
//!
//! The chained cases run a block's `ProcessBlock` resets, Core's (`oracle_dml_block_effects`) on
//! Core's store and our `process_block` effect log through `EffectAppliers::apply_log` on ours,
//! from the same banned entries: O2a-EV's crafted blocks, and mainnet's real ProUpServTx block
//! (DEC-782 A2(a); A2(b), a connect through rdashd's `NodeEvo`, is CH7a-5's BL-526).
// Core: evo/deterministicmns.cpp:733-748; masternode/meta.cpp:187-195; masternode/meta.h:73-84

use std::sync::Arc;

use dash_consensus::primitives::decode_header;
use dash_evo::llmq::QuorumSnapshot;
use dash_primitives::{BlockHash, LlmqType};
use serde_json::Value;

use dash_dml::testkit::replay::mainnet;
use dash_evo::effect::Effect;
use dash_evo::types::ProTxHash;
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, ContextCoin, DEPLOYMENT_V24, MetaOp, MetaOutput,
    MnList as CoreList, Oracle, Pin, ThresholdState, Verdict,
};
use dash_p2p_core::PlatformBan;
use dash_params::ChainParams;
use dash_types::codec::BaseCodec as _;
use dashd_chainstate::chain::Fatal;
use dashd_chainstate::evo::effects::{
    EffectAppliers, PlatformBanApplier as _, SnapshotApplier, UnreachableIsLock,
    UnreachableMineable, UnreachableNotify,
};
use dashd_masternode::flatdb::core_layout::core_body;
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_node::chainstate::MetaPlatformBans;
use node_l1::dml_apply::cases;
use node_l1::dml_block::our_block_effects;
use node_l1::mnsync::{NETWORKS, network, platform_ban_message};

const PIN: Pin = Pin::V24_0;
const PROUPSERV_BLOCK: &str = include_str!("data/proupserv-mainnet-1028257.json");
const CONTEXTUAL_MAINNET: &str =
    include_str!("../../dash-oracle-sys/tests/data/contextual-mainnet.json");

/// `ops` on Core's store and on ours (resets through the applier); every answer after a reset
/// and the final store agree.
fn agree(selector: u8, ops: &[MetaOp]) {
    let core = Oracle::new(PIN).mncache_step(&network(selector).0, ops).expect("in the domain");
    let metaman = Arc::new(MasternodeMetaMan::new());
    let mut bans = MetaPlatformBans(Arc::clone(&metaman));
    let mut outputs = core.outputs.iter();
    for op in ops {
        let ours = match op {
            MetaOp::AllowMixing(h) => {
                metaman.allow_mixing(&ProTxHash::from(*h));
                continue;
            }
            MetaOp::ResetPlatformBan(h, height) => {
                bans.reset(&ProTxHash::from(*h), *height).expect("never a fault");
                outputs.next().expect("Core answers every reset");
                continue;
            }
            MetaOp::SetPlatformBan(inv, message) => {
                let msg = PlatformBan::decode(&mut &message[..]).expect("a ban message");
                let protx: [u8; 32] = message[..32].try_into().expect("32 bytes");
                let height = i32::from_le_bytes(message[32..36].try_into().expect("4 bytes"));
                metaman.set_platform_ban(*inv, &ProTxHash::from(protx), height, msg)
            }
            MetaOp::IsPlatformBanned(h) => metaman.is_platform_banned(&ProTxHash::from(*h)),
            other => panic!("not in this test's scripts: {other:?}"),
        };
        let core_out = outputs.next().expect("Core answers this op");
        assert_eq!(core_out, &MetaOutput::Flag(ours), "network {selector}: {op:?}");
    }
    assert!(outputs.next().is_none(), "network {selector}: Core answered more ops");
    assert_eq!(core.store, core_body(&metaman.snapshot()), "network {selector}: mncache store");
}

fn ban(protx: [u8; 32], height: i32) -> Vec<u8> {
    platform_ban_message(protx, height, [0x33; 32])
}

/// A reset of a masternode with no entry makes none; an entry never banned (its updated height 0)
/// takes the reset's height, which a later ban must reach; a banned entry is unbanned only above
/// its ban's height; and the extreme heights.
#[test]
fn applied_resets_leave_cores_store() {
    let (a, b, c) = ([0xa1; 32], [0xb2; 32], [0xc3; 32]);
    let scripts: [Vec<MetaOp>; 4] = [
        vec![MetaOp::ResetPlatformBan(a, 5), MetaOp::IsPlatformBanned(a)],
        vec![
            MetaOp::AllowMixing(a),
            MetaOp::ResetPlatformBan(a, 0),
            MetaOp::ResetPlatformBan(a, 12),
            MetaOp::SetPlatformBan([1; 32], ban(a, 11)),
            MetaOp::IsPlatformBanned(a),
            MetaOp::SetPlatformBan([2; 32], ban(a, 12)),
            MetaOp::IsPlatformBanned(a),
        ],
        vec![
            MetaOp::AllowMixing(a),
            MetaOp::SetPlatformBan([3; 32], ban(a, 10)),
            MetaOp::ResetPlatformBan(a, 9),
            MetaOp::IsPlatformBanned(a),
            MetaOp::ResetPlatformBan(a, 10),
            MetaOp::IsPlatformBanned(a),
            MetaOp::ResetPlatformBan(a, 11),
            MetaOp::IsPlatformBanned(a),
        ],
        vec![
            MetaOp::SetPlatformBan([4; 32], ban(b, 3)),
            MetaOp::ResetPlatformBan(b, 4),
            MetaOp::IsPlatformBanned(b),
            MetaOp::AllowMixing(c),
            MetaOp::ResetPlatformBan(c, -1),
            MetaOp::ResetPlatformBan(c, i32::MAX),
            MetaOp::ResetPlatformBan(c, i32::MIN),
            MetaOp::IsPlatformBanned(c),
        ],
    ];
    for selector in 0..NETWORKS.len() as u8 {
        for ops in &scripts {
            agree(selector, ops);
        }
    }
}

/// The node's appliers but the quorum-snapshot store's (`StoreSnapshots` needs the node's evo
/// store): `process_block` emits no `StoreQuorumSnapshot`, so this one faults.
struct NoQuorumSnapshots;

impl SnapshotApplier for NoQuorumSnapshots {
    fn write(&mut self, _: LlmqType, _: &BlockHash, _: &QuorumSnapshot) -> Result<(), Fatal> {
        Err(Fatal::new("process_block emits no quorum snapshot"))
    }
}

/// One accepted block's platform-ban resets reach the store as Core's do: every masternode
/// either side resets starts banned below the block's height on both stores; Core's
/// `ProcessBlock` resets (`oracle_dml_block_effects`) go to its store (E3), our `process_block`
/// effect log goes through `EffectAppliers::apply_log` with dashd-node's `MetaPlatformBans` to
/// ours, and the `mncache.dat` bodies agree. The block's height, whose resets these are (the
/// callers check it against Core's: `m_platform_ban_updated` takes it); `None` when the block
/// resets nothing in Core.
fn chain_block(
    oracle: &Oracle,
    chain: &Chain,
    params: &ChainParams,
    ctx: &Context,
    block: &[u8],
    name: &str,
) -> Option<i32> {
    let (core, effects) = oracle.dml_block_effects(chain, block, ctx, false, false).unwrap();
    assert_eq!(core.verdict, Verdict::Accept, "{name}");
    if effects.resets.is_empty() {
        return None;
    }
    let (height, log) = our_block_effects(params, ctx, block)
        .unwrap_or_else(|d| panic!("{name}: {d}"))
        .expect("our build accepts what Core accepts");
    let mut banned: Vec<[u8; 32]> = effects.resets.clone();
    for effect in &log {
        if let Effect::ResetPlatformBan {
            pro_tx_hash,
            height: at,
        } = effect
        {
            assert_eq!(*at, height, "{name}");
            banned.push(*pro_tx_hash.as_bytes());
        }
    }
    banned.sort_unstable();
    banned.dedup();
    let seeds: Vec<([u8; 32], Vec<u8>)> =
        banned.iter().enumerate().map(|(i, p)| ([i as u8; 32], ban(*p, height - 1))).collect();
    let mut ops: Vec<MetaOp> =
        seeds.iter().map(|(inv, m)| MetaOp::SetPlatformBan(*inv, m.clone())).collect();
    ops.extend(effects.resets.iter().map(|p| MetaOp::ResetPlatformBan(*p, height)));
    let core_run = oracle.mncache_step(chain, &ops).expect("in the domain");
    let metaman = Arc::new(MasternodeMetaMan::new());
    for (inv, message) in &seeds {
        let msg = PlatformBan::decode(&mut &message[..]).expect("a ban message");
        let protx: [u8; 32] = message[..32].try_into().expect("32 bytes");
        assert!(metaman.set_platform_ban(*inv, &ProTxHash::from(protx), height - 1, msg));
    }
    let mut appliers = EffectAppliers {
        snapshot: Box::new(NoQuorumSnapshots),
        is_lock: Box::new(UnreachableIsLock),
        mineable: Box::new(UnreachableMineable),
        platform_ban: Box::new(MetaPlatformBans(Arc::clone(&metaman))),
        notify: Box::new(UnreachableNotify),
    };
    appliers.apply_log(&log).unwrap_or_else(|e| panic!("{name}: {}", e.0));
    assert_eq!(core_run.store, core_body(&metaman.snapshot()), "{name}: mncache store");
    Some(height)
}

/// O2a-EV's crafted blocks whose `ProcessBlock` resets bans, through [`chain_block`].
#[test]
fn block_resets_leave_cores_store() {
    let oracle = Oracle::new(PIN);
    let mut chained = 0;
    for case in cases() {
        let chain = case.chain();
        let (core, _) =
            oracle.dml_block_effects(&chain, &case.block, &case.ctx, false, false).unwrap();
        if core.verdict == Verdict::Accept
            && let Some(height) =
                chain_block(&oracle, &chain, &case.params, &case.ctx, &case.block, &case.name)
        {
            // The block after the context's tip, Core's `pindex->nHeight` there.
            let tip = case.ctx.first_height + case.ctx.headers.len() as i32 - 1;
            assert_eq!(height, tip + 1, "{}", case.name);
            chained += 1;
        }
    }
    eprintln!("{chained} crafted blocks with platform-ban resets chained to the store");
    assert!(chained > 0, "no crafted block resets a ban");
}

/// Mainnet's block 1,028,257 with its first ProUpServTx after DIP0003 (8e5bdc5f…), over its
/// context in dash-oracle-sys's `contextual-mainnet.json` (`proupserv`: the headers and the list
/// after its parent), through [`chain_block`]: Core's real block resets the revived masternode,
/// and so do we. The block is `tests/data/proupserv-mainnet-1028257.json` (provenance there).
#[test]
fn mainnet_proupserv_block_resets_reach_cores_store() {
    let unhex = |v: &Value| -> Vec<u8> {
        let s = v.as_str().expect("a hex string");
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    };
    let block_data: Value = serde_json::from_str(PROUPSERV_BLOCK).unwrap();
    let samples: Value = serde_json::from_str(CONTEXTUAL_MAINNET).unwrap();
    let sample = &samples["proupserv"];
    assert_eq!(block_data["height"], sample["height"]);
    let first_height = i32::try_from(sample["first_height"].as_i64().unwrap()).unwrap();
    let raw = unhex(&sample["headers"]);
    let headers: Vec<[u8; 80]> = raw.chunks(80).map(|h| h.try_into().unwrap()).collect();
    let block = unhex(&block_data["block"]);
    let parent = decode_header(&mut &headers[headers.len() - 1][..]).unwrap().value().hash();
    assert_eq!(block[4..36], parent.as_bytes()[..], "the block's parent");
    let tip = first_height + headers.len() as i32 - 1;
    // The external collaterals its ProRegTxs reference (Core's `bad-protx-collateral` without).
    let coins = block_data["coins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let mut txid: [u8; 32] = unhex(&c["txid"]).try_into().unwrap();
            txid.reverse();
            ContextCoin {
                txid,
                vout: u32::try_from(c["vout"].as_u64().unwrap()).unwrap(),
                height: u32::try_from(c["height"].as_u64().unwrap()).unwrap(),
                coinbase: c["coinbase"].as_bool().unwrap(),
                amount: c["amount"].as_i64().unwrap(),
                script_pubkey: unhex(&c["script"]),
            }
        })
        .collect();
    let ctx = Context::new(
        first_height,
        headers,
        true,
        vec![(DEPLOYMENT_V24, ThresholdState::Defined)],
        sample["adjusted_time"].as_i64().unwrap() + 60,
        Checkpoints::Enabled {
            best_header_height: tip,
        },
        CoinsView::AtTip(coins),
        CoreList::Supplied(unhex(&sample["prev_list"])),
        vec![],
    );
    let oracle = Oracle::new(PIN);
    let height = chain_block(&oracle, &Chain::main(), mainnet(), &ctx, &block, "mainnet 1028257");
    // Core's `nHeight`, which `ResetPlatformBan` stores as `m_platform_ban_updated`.
    assert_eq!(height.map(i64::from), block_data["height"].as_i64());
}
