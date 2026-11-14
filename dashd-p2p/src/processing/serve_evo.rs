//! P6b's quorum-rotation-info serving handler.

use dash_p2p::CommandString;
use dash_p2p::policy::Throw;
use dash_p2p_core::GetQrInfo;

use dash_p2p::OutMsg;

use super::registry::{HandlerCx, NetHandler};
use super::seams::QrInfoError;

/// Core's `EFFICIENT_QRINFO_VERSION`.
const EFFICIENT_QRINFO_VERSION: i32 = 70236;

/// What Core does after `BuildQuorumRotationInfo` refuses a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QrInfoPeerOutcome {
    /// The node lacks requested data; log and do not score.
    Unscored,
    /// The request is refused; score the peer by this amount.
    Misbehaving(i32),
}

/// The owner of the `getqrinfo` registry row.
#[derive(Debug, Default)]
pub struct EvoServeHandler;

static COMMANDS: [CommandString; 1] = [CommandString::GETQRINFO];

impl NetHandler for EvoServeHandler {
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    /// Core: `net_processing.cpp:5678-5705`.
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        let mut data = payload;
        let request = match cx.read::<GetQrInfo>(&mut data) {
            Ok(request) => request,
            Err(error) => {
                // An oversized base block list throws before any element is decoded; attribute it
                // to the peer here rather than letting the outer catch drop it silently.
                let why =
                    format!("malformed getqrinfo received. peer={} error={}", cx.id(), error.what);
                cx.misbehaving(100, &why);
                return Ok(());
            }
        };

        let legacy = cx.peer.common_version().get() < EFFICIENT_QRINFO_VERSION;
        let result = cx.evo.qrinfo(&request, cx.ctx.version(), legacy);
        match result {
            Ok(reply) => {
                cx.push(OutMsg::new(CommandString::QRINFO, reply));
            }
            Err(error) => {
                let outcome = error.peer_outcome();
                let text = match error {
                    QrInfoError::Unavailable(error) | QrInfoError::Refused(error) => error,
                };
                let line = format!(
                    "getquorumrotationinfo failed for size(baseBlockHashes)={}, blockRequestHash={}. error={text}",
                    request.base_block_hashes().len(),
                    request.block_request_hash(),
                );
                match outcome {
                    QrInfoPeerOutcome::Unscored => tracing::debug!(target: "net", "{line}"),
                    QrInfoPeerOutcome::Misbehaving(score) => cx.misbehaving(score, &line),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::processing::seams::QrInfoError;

    #[test]
    fn refusal_scores_one_and_unavailable_is_log_only() {
        assert_eq!(
            QrInfoError::Refused("bad".into()).peer_outcome(),
            QrInfoPeerOutcome::Misbehaving(1)
        );
        assert_eq!(
            QrInfoError::Unavailable("missing".into()).peer_outcome(),
            QrInfoPeerOutcome::Unscored
        );
    }
}
