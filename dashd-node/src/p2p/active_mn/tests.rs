use std::sync::Arc;

use dash_dml::MnList;
use dash_dml::store::StoreError;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{NoWaitGuard, WaitError, genesis_block};
use dashd_masternode::active::state::ListError;
use dashd_masternode::active::worker::ListForBlock;

use super::super::amb::{node, regtest};
use super::*;

// Core: active/masternode.cpp:184-194 at v24.0.0
#[test]
fn core_only_list_unavailable_is_block_data_unavailable() {
    let list = Arc::new(MnList::default_pre_dip3());
    assert!(Arc::ptr_eq(&list_reply(Ok(Ok(Arc::clone(&list)))).unwrap(), &list));
    let block = BlockHash::from([7; 32]);
    assert_eq!(
        list_reply(Ok(Err(StoreError::ListUnavailable(block)))).unwrap_err(),
        ListError::Unavailable(block)
    );
    for fault in
        [StoreError::read("disk"), StoreError::inconsistent("diff"), StoreError::NotBuilt("evo")]
    {
        assert_eq!(
            list_reply(Ok(Err(fault.clone()))).unwrap_err(),
            ListError::Fault(fault),
            "a fault, not a skip"
        );
    }
    assert!(matches!(
        list_reply(Err(WaitError::Disconnected)).unwrap_err(),
        ListError::Fault(StoreError::Read { .. })
    ));
}

// Core: active/masternode.cpp:132; evo/deterministicmns.h:796-799 at v24.0.0
#[test]
fn core_node_lists_answer_from_v_and_refuse_the_no_wait_marker() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = node(dir.path(), &["-connect=0", "-dnsseed=0", "-fixedseeds=0", "-listen=0"]);
    node.stop_net();
    let v = node.chain().unwrap().handle().clone();
    let genesis = v.view_handle().lookup(&genesis_block(&regtest()).hash()).unwrap();
    let lists = node_lists(v);
    assert_eq!(lists.list_for_block(genesis).unwrap().len(), 0);
    let _marked = NoWaitGuard::set();
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = lists.list_for_block(genesis);
    }));
    assert!(refused.is_err(), "a marked thread never issues the chain command");
}

mod local_facts {
    use std::sync::Arc;

    use dash_p2p::local::LocalScore;
    use dash_p2p::{ConnectionType, NetAddr, Permissions, Service};
    use dash_params::{ChainParamsBuilder, Network, Release};
    use dashd_masternode::active::ActiveState;
    use dashd_masternode::active::state::{LocalFacts, LogLine, get_local_address};
    use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
    use dashd_p2p::sim::{Sim, SimPeer};

    use super::super::{LocalFactsSource, assemble, peer_locals, toward_v4};

    fn sim(network: Network) -> Sim {
        let params = Arc::new(ChainParamsBuilder::new(network, Release::V24_0).build().unwrap());
        let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
        Sim::new(params, identity)
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap()
    }

    fn source(sim: &Sim, rt: &tokio::runtime::Runtime) -> LocalFactsSource {
        LocalFactsSource::new(Arc::clone(&sim.core), rt.handle().clone())
    }

    fn peer(sim: &Sim, n: u8) -> SimPeer {
        sim.connect(n, ConnectionType::OutboundFullRelay, Permissions::NONE)
    }

    fn public() -> Service {
        Service::new(NetAddr::ipv4([1, 2, 3, 4]), 19999)
    }

    fn kept() -> ActiveState {
        ActiveState {
            service: Service::new(NetAddr::ipv4([9, 9, 9, 9]), 1),
            ..ActiveState::default()
        }
    }

    fn resolve(facts: &LocalFacts) -> (ActiveState, Result<(), LogLine>) {
        let mut st = kept();
        let found = get_local_address(&mut st, facts);
        (st, found)
    }

    // Core: active/masternode.cpp:237-240 at v24.0.0
    #[test]
    fn core_local_v4_address_is_found_first() {
        let (sim, rt) = (sim(Network::Testnet3), runtime());
        let _peer = peer(&sim, 1);
        assert!(sim.core.add_local(public(), LocalScore::Manual));
        let facts = source(&sim, &rt).collect(|_| None);
        assert!(facts.require_routable && facts.listen);
        assert_eq!(facts.local_toward_v4, Some(public()));
        assert_eq!(facts.valid, [public()]);
        let (st, found) = resolve(&facts);
        assert!(found.is_ok());
        assert_eq!(st.service, public());
    }

    // Core: active/masternode.cpp:241-246 at v24.0.0
    #[test]
    fn core_chain_without_routable_requirement_takes_loopback() {
        let (sim, rt) = (sim(Network::Regtest), runtime());
        let facts = source(&sim, &rt).collect(|_| None);
        assert!(!facts.require_routable);
        let (st, found) = resolve(&facts);
        assert!(found.is_ok());
        let port = sim.core.config().listen_port;
        assert_eq!(st.service, Service::new(NetAddr::ipv4([127, 0, 0, 1]), port));
    }

    // Core: active/masternode.cpp:250-259 at v24.0.0
    #[test]
    fn core_first_ipv4_peer_with_a_valid_local_address() {
        let (sim, rt) = (sim(Network::Testnet3), runtime());
        let _peers = (peer(&sim, 1), peer(&sim, 2));
        // Both reads rank one table toward an IPv4 partner, so this branch needs the table to
        // gain the address between them, as it can in Core between `GetLocal` and the walk.
        let toward = toward_v4(&sim.core);
        assert!(sim.core.add_local(public(), LocalScore::Manual));
        let facts = assemble(&sim.core, toward, peer_locals(&sim.core));
        assert_eq!(facts.local_toward_v4, None);
        assert_eq!(facts.peers.len(), 2);
        assert!(facts.peers.iter().all(|p| p.is_ipv4 && p.local_addr == Some(public())));
        let (st, found) = resolve(&facts);
        assert!(found.is_ok());
        assert_eq!(st.service, public());
        // Read at once, the same table takes the first branch.
        assert_eq!(source(&sim, &rt).collect(|_| None).local_toward_v4, Some(public()));
    }

    // Core: active/masternode.cpp:260-265 at v24.0.0
    #[test]
    fn core_empty_peer_table_is_an_error() {
        let (sim, rt) = (sim(Network::Testnet3), runtime());
        let facts = source(&sim, &rt).collect(|_| None);
        assert!(facts.peers.is_empty());
        let (st, found) = resolve(&facts);
        let Err(LogLine::Error {
            function: "GetLocalAddress",
            message,
        }) = found
        else {
            panic!("an error line");
        };
        assert!(message.starts_with("Can't detect valid external address."));
        assert_eq!(st.error, message);
    }

    // Core: active/masternode.cpp:248-267 at v24.0.0
    #[test]
    fn core_peers_without_a_usable_address_keep_the_service() {
        let (sim, rt) = (sim(Network::Testnet3), runtime());
        let _peer = peer(&sim, 1);
        let facts = source(&sim, &rt).collect(|_| None);
        assert_eq!(facts.peers.len(), 1);
        assert!(facts.valid.is_empty());
        let (st, found) = resolve(&facts);
        assert!(found.is_ok());
        assert_eq!(st.service, kept().service);
    }

    // Core: active/masternode.cpp:157-161 at v24.0.0
    #[test]
    fn core_inbound_check_runs_only_toward_the_planned_service() {
        let (sim, rt) = (sim(Network::Regtest), runtime());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ours = Service::from_socket_addr(listener.local_addr().unwrap());
        let source = source(&sim, &rt);
        assert!(!source.collect(|_| None).self_connect_ok);
        let mut asked = None;
        let facts = source.collect(|facts| {
            asked = Some(facts.self_connect_ok);
            Some(ours.clone())
        });
        assert_eq!(asked, Some(false), "planned before the connect");
        assert!(facts.self_connect_ok);
        drop(listener);
        assert!(!source.collect(|_| Some(ours.clone())).self_connect_ok);
    }
}

pub(crate) mod tip_worker {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use dash_args::ArgsManager;
    use dash_consensus::BlockId;
    use dash_consensus::deployments::dip0003_enforced;
    use dash_dml::store::StoreError;
    use dash_dml::testkit::dmn_with;
    use dash_dml::{MnList, MnType};
    use dash_netaddr::{NetAddr, Service};
    use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
    use dash_pkc::bls::BlsSchemeKind;
    use dash_primitives::{AddrV1, BlockHash, NetInfo, NetInfoV1, ServiceV1};
    use dashd_chainstate::chain::{
        Blocking, Queued, RecordingAbort, ValidationSignals, no_wait_marked,
    };
    use dashd_masternode::active::state::{ListError, LocalFacts};
    use dashd_masternode::active::worker::{CheckedListForBlock, ListForBlock, TipEvent};
    use dashd_masternode::active::{ActiveMasternode, State, parse};

    use super::super::{ActiveDuties, ActiveTip, CollectFacts, TipEntries, wire};
    use crate::test_log::logged;

    const HEIGHT: u32 = 100_000;

    fn params() -> Arc<ChainSetup> {
        let p =
            Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
        assert!(dip0003_enforced(HEIGHT as i32, &p));
        p
    }

    fn id(n: u32) -> BlockId {
        BlockId(n)
    }

    /// Lists by block; the first call waits for `gate` when one is set.
    #[derive(Default)]
    struct Lists {
        by_block: Mutex<HashMap<BlockId, Result<Arc<MnList>, ListError>>>,
        calls: Mutex<Vec<(BlockId, Option<String>)>>,
        gate: Mutex<Option<mpsc::Receiver<()>>>,
        panics_at: Mutex<Vec<BlockId>>,
    }

    struct Shared(Arc<Lists>);

    impl ListForBlock for Shared {
        fn list_for_block(&self, block: BlockId) -> Result<Arc<MnList>, ListError> {
            let lists = &self.0;
            let thread = std::thread::current().name().map(str::to_owned);
            lists.calls.lock().unwrap().push((block, thread));
            if lists.panics_at.lock().unwrap().contains(&block) {
                panic!("the list read panicked");
            }
            if let Some(gate) = lists.gate.lock().unwrap().take() {
                gate.recv_timeout(Duration::from_secs(60)).unwrap();
            }
            lists
                .by_block
                .lock()
                .unwrap()
                .get(&block)
                .cloned()
                .unwrap_or_else(|| Ok(Arc::new(MnList::default_pre_dip3())))
        }
    }

    /// Entries `n` at `HEIGHT + n` on `n - 1`; the tip `tip`.
    struct Entries(Option<BlockId>);

    impl TipEntries for Entries {
        fn entry(&self, id: BlockId) -> Option<(u32, Option<BlockId>)> {
            Some((HEIGHT + id.0, id.0.checked_sub(1).map(BlockId)))
        }

        fn tip(&self) -> Option<BlockId> {
            self.0
        }
    }

    fn ours() -> Service {
        Service::new(NetAddr::ipv4([127, 0, 0, 1]), 19899)
    }

    /// Regtest facts by default: the loopback service, the inbound check passing toward what is
    /// planned. A hook, if set, runs once as the next collection starts.
    #[derive(Default)]
    struct Facts(
        Mutex<Vec<Option<Service>>>,
        Mutex<Option<LocalFacts>>,
        Mutex<Option<Box<dyn FnOnce() + Send>>>,
    );

    impl CollectFacts for Arc<Facts> {
        fn collect_facts(&self, planned: &dyn Fn(&LocalFacts) -> Option<Service>) -> LocalFacts {
            let hook = self.2.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
            let mut facts = self.1.lock().unwrap().clone().unwrap_or(LocalFacts {
                listen: true,
                require_routable: false,
                local_toward_v4: None,
                loopback_listen: ours(),
                peers: Vec::new(),
                valid: Vec::new(),
                self_connect_ok: false,
            });
            let target = planned(&facts);
            facts.self_connect_ok = target.is_some();
            self.0.lock().unwrap().push(target);
            facts
        }
    }

    fn active() -> Arc<ActiveMasternode> {
        let args = ArgsManager::new();
        args.force_set_arg("-masternodeblsprivkey", &"01".repeat(32));
        Arc::new(ActiveMasternode::new(parse(&args).unwrap().unwrap()))
    }

    /// A list holding `active`'s masternode at [`ours`].
    pub(crate) fn list_with(active: &ActiveMasternode) -> Arc<MnList> {
        let dmn = dmn_with(1, MnType::Regular, |_, cold| {
            cold.version = 2;
            cold.pub_key_operator =
                dash_dml::OperatorKey::from_value(active.pubkey(), BlsSchemeKind::Basic);
            cold.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
                AddrV1::from_bytes(ours().addr.v1_bytes()),
                ours().port,
            ))));
        });
        let mut list = MnList::default_pre_dip3();
        list.insert_for_tests(dmn);
        Arc::new(list)
    }

    pub(crate) struct Rig {
        pub(crate) active: Arc<ActiveMasternode>,
        lists: Arc<Lists>,
        facts: Arc<Facts>,
        pub(crate) abort: Arc<RecordingAbort>,
        params: Arc<ChainSetup>,
    }

    impl Rig {
        pub(crate) fn new() -> Self {
            Self {
                active: active(),
                lists: Arc::default(),
                facts: Arc::default(),
                abort: Arc::default(),
                params: params(),
            }
        }

        /// On a chain that requires a routable external address.
        fn routable() -> Self {
            let params = Arc::new(
                ChainParamsBuilder::new(Network::Testnet3, Release::V24_0).build().unwrap(),
            );
            assert!(params.require_routable_external_ip);
            assert!(dip0003_enforced(HEIGHT as i32, &params));
            Self {
                params,
                ..Self::new()
            }
        }

        pub(crate) fn tip(&self, tip: Option<BlockId>) -> ActiveTip {
            ActiveTip {
                active: Arc::clone(&self.active),
                lists: Box::new(CheckedListForBlock::new(
                    Shared(Arc::clone(&self.lists)),
                    no_wait_marked,
                )),
                entries: Box::new(Entries(tip)),
                params: Arc::clone(&self.params),
                local: Box::new(Arc::clone(&self.facts)),
                abort: self.abort.clone(),
                duties: None,
            }
        }

        pub(crate) fn set(&self, block: u32, list: Result<Arc<MnList>, ListError>) {
            self.lists.by_block.lock().unwrap().insert(id(block), list);
        }

        fn calls(&self) -> Vec<BlockId> {
            self.lists.calls.lock().unwrap().iter().map(|(b, _)| *b).collect()
        }
    }

    // Core: active/context.cpp:92-98; validationinterface.cpp:193 at v24.0.0
    #[test]
    fn core_signals_listener_only_posts_and_the_worker_keeps_order() {
        let rig = Rig::new();
        let (release, gate) = mpsc::channel();
        *rig.lists.gate.lock().unwrap() = Some(gate);
        let wiring = wire(rig.tip(None)).unwrap();
        let signals = ValidationSignals::default();
        signals.register(Arc::clone(&wiring.listener));
        let abort = Arc::new(RecordingAbort::default());
        let running = signals.start(abort.clone()).unwrap();
        for n in 1..=3 {
            signals.enqueue(Queued::UpdatedBlockTip {
                new: id(n),
                fork: Some(id(n - 1)),
                ibd: false,
            });
        }
        // The signals thread runs all three while the worker is held at its first list.
        let (synced, done) = mpsc::channel();
        let queue = signals.clone();
        std::thread::spawn(move || synced.send(queue.sync(&mut Blocking)).unwrap());
        assert_eq!(done.recv_timeout(Duration::from_secs(60)).unwrap(), Ok(()));
        assert!(rig.calls().len() <= 1);
        release.send(()).unwrap();
        running.stop().unwrap().unwrap();
        assert!(abort.reports().is_empty());
        signals.unregister(&wiring.listener);
        drop(wiring.listener);
        drop(wiring.startup);
        wiring.worker.join().unwrap();
        assert_eq!(rig.calls(), [id(1), id(2), id(3)]);
        assert!(
            rig.lists.calls.lock().unwrap().iter().all(|(_, t)| t.as_deref() == Some("am1-tip"))
        );
        assert!(rig.abort.reports().is_empty());
    }

    // Core: active/context.cpp:94-95 at v24.0.0
    #[test]
    fn core_ibd_and_disconnect_only_tips_are_skipped() {
        let rig = Rig::new();
        let tip = rig.tip(None);
        tip.updated_block_tip(TipEvent {
            new: id(1),
            fork: None,
            ibd: true,
        });
        tip.updated_block_tip(TipEvent {
            new: id(1),
            fork: Some(id(1)),
            ibd: false,
        });
        assert!(rig.calls().is_empty());
    }

    /// The DKG's tip update with the manager's state as it ran.
    struct SeenDuties {
        active: Arc<ActiveMasternode>,
        seen: Mutex<Vec<(BlockId, bool, State)>>,
    }

    impl ActiveDuties for SeenDuties {
        fn updated_block_tip(&self, new: BlockId, ibd: bool) {
            self.seen.lock().unwrap().push((new, ibd, self.active.snapshot().state));
        }
    }

    /// DK1-L3: `ActiveContext::UpdatedBlockTip` calls `qdkgsman->UpdatedBlockTip` last, after
    /// the manager's update (here its `Init`, which makes it READY), and never for an IBD or a
    /// disconnect-only tip; a tip whose manager update returns early still reaches it.
    // Core: v24.0.0 src/active/context.cpp:93-113 ActiveContext::UpdatedBlockTip
    #[test]
    fn core_the_dkg_hears_the_tip_after_the_masternode_manager() {
        let rig = Rig::new();
        rig.set(5, Ok(list_with(&rig.active)));
        let seen = Arc::new(SeenDuties {
            active: Arc::clone(&rig.active),
            seen: Mutex::default(),
        });
        let tip = ActiveTip {
            duties: Some(Arc::clone(&seen) as _),
            ..rig.tip(Some(id(5)))
        };
        tip.updated_block_tip(TipEvent {
            new: id(5),
            fork: None,
            ibd: true,
        });
        tip.updated_block_tip(TipEvent {
            new: id(5),
            fork: Some(id(5)),
            ibd: false,
        });
        assert!(seen.seen.lock().unwrap().is_empty());
        tip.updated_block_tip(TipEvent {
            new: id(5),
            fork: None,
            ibd: false,
        });
        assert_eq!(*seen.seen.lock().unwrap(), [(id(5), false, State::Ready)]);
        struct NoEntries;
        impl TipEntries for NoEntries {
            fn entry(&self, _: BlockId) -> Option<(u32, Option<BlockId>)> {
                None
            }
            fn tip(&self) -> Option<BlockId> {
                None
            }
        }
        let rig = Rig::new();
        let seen = Arc::new(SeenDuties {
            active: Arc::clone(&rig.active),
            seen: Mutex::default(),
        });
        let tip = ActiveTip {
            duties: Some(Arc::clone(&seen) as _),
            entries: Box::new(NoEntries),
            ..rig.tip(None)
        };
        tip.updated_block_tip(TipEvent {
            new: id(1),
            fork: None,
            ibd: false,
        });
        assert!(rig.calls().is_empty(), "the manager returned at the missing entry");
        assert_eq!(seen.seen.lock().unwrap().len(), 1);
    }

    // Core: init.cpp:2452-2466, :2538-2547; active/context.cpp:88-91 at v24.0.0
    #[test]
    fn core_startup_init_runs_before_the_listener_hears_the_broadcast() {
        let rig = Rig::new();
        rig.set(4, Ok(list_with(&rig.active)));
        rig.set(5, Ok(list_with(&rig.active)));
        let wiring = wire(rig.tip(Some(id(5)))).unwrap();
        // The kick before `Init` is `CDSNotificationInterface`'s alone.
        wiring.listener.initialize_current_block_tip(Some(id(5)), false);
        wiring.startup.init();
        assert_eq!(rig.active.snapshot().state, State::Ready);
        assert_eq!(rig.calls(), [id(5)]);
        assert_eq!(*rig.facts.0.lock().unwrap(), [Some(ours())], "checked inbound once");
        wiring.listener.initialize_current_block_tip(Some(id(5)), false);
        drop(wiring.listener);
        drop(wiring.startup);
        wiring.worker.join().unwrap();
        // The broadcast's tip, READY: the old and new lists, nothing reset, no second check.
        assert_eq!(rig.calls(), [id(5), id(4), id(5)]);
        assert_eq!(rig.active.snapshot().state, State::Ready);
        assert_eq!(*rig.facts.0.lock().unwrap(), [Some(ours()), None]);
    }

    fn ready(rig: &Rig) -> ActiveTip {
        rig.set(5, Ok(list_with(&rig.active)));
        let tip = rig.tip(Some(id(5)));
        tip.init();
        assert_eq!(rig.active.snapshot().state, State::Ready);
        tip
    }

    // Core: active/masternode.cpp:184-194 at v24.0.0
    #[test]
    fn core_block_data_unavailable_from_ready_logs_and_keeps_the_state() {
        let rig = Rig::new();
        let tip = ready(&rig);
        let before = rig.active.snapshot();
        let block = BlockHash::from([7; 32]);
        rig.set(5, Err(ListError::Unavailable(block)));
        let ((), log) = logged(|| {
            tip.updated_block_tip(TipEvent {
                new: id(6),
                fork: Some(id(5)),
                ibd: false,
            })
        });
        assert_eq!(rig.active.snapshot(), before);
        assert!(rig.abort.reports().is_empty());
        let line = format!(
            "CActiveMasternodeManager::UpdatedBlockTip -- masternode list unavailable: {}",
            StoreError::ListUnavailable(block)
        );
        assert!(log.iter().any(|(_, l)| *l == line), "{log:?}");
    }

    // Core: active/masternode.cpp:184-194 at v24.0.0
    #[test]
    fn core_list_fault_aborts_and_leaves_no_identity_in_use() {
        let rig = Rig::new();
        let tip = ready(&rig);
        rig.set(6, Err(ListError::Fault(StoreError::read("disk"))));
        tip.updated_block_tip(TipEvent {
            new: id(6),
            fork: Some(id(5)),
            ibd: false,
        });
        let st = rig.active.snapshot();
        assert_eq!(st.state, State::SomeError);
        assert_eq!((st.pro_tx_hash, st.outpoint), (None, None));
        assert_eq!(rig.abort.reports().len(), 1);
    }

    // Core: init.cpp:2538-2539; active/masternode.cpp:132 at v24.0.0
    #[test]
    fn core_startup_list_fault_aborts() {
        let rig = Rig::new();
        rig.set(5, Err(ListError::Fault(StoreError::inconsistent("diff"))));
        rig.tip(Some(id(5))).init();
        assert_eq!(rig.abort.reports().len(), 1);
        assert_eq!(rig.active.pro_tx_hash(), None);
    }

    // Core: init.cpp:2462 at v24.0.0
    #[test]
    fn unarmed_listener_ignores_the_startup_kick() {
        let rig = Rig::new();
        let wiring = wire(rig.tip(None)).unwrap();
        wiring.listener.initialize_current_block_tip(Some(id(5)), false);
        drop(wiring.listener);
        drop(wiring.startup);
        wiring.worker.join().unwrap();
        assert!(rig.calls().is_empty());
    }

    /// Facts on a routable chain: `-listen` as given, a public IPv4 local address if `found`.
    fn routable_facts(listen: bool, found: bool) -> LocalFacts {
        let public = Service::new(NetAddr::ipv4([1, 2, 3, 4]), 19999);
        LocalFacts {
            listen,
            require_routable: true,
            local_toward_v4: found.then(|| public.clone()),
            loopback_listen: ours(),
            peers: Vec::new(),
            valid: if found {
                vec![public]
            } else {
                Vec::new()
            },
            self_connect_ok: false,
        }
    }

    /// With `-listen=0` on a routable chain, or no usable local address, `Init` stops with
    /// SOME_ERROR before `GetListForBlock`: a faulty list is never read, so nothing aborts. Once
    /// the checks pass, the list is read and its fault aborts.
    // Core: active/masternode.cpp:119-132, :224-226 at v24.0.0
    #[test]
    fn core_init_reads_the_list_only_after_the_listen_and_address_checks() {
        for (listen, found, message) in [
            (false, true, "Masternode must accept connections from outside."),
            (true, false, "Can't detect valid external address."),
        ] {
            let rig = Rig::routable();
            *rig.facts.1.lock().unwrap() = Some(routable_facts(listen, found));
            rig.set(5, Err(ListError::Fault(StoreError::read("disk"))));
            rig.set(6, Err(ListError::Unavailable(BlockHash::from([7; 32]))));
            let tip = rig.tip(Some(id(5)));
            tip.init();
            tip.updated_block_tip(TipEvent {
                new: id(6),
                fork: Some(id(5)),
                ibd: false,
            });
            let st = rig.active.snapshot();
            assert_eq!(st.state, State::SomeError);
            assert!(st.error.starts_with(message), "{}", st.error);
            assert!(rig.calls().is_empty(), "no list read: {:?}", rig.calls());
            assert!(rig.abort.reports().is_empty());
            assert_eq!(*rig.facts.0.lock().unwrap(), [None, None], "no inbound check");

            *rig.facts.1.lock().unwrap() = Some(routable_facts(true, true));
            tip.init();
            assert_eq!(rig.calls(), [id(5)]);
            assert_eq!(rig.abort.reports().len(), 1);
            tip.updated_block_tip(TipEvent {
                new: id(6),
                fork: Some(id(5)),
                ibd: false,
            });
            assert_eq!(rig.calls(), [id(5), id(6)]);
            assert_eq!(rig.abort.reports().len(), 2);
        }
    }

    /// The first panic in a tip's work is `AbortNode` with V's signals thread's text; the worker
    /// then processes no more tips, as that thread stops, and still ends when its senders go.
    // Core: validationinterface.cpp:193 at v24.0.0
    #[test]
    fn a_panicking_tip_aborts_once_and_the_worker_processes_no_more_tips() {
        let rig = Rig::new();
        *rig.lists.panics_at.lock().unwrap() = vec![id(1), id(2)];
        let wiring = wire(rig.tip(None)).unwrap();
        wiring.listener.updated_block_tip(id(1), Some(id(0)), false);
        wiring.listener.updated_block_tip(id(2), Some(id(1)), false);
        drop(wiring.listener);
        drop(wiring.startup);
        wiring.worker.join().expect("the worker did not die");
        assert_eq!(rig.calls(), [id(1)]);
        assert_eq!(
            rig.abort.messages(),
            ["a validation interface listener panicked: the list read panicked"]
        );
    }

    /// Not READY as the tip worker reads the state is `Init`, even if startup's `Init` makes the
    /// state READY before the worker takes the lock: no READY comparison, and a list error is
    /// `Init`'s unhandled exception, not READY's skipped tip.
    // Core: active/masternode.cpp:179-180, :222-226; active/masternode.h:54 at v24.0.0
    #[test]
    fn core_not_ready_as_read_is_init_even_if_the_state_turns_ready() {
        for unavailable in [false, true] {
            let rig = Rig::new();
            rig.set(5, Ok(list_with(&rig.active)));
            if unavailable {
                rig.set(6, Err(ListError::Unavailable(BlockHash::from([7; 32]))));
            }
            let startup = rig.tip(Some(id(5)));
            *rig.facts.2.lock().unwrap() = Some(Box::new(move || startup.init()));
            assert_ne!(rig.active.snapshot().state, State::Ready);
            rig.tip(None).updated_block_tip(TipEvent {
                new: id(6),
                fork: Some(id(5)),
                ibd: false,
            });
            assert_eq!(rig.calls(), [id(5), id(6)]);
            // Core's `Init` with block 6's list, which lacks this masternode, keeps the identity
            // startup's `Init` set; the READY comparison would reset it as REMOVED.
            let st = rig.active.snapshot();
            assert_eq!(st.state, State::Ready);
            assert!(st.pro_tx_hash.is_some());
            let aborts = rig.abort.messages();
            if unavailable {
                assert_eq!(aborts.len(), 1, "{aborts:?}");
            } else {
                assert!(aborts.is_empty(), "{aborts:?}");
            }
        }
    }
}
