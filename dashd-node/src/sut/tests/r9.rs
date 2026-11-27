//! GV1b R9 (DEC-1927): dashd-governance's replay of S0's restart recording on this crate's
//! process. `NodeSut` runs with governance enabled, its store, load, listener and init callback
//! the production ones, and the recording's `restart` is a clean `NodeSut` restart. The init
//! callback runs but is not observable here: since dashpay/dash#8097 the load already sets the
//! cached height to the tip, so its `tip` changes nothing (`node/tests/governance.rs` checks it).

use std::sync::mpsc;
use std::time::{Duration, Instant};

use dashd_governance::GovExec as _;
use dashd_governance::{HostParts, HostProcess, NodeHost, r9_restart};
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_masternode::netfulfilled::NetFulfilledRequestManager;
use dashd_masternode::sync::MasternodeSync;

use super::*;
use crate::governance::NodeGovernance;

/// `gov_capture.py`'s chain (the fixtures' `PROVENANCE.md`), with governance on.
const R9_ARGS: [&str; 7] = [
    "-regtest",
    "-printtoconsole=0",
    "-dip3params=2:2",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
    "-budgetparams=10:10:10",
    "-disablegovernance=0",
];

/// The replay's parts, over which every open makes its governance, and that open's mnsync.
struct Rig {
    parts: Arc<HostParts>,
    sync: Mutex<Option<Arc<MasternodeSync>>>,
}

impl Rig {
    fn sync(&self) -> Arc<MasternodeSync> {
        self.sync.lock().unwrap().clone().expect("made with the open's governance")
    }
}

impl GovernanceRig for Rig {
    fn make(
        &self,
        enabled: bool,
        chain: &ChainSetup,
        metaman: &Arc<MasternodeMetaMan>,
        netfulfilled: &Arc<NetFulfilledRequestManager>,
    ) -> Result<Arc<NodeGovernance>, InitFailure> {
        let sync = self.parts.mnsync(Arc::clone(netfulfilled));
        *self.sync.lock().unwrap() = Some(Arc::clone(&sync));
        let core = Arc::clone(&self.parts.core);
        let governance = NodeGovernance::new(enabled, chain, core, sync, Arc::clone(metaman))?;
        governance.bind_index(Some(Arc::clone(&self.parts.index)));
        Ok(governance)
    }

    fn listeners(&self, v: &ChainHandle) -> [Arc<dyn ValidationInterface>; 2] {
        [(self.parts.before)(self.sync(), v.view_handle()), Arc::clone(&self.parts.after)]
    }
}

/// `NodeSut` as R9's node, its mock time the sim's.
struct Host {
    dir: tempfile::TempDir,
    rig: Option<Arc<Rig>>,
    sut: Option<NodeSut>,
}

impl Host {
    fn sut(&self) -> &NodeSut {
        self.sut.as_ref().expect("opened")
    }

    fn rig(&self) -> &Rig {
        self.rig.as_ref().expect("opened")
    }

    fn now(&self) -> i64 {
        self.rig().parts.clock.node_now().secs()
    }

    fn governance(&self) -> &Arc<NodeGovernance> {
        self.sut().steps.governance.as_ref().expect("made at the open")
    }

    /// The open process, which must have loaded.
    fn process(&self) -> HostProcess {
        let sut = self.sut();
        assert!(sut.down().is_none(), "R9's node: {:?}", sut.down());
        let governance = self.governance();
        let fulfilled = Arc::clone(sut.steps.netfulfilled().expect("made at step 6"));
        let parts = &self.rig().parts;
        HostProcess {
            v: sut.chain().expect("V runs").clone(),
            handle: Arc::clone(governance.handle()),
            sync: self.rig().sync(),
            net: governance.net_handler(&fulfilled, &parts.shared, &parts.net),
            fulfilled,
        }
    }
}

impl NodeHost for Host {
    fn open(&mut self, parts: Arc<HostParts>) -> HostProcess {
        let rig = Arc::new(Rig {
            parts,
            sync: Mutex::default(),
        });
        self.rig = Some(Arc::clone(&rig));
        let coins = Arc::new(MemoryEngine::new(&CHAINSTATE_SCHEMA));
        let mut sut = NodeSut::unopened(self.dir.path(), &R9_ARGS, self.now(), ui(), coins)
            .expect("R9's arguments");
        sut.governance_rig = Some(rig);
        sut.open();
        self.sut = Some(sut);
        self.process()
    }

    fn restart(&mut self) -> HostProcess {
        let now = self.now();
        let sut = self.sut.as_mut().expect("opened");
        sut.teardown(Teardown::Clean);
        sut.clock().set_mocktime(now);
        sut.open();
        self.process()
    }

    fn settle(&self) {
        let sut = self.sut();
        sut.clock().set_mocktime(self.now());
        assert!(sut.sync_signals(), "V's signals");
        let governance = self.governance();
        // V runs the commands in the order posted: those V's signals posted ran before this one.
        let (tx, rx) = mpsc::channel();
        governance.exec().post(Box::new(move |_| {
            let _ = tx.send(());
        }));
        rx.recv_timeout(Duration::from_secs(60)).expect("V ran governance's commands");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !governance.effects_applied() {
            assert!(Instant::now() < deadline, "the effects worker never caught up");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(sut.latched().is_none(), "R9's node latched: {:?}", sut.latched());
    }

    fn stop(&mut self) {
        if let Some(mut sut) = self.sut.take() {
            sut.teardown(Teardown::Clean);
            assert!(sut.down().is_none(), "R9's node: {:?}", sut.down());
        }
    }
}

/// R9 (DEC-1927): governance's store written at a clean stop, loaded at the next start
/// (`LoadCache` with the store present) and `InitOnLoad`, with V's init callback run but not
/// observable (see the module doc), then the next superblock judged as dashd v24.0.0 judged
/// it, with dashpay/dash#8097 the one difference
/// (`dashd_governance::r9_restart` asserts the replay's counts).
// Core: init.cpp v24.0.2:414, :2260-2267; governance/governance.cpp v24.0.2:91-112
#[test]
fn r9_governance_across_a_clean_restart() {
    let host = Host {
        dir: tempfile::tempdir().unwrap(),
        rig: None,
        sut: None,
    };
    r9_restart(Box::new(host));
}
