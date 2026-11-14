use dash_dml::MnType;
use dash_dml::testkit::{dmn_with, operator_key};
use dash_netaddr::NetAddr;
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{NetInfo, NetInfoV1};
use dash_types::codec::BaseCodec;

use super::*;

fn service(last: u8) -> Service {
    Service::new(NetAddr::ipv4([1, 2, 3, last]), 9999)
}

fn entry(n: u8, op: u8, addr: u8, banned: bool) -> Arc<Dmn> {
    dmn_with(n, MnType::Regular, |d, c| {
        c.version = 2;
        c.pub_key_operator = operator_key(op, BlsSchemeKind::Basic);
        let mut bytes = service(addr).addr.v1_bytes().to_vec();
        bytes.extend_from_slice(&9999u16.to_be_bytes());
        let mut raw: &[u8] = &bytes;
        c.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::decode(&mut raw).unwrap()));
        if banned {
            d.hot.ban_if_not_banned(120);
        }
    })
}

fn tip() -> TipCtx {
    TipCtx {
        height: 120,
        dip3_enforced: true,
        require_routable: true,
    }
}

fn facts() -> LocalFacts {
    LocalFacts {
        listen: true,
        require_routable: true,
        local_toward_v4: Some(service(1)),
        loopback_listen: Service::new(NetAddr::ipv4([127, 0, 0, 1]), 9999),
        peers: Vec::new(),
        valid: vec![service(1), service(2)],
        self_connect_ok: true,
    }
}

#[derive(Default)]
struct Lookup {
    entries: Vec<Arc<Dmn>>,
    no_operator: bool,
    missing: Option<[u8; 32]>,
    valid_override: Option<bool>,
}

impl Lookup {
    fn one(dmn: Arc<Dmn>) -> Self {
        Self {
            entries: vec![dmn],
            ..Default::default()
        }
    }
}

impl MnLookup for Lookup {
    fn by_operator_key(&self, key: &OperatorPubKey) -> Option<Arc<Dmn>> {
        if self.no_operator {
            return None;
        }
        self.entries.iter().rev().find(|d| d.cold.pub_key_operator.get() == *key).cloned()
    }
    fn is_valid(&self, pro: &[u8; 32]) -> bool {
        self.valid_override.unwrap_or_else(|| {
            self.entries.iter().any(|d| d.pro_tx_hash.as_bytes() == pro && d.is_valid())
        })
    }
    fn is_pose_banned(&self, pro: &[u8; 32]) -> bool {
        self.entries.iter().any(|d| d.pro_tx_hash.as_bytes() == pro && d.hot.is_banned())
    }
    fn get(&self, pro: &[u8; 32]) -> Option<Arc<Dmn>> {
        if self.missing.as_ref() == Some(pro) {
            return None;
        }
        self.entries.iter().find(|d| d.pro_tx_hash.as_bytes() == pro).cloned()
    }
}

fn ready(dmn: &Dmn) -> ActiveState {
    ActiveState {
        state: State::Ready,
        pro_tx_hash: Some(*dmn.pro_tx_hash.as_bytes()),
        outpoint: Some(dmn.collateral),
        service: service(1),
        ..Default::default()
    }
}

// Core: active/masternode.cpp:112-173 at v24.0.0
#[test]
fn core_init_transition_table() {
    let matched = entry(1, 1, 1, false);
    let key = matched.cold.pub_key_operator.get();
    for case in 0..10 {
        let mut st = ActiveState::default();
        let mut tip = tip();
        let mut local = facts();
        let mut list = Lookup::one(Arc::clone(&matched));
        let expected = match case {
            0 => {
                tip.dip3_enforced = false;
                State::WaitingForProtx
            }
            1 => {
                local.listen = false;
                State::SomeError
            }
            2 => {
                local.local_toward_v4 = None;
                State::SomeError
            }
            3 => {
                list.entries.clear();
                State::WaitingForProtx
            }
            4 => {
                list.entries = vec![entry(1, 1, 1, true)];
                State::PoseBanned
            }
            5 => {
                list.valid_override = Some(false);
                State::Removed
            }
            6 => {
                list.entries = vec![entry(1, 1, 2, false)];
                State::SomeError
            }
            7 => {
                local.self_connect_ok = false;
                State::SomeError
            }
            8 => State::Ready,
            9 => {
                tip.require_routable = false;
                local.require_routable = false;
                local.listen = false;
                local.self_connect_ok = false;
                State::Ready
            }
            _ => unreachable!(),
        };
        let log = init(&mut st, &key, &tip, &list, &local);
        assert_eq!(st.state, expected, "{case}");
        if expected == State::Ready {
            assert_eq!(st.pro_tx_hash, Some(*matched.pro_tx_hash.as_bytes()));
            assert_eq!(st.outpoint, Some(matched.collateral));
            assert!(matches!(&log[..], [LogLine::Init(_), LogLine::CheckingInbound(_)]));
        } else {
            assert_eq!(st.pro_tx_hash, None);
            assert_eq!(st.outpoint, None);
        }
        match case {
            1 => assert!(st.error.starts_with("Masternode must accept connections from outside.")),
            2 => assert!(st.error.starts_with("Can't detect valid external address.")),
            6 => assert_eq!(st.error, "Local address does not match the address from ProTx"),
            7 => assert_eq!(st.error, "Could not connect to 1.2.3.1:9999"),
            _ => {}
        }
    }
}

// Core: active/masternode.cpp:184-221 at v24.0.0
#[test]
fn core_reset_transition_table() {
    let original = entry(1, 1, 1, false);
    let key = original.cold.pub_key_operator.get();
    for (cause, missing_old) in [
        (State::Removed, false),
        (State::SomeError, true),
        (State::SomeError, false),
        (State::OperatorKeyChanged, false),
        (State::ProtxIpChanged, false),
    ] {
        for reregister in [false, true] {
            let mut st = ready(&original);
            st.error = "retained".into();
            let mut old = Lookup::one(Arc::clone(&original));
            let mut new = Lookup::one(Arc::clone(&original));
            match cause {
                State::Removed => new.entries.clear(),
                State::SomeError if missing_old => {
                    old.missing = Some(*original.pro_tx_hash.as_bytes())
                }
                State::SomeError => new.missing = Some(*original.pro_tx_hash.as_bytes()),
                State::OperatorKeyChanged => new.entries = vec![entry(1, 2, 1, false)],
                State::ProtxIpChanged => new.entries = vec![entry(1, 1, 2, false)],
                _ => unreachable!(),
            }
            new.no_operator = !reregister;
            let replacement = entry(2, 1, 1, false);
            if reregister {
                new.entries.push(Arc::clone(&replacement));
            }
            updated_block_tip(&mut st, &key, &tip(), Ok((&old, &new)), &facts()).unwrap();
            assert_eq!(st.error, "retained");
            if reregister {
                assert_eq!(st.state, State::Ready, "{cause:?}");
                assert_eq!(st.pro_tx_hash, Some(*replacement.pro_tx_hash.as_bytes()));
                assert_eq!(st.outpoint, Some(replacement.collateral));
            } else {
                assert_eq!(st.state, cause);
                assert_eq!(st.pro_tx_hash, None);
                assert_eq!(st.outpoint, None);
            }
        }
    }
}

// Core: active/masternode.cpp:205-207, :140-145 at v24.0.0
#[test]
fn core_reset_reinitializes_banned_and_changed_address() {
    let original = entry(1, 1, 1, false);
    let key = original.cold.pub_key_operator.get();
    let old = Lookup::one(Arc::clone(&original));
    for (new, state) in
        [(entry(1, 1, 1, true), State::PoseBanned), (entry(1, 1, 2, false), State::SomeError)]
    {
        let mut st = ready(&original);
        updated_block_tip(&mut st, &key, &tip(), Ok((&old, &Lookup::one(new))), &facts()).unwrap();
        assert_eq!(st.state, state);
        assert_eq!(st.pro_tx_hash, None);
        assert_eq!(st.outpoint, None);
    }
}

// Core: active/masternode.cpp:177-194, :223-227 at v24.0.0
#[test]
fn core_update_skip_unchanged_and_retry() {
    let dmn = entry(1, 1, 1, false);
    let list = Lookup::one(Arc::clone(&dmn));
    let key = dmn.cold.pub_key_operator.get();
    let mut st = ready(&dmn);
    let before = st.clone();
    let block = BlockHash::from([42; 32]);
    let log =
        updated_block_tip(&mut st, &key, &tip(), Err(ListError::Unavailable(block)), &facts())
            .unwrap();
    assert!(matches!(&log[..], [LogLine::ListUnavailable(b)] if *b == block));
    assert_eq!(
        log[0].render(|_| unreachable!()),
        format!(
            "CActiveMasternodeManager::UpdatedBlockTip -- masternode list unavailable: {}\n",
            StoreError::ListUnavailable(block)
        )
    );
    assert_eq!(st, before);
    assert!(
        updated_block_tip(&mut st, &key, &tip(), Ok((&list, &list)), &facts()).unwrap().is_empty()
    );
    assert_eq!(st, before);
    let mut pre_dip3 = tip();
    pre_dip3.dip3_enforced = false;
    updated_block_tip(
        &mut st,
        &key,
        &pre_dip3,
        Err(ListError::Fault(StoreError::read("unused"))),
        &facts(),
    )
    .unwrap();
    assert_eq!(st, before);
    for state in [
        State::WaitingForProtx,
        State::PoseBanned,
        State::Removed,
        State::OperatorKeyChanged,
        State::ProtxIpChanged,
        State::SomeError,
    ] {
        st = ActiveState {
            state,
            ..Default::default()
        };
        updated_block_tip(&mut st, &key, &tip(), Ok((&list, &list)), &facts()).unwrap();
        assert_eq!(st.state, State::Ready);
    }
}

// Core: active/masternode.cpp:186-194 at v24.0.0
#[test]
fn core_list_error_mapping_and_fault_identity() {
    let block = BlockHash::from([42; 32]);
    assert_eq!(ListError::from(StoreError::ListUnavailable(block)), ListError::Unavailable(block));
    let dmn = entry(1, 1, 1, false);
    let key = dmn.cold.pub_key_operator.get();
    for error in
        [StoreError::read("read"), StoreError::inconsistent("row"), StoreError::NotBuilt("list")]
    {
        assert_eq!(ListError::from(error.clone()), ListError::Fault(error.clone()));
        let mut st = ready(&dmn);
        assert_eq!(
            updated_block_tip(&mut st, &key, &tip(), Err(error.clone().into()), &facts())
                .unwrap_err(),
            error
        );
        assert_eq!(st.state, State::SomeError);
        assert_eq!(st.pro_tx_hash, None);
        assert_eq!(st.outpoint, None);
    }
    let mut st = ActiveState::default();
    assert!(
        updated_block_tip(&mut st, &key, &tip(), Err(ListError::Unavailable(block)), &facts())
            .is_err()
    );
}

// Core: active/masternode.cpp:230-276 at v24.0.0
#[test]
fn core_local_address_decision_table() {
    for case in 0..7 {
        let mut st = ActiveState {
            service: service(9),
            ..Default::default()
        };
        let mut local = facts();
        match case {
            0 => {}
            1 => {
                local.local_toward_v4 = None;
                local.require_routable = false;
            }
            2 => {
                local.local_toward_v4 = None;
                local.peers = vec![
                    PeerLocal {
                        is_ipv4: false,
                        local_addr: Some(service(1)),
                    },
                    PeerLocal {
                        is_ipv4: true,
                        local_addr: Some(service(2)),
                    },
                    PeerLocal {
                        is_ipv4: true,
                        local_addr: Some(service(1)),
                    },
                ];
            }
            3 => local.local_toward_v4 = None,
            4 => {
                local.local_toward_v4 = None;
                local.peers.push(PeerLocal {
                    is_ipv4: true,
                    local_addr: Some(service(9)),
                });
            }
            5 => {
                local.local_toward_v4 = Some(service(8));
                local.peers.push(PeerLocal {
                    is_ipv4: false,
                    local_addr: None,
                });
            }
            6 => {
                local.local_toward_v4 = Some(service(8));
                local.require_routable = false;
            }
            _ => unreachable!(),
        }
        let log = get_local_address(&mut st, &local);
        assert_eq!(log.is_err(), case == 3);
        assert_eq!(
            st.service,
            match case {
                0 => service(1),
                1 | 6 => local.loopback_listen,
                2 => service(2),
                5 => service(8),
                _ => service(9),
            }
        );
    }
}

// Core: active/masternode.cpp:132-151; evo/deterministicmns.cpp:93-102 at v24.0.0
#[test]
fn core_mn_lookup_uses_the_block_list() {
    let dmn = entry(1, 1, 1, false);
    let key = dmn.cold.pub_key_operator.get();
    let mut list = MnList::default_pre_dip3();
    list.insert_for_tests(Arc::clone(&dmn));
    assert_eq!(MnLookup::by_operator_key(&list, &key).unwrap().pro_tx_hash, dmn.pro_tx_hash);
    assert!(MnLookup::is_valid(&list, dmn.pro_tx_hash.as_bytes()));
    assert!(!MnLookup::is_pose_banned(&list, dmn.pro_tx_hash.as_bytes()));
    assert_eq!(
        MnLookup::get(&list, dmn.pro_tx_hash.as_bytes()).unwrap().collateral,
        dmn.collateral
    );
}

fn unhex<const N: usize>(text: &str, reverse: bool) -> [u8; N] {
    assert_eq!(text.len(), N * 2);
    let mut bytes =
        std::array::from_fn(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap());
    if reverse {
        bytes.reverse();
    }
    bytes
}

fn recorded_list(row: &serde_json::Value) -> MnList {
    use dash_pkc::bls::BlsSchemeSource;
    use dash_primitives::{OutPoint, TxHash};
    let mut list = MnList::default_pre_dip3();
    for (n, info) in row["list"].as_array().unwrap().iter().enumerate() {
        let state = &info["state"];
        let service = state["addresses"]["core_p2p"]
            .as_array()
            .and_then(|addrs| addrs.first())
            .map(|addr| Service::from_socket_addr(addr.as_str().unwrap().parse().unwrap()))
            .unwrap_or_default();
        let mut bytes = service.addr.v1_bytes().to_vec();
        bytes.extend_from_slice(&service.port.to_be_bytes());
        let dmn = dmn_with(n as u8 + 1, MnType::Regular, |d, c| {
            d.pro_tx_hash = ProTxHash::from(unhex::<32>(info["proTxHash"].as_str().unwrap(), true));
            d.collateral = OutPoint::new(
                TxHash::from(unhex::<32>(info["collateralHash"].as_str().unwrap(), true)),
                info["collateralIndex"].as_u64().unwrap() as u32,
            );
            c.version = state["version"].as_i64().unwrap() as i32;
            c.pub_key_operator = dash_dml::OperatorKey::from_bytes(
                &unhex::<48>(state["pubKeyOperator"].as_str().unwrap(), false),
                BlsSchemeSource::Version(BlsSchemeKind::from_legacy(c.version == 1)),
            );
            c.net_info =
                Arc::new(NetInfo::Legacy(NetInfoV1::decode(&mut bytes.as_slice()).unwrap()));
            if state["PoSeBanHeight"].as_i64().unwrap() != -1 {
                d.hot.ban_if_not_banned(state["PoSeBanHeight"].as_i64().unwrap() as i32);
            }
        });
        list.insert_for_tests(dmn);
    }
    list
}

// Core: active/masternode.cpp:112-276 at v24.0.0
#[test]
fn core_recorded_state_and_list_replay() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/am1-state/core-v24.json"))
            .unwrap();
    assert_eq!(fixture["core_commit"], "938fba502824b9d850b7c710601456dc3d71603e");
    let offline = fixture["offline_blocks"].as_array().unwrap();
    assert!(!offline.is_empty());
    for row in offline {
        assert!(row["status"].is_null());
        let list = recorded_list(row);
        let info = &row["info"];
        let pro = ProTxHash::from(unhex::<32>(info["proTxHash"].as_str().unwrap(), true));
        assert_eq!(
            list.get(&pro).unwrap().hot.banned_height(),
            info["state"]["PoSeBanHeight"].as_i64().unwrap() as i32
        );
    }
    assert!(offline.last().unwrap()["info"]["state"]["PoSeBanHeight"].as_i64().unwrap() >= 0);
    assert_eq!(
        fixture["rows"].as_array().unwrap().last().unwrap()["status"]["state"],
        "POSE_BANNED"
    );
    let mut st = ActiveState::default();
    let mut old = MnList::default_pre_dip3();
    let mut saw = std::collections::BTreeSet::new();
    for row in fixture["rows"].as_array().unwrap() {
        let list = recorded_list(row);
        let key = OperatorPubKey::from_set_bytes_basic(&unhex::<48>(
            row["operator_public"].as_str().unwrap(),
            false,
        ));
        let service =
            Service::from_socket_addr(row["local_service"].as_str().unwrap().parse().unwrap());
        let local = LocalFacts {
            listen: true,
            require_routable: false,
            local_toward_v4: Some(service.clone()),
            loopback_listen: service.clone(),
            peers: Vec::new(),
            valid: vec![service],
            self_connect_ok: true,
        };
        let tip = TipCtx {
            height: row["height"].as_u64().unwrap() as u32,
            dip3_enforced: true,
            require_routable: false,
        };
        let log = if row["step"] == "offline_restart" {
            st = ActiveState::default();
            let startup_tip = TipCtx {
                height: offline[0]["height"].as_u64().unwrap() as u32 - 1,
                ..tip
            };
            let mut log = init(&mut st, &key, &startup_tip, &old, &local);
            for block in offline {
                let next = recorded_list(block);
                let block_tip = TipCtx {
                    height: block["height"].as_u64().unwrap() as u32,
                    ..tip
                };
                log.extend(
                    updated_block_tip(&mut st, &key, &block_tip, Ok((&old, &next)), &local)
                        .unwrap(),
                );
                old = next;
            }
            log
        } else {
            updated_block_tip(&mut st, &key, &tip, Ok((&old, &list)), &local).unwrap()
        };
        let status = &row["status"];
        assert_eq!(st.state_string(), status["state"], "{}", row["step"]);
        assert_eq!(st.status(), status["status"], "{}", row["step"]);
        assert_eq!(st.service.to_string(), status["service"]);
        let pro = status["proTxHash"].as_str().map(|hex| unhex::<32>(hex, true));
        assert_eq!(st.pro_tx_hash, pro);
        assert_eq!(
            st.outpoint
                .map(|op| format!("{}-{}", op.txid(), op.index()))
                .unwrap_or_else(|| format!("{}-4294967295", "00".repeat(32))),
            status["outpoint"]
        );
        let observed: Vec<_> =
            row["logs"].as_array().unwrap().iter().map(|line| line.as_str().unwrap()).collect();
        assert_eq!(observed.len(), log.len(), "{}", row["step"]);
        for (actual, expected) in observed[observed.len() - log.len()..].iter().zip(log) {
            match expected {
                LogLine::Init(dmn) => assert!(actual.starts_with(&format!(
                    "CActiveMasternodeManager::Init -- proTxHash={}, proTx=",
                    dmn.pro_tx_hash
                ))),
                line @ (LogLine::CheckingInbound(_)
                | LogLine::Error {
                    ..
                }) => {
                    assert_eq!(*actual, line.render(|_| unreachable!()).trim_end());
                }
                LogLine::ListUnavailable(_) => panic!("unexpected unavailable list"),
            }
        }
        saw.insert(st.state_string());
        old = list;
    }
    assert_eq!(saw, ["ERROR", "POSE_BANNED", "READY", "REMOVED"].into());
}

// Core: active/masternode.cpp:112-228; active/masternode.h:55, :67-69 at v24.0.0
#[test]
fn core_active_shell_applies_the_machine() {
    use crate::active::{ActiveMasternode, parse};
    use dash_args::ArgsManager;
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", &"01".repeat(32));
    let active = ActiveMasternode::new(parse(&args).unwrap().unwrap());
    let dmn = dmn_with(1, MnType::Regular, |_, c| {
        c.version = 2;
        c.pub_key_operator =
            dash_dml::OperatorKey::from_value(active.pubkey(), BlsSchemeKind::Basic);
        c.net_info = Arc::clone(&entry(1, 1, 1, false).cold.net_info);
    });
    let list = Lookup::one(Arc::clone(&dmn));
    assert_eq!(active.init(&tip(), &list, &facts()).len(), 2);
    assert_eq!(active.state_string(), "READY");
    assert_eq!(active.pro_tx_hash(), Some(*dmn.pro_tx_hash.as_bytes()));
    assert_eq!(active.snapshot().outpoint, Some(dmn.collateral));
    active.updated_block_tip(&tip(), Ok((&list, &Lookup::default())), &facts()).unwrap();
    assert_eq!(active.state_string(), "REMOVED");
    assert_eq!(active.pro_tx_hash(), None);
    assert_eq!(active.snapshot().outpoint, None);
}
