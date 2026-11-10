//! CH7a-5a's tests on dashd v24.0.0-rc.4's recorded masternode chain
//! (`dashd-chainstate/tests/data/ch7a5`, DIP0003 at 48, DIP0024 moved out): an EvoNode and a
//! regular masternode register at 116 and 117, the EvoNode's ProUpServTx is in 141 and its
//! ProUpRegTx in 161, the regular node is revoked in 181 and its collateral spent in 201; 300
//! blocks, all before V20 (432). No masternode runs, so no DKG completes and every
//! commitment is null.
//!
//! - The differential (L1, CH7 §7.2): the blocks go to a fresh dashd's `submitblock` and to V; at
//!   each height dashd's `protx list registered true` must equal V's list at its tip, field by
//!   field as Core prints it (`CDeterministicMN::ToJson`). Then `invalidateblock 116` on both
//!   takes them below the registrations, and `reconsiderblock` returns both to 300. In each of
//!   the three, H7's notifications must be Core's, in order, derived from V's list at each
//!   height: one per block whose diff from its parent's list (or, disconnecting, back to it) has
//!   changes, with the old list and that diff.
//! - BL-526: a platform ban in the node's masternode meta store is reset by the ProUpServTx's
//!   connect, at its height, through `NodeEvo` and dashd-node's `MetaPlatformBans`; a just-check
//!   of that block and its disconnect leave the store as they found it. node-l1's
//!   `l1_platform_ban` compares the same store with Core's bytes.
//!
//! Needs dashd v24.0.0 for the differential (see `v24`).

mod v24;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dash_consensus::Block;
use dash_dml::netinfo::{is_empty, primary};
use dash_dml::{Dmn, MnList, MnListDiff, SameRecord, payload};
use dash_netaddr::Service;
use dash_primitives::{BlockHash, NetInfo, SpecialPayload, TxType};
use dashd_chainstate::chain::ValidationInterface;
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_node::logging::Logger;
use dashd_node::sut::NodeSut;
use dashd_node::ui::Ui;
use dashd_rpc::key_io::{Destination, KeyIoParams, encode_destination, extract_destination};
use serde_json::{Value, json};

use v24::{ARGS, Harness, T0, blocks, hex};

/// dashd's masternode chain, blocks 1 to 300.
const MAIN: &str = include_str!("../../dashd-chainstate/tests/data/ch7a5/blocks.txt");
/// DIP0003's height on the recorded chain.
const DIP3: usize = 48;

/// A special transaction the recording's header names: `# HEIGHT KIND TXID`.
struct Event {
    height: usize,
    txid: String,
}

/// The recording's transaction of `kind` (`record.sh`'s names).
fn event(kind: &str) -> Event {
    let found: Vec<Event> = MAIN
        .lines()
        .take_while(|l| l.starts_with('#'))
        .filter_map(|l| {
            let words: Vec<&str> = l.trim_start_matches("# ").split(' ').collect();
            match words[..] {
                [height, k, txid] if k == kind && txid.len() == 64 => Some(Event {
                    height: height.parse().ok()?,
                    txid: txid.to_owned(),
                }),
                _ => None,
            }
        })
        .collect();
    let [e] = <[Event; 1]>::try_from(found).ok().expect("one event of the kind");
    e
}

/// `key_io.cpp`'s regtest parameters, as `dashd-node`'s RPC takes them.
fn key_io(sut: &NodeSut) -> KeyIoParams {
    let a = &sut.params().addr_params;
    KeyIoParams {
        pubkey_address: a.pubkey_addr,
        script_address: a.script_addr,
        secret_key: a.secret_key,
        ext_public_key: a.ext_pubkey,
        ext_secret_key: a.ext_secret,
        platform_hrp: sut.params().platform_hrp.map(str::to_owned),
    }
}

/// Core's `CDeterministicMN::ToJson()` for `dmn` (`rpc/evo.cpp:63-86`, its state's
/// `evo/core_write.cpp:154-196`), without `collateralAddress`, which needs `-txindex`: what
/// `protx list registered true` prints of it, less `BuildDMNListEntry`'s confirmations, wallet
/// and meta fields. The deprecated `service` fields are off by default. Only what the recording
/// has: version 1 and 2 states (legacy `netInfo`, `scriptPayout`), no shared masternodes.
// Core: rpc/evo.cpp:63-86; evo/core_write.cpp:38-57, :154-196; evo/netinfo.cpp:359-374
fn core_json(dmn: &Dmn, key_io: &KeyIoParams) -> Value {
    let (hot, cold) = (&dmn.hot, &*dmn.cold);
    assert!(cold.version < 3 && cold.shares.is_empty(), "not in the recording: {dmn:?}");
    let pkh = |id: &[u8; 20]| encode_destination(&Destination::PkHash(*id), key_io);
    let address =
        |script: &[u8]| extract_destination(script).map(|d| encode_destination(&d, key_io));

    let mut addresses = serde_json::Map::new();
    if !is_empty(&cold.net_info) {
        let p = primary(&cold.net_info);
        addresses.insert("core_p2p".into(), json!([p.to_string()]));
        // `GetNetInfoWithLegacyFields`: an EvoNode's platform ports on the primary address.
        if dmn.is_evo() && matches!(*cold.net_info, NetInfo::Legacy(_)) {
            let at = |port| {
                Service {
                    addr: p.addr.clone(),
                    port,
                }
                .to_string()
            };
            addresses.insert("platform_https".into(), json!([at(cold.platform_http_port)]));
            addresses.insert("platform_p2p".into(), json!([at(cold.platform_p2p_port)]));
        }
    }
    let mut state = json!({
        "version": cold.version,
        "addresses": addresses,
        "registeredHeight": cold.registered_height,
        "lastPaidHeight": hot.last_paid_height,
        "consecutivePayments": hot.consecutive_payments,
        "PoSePenalty": hot.pose_penalty,
        "PoSeRevivedHeight": hot.pose_revived_height,
        "PoSeBanHeight": hot.banned_height(),
        "revocationReason": cold.revocation_reason,
        "ownerAddress": pkh(cold.key_id_owner.as_bytes()),
        "votingAddress": pkh(cold.key_id_voting.as_bytes()),
        "pubKeyOperator": hex(&cold.pub_key_operator.to_bytes_as(cold.key_scheme())),
    });
    let o = state.as_object_mut().expect("an object");
    if dmn.is_evo() {
        // `uint160::ToString`: the bytes reversed.
        let mut id = *cold.platform_node_id.as_bytes();
        id.reverse();
        o.insert("platformNodeID".into(), json!(hex(&id)));
    }
    if let Some(a) = address(&cold.script_payout) {
        o.insert("payoutAddress".into(), json!(a));
    }
    if let Some(a) = address(&cold.script_operator_payout) {
        o.insert("operatorPayoutAddress".into(), json!(a));
    }
    // `static_cast<double>(nOperatorReward) / 100`, which UniValue prints without a fraction
    // when it has none.
    let reward = dmn.operator_reward;
    let reward = if reward.is_multiple_of(100) {
        json!(reward / 100)
    } else {
        json!(f64::from(reward) / 100.0)
    };
    json!({
        "type": if dmn.is_evo() { "Evo" } else { "Regular" },
        "proTxHash": dmn.pro_tx_hash.to_string(),
        "collateralHash": dmn.collateral.txid().to_string(),
        "collateralIndex": dmn.collateral.index(),
        "operatorReward": reward,
        "state": state,
    })
}

/// `list` as [`core_json`]'s objects by proTxHash.
fn ours(list: &MnList, key_io: &KeyIoParams) -> BTreeMap<String, Value> {
    list.iter().map(|dmn| (dmn.pro_tx_hash.to_string(), core_json(dmn, key_io))).collect()
}

/// dashd's `protx list registered true height` by proTxHash, without the fields [`core_json`]
/// leaves out.
fn dashd_list(h: &Harness, height: usize) -> BTreeMap<String, Value> {
    let list = h.dashd().call("protx", json!(["list", "registered", true, height]));
    let list = list.as_array().expect("an array").clone();
    list.into_iter()
        .map(|mut mn| {
            let o = mn.as_object_mut().expect("an object");
            for key in ["collateralAddress", "confirmations", "metaInfo", "wallet"] {
                o.remove(key);
            }
            (o["proTxHash"].as_str().expect("a hash").to_owned(), mn)
        })
        .collect()
}

/// Both nodes agree at `height`, whose tip must be `tip`: [`Harness::assert_agree_on_tip`], then
/// dashd's list at the height against V's at its tip. Returns V's list.
fn assert_agree(h: &Harness, tip: &BlockHash, height: usize, key_io: &KeyIoParams) -> Arc<MnList> {
    let (list, _) = h.assert_agree_on_tip(tip, height);
    assert_eq!(ours(&list, key_io), dashd_list(h, height), "protx list at {height}");
    list
}

type Signal = (bool, Arc<MnList>, Arc<MnListDiff>);

/// The MN-list notifications V sent (H7's `NotifyMasternodeListChanged`).
#[derive(Default)]
struct Lists(Mutex<Vec<Signal>>);

impl Lists {
    fn take(&self) -> Vec<Signal> {
        std::mem::take(&mut *self.0.lock().expect("lists"))
    }
}

impl ValidationInterface for Lists {
    fn notify_masternode_list_changed(
        &self,
        undo: bool,
        old: &Arc<MnList>,
        diff: &Arc<MnListDiff>,
    ) {
        self.0.lock().expect("lists").push((undo, Arc::clone(old), Arc::clone(diff)));
    }
}

/// Core's notification for V's move from the list `from` to the list `to`, if it sends one: `from`
/// and `from.BuildDiff(to)` when that has changes. A connect's
/// (`evo/deterministicmns.cpp:710-711`, `validation.cpp:2924-2927`), or a disconnect's, whose
/// stored forward diff must have changes too (`deterministicmns.cpp:795-808`,
/// `validation.cpp:2374-2377`).
// Core: evo/deterministicmns.cpp:710-711, :795-808; validation.cpp:2374-2377, :2924-2927
fn expected(
    undo: bool,
    from: &Arc<MnList>,
    to: &Arc<MnList>,
) -> Option<(bool, Arc<MnList>, String)> {
    let diff = from.diff_to(to);
    // A disconnect's stored diff is its connect's, from `to` to `from`.
    let sent = diff.has_changes() && (!undo || to.diff_to(from).has_changes());
    sent.then(|| (undo, Arc::clone(from), format!("{diff:?}")))
}

/// V's notifications are `want`, in order: each one's direction, old list (as a record, its
/// position included) and diff.
fn assert_signals(phase: &str, got: &[Signal], want: &[(bool, Arc<MnList>, String)]) {
    let at = |old: &Arc<MnList>| old.height();
    assert_eq!(
        got.iter().map(|s| (s.0, at(&s.1))).collect::<Vec<_>>(),
        want.iter().map(|w| (w.0, at(&w.1))).collect::<Vec<_>>(),
        "{phase}: the notifications' directions and old lists' heights"
    );
    for ((undo, old, diff), (_, want_old, want_diff)) in got.iter().zip(want) {
        assert!(old.same_record(want_old), "{phase}: the old list at {} (undo {undo})", at(old));
        assert_eq!(&format!("{diff:?}"), want_diff, "{phase}: the diff from {}", at(old));
    }
}

/// Whether `block` carries a quorum commitment (type 6), and whether every one it carries is
/// null.
fn commitments(block: &Block) -> (bool, bool) {
    let qcs: Vec<bool> = block
        .txs()
        .iter()
        .filter(|tx| tx.is_special_tx_version() && tx.tx_type() == TxType::QuorumCommitment)
        .map(|tx| match payload(tx) {
            Some(SpecialPayload::QuorumCommitment(p)) => p.commitment().is_null(),
            other => panic!("a commitment payload: {other:?}"),
        })
        .collect();
    (!qcs.is_empty(), qcs.iter().all(|&null| null))
}

/// The block at `height` holds the transaction `e` names, of type `kind`.
fn assert_holds(main: &[Arc<Block>], e: &Event, kind: Option<TxType>) {
    let block = &main[e.height - 1];
    let tx = block.txs().iter().find(|tx| tx.txid().to_string() == e.txid);
    let tx = tx.unwrap_or_else(|| panic!("{} not in {}", e.txid, e.height));
    let special = tx.is_special_tx_version().then(|| tx.tx_type());
    assert_eq!(special, kind, "{} at {}", e.txid, e.height);
}

/// Feeds the blocks to both nodes, comparing after each, then runs the invalidate below the
/// registrations and the reconsider back.
fn differential() {
    let Some(h) = Harness::start() else {
        return;
    };
    let key_io = key_io(&h.sut);
    let lists = Arc::new(Lists::default());
    assert!(h.sut.register_listener(lists.clone()));
    let main = blocks(MAIN);
    assert_eq!(main.len(), 300);
    // V's list at each height: `tip(h)`.
    let mut lists_at = Vec::new();
    for (i, b) in main.iter().enumerate() {
        h.submit(b, i + 1);
        lists_at.push(assert_agree(&h, &b.hash(), i + 1, &key_io));
    }
    let tip = |height: usize| &lists_at[height - 1];
    // Both masternodes are listed from 117 until the spend of the regular one's collateral.
    let (evo, spend) = (event("register_fund_evo").height, event("collateral_spend").height);
    let size = |height: usize| tip(height).len();
    assert_eq!((size(evo - 1), size(evo + 1), size(spend - 1), size(spend)), (0, 2, 2, 1));
    // Each connect from DIP0003 on, in order.
    let connects: Vec<_> =
        (DIP3..=300).filter_map(|height| expected(false, tip(height - 1), tip(height))).collect();
    assert!(connects.len() > 6, "registrations, updates and payments: {}", connects.len());
    assert_signals("the connects", &lists.take(), &connects);

    // `invalidateblock 116`: both walk back to 115, below the registrations, disconnecting from
    // the tip down.
    let cut = main[evo - 1].hash();
    h.dashd().call("invalidateblock", json!([cut.to_string()]));
    let chain = h.sut.chain().expect("V runs");
    assert_eq!(chain.invalidate_block(cut).recv(), Ok(Ok(())));
    assert_eq!(assert_agree(&h, &main[evo - 2].hash(), evo - 1, &key_io).len(), 0);
    let undos: Vec<_> = (evo..=300)
        .rev()
        .filter_map(|height| expected(true, tip(height), tip(height - 1)))
        .collect();
    assert_signals("the disconnects", &lists.take(), &undos);

    // `reconsiderblock 116`: both return to 300, through the same connects.
    h.dashd().call("reconsiderblock", json!([cut.to_string()]));
    let chain = h.sut.chain().expect("V runs");
    assert_eq!(chain.reconsider_block(cut, false).recv(), Ok(Ok(())));
    assert_eq!(assert_agree(&h, &main[299].hash(), 300, &key_io).len(), 1);
    let reconnects: Vec<_> =
        (evo..=300).filter_map(|height| expected(false, tip(height - 1), tip(height))).collect();
    assert_signals("the reconnects", &lists.take(), &reconnects);
}

/// The platform ban BL-526 plants: Core's `platformban` fields for `protx` at `height`, signed by
/// no quorum (the store does not verify it).
fn plant_ban(metaman: &MasternodeMetaMan, protx: &dash_dml::ProTxHash, height: i32) {
    use dash_types::codec::BaseCodec as _;
    let mut msg = Vec::with_capacity(164);
    msg.extend(protx.as_bytes());
    msg.extend(height.to_le_bytes());
    msg.extend([0x33; 32]);
    msg.extend([0u8; 96]);
    let ban = dash_p2p_core::PlatformBan::decode(&mut &msg[..]).expect("a ban message");
    assert!(metaman.set_platform_ban([0x5b; 32], protx, height, ban), "the ban takes");
}

/// BL-526 on V alone: blocks to the ProUpServTx's parent, a ban planted below it, then the
/// just-check, the connect, the disconnect and the reconnect of the ProUpServTx's block.
fn platform_ban_reset() {
    let dir = tempfile::tempdir().unwrap();
    let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
    let argv = ["-regtest", "-printtoconsole=0", ARGS[0], ARGS[1]];
    let sut = NodeSut::new(dir.path(), &argv, T0, ui).expect("the SUT starts");
    let chain = sut.chain().expect("V runs");
    let main = blocks(MAIN);
    let up = event("update_service_evo").height;
    let registered = event("register_fund_evo");
    let tx = main[registered.height - 1]
        .txs()
        .iter()
        .find(|tx| tx.txid().to_string() == registered.txid);
    let evo = dash_dml::ProTxHash::from(*tx.expect("the ProRegTx").txid().as_bytes());
    for (i, b) in main[..up - 1].iter().enumerate() {
        let r = chain.process_block(Arc::clone(b), true, None, true).recv().expect("V");
        assert!(r.accepted && r.state.is_valid(), "V at {}: {r:?}", i + 1);
    }
    let metaman = sut.metaman().expect("step 6 ran");
    let banned_at = (up - 10) as i32;
    plant_ban(metaman, &evo, banned_at);
    let banned = metaman.snapshot();
    let block = Arc::clone(&main[up - 1]);

    // `TestBlockValidity`, Core's one `fJustCheck` connect: no `ProcessBlock`, so no reset. V
    // runs it on Core's dummy index through the evo steps (MN0-2b-1): dashd's block is valid,
    // and the store is left as it was.
    let state = chain.test_block_validity(Arc::clone(&block), false, true).recv().expect("V");
    let state = state.expect("on the tip");
    assert!(state.is_valid(), "the just-check passes: {state:?}");
    assert_eq!(metaman.snapshot(), banned, "a just-check leaves the store");

    // The connect: `ResetPlatformBan(proTxHash, 141)`, as Core's would on its own store.
    let r = chain.process_block(Arc::clone(&block), true, None, true).recv().expect("V");
    assert!(r.accepted && r.state.is_valid(), "V at {up}: {r:?}");
    let reset = metaman.snapshot();
    let expected = MasternodeMetaMan::new();
    plant_ban(&expected, &evo, banned_at);
    assert!(expected.reset_platform_ban(&evo, up as i32));
    assert_eq!(reset, expected.snapshot(), "the connect resets the ban at {up}");
    let info = metaman.info(&evo);
    assert_eq!((info.platform_ban, info.platform_ban_updated), (false, up as i32));

    // `UndoBlock` has no meta step: the disconnect leaves the ban reset.
    assert_eq!(chain.invalidate_block(block.hash()).recv(), Ok(Ok(())));
    assert_eq!(sut.chain().expect("V runs").view().snap.tip.tip_hash, main[up - 2].hash());
    assert_eq!(metaman.snapshot(), reset, "a disconnect leaves the store");
    // The reconnect's reset is at the height the store already has: no change.
    assert_eq!(chain.reconsider_block(block.hash(), false).recv(), Ok(Ok(())));
    assert_eq!(sut.chain().expect("V runs").view().snap.tip.tip_hash, block.hash());
    assert_eq!(metaman.snapshot(), reset, "the reconnect leaves the store");
}

/// Under `sut::`, the gate's feature-step filter (`[package.metadata.gate]`).
mod sut {
    use super::*;

    /// The recording's shape, which the tests' claims rest on: each named transaction in its
    /// block with its type; from DIP0003 every block of `llmq_test`'s mining windows (58-66,
    /// then every 24) carries a commitment, all null, and no other block does.
    #[test]
    fn the_masternode_chain_has_its_transactions_and_null_commitments() {
        let main = blocks(MAIN);
        assert_eq!(main.len(), 300);
        for (kind, ty) in [
            ("register_fund_evo", Some(TxType::ProviderRegister)),
            ("register_fund", Some(TxType::ProviderRegister)),
            ("update_service_evo", Some(TxType::ProviderUpdateService)),
            ("update_registrar", Some(TxType::ProviderUpdateRegistrar)),
            ("revoke", Some(TxType::ProviderUpdateRevoke)),
            ("collateral_spend", None),
        ] {
            assert_holds(&main, &event(kind), ty);
        }
        let in_window = |h: usize| h >= DIP3 && (h - DIP3) % 24 >= 10 && (h - DIP3) % 24 <= 18;
        for (i, b) in main.iter().enumerate() {
            assert_eq!(commitments(b), (in_window(i + 1), true), "{}", i + 1);
        }
    }

    #[test]
    fn the_masternode_chain_equals_dashds_protx_lists() {
        differential();
    }

    #[test]
    fn a_proupservtx_through_the_node_resets_the_platform_ban() {
        platform_ban_reset();
    }
}
