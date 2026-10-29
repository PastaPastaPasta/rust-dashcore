//! The readiness guard (CH3 §2.17, D31, D32, D54): G1's predicate, which the startup check of
//! CH1-node's load path also asks (`chain/load.rs`, CH3a D70), and the refusal texts.

use dash_consensus::ctx::{Deployment, DeploymentPos, DeploymentView};
use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
use dash_consensus::{BlockId, Tx};
use dash_params::{BuriedDeployment, ChainParams};
use dash_types::Numeric as _;

use super::evo::BlockAt;

/// The evo rules G1 refuses under an incomplete `EvoHooks`, as CS4 names them: where
/// `ProcessSpecialTxsInBlock` or `IsBlockPayeeValid` read evo state or check evo data
/// (`evo/specialtxman.cpp:816-861`, `:906`; `masternode/payments.cpp:227-240`). DIP0008, V19,
/// DIP0024 and BRR are not in it: active from height 1 on regtest, they gate nothing below DIP3
/// that `NoEvo` must check.
const G1_DEPLOYMENTS: [(Deployment, &str); 5] = [
    (Deployment::Buried(BuriedDeployment::Dip0003), "DIP0003"),
    (Deployment::Buried(BuriedDeployment::V20), "V20"),
    (Deployment::Buried(BuriedDeployment::MnRr), "MN_RR"),
    (Deployment::Versionbits(DeploymentPos::V24), "V24"),
    (Deployment::Versionbits(DeploymentPos::EvoShares), "EVO_SHARES"),
];

/// Readiness guard G1 (CH3 §2.17): what makes the block after `prev` with `coinbase` one an
/// incomplete `EvoHooks` cannot judge, on every network. Each listed deployment is asked as the
/// block sees it (`active_after(prev, …)`), in the list's order; then the coinbase's shape: a
/// special-version CbTx (`nVersion >= 3`, signed 16 bits, with raw `nType` 5), whose payload
/// `ProcessSpecialTxsInBlock` would decode. Below version 3 a raw type 5 carries no payload, and
/// `NoEvo` judges it exactly (D54). Without the block's data (the startup check, CH3a D70) only
/// the deployments are asked.
pub fn g1_rule(
    deployments: &dyn DeploymentView,
    prev: Option<BlockId>,
    coinbase: Option<&Tx>,
) -> Option<&'static str> {
    if let Some((_, name)) = G1_DEPLOYMENTS.iter().find(|(d, _)| deployments.active_after(prev, *d))
    {
        return Some(name);
    }
    // Core: primitives/transaction.h:357-364 (the payload is read at nVersion >= 3 only)
    coinbase
        .is_some_and(|cb| cb.version() >= 3 && cb.tx_type().to_base() == TRANSACTION_COINBASE)
        .then_some("a special-version coinbase")
}

/// G1's capability: what CH7a builds.
pub fn g1_capability(rule: &str) -> String {
    format!("evo validation (CH7a) for {rule}")
}

/// A readiness refusal's text (CH3 §2.17, CH3a D69): the block, the network and what this build
/// lacks to judge it, with the ALLOWED-DIFFS row that declares the gap.
pub fn refusal_text(params: &ChainParams, at: &BlockAt, capability: &str) -> String {
    format!(
        "rdashd cannot validate block {} at height {} on {}: {capability} is not built \
         (ALLOWED-DIFFS D18)",
        at.hash,
        at.height,
        params.network.network_id()
    )
}

/// G2's refusal (CH3a D69): the refusal text for what the helper is `missing`, then the way out
/// (R10): IBD lets G2 pass, and V is in IBD again once its tip is older than `-maxtipage`.
pub fn g2_refusal_text(params: &ChainParams, at: &BlockAt, missing: &str) -> String {
    format!(
        "{}; it connects once the tip is older than -maxtipage",
        refusal_text(params, at, missing)
    )
}

#[cfg(test)]
mod tests {
    use dash_consensus::primitives::Transaction;
    use dash_types::codec::BaseCodec as _;

    use super::*;

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

    /// A coinbase with Core's `nVersion` (low 16 bits) and raw `nType` (high 16 bits).
    fn coinbase(version: i16, tx_type: u16) -> Tx {
        let n32 = (u32::from(tx_type) << 16) | u32::from(version as u16);
        let mut raw = n32.to_le_bytes().to_vec();
        raw.push(1);
        raw.extend([0u8; 32]);
        raw.extend(u32::MAX.to_le_bytes());
        raw.extend([2, 0x51, 0x51]);
        raw.extend(u32::MAX.to_le_bytes());
        raw.push(0);
        raw.extend(0u32.to_le_bytes());
        if version >= 3 && tx_type != 0 {
            raw.push(0);
        }
        Tx::new(Transaction::decode(&mut raw.as_slice()).expect("a coinbase"))
    }

    #[test]
    fn g1_refuses_each_listed_deployment_and_nothing_else() {
        let cb = coinbase(1, 0);
        assert_eq!(g1_rule(&Only(None), None, Some(&cb)), None);
        for (d, name) in G1_DEPLOYMENTS {
            assert_eq!(g1_rule(&Only(Some(d)), Some(BlockId(1)), Some(&cb)), Some(name));
        }
        for d in [
            BuriedDeployment::Dip0008,
            BuriedDeployment::V19,
            BuriedDeployment::Dip0024,
            BuriedDeployment::Brr,
            BuriedDeployment::Dip0001,
            BuriedDeployment::Csv,
        ] {
            assert_eq!(
                g1_rule(&Only(Some(Deployment::Buried(d))), Some(BlockId(1)), Some(&cb)),
                None
            );
        }
    }

    /// D54: only a special-version CbTx shape is G1's; versions 1 and 2 with raw type 5, and a
    /// version-3 classical coinbase, are `NoEvo`'s.
    #[test]
    fn g1_refuses_a_special_version_cbtx_only() {
        let none = Only(None);
        assert_eq!(g1_rule(&none, None, Some(&coinbase(3, 5))), Some("a special-version coinbase"));
        assert_eq!(g1_rule(&none, None, None), None);
        for (version, tx_type) in [(1, 5), (2, 5), (3, 0), (-3, 5)] {
            assert_eq!(
                g1_rule(&none, None, Some(&coinbase(version, tx_type))),
                None,
                "{version} {tx_type}"
            );
        }
    }
}
