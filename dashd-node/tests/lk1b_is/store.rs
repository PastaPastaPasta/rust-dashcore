//! `llmq/isdb` behind a tap, for the S1d node tests: [`Tapped`] is an [`IsStore`] over the IsDb that
//! hands each call to a [`Tap`] around the IsDb's own call, and [`TapHost`] gives NodeSut a tapped
//! IsDb at every load while keeping the IsDb for the test.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dash_consensus::Tx;
use dash_consensus::primitives::{OutPoint, TxHash};
use dash_kv::{KvEngine, StoreError};
use dashd_locks::is::db::{ChainedRemoval, IsDb};
use dashd_locks::is::seams::{IsOverlay, IsStore, StageId};
use dashd_locks::is::{IsLockHash, LockedTx};
use dashd_node::instantsend::IsOpener;
use dashd_node::sut::{IsEngine, IsEngineHost, Teardown};

type Result<T> = std::result::Result<T, StoreError>;

/// One store call, as the node made it.
pub enum Call {
    /// A load's open of the store, `-reindex`'s wipe or not.
    Open {
        wipe: bool,
    },
    ConflictingLock(Tx),
    /// An attempt's read through its overlay.
    ConflictingLockIn(Tx),
    ByHash(IsLockHash, bool),
    ByTxid(TxHash),
    ByInput(OutPoint),
    Known(IsLockHash),
    WriteNew(IsLockHash, LockedTx),
    WriteMined(IsLockHash, i32),
    BlockConnected(Vec<Tx>, i32),
    BlockDisconnected(Vec<Tx>, i32),
    RemoveConfirmed(i32),
    RemoveArchived(i32),
    /// A stage, with the id the IsDb gave it once it succeeded.
    Stage {
        hash: IsLockHash,
        root: LockedTx,
        height: i32,
        id: Option<StageId>,
    },
    /// A stage's commit, with whether it succeeded once it ran.
    Commit {
        stage: StageId,
        ok: Option<bool>,
    },
    Discard,
}

/// A call with its transactions shown by txid.
impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let txids = |txs: &[Tx]| txs.iter().map(Tx::txid).collect::<Vec<_>>();
        match self {
            Call::Open {
                wipe,
            } => write!(f, "Open {{ wipe: {wipe} }}"),
            Call::ConflictingLock(tx) => write!(f, "ConflictingLock({})", tx.txid()),
            Call::ConflictingLockIn(tx) => write!(f, "ConflictingLockIn({})", tx.txid()),
            Call::ByHash(hash, cached) => write!(f, "ByHash({}, {cached})", hex(hash)),
            Call::ByTxid(txid) => write!(f, "ByTxid({txid})"),
            Call::ByInput(input) => write!(f, "ByInput({input:?})"),
            Call::Known(hash) => write!(f, "Known({})", hex(hash)),
            Call::WriteNew(hash, _) => write!(f, "WriteNew({})", hex(hash)),
            Call::WriteMined(hash, height) => write!(f, "WriteMined({}, {height})", hex(hash)),
            Call::BlockConnected(txs, height) => {
                write!(f, "BlockConnected({:?}, {height})", txids(txs))
            }
            Call::BlockDisconnected(txs, height) => {
                write!(f, "BlockDisconnected({:?}, {height})", txids(txs))
            }
            Call::RemoveConfirmed(height) => write!(f, "RemoveConfirmed({height})"),
            Call::RemoveArchived(height) => write!(f, "RemoveArchived({height})"),
            Call::Stage {
                hash,
                height,
                id,
                ..
            } => write!(f, "Stage({}, {height}, {id:?})", hex(hash)),
            Call::Commit {
                stage,
                ok,
            } => write!(f, "Commit({stage:?}, {ok:?})"),
            Call::Discard => write!(f, "Discard"),
        }
    }
}

fn hex(hash: &IsLockHash) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

impl Call {
    /// The call's variant name.
    pub fn name(&self) -> &'static str {
        match self {
            Call::Open {
                ..
            } => "Open",
            Call::ConflictingLock(_) => "ConflictingLock",
            Call::ConflictingLockIn(_) => "ConflictingLockIn",
            Call::ByHash(..) => "ByHash",
            Call::ByTxid(_) => "ByTxid",
            Call::ByInput(_) => "ByInput",
            Call::Known(_) => "Known",
            Call::WriteNew(..) => "WriteNew",
            Call::WriteMined(..) => "WriteMined",
            Call::BlockConnected(..) => "BlockConnected",
            Call::BlockDisconnected(..) => "BlockDisconnected",
            Call::RemoveConfirmed(_) => "RemoveConfirmed",
            Call::RemoveArchived(_) => "RemoveArchived",
            Call::Stage {
                ..
            } => "Stage",
            Call::Commit {
                ..
            } => "Commit",
            Call::Discard => "Discard",
        }
    }

    /// Whether the call is the attempt's own, through its overlay: the gate never parks it.
    pub fn attempt_side(&self) -> bool {
        matches!(
            self,
            Call::ConflictingLockIn(_) | Call::Stage { .. } | Call::Commit { .. } | Call::Discard
        )
    }
}

pub fn copy(tx: &Tx) -> Tx {
    Tx::new(tx.raw().clone())
}

/// What a [`Tapped`] store does around each call.
pub trait Tap: Send + Sync + 'static {
    /// Runs `inner`, the IsDb's call for `call`, exactly once. `inner` fills in what the call
    /// learns (a stage's id, a commit's success).
    fn around(&self, call: Call, inner: &mut dyn FnMut(&mut Call));
}

/// `f` run once through `tap.around` for `call`, and its result.
fn around<T: Tap + ?Sized, R>(tap: &T, call: Call, f: impl FnOnce(&mut Call) -> R) -> R {
    let (mut f, mut out) = (Some(f), None);
    tap.around(call, &mut |call| out = f.take().map(|f| f(call)));
    out.expect("the tap ran the call")
}

/// An [`IsStore`] over `db` whose every call goes through `tap`.
pub struct Tapped<E: KvEngine, T: Tap> {
    db: Arc<IsDb<E>>,
    tap: Arc<T>,
}

impl<E: KvEngine, T: Tap> Tapped<E, T> {
    fn run<R>(&self, call: Call, f: impl FnOnce(&IsDb<E>) -> R) -> R {
        around(&*self.tap, call, |_| f(&self.db))
    }
}

impl<E: KvEngine + 'static, T: Tap> IsStore for Tapped<E, T> {
    fn conflicting_lock(&self, tx: &Tx) -> Result<Option<(IsLockHash, LockedTx)>> {
        self.run(Call::ConflictingLock(copy(tx)), |db| db.conflicting_lock(tx))
    }

    fn conflicting_lock_in(
        &self,
        overlay: &IsOverlay,
        tx: &Tx,
    ) -> Result<Option<(IsLockHash, LockedTx)>> {
        self.run(Call::ConflictingLockIn(copy(tx)), |db| db.conflicting_lock_in(overlay, tx))
    }

    fn by_hash(&self, hash: &IsLockHash, use_cache: bool) -> Result<Option<LockedTx>> {
        self.run(Call::ByHash(*hash, use_cache), |db| db.by_hash(hash, use_cache))
    }

    fn by_txid(&self, txid: &TxHash) -> Result<Option<LockedTx>> {
        self.run(Call::ByTxid(*txid), |db| db.by_txid(txid))
    }

    fn by_input(&self, input: &OutPoint) -> Result<Option<LockedTx>> {
        self.run(Call::ByInput(*input), |db| db.by_input(input))
    }

    fn known(&self, hash: &IsLockHash) -> Result<bool> {
        self.run(Call::Known(*hash), |db| db.known(hash))
    }

    fn write_new(&self, hash: &IsLockHash, lock: &LockedTx) -> Result<()> {
        self.run(Call::WriteNew(*hash, lock.clone()), |db| db.write_new(hash, lock))
    }

    fn write_mined(&self, hash: &IsLockHash, height: i32) -> Result<()> {
        self.run(Call::WriteMined(*hash, height), |db| db.write_mined(hash, height))
    }

    fn block_connected(&self, transactions: &[Tx], height: i32) -> Result<()> {
        let call = Call::BlockConnected(transactions.iter().map(copy).collect(), height);
        self.run(call, |db| db.block_connected(transactions, height))
    }

    fn block_disconnected(&self, transactions: &[Tx], height: i32) -> Result<()> {
        let call = Call::BlockDisconnected(transactions.iter().map(copy).collect(), height);
        self.run(call, |db| db.block_disconnected(transactions, height))
    }

    fn remove_confirmed(&self, height: i32) -> Result<BTreeMap<IsLockHash, LockedTx>> {
        self.run(Call::RemoveConfirmed(height), |db| db.remove_confirmed(height))
    }

    fn remove_archived(&self, height: i32) -> Result<()> {
        self.run(Call::RemoveArchived(height), |db| db.remove_archived(height))
    }

    fn stage_remove_chained(
        &self,
        overlay: &mut IsOverlay,
        hash: &IsLockHash,
        root: &LockedTx,
        archive_h: i32,
    ) -> Result<(ChainedRemoval, StageId)> {
        let call = Call::Stage {
            hash: *hash,
            root: root.clone(),
            height: archive_h,
            id: None,
        };
        around(&*self.tap, call, |call| {
            let staged = self.db.stage_remove_chained(overlay, hash, root, archive_h);
            if let (
                Call::Stage {
                    id,
                    ..
                },
                Ok((_, stage)),
            ) = (call, &staged)
            {
                *id = Some(*stage);
            }
            staged
        })
    }

    fn commit_staged(&self, overlay: &mut IsOverlay, stage: StageId) -> Result<()> {
        let call = Call::Commit {
            stage,
            ok: None,
        };
        around(&*self.tap, call, |call| {
            let committed = self.db.commit_staged(overlay, stage);
            if let Call::Commit {
                ok,
                ..
            } = call
            {
                *ok = Some(committed.is_ok());
            }
            committed
        })
    }

    fn discard_staged(&self, overlay: &mut IsOverlay) {
        self.tap.around(Call::Discard, &mut |_| self.db.discard_staged(overlay));
    }
}

/// An [`IsEngineHost`] over `engine` as [`IsEngine`] keeps it, whose every load opens the IsDb
/// through `tap` (as a [`Call::Open`]) and gets it behind `tap`. The last load's IsDb stays
/// readable through [`TapHost::db`].
pub struct TapHost<E: KvEngine, T: Tap> {
    host: Arc<IsEngine<E>>,
    tap: Arc<T>,
    db: Arc<Mutex<Option<Arc<IsDb<E>>>>>,
}

impl<E: KvEngine + 'static, T: Tap> TapHost<E, T> {
    pub fn new(engine: Arc<E>, tap: Arc<T>) -> Self {
        TapHost {
            host: Arc::new(IsEngine(engine)),
            tap,
            db: Arc::default(),
        }
    }

    /// The last load's IsDb.
    pub fn db(&self) -> Option<Arc<IsDb<E>>> {
        self.db.lock().unwrap().clone()
    }
}

impl<E: KvEngine + 'static, T: Tap> IsEngineHost for TapHost<E, T> {
    fn opener(&self) -> IsOpener {
        let (host, tap, slot) =
            (Arc::clone(&self.host), Arc::clone(&self.tap), Arc::clone(&self.db));
        Box::new(move |wipe| {
            let open = Call::Open {
                wipe,
            };
            let db = Arc::new(around(&*tap, open, |_| host.open(wipe))?);
            *slot.lock().unwrap() = Some(Arc::clone(&db));
            Ok(Arc::new(Tapped {
                db,
                tap,
            }) as Arc<dyn IsStore>)
        })
    }

    fn teardown(&self, how: Teardown) {
        self.host.teardown(how);
    }

    fn rows(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.host.rows()
    }

    fn replace(&self, rows: &[(Vec<u8>, Vec<u8>)]) {
        self.host.replace(rows);
    }
}
