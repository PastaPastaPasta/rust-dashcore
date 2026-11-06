use dash_params::{BuriedDeployment, ChainParamsBuilder, Network, Release};

use super::*;

fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest")
}

/// Active from a height, as Core's buried deployments: `active_after(prev)` is
/// `prev.height + 1 >= from`, with block ids equal to heights.
struct From(u32);

impl DeploymentView for From {
    fn active_after(&self, prev: Option<BlockId>, _: Deployment) -> bool {
        assert_ne!(prev, Some(DUMMY_ID), "the view under the proposal never sees the dummy");
        prev.map_or(0, |p| p.0 + 1) >= self.0
    }

    fn active_at(&self, block: BlockId, _: Deployment) -> bool {
        assert_ne!(block, DUMMY_ID, "the view under the proposal never sees the dummy");
        block.0 >= self.0
    }

    fn versionbits(&self, prev: Option<BlockId>, _: DeploymentPos) -> Option<VersionbitsAnswer> {
        assert_ne!(prev, Some(DUMMY_ID), "the view under the proposal never sees the dummy");
        None
    }
}

const DIP3: Deployment = Deployment::Buried(BuriedDeployment::Dip0003);

/// The dummy on `parent` (ids are heights), over `inner`.
fn on<'a>(p: &'a ChainSetup, inner: &'a From, parent: u32) -> ProposalDeployments<'a> {
    ProposalDeployments::new(inner, p, BlockId(parent), parent + 1)
}

#[test]
fn the_dummy_is_active_where_its_parents_child_is() {
    let p = regtest();
    let inner = From(48);
    for (parent, active) in [(46, false), (47, true), (48, true)] {
        let d = on(&p, &inner, parent);
        assert_eq!(d.active_at(DUMMY_ID, DIP3), active, "parent {parent}");
        assert_eq!(d.active_at(DUMMY_ID, DIP3), inner.active_after(Some(BlockId(parent)), DIP3));
        assert!(d.take_fault().is_none());
    }
}

#[test]
fn other_blocks_are_the_inner_views() {
    let p = regtest();
    let inner = From(48);
    let d = on(&p, &inner, 100);
    for h in [0, 47, 48, 100] {
        assert_eq!(d.active_at(BlockId(h), DIP3), inner.active_at(BlockId(h), DIP3));
        assert_eq!(
            d.active_after(Some(BlockId(h)), DIP3),
            inner.active_after(Some(BlockId(h)), DIP3)
        );
    }
    assert_eq!(d.active_after(None, DIP3), inner.active_after(None, DIP3));
    assert!(d.take_fault().is_none());
}

/// Core's step 12 asks `DeploymentActiveAfter(indexDummy, V19)` (rc.5 `specialtxman.cpp:1043`):
/// a buried question about the dummy's child is answered from its height, as Core's.
#[test]
fn a_buried_question_about_the_dummys_child_is_cores_height_answer() {
    let p = regtest();
    let inner = From(0);
    let mut checked = 0;
    for dep in [BuriedDeployment::V19, BuriedDeployment::Dip0003, BuriedDeployment::V20] {
        let at = p.consensus.deployment_height(dep).to_u32();
        // The dummy is above genesis, so its parent is too.
        if at < 3 {
            continue;
        }
        checked += 1;
        for (dummy, active) in [(at - 2, false), (at - 1, true), (at, true)] {
            let d = on(&p, &inner, dummy - 1);
            let got = d.active_after(Some(DUMMY_ID), Deployment::Buried(dep));
            assert_eq!(got, active, "{dep:?} after a dummy at {dummy}");
        }
    }
    assert!(checked >= 2, "regtest's buried heights above 2");
    assert!(on(&p, &inner, 10).take_fault().is_none());
}

/// C-2: a versionbits question about the dummy's child is an assert, once, and never reaches the
/// cache.
#[test]
fn a_versionbits_question_about_the_dummys_child_is_an_assert() {
    let p = regtest();
    let inner = From(48);
    let d = on(&p, &inner, 100);
    assert!(!d.active_after(Some(DUMMY_ID), Deployment::Versionbits(DeploymentPos::V24)));
    let fault = d.take_fault().expect("an assert");
    assert!(!fault.is_abort_node(), "Core's assert, not an AbortNode: {fault:?}");
    assert!(d.take_fault().is_none(), "taken once");

    assert_eq!(d.versionbits(Some(DUMMY_ID), DeploymentPos::V24), None);
    assert!(d.take_fault().is_some());
}
