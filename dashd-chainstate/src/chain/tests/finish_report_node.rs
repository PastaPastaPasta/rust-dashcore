//! PLAN-LK1b §5.3 item 7 with `NodeEvo` in the node's connector: every recorded attempt of
//! dashd's masternode chain, its connects past DIP0003 and a proposal on its tip, reaches
//! `record_connect` with `NodeEvo`'s report (`Known`, its whole log applied), not the default's
//! `Unknown`.

use super::connector::finish_report::Reports;
use super::evo_chain::recorded_config_on;
use super::scratch_state_hash::mn_chain;
use super::support::{Rig, RigConfig};
use crate::chain::BlockState;
use crate::connect::EffectCoverage;

#[test]
fn node_evo_reports_reach_the_connect_and_proposal_records() {
    let chain = mn_chain();
    let reports = Reports::default();
    let cfg = RigConfig {
        params: Some(chain.p.clone()),
        ..RigConfig::default()
    };
    let (cfg, kept) =
        recorded_config_on(cfg, chain.p.clone(), chain.mocktime, Some(Box::new(reports.clone())));
    let rig = Rig::with(cfg);
    let (connected, next) = chain.blocks.split_at(80);
    for m in connected {
        rig.accept(m);
    }
    let proposed = rig.v.test_block_validity(next[0].block.clone(), false, true).recv();
    assert_eq!(proposed.ok(), Some(Ok(BlockState::Valid)));
    kept.assert_no_abort(&rig);
    let all = reports.all();
    assert_eq!(all.len(), connected.len() + 2, "genesis's, each connect's and the proposal's");
    assert!(all.last().is_some_and(|r| r.just_check), "the proposal's record is last");
    for r in &all {
        assert!(r.outcome_ok && r.finish.result.is_ok(), "{r:?}");
        let EffectCoverage::Known {
            emitted,
            completed,
        } = &r.finish.effects
        else {
            panic!("NodeEvo reports its log: {r:?}");
        };
        assert_eq!(*completed, emitted.len(), "{r:?}");
    }
}
