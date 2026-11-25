//! MN1-4a: the template-governance seam on the node's own connector, built by `chainstate.rs`
//! and queried through the node's `ChainHandle` (FillBlockPayments' superblock half,
//! `masternode/payments.cpp:449-453` at v24.0.0).

use dash_dml::MnList;
use dash_primitives::TxOut;
use dashd_chainstate::connect::{SuperblockTemplate, TemplateGovernance, TemplateGovernanceSlot};

use super::*;

/// Answers `NotTriggered` at 1 and `outs` at 2, and checks it is handed the list its turn read.
struct FakeGovernance {
    outs: Vec<TxOut>,
    turn_list: Mutex<Option<usize>>,
    calls: AtomicU32,
}

impl TemplateGovernance for FakeGovernance {
    fn superblock_for_template(
        &self,
        h: u32,
        dmn_tip_list: &MnList,
    ) -> Result<SuperblockTemplate, Fatal> {
        let expected = self.turn_list.lock().unwrap().expect("the turn recorded its list");
        assert_eq!(dmn_tip_list as *const MnList as usize, expected, "the turn's DMN tip list");
        self.calls.fetch_add(1, Ordering::SeqCst);
        match h {
            1 => Ok(SuperblockTemplate::NotTriggered),
            2 => Ok(SuperblockTemplate::Triggered(self.outs.clone())),
            _ => panic!("unexpected template height {h}"),
        }
    }
}

/// One query turn: `dmn_list()`, then `superblock_for_template(h)` with that list.
fn superblock(
    chain: &ChainHandle,
    fake: Option<Arc<FakeGovernance>>,
    h: u32,
) -> Result<SuperblockTemplate, Fatal> {
    chain
        .query_or_stop(move |q| {
            let list = q.evo_view().dmn_list().map_err(|e| Fatal::new(e.to_string()))?;
            if let Some(fake) = &fake {
                *fake.turn_list.lock().unwrap() = Some(Arc::as_ptr(&list) as usize);
            }
            q.superblock_for_template(h, &list)
        })
        .recv()
        .expect("V answers")
}

#[test]
fn the_nodes_connector_answers_from_the_slot_it_was_built_with() {
    let dir = tempfile::tempdir().unwrap();
    let sut = sut(dir.path(), &[]);
    loaded(&sut);
    let chain = sut.chain().expect("V runs").clone();
    let slot: TemplateGovernanceSlot = sut.steps.chainstate().template_governance_slot();

    assert!(slot.get().is_none());
    assert!(matches!(
        superblock(&chain, None, 1),
        Ok(SuperblockTemplate::Unavailable {
            owner: "GV1b"
        })
    ));

    let outs =
        vec![TxOut::from_duffs(11, vec![0x51].into()), TxOut::from_duffs(22, vec![0x52].into())];
    let fake = Arc::new(FakeGovernance {
        outs: outs.clone(),
        turn_list: Mutex::new(None),
        calls: AtomicU32::new(0),
    });
    assert!(slot.register(fake.clone()).is_ok());
    assert!(matches!(
        superblock(&chain, Some(fake.clone()), 1),
        Ok(SuperblockTemplate::NotTriggered)
    ));
    match superblock(&chain, Some(fake.clone()), 2) {
        Ok(SuperblockTemplate::Triggered(got)) => assert_eq!(got, outs, "in payment order"),
        other => panic!("expected the triggered payments, got {:?}", other.err()),
    }

    let second = Arc::new(FakeGovernance {
        outs: Vec::new(),
        turn_list: Mutex::new(None),
        calls: AtomicU32::new(0),
    });
    let refused = slot.register(second.clone()).expect_err("a second registration is refused");
    assert!(Arc::ptr_eq(&refused, &(second.clone() as Arc<dyn TemplateGovernance>)));
    match superblock(&chain, Some(fake.clone()), 2) {
        Ok(SuperblockTemplate::Triggered(got)) => assert_eq!(got, outs, "the first still answers"),
        other => panic!("expected the first fake's payments, got {:?}", other.err()),
    }
    assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
    assert_eq!(second.calls.load(Ordering::SeqCst), 0);
    assert_eq!(sut.down(), None, "no turn stopped V");
}
