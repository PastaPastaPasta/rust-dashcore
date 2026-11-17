use std::collections::BTreeMap;
use std::sync::Arc;

use dash_chain::BlockRef as TipRef;
use dash_dml::{Dmn, MnList, ProTxHash};
use dashd_chainstate::chain::ChainQuery;
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_rpc::commands::internal_bug;
use dashd_rpc::evo::{
    BlockArg, BlockFailure, BlockRef, EvoError, MetaFacts, MnListAt, MnListDiffFacts, MnListRequest,
};
use dashd_rpc::server::NodeControl;

use super::{ChainSlot, block_hash, block_index};
use crate::rpc::CoinsRead;

/// `GetListForBlock` at a height of the active chain (`protx list`) or at a block (`protx info`),
/// in one V turn, with the collateral's `GetUTXOConfirmations` at V's tip and the meta info of
/// the entries printed in full only ([`MnListRequest::detailed_entries`]). Without evo state the
/// list read refuses, after the argument checks; without the server (shutdown) there is no clock
/// to print elapsed times against, and the read refuses.
// Core: rpc/evo.cpp:1724-1735, :1802-1836
pub(super) fn list(slot: &ChainSlot, request: MnListRequest) -> Result<MnListAt, EvoError> {
    let writer = slot.slot().clone().ok_or(EvoError::Unavailable)?;
    let control = node_control(slot);
    let coins = writer.coins.clone();
    let turn_control = control.clone();
    let read = writer.chain.query(move |q| {
        let (tip, _) = q.chain.private_tip().ok_or(EvoError::Unavailable)?;
        let tip_height = tip.height as i32;
        let (id, height) = match request {
            MnListRequest::Height {
                height,
                ..
            } => {
                let height = height.unwrap_or(tip_height);
                if !(1..=tip_height).contains(&height) {
                    return Err(EvoError::InvalidHeight);
                }
                let id = q.chain.ancestor_private(height as u32).ok_or(EvoError::InvalidHeight)?;
                (id, height)
            }
            MnListRequest::Info {
                block: Some(hash),
                ..
            } => {
                let arg = BlockArg {
                    name: "blockHash",
                    value_str: hash.to_string(),
                    syntax: Ok(BlockRef::Hash(hash)),
                };
                // Core: rpc/evo.cpp:1814-1822
                let entry = block_index(q, &arg, hash, false)?;
                if !q.chain.active_contains_private(entry.id) {
                    return Err(EvoError::NotInMainChain);
                }
                (entry.id, entry.height as i32)
            }
            MnListRequest::Info {
                block: None,
                ..
            } => (tip.id, tip_height),
        };
        let list = q.evo_view().list_for_block(id)?;
        let block = q.chain.reader().entry(id).ok_or(EvoError::Unavailable)?.hash;
        let confirmations = confirmations(coins.as_ref(), tip, &request.detailed_entries(&list))?;
        // Core's `dmn.ToJson()` runs before this check, and its `GetPlatformPort` check could
        // fail first under -deprecatedrpc=service; no Evo entry that passed `Validate` reaches it.
        if let MnListRequest::Info {
            pro_tx_hash,
            ..
        } = request
            && confirmations.get(&pro_tx_hash) == Some(&-1)
        {
            let control = turn_control.ok_or(EvoError::Unavailable)?;
            return Err(spent_collateral(control.as_ref()));
        }
        Ok((height, block, list, tip_height, confirmations))
    });
    let (height, block, list, tip_height, confirmations) =
        read.recv().map_err(|_| EvoError::Unavailable)??;
    let now = control.ok_or(EvoError::Unavailable)?.now();
    let meta = meta(slot.meta.get(), &request.detailed_entries(&list));
    Ok(MnListAt {
        height,
        block,
        list,
        tip_height,
        confirmations,
        meta,
        now,
    })
}

/// The node services of the server the slot serves: the mockable clock and the identity.
fn node_control(slot: &ChainSlot) -> Option<Arc<dyn NodeControl>> {
    let server = slot.server.lock().unwrap_or_else(|p| p.into_inner()).upgrade()?;
    Some(Arc::clone(server.control()))
}

/// `GetUTXOConfirmations` of each masternode's collateral: the tip height less the coin's, plus
/// one; -1 for a spent or unknown coin. The coins read runs in the same V turn as `tip`.
// Core: validation.cpp:343-349
fn confirmations(
    coins: Option<&CoinsRead>,
    tip: TipRef,
    listed: &[&Arc<Dmn>],
) -> Result<BTreeMap<ProTxHash, i32>, EvoError> {
    if listed.is_empty() {
        return Ok(BTreeMap::new());
    }
    let coins = coins.ok_or(EvoError::Unavailable)?;
    let tip_height = tip.height as i32;
    listed
        .iter()
        .map(|dmn| {
            let (coin, at) = coins(&dmn.collateral)
                .map_err(|_| EvoError::Failed(dashd_rpc::blockchain::ABORTED.into()))?;
            debug_assert_eq!(at.height, tip.height, "the coins read is at V's tip");
            let confirmations = coin.map_or(-1, |coin| tip_height - coin.height() as i32 + 1);
            Ok((dmn.pro_tx_hash, confirmations))
        })
        .collect()
}

/// `GetInfo(proTxHash)` of each listed masternode: a default entry for one the store lacks.
// Core: masternode/meta.cpp:77-81
fn meta(
    meta: Option<&Arc<MasternodeMetaMan>>,
    listed: &[&Arc<Dmn>],
) -> BTreeMap<ProTxHash, MetaFacts> {
    let Some(meta) = meta else {
        return BTreeMap::new();
    };
    listed
        .iter()
        .map(|dmn| {
            let info = meta.info(&dmn.pro_tx_hash);
            let facts = MetaFacts {
                last_dsq: info.last_dsq,
                mixing_tx_count: info.mixing_tx_count,
                outbound_attempt_count: info.outbound_attempt_count,
                last_outbound_attempt: info.last_outbound_attempt,
                last_outbound_success: info.last_outbound_success,
                is_platform_banned: info.platform_ban,
                platform_ban_height_updated: info.platform_ban_updated,
            };
            (dmn.pro_tx_hash, facts)
        })
        .collect()
}

/// `protx info`'s `BuildDMNListEntry` at a block for spent collateral: without txindex
/// `GetTransaction` finds nothing, so `CHECK_NONFATAL(pindexMined != nullptr)` fails.
// Core: rpc/evo.cpp:1606-1610
fn spent_collateral(control: &dyn NodeControl) -> EvoError {
    EvoError::Param(internal_bug(
        control,
        "pindexMined != nullptr",
        file!(),
        line!(),
        "BuildDMNListEntry",
    ))
}

/// `protx listdiff`'s reads in one V turn: `ParseBlockIndex` for the base, then for the block,
/// both in the active chain, then both lists and `BuildDiff` in internal-id order.
// Core: rpc/evo.cpp:1955-2003
pub(super) fn list_diff(
    slot: &ChainSlot,
    base: BlockArg,
    block: BlockArg,
) -> Result<MnListDiffFacts, EvoError> {
    let writer = slot.slot().clone().ok_or(EvoError::Unavailable)?;
    writer
        .chain
        .query(move |q| {
            // Core: rpc/evo.cpp:1861
            let resolve = |arg: &BlockArg| {
                block_hash(q, arg)
                    .and_then(|hash| block_index(q, arg, hash, false))
                    .map_err(|_| arg.failure(BlockFailure::BadArg))
            };
            let base = resolve(&base)?;
            let block = resolve(&block)?;
            if !q.chain.active_contains_private(base.id)
                || !q.chain.active_contains_private(block.id)
            {
                return Err(EvoError::NotInMainChain);
            }
            diff_facts(q, base.id, block.id, base.height as i32, block.height as i32)
        })
        .recv()
        .map_err(|_| EvoError::Unavailable)?
}

fn diff_facts(
    q: &ChainQuery<'_>,
    base: dash_consensus::BlockId,
    block: dash_consensus::BlockId,
    base_height: i32,
    block_height: i32,
) -> Result<MnListDiffFacts, EvoError> {
    let view = q.evo_view();
    let base_list = view.list_for_block(base)?;
    let list = view.list_for_block(block)?;
    facts_between(&base_list, &list, base_height, block_height)
}

/// `BuildDiff` from `base_list` to `list`, each array in internal-id order: removed entries
/// from `base_list`, added and updated ones from `list`.
///
/// Deliberate divergence from Core: an internal id missing from its list answers a store error
/// (`EvoError::Store`), where Core's `CHECK_NONFATAL(dmn)` answers an internal bug. Only a
/// corrupt list reaches it, and it is never an abort.
// Core: rpc/evo.cpp:1972-1996
fn facts_between(
    base_list: &MnList,
    list: &MnList,
    base_height: i32,
    block_height: i32,
) -> Result<MnListDiffFacts, EvoError> {
    let diff = base_list.diff_to(list);
    let missing = |id: u64| EvoError::Store(format!("masternode internal id {id} not in its list"));
    let removed = diff
        .removed
        .iter()
        .map(|id| base_list.get_by_internal_id(*id).map(|dmn| dmn.pro_tx_hash).ok_or(missing(*id)))
        .collect::<Result<_, _>>()?;
    let updated = diff
        .updated
        .into_iter()
        .map(|(id, state)| {
            list.get_by_internal_id(id).map(|dmn| (Arc::clone(dmn), state)).ok_or(missing(id))
        })
        .collect::<Result<_, _>>()?;
    Ok(MnListDiffFacts {
        base_height,
        block_height,
        added: diff.added,
        removed,
        updated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::Chain;
    use crate::chain::tests::{fresh, imported, regtest, startup_over};
    use crate::logging::Logger;
    use crate::rpc::{DeprecatedRpc, command_table};
    use crate::scheduler::Scheduler;
    use crate::shutdown::ShutdownState;
    use dash_clock::{Clock, MockClock};
    use dash_consensus::BlockId;
    use dash_consensus::coins::Coin;
    use dash_primitives::{BlockHash, OutPoint};
    use dashd_chainstate::chain::{MemBlockFiles, NoFlush, NullConnector, RecordingAbort};
    use dashd_rpc::UniValue;
    use dashd_rpc::evo::{MnListType, NodeEvo};
    use dashd_rpc::protocol::{RPC_INVALID_ADDRESS_OR_KEY, RpcError};
    use dashd_rpc::request::JsonRpcRequest;

    /// A masternode at `1.2.3.<pro_tx>` whose collateral is output `index` of `[2; 32]`.
    fn dmn(pro_tx: u8, index: u32) -> Arc<Dmn> {
        let mut bytes = vec![pro_tx; 32];
        bytes.push(pro_tx);
        bytes.extend([2; 32]);
        bytes.extend(index.to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        for n in [2i32, 1, 0, 0, 0, -1, -1] {
            bytes.extend(n.to_le_bytes());
        }
        bytes.extend(0u16.to_le_bytes());
        bytes.extend([0; 64]);
        bytes.extend([pro_tx; 20]);
        bytes.extend([0; 48 + 20]);
        bytes.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 1, 2, 3, pro_tx]);
        bytes.extend(19999u16.to_be_bytes());
        bytes.extend([0; 2 + 20]);
        bytes.extend(0u16.to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        let mut input = bytes.as_slice();
        let dmn = dash_dml::codec::core_disk::decode_dmn(&mut input).unwrap();
        assert!(input.is_empty());
        Arc::new(dmn)
    }

    fn tip() -> TipRef {
        TipRef {
            id: BlockId(10),
            hash: BlockHash::from([10; 32]),
            height: 10,
        }
    }

    fn aborted() -> CoinsRead {
        Arc::new(|_: &OutPoint| Err(dashd_rpc::blockchain::Aborted))
    }

    #[test]
    fn confirmations_count_from_the_tip_and_spent_collateral_is_minus_one() {
        let coins: CoinsRead = Arc::new(|out: &OutPoint| {
            let coin = (out.index() == 0).then(|| Coin::new(Default::default(), &[], 4, false));
            Ok((coin, tip()))
        });
        let (unspent, spent) = (dmn(1, 0), dmn(3, 1));
        let got = confirmations(Some(&coins), tip(), &[&unspent, &spent]).unwrap();
        assert_eq!(got[&unspent.pro_tx_hash], 7);
        assert_eq!(got[&spent.pro_tx_hash], -1);
        assert!(confirmations(None, tip(), &[]).unwrap().is_empty());
        assert_eq!(confirmations(None, tip(), &[&unspent]).unwrap_err(), EvoError::Unavailable);
        assert!(matches!(
            confirmations(Some(&aborted()), tip(), &[&unspent]).unwrap_err(),
            EvoError::Failed(_)
        ));
    }

    /// A list of masternodes 1 (Regular, banned) and 2 (Evo).
    fn two_listed() -> MnList {
        let mut list = MnList::new(BlockHash::from([9; 32]), 9, 0);
        let mut banned = (*dmn(1, 0)).clone();
        banned.hot.ban_if_not_banned(5);
        list.try_add(Arc::new(banned), true).unwrap();
        let evo = Dmn {
            mn_type: dash_evo::types::RawMnType(1),
            ..(*dmn(2, 1)).clone()
        };
        list.try_add(Arc::new(evo), true).unwrap();
        list
    }

    fn height(mn_type: MnListType, detailed: bool) -> MnListRequest {
        MnListRequest::Height {
            height: None,
            mn_type,
            detailed,
        }
    }

    /// Core reads no collateral for a non-detailed list, so a failing coins read cannot fail it;
    /// a detailed list reads the entries of its type only.
    #[test]
    fn non_detailed_list_reads_no_coins() {
        let list = two_listed();
        for mn_type in [MnListType::Registered, MnListType::Valid, MnListType::Evo] {
            let entries = height(mn_type, false).detailed_entries(&list);
            assert!(entries.is_empty());
            assert!(confirmations(Some(&aborted()), tip(), &entries).unwrap().is_empty());
            assert!(confirmations(None, tip(), &entries).unwrap().is_empty());
        }
        let ids = |request: MnListRequest| {
            request.detailed_entries(&list).iter().map(|d| d.pro_tx_hash).collect::<Vec<_>>()
        };
        let [banned, evo] = [dmn(1, 0).pro_tx_hash, dmn(2, 1).pro_tx_hash];
        assert_eq!(ids(height(MnListType::Registered, true)).len(), 2);
        assert_eq!(ids(height(MnListType::Valid, true)), [evo]);
        assert_eq!(ids(height(MnListType::Evo, true)), [evo]);
        assert!(matches!(
            confirmations(
                Some(&aborted()),
                tip(),
                &height(MnListType::Valid, true).detailed_entries(&list)
            )
            .unwrap_err(),
            EvoError::Failed(_)
        ));
        let info = |pro_tx_hash| MnListRequest::Info {
            pro_tx_hash,
            block: None,
        };
        assert_eq!(ids(info(banned)), [banned]);
        assert!(ids(info(ProTxHash::from([7; 32]))).is_empty());
    }

    /// `protx info` builds meta for its one masternode; unknown ones default.
    #[test]
    fn info_reads_meta_for_one_masternode() {
        let list = two_listed();
        let banned = dmn(1, 0).pro_tx_hash;
        let request = MnListRequest::Info {
            pro_tx_hash: banned,
            block: None,
        };
        assert!(meta(None, &request.detailed_entries(&list)).is_empty());
        let man = Arc::new(MasternodeMetaMan::new());
        let got = meta(Some(&man), &request.detailed_entries(&list));
        assert_eq!(got.keys().copied().collect::<Vec<_>>(), [banned]);
        assert_eq!(got[&banned], MetaFacts::default());
        assert!(
            meta(Some(&man), &height(MnListType::Registered, false).detailed_entries(&list))
                .is_empty()
        );
    }

    /// `BuildDiff`'s arrays follow internal-id order, which here differs from proTxHash order in
    /// each of them.
    #[test]
    fn listdiff_arrays_follow_internal_id_order() {
        let with_id = |pro_tx: u8, id: u64| {
            Arc::new(Dmn {
                internal_id: id,
                ..(*dmn(pro_tx, u32::from(pro_tx))).clone()
            })
        };
        let mut base = MnList::new(BlockHash::from([1; 32]), 1, 0);
        for (pro_tx, id) in [(1, 3), (2, 1), (3, 2), (4, 4), (5, 0)] {
            base.try_add(with_id(pro_tx, id), true).unwrap();
        }
        let mut list = base.clone();
        for pro_tx in [3, 5] {
            list.remove(&dmn(pro_tx, 0).pro_tx_hash).unwrap();
        }
        for pro_tx in [1, 2] {
            let hash = dmn(pro_tx, 0).pro_tx_hash;
            let mut hot = list.get(&hash).unwrap().hot;
            hot.last_paid_height = 9;
            list.update_hot(&hash, hot).unwrap();
        }
        for (pro_tx, id) in [(6, 6), (7, 5)] {
            list.try_add(with_id(pro_tx, id), true).unwrap();
        }
        let facts = facts_between(&base, &list, 1, 2).unwrap();
        let hash = |pro_tx: u8| dmn(pro_tx, 0).pro_tx_hash;
        assert_eq!(
            facts.added.iter().map(|d| d.pro_tx_hash).collect::<Vec<_>>(),
            [hash(7), hash(6)]
        );
        assert_eq!(facts.removed, [hash(5), hash(3)]);
        assert_eq!(
            facts.updated.iter().map(|(d, _)| d.pro_tx_hash).collect::<Vec<_>>(),
            [hash(2), hash(1)]
        );
    }

    /// A node with no evo state: `protx list`, `protx info` and `protx listdiff` answer their
    /// argument and block lookup errors as Core does, in one V query turn (no `OnV` turn to
    /// drop), and read Core's empty pre-DIP3 list.
    #[test]
    fn no_evo_state_answers_argument_and_lookup_errors_in_core_order() {
        let params = regtest();
        let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_296_688_662));
        let abort = Arc::new(RecordingAbort::default());
        let mut v =
            Chain::start(fresh(&params, &clock, &abort, Box::new(MemBlockFiles::default())))
                .unwrap();
        startup_over(&mut v, Box::new(NullConnector), Box::new(NoFlush));
        imported(&mut v);
        let slot = Arc::new(ChainSlot::default());
        let table = command_table(
            &params,
            &Arc::new(Logger::new()),
            &Arc::new(ShutdownState::new()),
            &Arc::new(MockClock::at(1_700_000_000)),
            (true, &Scheduler::new()),
            &slot,
            &Arc::default(),
            &Arc::default(),
            &Arc::default(),
            DeprecatedRpc::default(),
            "regtest".into(),
        );
        table.start();
        table.set_warmup_finished();
        slot.attach(v.handle(), v.signals(), None, None, None);
        let genesis = v.handle().view().snap.tip.tip_hash;
        let unknown = BlockHash::from([0xcd; 32]);
        let pro_tx_hash = ProTxHash::from([1; 32]);
        let arg = |name, hash: BlockHash| BlockArg {
            name,
            value_str: hash.to_string(),
            syntax: Ok(BlockRef::Hash(hash)),
        };
        for height in [None, Some(0), Some(1)] {
            assert_eq!(
                slot.mn_list(MnListRequest::Height {
                    height,
                    mn_type: MnListType::Registered,
                    detailed: true
                })
                .unwrap_err(),
                EvoError::InvalidHeight
            );
        }
        assert_eq!(
            slot.mn_list(MnListRequest::Info {
                pro_tx_hash,
                block: Some(unknown)
            })
            .unwrap_err(),
            arg("blockHash", unknown).failure(BlockFailure::NotFound(unknown))
        );
        // Before DIP0003 every list is Core's empty one, which no evo state is needed for.
        for block in [None, Some(genesis)] {
            let at = slot
                .mn_list(MnListRequest::Info {
                    pro_tx_hash,
                    block,
                })
                .unwrap();
            assert_eq!((at.height, at.block, at.list.len()), (0, genesis, 0));
            assert_eq!(at.now, 1_700_000_000);
        }
        assert_eq!(
            slot.mn_list_diff(arg("baseBlock", unknown), arg("block", genesis)).unwrap_err(),
            arg("baseBlock", unknown).failure(BlockFailure::BadArg)
        );
        assert_eq!(
            slot.mn_list_diff(arg("baseBlock", genesis), arg("block", unknown)).unwrap_err(),
            arg("block", unknown).failure(BlockFailure::BadArg)
        );
        let height = |name| BlockArg {
            name,
            value_str: "1".into(),
            syntax: Ok(BlockRef::Height(1)),
        };
        assert_eq!(
            slot.mn_list_diff(height("baseBlock"), arg("block", genesis)).unwrap_err(),
            height("baseBlock").failure(BlockFailure::BadArg)
        );
        let diff = slot.mn_list_diff(arg("baseBlock", genesis), arg("block", genesis)).unwrap();
        assert_eq!((diff.base_height, diff.block_height), (0, 0));
        assert!(diff.added.is_empty() && diff.removed.is_empty() && diff.updated.is_empty());
        let call = |method: &str, params: &str| {
            table
                .execute(&JsonRpcRequest {
                    method: method.into(),
                    params: UniValue::read(params.as_bytes()).unwrap(),
                    ..Default::default()
                })
                .unwrap_err()
        };
        let hex = "cd".repeat(32);
        assert_eq!(
            call("protx info", &format!(r#"["{}","{hex}"]"#, "01".repeat(32))).to_object(),
            RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found").to_object()
        );
        assert_eq!(
            call("protx listdiff", &format!(r#"["{hex}","{genesis}"]"#)).to_object(),
            RpcError::Runtime(format!(
                "baseBlock must be a block hash or chain height and not {hex}"
            ))
            .to_object()
        );
        assert_eq!(
            call("protx list", "[]").to_object(),
            RpcError::new(-8, "invalid height specified").to_object()
        );
        let unknown_mn = "01".repeat(32);
        assert_eq!(
            call("protx info", &format!(r#"["{unknown_mn}","{genesis}"]"#)).to_object(),
            RpcError::new(-8, format!("{unknown_mn} not found")).to_object()
        );
        let EvoError::Param(RpcError::Runtime(text)) = spent_collateral(table.control().as_ref())
        else {
            panic!()
        };
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("Internal bug detected: pindexMined != nullptr"));
        assert!(lines.next().unwrap().ends_with(" (BuildDMNListEntry)"));
        assert!(lines.next().unwrap().starts_with(&format!("{} ", crate::PACKAGE_NAME)));
        assert_eq!(
            lines.next(),
            Some(format!("Please report this issue here: {}", dashd_rpc::BUG_REPORT_URL).as_str())
        );
        assert!(text.ends_with('\n'));
        slot.detach();
        assert!(v.stop().is_some_and(|exit| exit.fatal.is_none()));
        assert!(abort.messages().is_empty());
    }
}
