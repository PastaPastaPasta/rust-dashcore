//! The readiness guard (CH3 §2.17, D31, D32, D54): G1's predicate, which the startup check of
//! CH1-node's load path also asks (`chain/load.rs`, CH3a D70), and the refusal texts.

use dash_consensus::BlockId;
use dash_consensus::ctx::{Deployment, DeploymentPos, DeploymentView};
use dash_consensus::deployments::EhfReadError;
use dash_params::{BuriedDeployment, ChainSetup};

use super::evo::BlockAt;

/// The evo rules G1 refuses under an incomplete `EvoHooks`, as CS4 names them: where
/// `ProcessSpecialTxsInBlock` or `IsBlockPayeeValid` read evo state or check evo data
/// (`evo/specialtxman.cpp:876-921`, `:966` at v24.0.0; `masternode/payments.cpp:227-240`). DIP0008,
/// V19, DIP0024 and BRR are not in it: active from height 1 on regtest, they gate nothing below
/// DIP3 that `NoEvo` must check.
const G1_DEPLOYMENTS: [(Deployment, &str); 5] = [
    (Deployment::Buried(BuriedDeployment::Dip0003), "DIP0003"),
    (Deployment::Buried(BuriedDeployment::V20), "V20"),
    (Deployment::Buried(BuriedDeployment::MnRr), "MN_RR"),
    (Deployment::Versionbits(DeploymentPos::V24), "V24"),
    (Deployment::Versionbits(DeploymentPos::EvoShares), "EVO_SHARES"),
];

/// Readiness guard G1 (CH3 §2.17): the first listed deployment active for the block after `prev`
/// (`active_after(prev, …)`), which an incomplete `EvoHooks` cannot judge, on every network.
/// Below them the hooks judge every block, a special-version CbTx included (CH7 D57, BL-516).
/// A versionbits read's error is `GetForBlock`'s throw (BL-683).
pub fn g1_rule(
    deployments: &dyn DeploymentView,
    prev: Option<BlockId>,
) -> Result<Option<&'static str>, EhfReadError> {
    for (d, name) in G1_DEPLOYMENTS {
        if deployments.try_active_after(prev, d)? {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// G1's capability: what CH7a builds.
pub fn g1_capability(rule: &str) -> String {
    format!("evo validation (CH7a) for {rule}")
}

/// A readiness refusal's text (CH3 §2.17, CH3a D69): the block, the network and what this build
/// lacks to judge it, with the registry row that declares the gap (`compat/ALLOWED-DIFFS.md`
/// AD-CH3-4).
pub fn refusal_text(params: &ChainSetup, at: &BlockAt, capability: &str) -> String {
    format!(
        "rdashd cannot validate block {} at height {} on {}: {capability} is not built \
         (ALLOWED-DIFFS AD-CH3-4)",
        at.hash,
        at.height,
        params.network.network_id()
    )
}

/// G2's refusal (CH3a D69): the refusal text for what the helper is `missing`, then the way out
/// (R10): IBD lets G2 pass, and V is in IBD again once its tip is older than `-maxtipage`.
pub fn g2_refusal_text(params: &ChainSetup, at: &BlockAt, missing: &str) -> String {
    format!(
        "{}; it connects once the tip is older than -maxtipage",
        refusal_text(params, at, missing)
    )
}

#[cfg(test)]
mod tests {
    use dash_params::{ChainParamsBuilder, Network, Release};
    use dash_primitives::BlockHash;

    use super::*;
    use crate::connect::helper::INTERIM_REFUSAL;

    /// A deployment view with exactly `active` active, whatever the block.
    struct Only(Option<Deployment>);

    impl DeploymentView for Only {
        fn active_after(&self, _: Option<BlockId>, d: Deployment) -> bool {
            self.0 == Some(d)
        }

        fn active_at(&self, _: BlockId, d: Deployment) -> bool {
            self.0 == Some(d)
        }
    }

    fn g1(view: &Only, prev: Option<BlockId>) -> Option<&'static str> {
        g1_rule(view, prev).expect("Only reads no EHF rows")
    }

    #[test]
    fn g1_refuses_each_listed_deployment_and_nothing_else() {
        assert_eq!(g1(&Only(None), None), None);
        for (d, name) in G1_DEPLOYMENTS {
            assert_eq!(g1(&Only(Some(d)), Some(BlockId(1))), Some(name));
        }
        for d in [
            BuriedDeployment::Dip0008,
            BuriedDeployment::V19,
            BuriedDeployment::Dip0024,
            BuriedDeployment::Brr,
            BuriedDeployment::Dip0001,
            BuriedDeployment::Csv,
        ] {
            assert_eq!(g1(&Only(Some(Deployment::Buried(d))), Some(BlockId(1))), None);
        }
    }

    /// BL-699: both forms name the registry row (AD-CH3-4), with G2's way out after it.
    #[test]
    fn refusal_text_names_its_registry_row() {
        let p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest");
        let at = BlockAt {
            hash: BlockHash::from([0x18; 32]),
            height: 1,
            prev: None,
            id: None,
        };
        let text = refusal_text(&p, &at, &g1_capability("DIP0003"));
        assert!(
            text.ends_with(
                "evo validation (CH7a) for DIP0003 is not built (ALLOWED-DIFFS AD-CH3-4)"
            ),
            "{text}"
        );
        let g2 = g2_refusal_text(&p, &at, INTERIM_REFUSAL);
        assert!(
            g2.ends_with(
                "is not built (ALLOWED-DIFFS AD-CH3-4); it connects once the tip is older than \
                 -maxtipage"
            ),
            "{g2}"
        );
    }
}
