//! Stand-ins for the masternode seams (BL-702 §5), for the processor's sims and AM1's handler
//! tests: a list with a read counter and a publication hook, a fixed list, and a recording signer.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use dash_evo::llmq::BlsPkValue;
use dash_evo::types::RawMnType;
use dash_p2p::{PeerHandle, Service};
use dash_p2p_core::{MnAuth, PlatformBan};
use dash_params::LlmqType;
use dash_types::codec::BaseCodec;
use dash_types::{Hashable, Numeric};
use parking_lot::Mutex;

use crate::processing::seams::{
    MasternodeNet, MnAuthChallenge, MnAuthSigner, MnListView, OperatorKeyRef, PlatformBans,
    PlatformQuorums,
};

/// A fixed masternode list: the operator keys and service slots it was built with.
#[derive(Clone, Debug, Default)]
pub struct FakeMnList {
    operators: HashMap<[u8; 32], OperatorKeyRef>,
    mn_types: HashMap<[u8; 32], RawMnType>,
    services: HashMap<Service, [u8; 32]>,
    bls_legacy_scheme: bool,
}

impl FakeMnList {
    /// An empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// The list with masternode `pro_reg_tx_hash` operated by `key`.
    pub fn with(mut self, pro_reg_tx_hash: [u8; 32], key: OperatorKeyRef) -> Self {
        self.operators.insert(pro_reg_tx_hash, key);
        self
    }

    /// Sets the stored masternode type for `pro_reg_tx_hash`.
    pub fn with_mn_type(mut self, pro_reg_tx_hash: [u8; 32], mn_type: RawMnType) -> Self {
        self.mn_types.insert(pro_reg_tx_hash, mn_type);
        self
    }

    /// The list entry `pro_reg_tx_hash` at `service`.
    pub fn with_service(mut self, service: Service, pro_reg_tx_hash: [u8; 32]) -> Self {
        self.services.insert(service, pro_reg_tx_hash);
        self
    }

    /// The process scheme at this publication.
    pub fn with_bls_legacy_scheme(mut self, legacy: bool) -> Self {
        self.bls_legacy_scheme = legacy;
        self
    }
}

impl MnListView for FakeMnList {
    fn operator(&self, pro_reg_tx_hash: &[u8; 32]) -> Option<OperatorKeyRef> {
        self.operators.get(pro_reg_tx_hash).copied()
    }

    fn by_service(&self, service: &Service) -> Option<[u8; 32]> {
        self.services.get(service).copied()
    }

    fn bls_legacy_scheme(&self) -> bool {
        self.bls_legacy_scheme
    }

    fn mn_type(&self, pro_reg_tx_hash: &[u8; 32]) -> Option<RawMnType> {
        self.operators.get(pro_reg_tx_hash)?;
        self.mn_types.get(pro_reg_tx_hash).copied()
    }
}

/// A [`MasternodeNet`] that counts its reads. [`FakeMnNet::on_read`] publishes a new list right
/// after a given read, so a test can race a publication against a handler's kept list.
pub struct FakeMnNet {
    synced: AtomicBool,
    current: Mutex<Arc<dyn MnListView>>,
    reads: AtomicUsize,
    hook: Mutex<Option<(usize, Arc<dyn MnListView>)>>,
    qwatch: AtomicBool,
    qwatch_calls: AtomicUsize,
}

impl FakeMnNet {
    /// A node `synced` or not, with `list` published and no `qwatch` wanted.
    pub fn new(synced: bool, list: Arc<dyn MnListView>) -> Self {
        Self {
            synced: AtomicBool::new(synced),
            current: Mutex::new(list),
            reads: AtomicUsize::new(0),
            hook: Mutex::new(None),
            qwatch: AtomicBool::new(false),
            qwatch_calls: AtomicUsize::new(0),
        }
    }

    /// Sets `IsBlockchainSynced()`.
    pub fn set_synced(&self, synced: bool) {
        self.synced.store(synced, Ordering::SeqCst);
    }

    /// Publishes `list`: every later [`MasternodeNet::tip_list`] returns it.
    pub fn publish(&self, list: Arc<dyn MnListView>) {
        *self.current.lock() = list;
    }

    /// Publishes `list` right after the `n`th [`MasternodeNet::tip_list`] call (counted from
    /// the first call ever), once.
    pub fn on_read(&self, n: usize, list: Arc<dyn MnListView>) {
        *self.hook.lock() = Some((n, list));
    }

    /// How many times [`MasternodeNet::tip_list`] was called.
    pub fn tip_list_reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    /// What [`MasternodeNet::wants_qwatch`] answers for every peer.
    pub fn set_wants_qwatch(&self, wants: bool) {
        self.qwatch.store(wants, Ordering::SeqCst);
    }

    /// How many times [`MasternodeNet::wants_qwatch`] was called.
    pub fn wants_qwatch_calls(&self) -> usize {
        self.qwatch_calls.load(Ordering::SeqCst)
    }
}

impl MasternodeNet for FakeMnNet {
    fn blockchain_synced(&self) -> bool {
        self.synced.load(Ordering::SeqCst)
    }

    fn tip_list(&self) -> Arc<dyn MnListView> {
        let list = Arc::clone(&self.current.lock());
        let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        let due = {
            let mut hook = self.hook.lock();
            hook.take_if(|(at, _)| *at == n)
        };
        if let Some((_, next)) = due {
            self.publish(next);
        }
        list
    }

    fn wants_qwatch(&self, _peer: &PeerHandle) -> bool {
        self.qwatch_calls.fetch_add(1, Ordering::SeqCst);
        self.qwatch.load(Ordering::SeqCst)
    }
}

/// A signer that records each challenge it is asked to sign and answers a fixed message.
pub struct FakeSigner {
    reply: Option<MnAuth>,
    seen: Mutex<Vec<MnAuthChallenge>>,
}

impl FakeSigner {
    /// A signer answering `reply` (`None`: our proTxHash is null).
    pub fn new(reply: Option<MnAuth>) -> Self {
        Self {
            reply,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Every challenge it was asked to sign, in order.
    pub fn challenges(&self) -> Vec<MnAuthChallenge> {
        self.seen.lock().clone()
    }

    /// How many times it was asked to sign.
    pub fn calls(&self) -> usize {
        self.seen.lock().len()
    }
}

impl MnAuthSigner for FakeSigner {
    fn mnauth_payload(&self, c: &MnAuthChallenge) -> Option<MnAuth> {
        self.seen.lock().push(*c);
        self.reply.clone()
    }
}

/// An `mnauth` with proRegTxHash `[tag; 32]` and signature bytes `[tag; 96]` (not a valid
/// signature: the seams carry the bytes as read).
pub fn sample_mnauth(tag: u8) -> MnAuth {
    let bytes = [tag; 32 + 96];
    MnAuth::decode(&mut &bytes[..]).expect("128 bytes decode as an mnauth")
}

/// A configurable platform-ban store for processor simulations.
#[derive(Default)]
pub struct FakePlatformBans {
    enabled: AtomicBool,
    values: Mutex<HashMap<[u8; 32], PlatformBan>>,
    set_calls: AtomicUsize,
}

impl FakePlatformBans {
    /// Creates an enabled empty store.
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            ..Self::default()
        }
    }

    /// Sets whether the platform-ban manager is installed.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    /// Inserts a ban for getdata tests.
    pub fn insert(&self, ban: PlatformBan) -> [u8; 32] {
        let hash = ban.hash().to_lendian();
        self.values.lock().insert(hash, ban);
        hash
    }

    /// How many times `SetPlatformBan` was called.
    pub fn set_calls(&self) -> usize {
        self.set_calls.load(Ordering::SeqCst)
    }
}

impl PlatformBans for FakePlatformBans {
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    fn already_have(&self, inv_hash: &[u8; 32]) -> bool {
        self.values.lock().contains_key(inv_hash)
    }

    fn set(&self, inv_hash: [u8; 32], ban: PlatformBan) -> bool {
        self.set_calls.fetch_add(1, Ordering::SeqCst);
        self.values.lock().insert(inv_hash, ban).is_none()
    }

    fn get(&self, inv_hash: &[u8; 32]) -> Option<PlatformBan> {
        self.values.lock().get(inv_hash).cloned()
    }
}

/// A fixed platform quorum key with a lookup counter for processor simulations.
pub struct FakePlatformQuorums {
    enabled: AtomicBool,
    key: Mutex<Option<(LlmqType, [u8; 32], BlsPkValue)>>,
    lookups: AtomicUsize,
}

impl FakePlatformQuorums {
    /// Creates an enabled lookup with one optional key.
    pub fn new(key: Option<(LlmqType, [u8; 32], BlsPkValue)>) -> Self {
        Self {
            enabled: AtomicBool::new(true),
            key: Mutex::new(key),
            lookups: AtomicUsize::new(0),
        }
    }

    /// Sets whether the platform quorum manager is installed.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    /// How many quorum lookups were made.
    pub fn lookups(&self) -> usize {
        self.lookups.load(Ordering::SeqCst)
    }
}

impl PlatformQuorums for FakePlatformQuorums {
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    fn quorum_pubkey(&self, llmq_type: LlmqType, quorum_hash: &[u8; 32]) -> Option<BlsPkValue> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        let key = self.key.lock();
        key.as_ref()
            .filter(|(t, h, _)| *t == llmq_type && h == quorum_hash)
            .map(|(_, _, key)| key.clone())
    }
}
