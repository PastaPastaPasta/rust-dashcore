use std::panic::{AssertUnwindSafe, catch_unwind};

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

/// V's deployments over a source whose every read fails: the `try_*` forms give the error and
/// the infallible ones the backstop's panic, as CS4's `Deployments` over a faulting source.
struct Faulting;

impl Faulting {
    fn error() -> EhfReadError {
        EhfReadError::new("failed-getehfforblock-read")
    }
}

impl DeploymentView for Faulting {
    fn active_after(&self, _: Option<BlockId>, _: Deployment) -> bool {
        panic!("GetForBlock: {}", Faulting::error())
    }

    fn active_at(&self, _: BlockId, _: Deployment) -> bool {
        panic!("GetForBlock: {}", Faulting::error())
    }

    fn versionbits(&self, _: Option<BlockId>, _: DeploymentPos) -> Option<VersionbitsAnswer> {
        panic!("GetForBlock: {}", Faulting::error())
    }

    fn try_active_after(&self, _: Option<BlockId>, _: Deployment) -> Result<bool, EhfReadError> {
        Err(Faulting::error())
    }

    fn try_versionbits(
        &self,
        _: Option<BlockId>,
        _: DeploymentPos,
    ) -> Result<Option<VersionbitsAnswer>, EhfReadError> {
        Err(Faulting::error())
    }
}

/// BL-683 T11 (proposals): the proposal's deployments as a `&dyn DeploymentView`, asked about a
/// real parent, forward the `try_*` forms to V's: the read's error, no unwind into V.
#[test]
fn the_proposal_deployments_try_answers_carry_the_read_error() {
    let p = regtest();
    let d = ProposalDeployments::new(&Faulting, &p, BlockId(100), 101);
    let view: &dyn DeploymentView = &d;
    let prev = Some(BlockId(100));
    let after = catch_unwind(AssertUnwindSafe(|| {
        view.try_active_after(prev, Deployment::Versionbits(DeploymentPos::V24)).err()
    }));
    let bits =
        catch_unwind(AssertUnwindSafe(|| view.try_versionbits(prev, DeploymentPos::V24).err()));
    for (what, got) in [("try_active_after", after), ("try_versionbits", bits)] {
        let err = got.unwrap_or_else(|_| panic!("{what} unwound"));
        let text = err.map(|e| e.to_string());
        assert_eq!(text.as_deref(), Some("failed-getehfforblock-read"), "{what}");
    }
    assert!(d.take_fault().is_none(), "a real parent is no dummy");
}
