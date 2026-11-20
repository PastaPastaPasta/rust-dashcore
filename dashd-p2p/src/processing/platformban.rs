//! Core v24's `ProcessPlatformBanMessage` extension (`net_processing.cpp:3844-3925`).

use std::sync::Arc;

use dash_consensus::BlsCtx;
use dash_consensus::hash::HashWriter;
use dash_consensus::primitives::{CoreWrite, write_compact_size};
use dash_evo::locks::{SigRead, read_eager_sig, sign_hash};
use dash_num::Hash256;
use dash_p2p::CommandString;
use dash_p2p::policy::Throw;
use dash_p2p_core::{InvType, Inventory, PlatformBan};
use dash_pkc::bls::core::{BlsVerdict, verify_insecure};
use dash_types::{Hashable, Numeric};

use super::registry::{Extension, HandlerCx, NetHandler};
use super::seams::{PlatformBans, PlatformQuorums};

const WINDOW: i64 = 576;
const WINDOW_TOLERANCE: i64 = 5;
const PLATFORMBAN_REQUEST_PREFIX: &[u8] = b"PlatformPoSeBan";

static COMMANDS: [CommandString; 1] = [CommandString::PLATFORMBAN];

/// Core's platform-ban cache and quorum lookup for one processor.
pub struct PlatformBanHandler {
    bans: Arc<dyn PlatformBans>,
    quorums: Arc<dyn PlatformQuorums>,
}

impl PlatformBanHandler {
    /// A handler backed by the supplied AM1/SP1 seams.
    pub fn new(bans: Arc<dyn PlatformBans>, quorums: Arc<dyn PlatformQuorums>) -> Self {
        Self {
            bans,
            quorums,
        }
    }
}

impl Default for PlatformBanHandler {
    fn default() -> Self {
        Self::new(Arc::new(super::seams::NoPlatformBans), Arc::new(super::seams::NoPlatformQuorums))
    }
}

impl std::fmt::Debug for PlatformBanHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformBanHandler").finish_non_exhaustive()
    }
}

impl NetHandler for PlatformBanHandler {
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    fn extension(&self) -> Option<Extension> {
        Some(Extension::PlatformBan)
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        if *cmd != CommandString::PLATFORMBAN
            || !self.bans.enabled()
            || !self.quorums.enabled()
            || !cx.mn_net.blockchain_synced()
        {
            return Ok(());
        }

        let mut data = payload;
        let ban: PlatformBan = cx.read(&mut data)?;
        // Core's PlatformBanMessage wraps this field with legacy=false (meta.h:178-180),
        // so its eager read always uses the basic scheme regardless of the view scheme.
        let sig = match read_eager_sig(ban.sig().as_bytes(), BlsCtx::BASIC) {
            SigRead::Unreadable => return Err(Throw::ios_failure("malleable BLS object")),
            SigRead::Invalid => None,
            SigRead::Valid(sig) => Some(sig),
        };
        let inv_hash = ban.hash();
        let inv_hash_bytes = inv_hash.to_lendian();
        let inv = Inventory::new(InvType::PlatformBan, inv_hash);

        // Core deliberately checks its dedupe cache before the log line and every expensive read.
        if self.bans.already_have(&inv_hash_bytes) {
            cx.post_process(super::registry::MessageProcessingResult {
                to_erase: Some(inv),
                ..Default::default()
            });
            return Ok(());
        }

        tracing::info!(
            target: "llmq",
            "PLATFORMBAN -- hash: {} protx_hash: {} height: {} peer={}",
            inv_hash,
            ban.pro_tx_hash(),
            ban.requested_height(),
            cx.id()
        );
        let list = cx.mn_net.tip_list();
        let protx = *ban.pro_tx_hash().as_bytes();
        // A list that cannot name the entry's type is treated as Core's missing entry.
        let Some(mn_type) = list.operator(&protx).and_then(|_| list.mn_type(&protx)) else {
            return score_and_erase(cx, inv, 1);
        };
        if !mn_type.is_evo() {
            tracing::info!(
                target: "llmq",
                "PLATFORMBAN -- hash: {} protx_hash: {} unexpected type of node",
                inv_hash,
                ban.pro_tx_hash()
            );
            return score_and_erase(cx, inv, 100);
        }

        let requested = i64::from(ban.requested_height());
        let tip = i64::from(cx.snap.tip.tip_height);
        if tip < requested || tip - WINDOW > requested {
            tracing::info!(
                target: "llmq",
                "PLATFORMBAN -- hash: {} protx_hash: {} unexpected height: {} tip: {}",
                inv_hash,
                ban.pro_tx_hash(),
                ban.requested_height(),
                tip
            );
            if tip + WINDOW_TOLERANCE < requested || tip - WINDOW - WINDOW_TOLERANCE > requested {
                return score_and_erase(cx, inv, 10);
            }
            return score_and_erase(cx, inv, 1);
        }

        let llmq_type = cx.core.config().params.consensus.llmq_type_platform;
        let quorum_hash = *ban.quorum_hash().as_bytes();
        let Some(pubkey) = self.quorums.quorum_pubkey(llmq_type, &quorum_hash) else {
            tracing::info!(
                target: "llmq",
                "PLATFORMBAN -- hash: {} protx_hash: {} missing quorum_hash: {} llmq_type: {}",
                inv_hash,
                ban.pro_tx_hash(),
                ban.quorum_hash(),
                llmq_type.to_base()
            );
            return score_and_erase(cx, inv, 100);
        };
        let id = platform_ban_request_id(&protx, ban.requested_height());
        let msg_hash = platform_ban_message_hash(&protx, ban.requested_height());
        let hash = sign_hash(llmq_type, &ban.quorum_hash(), &id, &msg_hash);
        let Some(sig) = sig else {
            tracing::info!(
                target: "llmq",
                "PLATFORMBAN -- hash: {} protx_hash: {} request_id: {} msg_hash: {} sig validation failed",
                inv_hash,
                ban.pro_tx_hash(),
                Hash256::from_lendian(id),
                Hash256::from_lendian(msg_hash)
            );
            return score_and_erase(cx, inv, 100);
        };
        if verify_insecure(&sig, &pubkey, &hash, cx.ctx.bls().scheme()) != BlsVerdict::Valid {
            tracing::info!(
                target: "llmq",
                "PLATFORMBAN -- hash: {} protx_hash: {} request_id: {} msg_hash: {} sig validation failed",
                inv_hash,
                ban.pro_tx_hash(),
                Hash256::from_lendian(id),
                Hash256::from_lendian(msg_hash)
            );
            return score_and_erase(cx, inv, 100);
        }

        let mut result = super::registry::MessageProcessingResult {
            to_erase: Some(inv),
            ..Default::default()
        };
        if self.bans.set(inv_hash_bytes, ban) {
            tracing::info!(
                target: "llmq",
                "PLATFORMBAN -- hash: {} forward message to other nodes",
                inv_hash
            );
            result.inventory.push(inv);
        }
        cx.post_process(result);
        Ok(())
    }

    fn already_have(&self, inv: &Inventory) -> bool {
        inv.inv_type() == InvType::PlatformBan
            && self.bans.already_have(&inv.inv_hash().to_lendian())
    }
}

fn score_and_erase(cx: &mut HandlerCx<'_>, inv: Inventory, score: i32) -> Result<(), Throw> {
    let mut result = super::registry::MessageProcessingResult::misbehaving(score, "");
    result.to_erase = Some(inv);
    cx.post_process(result);
    Ok(())
}

fn write_prefix(w: &mut impl CoreWrite) {
    write_compact_size(PLATFORMBAN_REQUEST_PREFIX.len() as u64, w);
    w.write(PLATFORMBAN_REQUEST_PREFIX);
}

/// Core's `SerializeHash(("PlatformPoSeBan", (protx, height)))` request id
/// (`net_processing.cpp:3906-3908`).
pub fn platform_ban_request_id(protx: &[u8; 32], height: i32) -> [u8; 32] {
    let mut w = HashWriter::new();
    write_prefix(&mut w);
    w.write(protx);
    w.write(&height.to_le_bytes());
    w.finish()
}

/// Core's `SerializeHash((protx, height))` message hash (`net_processing.cpp:3907-3909`).
pub fn platform_ban_message_hash(protx: &[u8; 32], height: i32) -> [u8; 32] {
    let mut w = HashWriter::new();
    w.write(protx);
    w.write(&height.to_le_bytes());
    w.finish()
}

#[cfg(test)]
mod tests {
    use dash_params::LlmqType;

    use super::*;

    #[test]
    fn request_id_has_the_platform_prefix() {
        let protx: [u8; 32] = core::array::from_fn(|i| i as u8);
        assert_eq!(
            platform_ban_message_hash(&protx, 123),
            [
                0x0f, 0xff, 0xd0, 0xb3, 0x07, 0xf4, 0xa8, 0x76, 0xda, 0x89, 0x4f, 0x48, 0xa8, 0x57,
                0x99, 0x1b, 0x3f, 0x10, 0x95, 0x5b, 0x79, 0xfb, 0xed, 0x8c, 0x19, 0x5e, 0x2c, 0x6b,
                0x1f, 0x9e, 0xfe, 0x81,
            ]
        );
        assert_eq!(
            platform_ban_request_id(&protx, 123),
            [
                0x09, 0x5c, 0x71, 0x82, 0x93, 0x78, 0x27, 0xc2, 0x26, 0x37, 0xb8, 0x46, 0xfb, 0xd8,
                0xef, 0x94, 0xe9, 0x3d, 0xd1, 0x30, 0x75, 0x6c, 0x67, 0xba, 0xd1, 0x82, 0x36, 0x25,
                0x19, 0x3e, 0x0d, 0x10,
            ]
        );
    }

    #[test]
    fn sign_hash_is_bound_to_the_request_and_message() {
        let q = dash_primitives::QuorumHash::from([2; 32]);
        let a = platform_ban_request_id(&[1; 32], 12);
        let b = platform_ban_message_hash(&[1; 32], 12);
        let h = sign_hash(LlmqType::LlmqTestPlatform, &q, &a, &b);
        assert_ne!(h, sign_hash(LlmqType::LlmqTestPlatform, &q, &a, &[0; 32]));
    }
}
