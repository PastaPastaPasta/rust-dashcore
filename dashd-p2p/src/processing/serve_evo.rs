//! P6b's `getmnlistd` serving seam.
//!
//! The source is deliberately narrow: the node adapter owns the one-V-turn reads and supplies
//! the already-built v24 reply bytes. This handler owns the request checks and Core's refusal
//! and scoring order.

use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, OutMsg};
use dash_p2p_core::GetMnListDiff;
use dash_primitives::BlockHash;
use dash_types::Numeric;

use super::registry::{HandlerCx, NetHandler};

/// A refusal from the mnlistdiff source, with Core's resulting peer score.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServeRefusal {
    /// `Some(1)` for a peer error, `None` for unavailable local data or a local failure.
    pub score: Option<i32>,
    /// Core's `errorRet` text.
    pub text: String,
}

fn rpc_hex(hash: &BlockHash) -> String {
    hash.as_bytes().iter().rev().map(|byte| format!("{byte:02x}")).collect()
}

fn unavailable(hash: &BlockHash) -> String {
    format!(
        "block data for block {} is not available (pruned or below an unvalidated snapshot base)",
        rpc_hex(hash)
    )
}

fn failure_text(base: &BlockHash, block: &BlockHash, error: &str) -> String {
    format!(
        "getmnlistdiff failed for baseBlockHash={}, blockHash={}. error={error}",
        rpc_hex(base),
        rpc_hex(block)
    )
}

/// The node-side source. RP2a-P2 replaces this stub with its one-`OnV` adapter.
pub trait EvoServe: Send {
    /// Build the bytes for one request at the negotiated protocol version.
    fn mnlistdiff(
        &mut self,
        base: BlockHash,
        block: BlockHash,
        proto: i32,
    ) -> Result<Vec<u8>, ServeRefusal>;
}

/// M2's no-evo source: the request is plausible, but this node has no list data.
#[derive(Debug, Default)]
pub struct NoEvoServe;

impl EvoServe for NoEvoServe {
    fn mnlistdiff(
        &mut self,
        _base: BlockHash,
        block: BlockHash,
        _proto: i32,
    ) -> Result<Vec<u8>, ServeRefusal> {
        Err(ServeRefusal {
            score: None,
            text: unavailable(&block),
        })
    }
}

/// Core's `getmnlistd` registry handler.
pub struct EvoServer {
    source: Box<dyn EvoServe>,
}

impl EvoServer {
    /// Installs a node or simulation source.
    pub fn new(source: Box<dyn EvoServe>) -> Self {
        Self {
            source,
        }
    }

    fn refuse(cx: &mut HandlerCx<'_>, base: &BlockHash, block: &BlockHash, refusal: ServeRefusal) {
        let text = failure_text(base, block, &refusal.text);
        tracing::debug!(target: "net", "{} peer={}", text, cx.id());
        if let Some(score) = refusal.score {
            cx.misbehaving(score, &text);
        }
    }
}

static COMMANDS: [CommandString; 1] = [CommandString::GETMNLISTD];

impl NetHandler for EvoServer {
    fn commands(&self) -> &'static [CommandString] {
        &COMMANDS
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        let mut data = payload;
        let request: GetMnListDiff = cx.read(&mut data)?;
        let requested_base = request.base_block_hash();
        let requested_block = request.block_hash();
        let base = if requested_base == BlockHash::ZERO {
            cx.chain.active_at(&cx.snap, 0).and_then(|id| cx.chain.entry(id))
        } else {
            cx.chain.entry_by_hash(&requested_base)
        };
        let block = cx.chain.entry_by_hash(&requested_block);
        let Some(base) = base else {
            Self::refuse(
                cx,
                &requested_base,
                &requested_block,
                ServeRefusal {
                    score: Some(1),
                    text: format!("block {} not found", rpc_hex(&requested_base)),
                },
            );
            return Ok(());
        };
        let Some(block) = block else {
            Self::refuse(
                cx,
                &requested_base,
                &requested_block,
                ServeRefusal {
                    score: Some(1),
                    text: format!("block {} not found", rpc_hex(&requested_block)),
                },
            );
            return Ok(());
        };
        if !cx.chain.in_active_chain(&cx.snap, base.id)
            || !cx.chain.in_active_chain(&cx.snap, block.id)
        {
            Self::refuse(
                cx,
                &requested_base,
                &requested_block,
                ServeRefusal {
                    score: Some(1),
                    text: format!(
                        "block {} and {} are not in the same chain",
                        rpc_hex(&requested_base),
                        rpc_hex(&requested_block)
                    ),
                },
            );
            return Ok(());
        }
        if base.height > block.height {
            Self::refuse(
                cx,
                &requested_base,
                &requested_block,
                ServeRefusal {
                    score: Some(1),
                    text: format!(
                        "base block {} is higher then block {}",
                        rpc_hex(&requested_base),
                        rpc_hex(&requested_block)
                    ),
                },
            );
            return Ok(());
        }
        let Some(status) = cx.chain.status(&cx.snap, block.id) else {
            Self::refuse(
                cx,
                &requested_base,
                &requested_block,
                ServeRefusal {
                    score: None,
                    text: unavailable(&requested_block),
                },
            );
            return Ok(());
        };
        if !status.have_data {
            Self::refuse(
                cx,
                &requested_base,
                &requested_block,
                ServeRefusal {
                    score: None,
                    text: unavailable(&requested_block),
                },
            );
            return Ok(());
        }
        let proto = cx.ctx.version().get();
        match self.source.mnlistdiff(requested_base, requested_block, proto) {
            Ok(bytes) => {
                cx.push(OutMsg::new(CommandString::MNLISTDIFF, bytes));
            }
            Err(refusal) => Self::refuse(cx, &requested_base, &requested_block, refusal),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_use_core_rpc_hex_order() {
        let mut bytes = [0; 32];
        bytes[..4].copy_from_slice(&[0x01, 0x02, 0xa0, 0xff]);
        let hash = BlockHash::from(bytes);
        assert_eq!(rpc_hex(&hash), format!("{}ffa00201", "00".repeat(28)));
    }

    #[test]
    fn unavailable_text_matches_core_sentinel() {
        let hash = BlockHash::from([0x11; 32]);
        assert_eq!(
            unavailable(&hash),
            format!(
                "block data for block {} is not available (pruned or below an unvalidated snapshot base)",
                "11".repeat(32)
            )
        );
    }

    #[test]
    fn refusal_text_has_core_prefix() {
        let base = BlockHash::from([0x01; 32]);
        let block = BlockHash::from([0x02; 32]);
        assert_eq!(
            failure_text(&base, &block, "block data unavailable"),
            format!(
                "getmnlistdiff failed for baseBlockHash={}, blockHash={}. error=block data unavailable",
                "01".repeat(32),
                "02".repeat(32)
            )
        );
    }

    #[test]
    fn unavailable_data_is_unscored() {
        let refusal = ServeRefusal {
            score: None,
            text: "unavailable".into(),
        };
        assert_eq!(refusal.score, None);
    }

    #[test]
    fn request_errors_score_one() {
        let refusal = ServeRefusal {
            score: Some(1),
            text: "unknown".into(),
        };
        assert_eq!(refusal.score, Some(1));
    }
}
