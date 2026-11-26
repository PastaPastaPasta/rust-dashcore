//! MN1-4a's template-governance query seam.

use std::sync::{Arc, Mutex, mpsc};

use dash_consensus::{Block, BlockId};
use dash_dml::MnList;
use dash_primitives::{BlockHash, TxOut};

use super::support::{Rig, RigConfig};
use crate::chain::{
    BlockConnector, BlockStatusWriter, ChainCommand, ChainHandle, ConnectCx, ConnectOutcome,
    DisconnectResult, Fatal, NullConnector, ProposalChecks, VContext, ViewTarget,
};
use crate::connect::{SuperblockTemplate, TemplateGovernance, TemplateGovernanceSlot};

struct FakeGovernance {
    expected_list: Mutex<Option<BlockHash>>,
    payments: Vec<TxOut>,
}

impl FakeGovernance {
    fn new(payments: Vec<TxOut>) -> Self {
        Self {
            expected_list: Mutex::new(None),
            payments,
        }
    }
}

impl TemplateGovernance for FakeGovernance {
    fn superblock_for_template(
        &self,
        h: u32,
        dmn_tip_list: &MnList,
    ) -> Result<SuperblockTemplate, Fatal> {
        let expected = self.expected_list.lock().unwrap().expect("query recorded its list");
        assert_eq!(*dmn_tip_list.block_hash(), expected, "the query's DMN tip list");
        match h {
            100 => Ok(SuperblockTemplate::NotTriggered),
            101 => Ok(SuperblockTemplate::Triggered(self.payments.clone())),
            102 => Err(Fatal::new("fake governance failure")),
            _ => unreachable!("unexpected template height {h}"),
        }
    }
}

struct GovernedConnector {
    slot: TemplateGovernanceSlot,
}

impl BlockConnector for GovernedConnector {
    fn template_governance(&self) -> Option<&dyn TemplateGovernance> {
        self.slot.get()
    }

    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.connect_block(cx, v, target, block, checked, id, st)
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        NullConnector.disconnect_block(cx, v, target, block, id)
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        check_pow: bool,
        check_merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        NullConnector.test_block_validity(cx, v, checks, block, prev, check_pow, check_merkle)
    }
}

fn query(
    rig: &Rig,
    gov: Option<&Arc<FakeGovernance>>,
    height: u32,
) -> Result<SuperblockTemplate, Fatal> {
    let expected = gov.cloned();
    rig.v
        .query_or_stop(move |q| {
            let list = q.evo_view().dmn_list().map_err(|e| Fatal::new(e.to_string()))?;
            if let Some(gov) = expected {
                *gov.expected_list.lock().unwrap() = Some(*list.block_hash());
            }
            q.superblock_for_template(height, &list)
        })
        .recv()
        .expect("V answers")
}

#[test]
fn template_governance_answers_all_results_through_a_chain_handle_query() {
    let empty_slot = TemplateGovernanceSlot::default();
    let unavailable = Rig::with(RigConfig {
        connector: Some(Box::new(GovernedConnector {
            slot: empty_slot,
        })),
        ..RigConfig::default()
    });
    assert!(matches!(
        query(&unavailable, None, 100),
        Ok(SuperblockTemplate::Unavailable {
            owner: "GV1b"
        })
    ));

    let slot = TemplateGovernanceSlot::default();
    let mut rig = Rig::with(RigConfig {
        connector: Some(Box::new(GovernedConnector {
            slot: slot.clone(),
        })),
        ..RigConfig::default()
    });
    let payments =
        vec![TxOut::from_duffs(11, vec![0x51].into()), TxOut::from_duffs(22, vec![0x52].into())];
    let fake = Arc::new(FakeGovernance::new(payments.clone()));
    assert!(slot.register(fake.clone()).is_ok());

    assert!(matches!(query(&rig, Some(&fake), 100), Ok(SuperblockTemplate::NotTriggered)));
    match query(&rig, Some(&fake), 101) {
        Ok(SuperblockTemplate::Triggered(actual)) => assert_eq!(actual, payments),
        Ok(_) => panic!("expected triggered template"),
        Err(error) => panic!("unexpected governance error: {error}"),
    }
    assert!(query(&rig, Some(&fake), 102).is_err());
    // A governance `Fatal` stops V with its text (plan §2.6).
    let exit = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some("fake governance failure"));
}

/// One turn's `ChainQuery::is_ibd` and then `V::is_ibd`.
fn both_ibd(rig: &Rig) -> (bool, bool) {
    let q = rig.v.query(|q| q.is_ibd()).recv().expect("V answers");
    let (tx, rx) = mpsc::channel();
    rig.on_v(move |v| {
        let _ = tx.send(v.is_ibd());
        Ok(())
    });
    (q, rx.recv().expect("V answers"))
}

#[test]
fn query_is_ibd_equals_vs_across_reindex_loading_and_the_latch() {
    let old = Rig::with(RigConfig::default());
    assert_eq!(both_ibd(&old), (true, true), "a day-old genesis tip");

    // The reindex flag is set before init's ABC, whose reads would otherwise latch a recent tip.
    let rig = Rig::with(RigConfig {
        after_genesis: Some(600),
        before_init: Some(Box::new(|v: &ChainHandle| {
            v.send(ChainCommand::SetLoading {
                importing: None,
                reindex: Some(true),
            })
            .expect("V runs");
        })),
        ..RigConfig::default()
    });
    assert_eq!(both_ibd(&rig), (true, true), "-reindex loading");
    rig.on_v(|v| {
        v.loading.set(None, Some(false));
        Ok(())
    });
    // Only the query reads IBD on the recent tip, so the latch it leaves is its own.
    assert!(!rig.v.query(|q| q.is_ibd()).recv().expect("V answers"), "a recent tip, not loading");
    rig.on_v(|v| {
        v.loading.set(None, Some(true));
        Ok(())
    });
    assert_eq!(both_ibd(&rig), (false, false), "the query's latch outlives a later load");
}
