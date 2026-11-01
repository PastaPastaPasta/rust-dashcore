//! The BLS scheme context (A1 §1.4.1 and R4; DEC-24 D2; DEC-71).
//!
//! Core reads one process-wide flag, `bls::bls_legacy_scheme`. Rust has none: every call site
//! that Core's flag would decide takes an explicit [`BlsCtx`], built by one of these rules.
//!
//! | Call site | Context |
//! |---|---|
//! | connecting, testing or disconnecting block `h` | [`bls_ctx_for_block`]`(params, h)`: legacy iff `h < V19Height` (Core's `ScopedBLSLegacyScheme`, `validation.cpp` v24:2490, 3272) |
//! | everywhere else: mempool, the miner, GBT, P2P intake, RPC | [`bls_ctx_after`]`(deployments, tip)`, taken at the call |
//! | DKG at a quorum base, local signing | [`bls_ctx_after`]`(deployments, base)` (`active/dkgsession.cpp` v24:34) |
//! | queued background work | the context captured with the job, fenced by the tip epoch (A1 §2.10.2) |
//!
//! A lock message's signature takes Core's flag more than once; the `locks` functions take the
//! reads as named fields (`locks::LockSchemes`, `locks::IslockSchemes`, `locks::ClsigHashSchemes`;
//! LQ3a D26). The `read` context is the one when the message is deserialized: a lazy field keeps
//! it, an eager one decodes under it, and it travels with the message to a queued job (LQ3a D27).
//! A check takes the scheme sampled for the check, Core's flag when it runs: the signing worker
//! for `locks::verify_qsigrec`, each of the InstantSend worker's two passes for
//! `locks::verify_islock` (`locks::IslockPassSchemes`, sampled at the start of each pass), the
//! ChainLock handler for `locks::verify_chainlock` (LQ3a D18, D21, D22, D25). It is not
//! [`bls_ctx_after`] of the visible tip: the flag flips inside `ConnectBlock` before the tip
//! moves, so the node does not derive it from the old tip alone. `locks::clsig_hash`'s
//! `write` context is the one when the message is hashed, right after it is read and before the
//! handler runs (`net_processing.cpp` v24:5797-5798; LQ3a D19, D24).
//!
//! v24's `ScopedBLSLegacyScheme` reverts the flag unless the connect commits, so a failed or
//! dry-run connect, and `verifychain`, leave the tip context as it was (A1 §2.25, DEC-82).
//!
//! **Where the context is applied (DEC-71).** A BLS field's *decode* never reads the global
//! context: version-sourced and always-basic fields decode under their own scheme, and the
//! global-sourced ones (the CbTx `bestCLSignature`, the asset-unlock `quorumSig`) decode
//! scheme-free, because Core's reject verdict does not depend on the scheme. The scheme is
//! resolved **at use**: verification, re-serialization for a hash, the commitment hash's public
//! key bytes. Every function that uses such a value takes the `BlsCtx` of its call site, e.g.
//! [`crate::ctx::EvoCtx::bls`] inside block processing.

use dash_consensus::BlockId;
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_params::BuriedDeployment;

pub use dash_consensus::ctx::bls_ctx_for_block;
pub use dash_pkc::bls::{BlsCtx, BlsSchemeKind, BlsSchemeSource};

/// Core's flag once `block` is the active tip: legacy iff V19 is not active after it.
///
/// This is the value Core holds at every call site outside block processing:
/// `ProcessSpecialTxsInBlock` switches to basic once V19 is active after the connected block
/// (`evo/specialtxman.cpp` v24:983-988), `UndoSpecialTxsInBlock` switches back below it
/// (v24:1005-1009), and startup sets it from the tip (`node/chainstate.cpp` v24:429-434). The
/// DKG takes it at the quorum base. `None` is genesis's null `pindexPrev`.
pub fn bls_ctx_after(deployments: &dyn DeploymentView, block: Option<BlockId>) -> BlsCtx {
    let v19 = Deployment::Buried(BuriedDeployment::V19);
    BlsCtx::from_legacy(!deployments.active_after(block, v19))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V19 active from height 10.
    struct V19At10;

    impl DeploymentView for V19At10 {
        fn active_after(&self, prev: Option<BlockId>, _: Deployment) -> bool {
            prev.is_some_and(|BlockId(h)| h + 1 >= 10)
        }

        fn active_at(&self, BlockId(h): BlockId, _: Deployment) -> bool {
            h >= 10
        }
    }

    #[test]
    fn the_tip_context_switches_after_the_block_before_v19() {
        // Block 9 is processed under legacy (`bls_ctx_for_block`), and once it is the tip the
        // flag is basic, because V19 is active after it.
        assert!(bls_ctx_after(&V19At10, None).is_legacy());
        assert!(bls_ctx_after(&V19At10, Some(BlockId(8))).is_legacy());
        assert!(!bls_ctx_after(&V19At10, Some(BlockId(9))).is_legacy());
        assert!(!bls_ctx_after(&V19At10, Some(BlockId(10))).is_legacy());
    }
}
