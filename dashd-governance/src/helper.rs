//! The superblock back end `ConnectBlock` reads (Core's `m_superblocks`): one published trigger
//! view pinned per `ConnectBlock`, read by `IsValid`, `IsSuperblockTriggered` and
//! `GetSuperblockPayments`, with the sentinel updates written back to the live triggers.

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_dml::MnList;
use dash_governance::manager::TriggerView;
use dash_governance::superblocks::Objects;
use dash_governance::tally::ObjectState;
use dash_governance::trigger::is_valid_block_height;
use dash_primitives::TxOut;
use dashd_chainstate::connect::SuperblockSource;

use crate::exec::GovExec;
use crate::handle::GovernanceHandle;
use crate::sync::DmnTip;

/// What one `ConnectBlock` reads: the view published when it began, the DMN-tip list, and this
/// block's copies of the trigger states its sentinel updates changed.
struct Pin {
    view: Arc<TriggerView>,
    list: Arc<MnList>,
    updated: BTreeMap<[u8; 32], ObjectState>,
}

/// The view's trigger states under this block's copies, copied on first change.
struct Overlay<'a> {
    base: &'a BTreeMap<[u8; 32], ObjectState>,
    own: &'a mut BTreeMap<[u8; 32], ObjectState>,
}

impl Objects for Overlay<'_> {
    fn state(&self, hash: &[u8; 32]) -> Option<&ObjectState> {
        self.own.get(hash).or_else(|| self.base.get(hash))
    }

    fn state_mut(&mut self, hash: &[u8; 32]) -> Option<&mut ObjectState> {
        if !self.own.contains_key(hash) {
            self.own.insert(*hash, self.base.get(hash)?.clone());
        }
        self.own.get_mut(hash)
    }
}

/// [`SuperblockSource`] over the governance handle. The DMN-tip list is read from V's published
/// view, never blocking.
pub struct GovernanceBackend<E> {
    handle: Arc<GovernanceHandle<E>>,
    dmn: Arc<dyn DmnTip>,
    pin: Option<Pin>,
}

impl<E: GovExec + 'static> GovernanceBackend<E> {
    pub fn new(handle: Arc<GovernanceHandle<E>>, dmn: Arc<dyn DmnTip>) -> Self {
        Self {
            handle,
            dmn,
            pin: None,
        }
    }

    /// The generation of the view the current `ConnectBlock` pinned.
    pub fn pinned_generation(&self) -> Option<u64> {
        self.pin.as_ref().map(|p| p.view.generation)
    }

    fn pin(&mut self) -> &mut Pin {
        self.pin.get_or_insert_with(|| Pin {
            view: self.handle.published(),
            list: self.dmn.dmn_tip_list(),
            updated: BTreeMap::new(),
        })
    }
}

impl<E: GovExec + 'static> SuperblockSource for GovernanceBackend<E> {
    fn begin_block(&mut self) {
        self.pin = None;
        self.pin();
    }

    // Core: governance/superblock.h v24.0.2:135
    fn gov_valid(&mut self) -> bool {
        self.pin().view.is_loaded
    }

    /// `AtTip`'s query over this block's copies of the triggers at `h`, which it updates. In the
    /// same call the live triggers it updated are written back and republished. A block that
    /// fails later keeps the write-back, and so does a dry run, as Core's update of the shared
    /// object stands.
    // Core: governance/superblock.cpp v24.0.2:578-620
    fn sb_triggered(&mut self, h: u32) -> bool {
        let handle = Arc::clone(&self.handle);
        let params = handle.params();
        let Ok(height) = i32::try_from(h) else {
            return false;
        };
        if !is_valid_block_height(height, params) {
            return false;
        }
        let now = handle.env().clock().now;
        let Pin {
            view,
            list,
            updated,
        } = self.pin();
        let mut objects = Overlay {
            base: &view.objects,
            own: updated,
        };
        let triggered = view.superblocks.is_triggered(list, height, &mut objects, now, params);
        let mut touched = Vec::new();
        for (hash, sb) in view.superblocks.iter() {
            let Some(state) = updated.get(hash).filter(|_| sb.height == height) else {
                continue;
            };
            touched.push(*hash);
            if state.cached_funding {
                break;
            }
        }
        if !touched.is_empty() {
            handle.write_back_sentinels(view, updated, &touched, list, now);
        }
        triggered
    }

    // Core: governance/superblock.cpp v24.0.2:549-576, :633-666
    fn sb_payments(&mut self, h: u32) -> Option<Vec<TxOut>> {
        let handle = Arc::clone(&self.handle);
        let height = i32::try_from(h).ok()?;
        let Pin {
            view,
            list,
            updated,
        } = self.pin();
        let objects = Overlay {
            base: &view.objects,
            own: updated,
        };
        view.superblocks.payments(list, height, &objects, handle.params())
    }
}
