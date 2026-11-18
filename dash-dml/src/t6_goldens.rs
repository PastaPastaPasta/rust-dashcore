//! T6 goldens (D2), recorded on base-sdk 29e01c83 under `test-vectors/t6/` and asserted
//! unchanged after the operator-key re-pin.
//!
//! Building with `T6_GOLDENS_RECORD` set rewrites the files instead of comparing.

use std::path::PathBuf;

/// `test-vectors/t6/<name>`.
fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test-vectors/t6").join(name)
}

/// Asserts `actual` is the golden `name`, or records it in a build with `T6_GOLDENS_RECORD` set.
pub(crate) fn golden(name: &str, actual: &str) {
    let path = path(name);
    if option_env!("T6_GOLDENS_RECORD").is_some() {
        std::fs::write(&path, actual).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        return;
    }
    let expected =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if expected != actual {
        let (line, (want, got)) = expected
            .lines()
            .map(Some)
            .chain(std::iter::repeat(None))
            .zip(actual.lines().map(Some).chain(std::iter::repeat(None)))
            .enumerate()
            .find(|(_, (want, got))| want != got)
            .expect("the texts differ");
        panic!(
            "{name} differs from its golden at line {}:\n  golden: {}\n  actual: {}",
            line + 1,
            want.unwrap_or("<end>"),
            got.unwrap_or("<end>"),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fmt::Write as _;

    use dash_consensus::primitives::{Block, Tx, TxRef, core_serialize, decode_block};
    use dash_consensus::subsidy::COIN;
    use dash_params::{ChainParamsBuilder, Network, Release};
    use std::sync::Arc;

    use dash_pkc::bls::{BlsLazyPk, BlsSchemeKind};
    use dash_primitives::{
        MnType as PayloadMnType, PlatformNodeId, ProRegTx, ProTxVersion, ProUpRegTx,
        ProUpSharedRegTx, SpecialPayload, TxHash, TxType,
    };
    use dash_types::Signable as _;

    use super::golden;
    use crate::build::pro_tx_of;
    use crate::codec::{core_disk, state_bytes};
    use crate::sml::merkle_root_mn_list_full;
    use crate::specialtx::testctx::{encoded, tx_of, txin, txout};
    use crate::testkit::operator_key;
    use crate::testkit::replay::{hex, read_vectors, replay};
    use crate::testkit::window::Window;
    use crate::unique::{NetProp, Prop, StateKeys, UniqueKey};
    use crate::{Dmn, MnList, NoTrace, ProTxHash};

    const VECTORS: &str = include_str!("../tests/data/ev2b-quirks.txt");

    /// The masternode a transaction carrying an operator key registers or updates.
    fn touched(tx: &Tx) -> Option<(&'static str, ProTxHash)> {
        let Some(Ok(decoded)) = tx.payload() else {
            return None;
        };
        Some(match decoded.payload() {
            SpecialPayload::ProviderRegister(_) => ("ProRegTx", pro_tx_of(&tx.txid())),
            SpecialPayload::ProviderUpdateRegistrar(p) => {
                ("ProUpRegTx", pro_tx_of(&p.pro_tx_hash()))
            }
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
                ("ProUpSharedRegTx", pro_tx_of(&p.pro_tx_hash()))
            }
            _ => return None,
        })
    }

    /// The masternode's unique-property keys as `AddMN` adds them, non-null ones only, each
    /// owned by the masternode in `list`.
    fn unique_keys(list: &MnList, dmn: &Dmn) -> BTreeSet<String> {
        let keys = StateKeys::build(&dmn.cold);
        let net = keys.net.iter().filter_map(|n| match n {
            NetProp::Addr {
                prop,
                ..
            } => Some(prop),
            NetProp::Invalid => None,
        });
        let mut props: Vec<_> = net.chain(keys.owners.iter()).chain([&keys.operator]).collect();
        if dmn.mn_type.is_evo() {
            props.push(&keys.platform);
        }
        props
            .into_iter()
            .filter(|p| !p.null)
            .map(|p| {
                let owner = list.unique_map().get(&p.key).map(|s| s.owner);
                assert_eq!(owner, Some(dmn.pro_tx_hash), "the masternode owns its key");
                hex(p.key.as_bytes())
            })
            .collect()
    }

    /// Every non-null unique-property key `AddMN` would put in `list`, for every masternode. The
    /// list's own unique map holds exactly these keys: each is in the map, and the counts agree.
    fn list_unique_keys(list: &MnList) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        let mut put = |key: &UniqueKey| {
            assert!(
                list.unique_map().contains(key),
                "the unique map holds {}",
                hex(key.as_bytes())
            );
            keys.insert(hex(key.as_bytes()));
        };
        for dmn in list.iter() {
            let collateral = Prop::collateral(&dmn.collateral);
            if !collateral.null {
                put(&collateral.key);
            }
            let state = StateKeys::build(&dmn.cold);
            for entry in state.net.iter() {
                if let NetProp::Addr {
                    prop,
                    ..
                } = entry
                    && !prop.null
                {
                    put(&prop.key);
                }
            }
            for owner in state.owners.iter().filter(|p| !p.null) {
                put(&owner.key);
            }
            if !state.operator.null {
                put(&state.operator.key);
            }
            if dmn.mn_type.is_evo() && !state.platform.null {
                put(&state.platform.key);
            }
        }
        assert_eq!(list.unique_map().len(), keys.len(), "no other key is in the unique map");
        keys
    }

    /// The touched masternodes of `txs` in `list` (built over `prev`): `MnCold` codec bytes, SML
    /// entry bytes and unique-property keys, after the list's merkleRootMNList.
    fn record(
        out: &mut String,
        classes: &mut BTreeMap<&'static str, usize>,
        label: &str,
        prev: &MnList,
        list: &MnList,
        txs: &[TxRef],
    ) {
        let mns: BTreeMap<_, _> =
            txs.iter().filter_map(|tx| touched(tx)).map(|(kind, h)| (h, kind)).collect();
        if mns.is_empty() {
            return;
        }
        let root = merkle_root_mn_list_full(list).expect("a root");
        writeln!(out, "{label} root {root}").unwrap();
        let all: Vec<_> = list_unique_keys(list).into_iter().collect();
        writeln!(out, "list-unique {}", all.join(",")).unwrap();
        for (pro_tx, tx_kind) in mns {
            let Some(dmn) = list.get(&pro_tx) else {
                continue;
            };
            let version = dmn.cold.version;
            let class = if dmn.cold.is_shared() {
                "shared"
            } else if dmn.mn_type.is_evo() {
                "evo"
            } else if prev.get(&pro_tx).is_some_and(|d| d.cold.version != version) {
                "migrated"
            } else if version == 1 {
                "legacy"
            } else {
                "basic"
            };
            *classes.entry(class).or_default() += 1;
            let mut sml = Vec::new();
            dmn.to_sml_entry().write_hashed(&mut sml);
            let unique: Vec<_> = unique_keys(list, dmn).into_iter().collect();
            writeln!(out, "mn {} {tx_kind} {class} v{version}", hex(pro_tx.as_bytes())).unwrap();
            writeln!(out, "  state {}", hex(&state_bytes(dmn))).unwrap();
            writeln!(out, "  sml {}", hex(&sml)).unwrap();
            writeln!(out, "  unique {}", unique.join(",")).unwrap();
        }
    }

    /// A version-2 ProUpRegTx moving the `nth` legacy masternode of `list` to a basic key: a new
    /// one, or with `same_key` its own key's point in the basic encoding.
    fn migration(list: &MnList, nth: usize, same_key: bool) -> TxRef {
        let dmn = list
            .iter()
            .filter(|d| d.cold.version == 1 && !d.cold.is_shared() && !d.mn_type.is_evo())
            .nth(nth)
            .expect("a legacy masternode");
        let key = if same_key {
            BlsLazyPk::from_value(dmn.cold.pub_key_operator.clone().get(), BlsSchemeKind::Basic)
        } else {
            operator_key(0x71, BlsSchemeKind::Basic)
        };
        let mut p = ProUpRegTx::new(
            ProTxVersion::BasicBls,
            TxHash::from(*dmn.pro_tx_hash.as_bytes()),
            0,
            key,
            dmn.cold.key_id_voting,
            dmn.cold.script_payout.clone().into(),
            Vec::new(),
            Default::default(),
        );
        p.reset_signature();
        let inputs = vec![txin([0x77; 32], 0)];
        Arc::new(tx_of(TxType::ProviderUpdateRegistrar, inputs, Vec::new(), encoded(&p)))
    }

    /// A version-3 Evo registration with an internal 4000 DASH collateral.
    fn evo_registration() -> TxRef {
        let p = crate::specialtx::trivial::tests::pro_reg_v3();
        let p = ProRegTx::new(
            p.version(),
            PayloadMnType::Evo,
            p.mode(),
            Default::default(),
            0,
            p.net_info().clone(),
            p.key_id_owner(),
            p.pub_key_operator().clone(),
            p.key_id_voting(),
            p.operator_reward(),
            p.script_payout().clone(),
            p.payouts().clone(),
            p.shares().clone(),
            p.join_sigs().clone(),
            p.early_period_blocks(),
            p.early_penalty(),
            p.inputs_hash(),
            Some(PlatformNodeId::from([0x77; 20])),
            p.platform_p2p_port(),
            p.platform_http_port(),
        );
        let inputs = vec![txin([0x78; 32], 0)];
        let outputs = vec![txout(4000 * COIN, vec![0x51])];
        Arc::new(tx_of(TxType::ProviderRegister, inputs, outputs, encoded(&p)))
    }

    /// A ProUpSharedRegTx moving the first shared masternode of `list` to a new basic key and a new
    /// voting key.
    fn shared_key_change(list: &MnList) -> Option<TxRef> {
        let dmn = list.iter().find(|d| d.cold.is_shared())?;
        let p = ProUpSharedRegTx::new(
            1,
            TxHash::from(*dmn.pro_tx_hash.as_bytes()),
            operator_key(0x72, BlsSchemeKind::Basic),
            crate::types::KeyId::from([0x73; 20]),
            Default::default(),
            Vec::new(),
        );
        let inputs = vec![txin([0x79; 32], 0)];
        Some(Arc::new(tx_of(
            TxType::ProviderUpdateSharedRegistrar,
            inputs,
            Vec::new(),
            encoded(&p),
        )))
    }

    /// T6 D2: every masternode a provider transaction touches, and the list's merkleRootMNList,
    /// at each block with one, in dash-dml's fixtures: the EV2b quirk vectors (regtest blocks
    /// over Core-recorded lists, accepted ones) and the first four such blocks of the mainnet
    /// build windows w1 and w2 (legacy registrations); then test-built transactions over them
    /// (see `test-vectors/t6/PROVENANCE.md`).
    #[test]
    fn t6_list_goldens() {
        let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .build()
            .expect("Core's parameters");
        let mut out = String::new();
        let mut classes = BTreeMap::new();
        let mut shared_changed = false;
        for v in read_vectors(VECTORS).expect("the vector file") {
            let ours = replay(&params, &v.ctx, &v.block, &mut NoTrace).expect("a replay");
            let Ok(list) = ours.result else {
                continue;
            };
            let prev = core_disk::decode_list_s3(&mut &v.ctx.list[..]).expect("Core's list");
            let block = decode_block(&mut &v.block[..]).expect("the block").into_value();
            record(&mut out, &mut classes, &format!("ev2b {}", v.name), &prev, &list, block.txs());
            if shared_changed {
                continue;
            }
            let Some(tx) = shared_key_change(&list) else {
                continue;
            };
            let txs = block.txs().iter().cloned().chain([tx.clone()]).collect();
            let block = core_serialize(&Block::new(*block.header(), txs));
            let ours = replay(&params, &v.ctx, &block, &mut NoTrace).expect("a replay");
            let changed = ours.result.expect("the shared operator change builds");
            let label = format!("ev2b {} shared operator change", v.name);
            record(&mut out, &mut classes, &label, &prev, &changed, &[tx]);
            shared_changed = true;
        }
        assert!(shared_changed, "no shared masternode to change");
        let windows: [(&str, &[u8]); 2] = [
            ("w1", include_bytes!("../benches/data/mainnet-dml-1028160-1030159.bin")),
            ("w2", include_bytes!("../benches/data/mainnet-dml-1371028-1373027.bin")),
        ];
        for (name, bytes) in windows {
            let window = Window::decode(bytes).expect("the window");
            let last = window.blocks().len() - 1;
            let mut prev = window.start().clone();
            let mut before_last = None;
            let mut kept = 0;
            window
                .build(window.start().clone(), &mut NoTrace, |i, list| {
                    let txs = window.blocks()[i].txs();
                    if kept < 4 && txs.iter().any(|tx| touched(tx).is_some()) {
                        let label = format!("{name} {}", window.height(i));
                        record(&mut out, &mut classes, &label, &prev, list, txs);
                        kept += 1;
                    }
                    if i == last {
                        before_last = Some(prev.clone());
                    }
                    prev = list.clone();
                })
                .expect("the window builds");
            let base = before_last.expect("the last block's list");
            let built = [
                ("migration", migration(&base, 0, false), false),
                ("same-key migration under v24", migration(&base, 1, true), true),
                ("evo registration under v24", evo_registration(), true),
            ];
            for (what, tx, v24) in built {
                let list = window
                    .build_block_with(last, &base, vec![tx.clone()], v24)
                    .unwrap_or_else(|e| panic!("{what}: {e:?}"));
                if let Some((_, pro_tx)) = touched(&tx).filter(|_| what.starts_with("same-key")) {
                    let (old, new) = (base.get(&pro_tx).unwrap(), list.get(&pro_tx).unwrap());
                    assert!(old.cold.pub_key_operator == new.cold.pub_key_operator, "{what}");
                    assert_eq!((old.hot, new.cold.version), (new.hot, 2), "{what}");
                }
                let label = format!("{name} {} {what}", window.height(last));
                record(&mut out, &mut classes, &label, &base, &list, &[tx]);
            }
        }
        for class in ["legacy", "basic", "migrated", "shared", "evo"] {
            assert!(classes.contains_key(class), "no {class} masternode: {classes:?}");
        }
        golden("dml-lists.txt", &out);
    }
}
