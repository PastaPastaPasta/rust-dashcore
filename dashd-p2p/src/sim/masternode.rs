//! Stand-ins for the masternode seams (BL-702 §5), for the processor's sims and AM1's handler
//! tests: a list with a read counter and a publication hook, a fixed list, and a recording signer.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use dash_p2p::PeerHandle;
use dash_p2p_core::MnAuth;
use dash_types::codec::BaseCodec;
use parking_lot::Mutex;

use crate::processing::seams::{
    MasternodeNet, MnAuthChallenge, MnAuthSigner, MnListView, OperatorKeyRef,
};

/// A fixed masternode list: the operator keys it was built with.
#[derive(Clone, Debug, Default)]
pub struct FakeMnList {
    operators: HashMap<[u8; 32], OperatorKeyRef>,
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
}

impl MnListView for FakeMnList {
    fn operator(&self, pro_reg_tx_hash: &[u8; 32]) -> Option<OperatorKeyRef> {
        self.operators.get(pro_reg_tx_hash).copied()
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
