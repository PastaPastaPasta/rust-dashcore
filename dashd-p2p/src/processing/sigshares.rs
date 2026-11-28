//! LLMQ signature-share branches of `NetSigning::ProcessMessage`.

use std::sync::Arc;

use dash_p2p::CommandString;
use dash_p2p::policy::Throw;
use dash_p2p::read::HandlerRead;
use dash_p2p_core::{BatchedSigSharesMsg, SigSesAnnMsg, SigShareMsg, SigSharesInvMsg};

use super::registry::HandlerCx;
use super::seams::{SigSharesInvKind, SigSharesSeam};

// Core: v24.0.0 src/llmq/net_signing.cpp:72-77, :103-108, :121-126, :137-142 NetSigning::ProcessMessage
fn read_or_ban<T: HandlerRead + 'static>(
    seam: &dyn SigSharesSeam,
    cx: &HandlerCx<'_>,
    cmd: &CommandString,
    data: &mut &[u8],
) -> Result<T, Throw> {
    cx.read(data).inspect_err(|err| {
        let peer = cx.id();
        tracing::debug!(
            target: "llmq-sigs",
            "NetSigning::ProcessMessage -- rejected {cmd} from peer={peer}: {}",
            err.what
        );
        seam.ban(peer);
    })
}

/// Dispatches the five signature-share commands over [`HandlerCx::sig_shares`].
// Core: v24.0.0 src/llmq/net_signing.cpp:27-45, :64-150 UnserializeBatchedSigShares, NetSigning::ProcessMessage
pub fn process_sig_shares(
    cx: &mut HandlerCx<'_>,
    cmd: &CommandString,
    payload: &[u8],
) -> Result<(), Throw> {
    let seam = Arc::clone(&cx.sig_shares);
    process_sig_shares_with(&*seam, cx, cmd, payload)
}

/// Dispatches the five signature-share commands against `seam` in Core's order.
// Core: v24.0.0 src/llmq/net_signing.cpp:27-45, :64-150 UnserializeBatchedSigShares, NetSigning::ProcessMessage
pub fn process_sig_shares_with(
    seam: &dyn SigSharesSeam,
    cx: &HandlerCx<'_>,
    cmd: &CommandString,
    payload: &[u8],
) -> Result<(), Throw> {
    if !seam.enabled() {
        return Ok(());
    }
    let mut data = payload;
    let peer = cx.id();
    match cmd.as_str() {
        // Core: v24.0.0 src/llmq/net_signing.cpp:66-95 NetSigning::ProcessMessage
        "qsigshare" => {
            if !seam.spork21_active() {
                return Ok(());
            }
            let msg: SigShareMsg = read_or_ban(seam, cx, cmd, &mut data)?;
            seam.sig_share(peer, msg.shares().clone());
        }
        // Core: v24.0.0 src/llmq/net_signing.cpp:97-114 NetSigning::ProcessMessage
        "qsigsesann" => {
            let msg: SigSesAnnMsg = read_or_ban(seam, cx, cmd, &mut data)?;
            seam.ses_ann(peer, msg.anns().clone());
        }
        // Core: v24.0.0 src/llmq/net_signing.cpp:115-132 NetSigning::ProcessMessage
        "qsigsinv" | "qgetsigs" => {
            let kind = if *cmd == CommandString::QSIGSINV {
                SigSharesInvKind::Inv
            } else {
                SigSharesInvKind::GetSigs
            };
            let msg: SigSharesInvMsg = read_or_ban(seam, cx, cmd, &mut data)?;
            seam.sig_shares_inv(peer, kind, msg.invs().clone());
        }
        // Core: v24.0.0 src/llmq/net_signing.cpp:27-45, :133-149 UnserializeBatchedSigShares, NetSigning::ProcessMessage
        "qbsigs" => {
            let msg: BatchedSigSharesMsg = read_or_ban(seam, cx, cmd, &mut data)?;
            seam.batched(peer, msg.batches().clone());
        }
        _ => {}
    }
    Ok(())
}
