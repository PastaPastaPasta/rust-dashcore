//! The intake's checks on a raw DKG payload before it is queued: its size cap and its framing.
//! Neither decodes a BLS object; the worker's read does that once, after the queue.

use dash_consensus::primitives::read_compact_size;
use dash_params::LlmqParams;

use crate::dkg::session::DkgMsgKind;

const PREFIX: usize = 1 + 32 + 32;
const PUBKEY: usize = 48;
const SIG: usize = 96;
const SECKEY: usize = 32;

/// `MaxDKGMessageSize`: the largest payload of `kind` a well-formed message under `params` can
/// have, with slack, capped at 1 MiB.
// Core: v24.0.0 src/llmq/net_dkg.cpp:44-76 MaxDKGMessageSize
pub fn max_dkg_message_size(kind: DkgMsgKind, params: &LlmqParams) -> usize {
    const COMPACT: usize = 5;
    const BLOB: usize = COMPACT + 128;
    const SLACK: usize = 1024;
    const HARD_CEILING: usize = 1 << 20;
    let size = usize::from(params.size);
    let threshold = usize::from(params.threshold);
    let cap = match kind {
        DkgMsgKind::Contribution => {
            PREFIX + (COMPACT + threshold * PUBKEY) + (PUBKEY + 32 + COMPACT + size * BLOB) + SIG
        }
        DkgMsgKind::Justification => PREFIX + (COMPACT + size * (4 + SECKEY)) + SIG,
        DkgMsgKind::Complaint => PREFIX + 2 * (COMPACT + size.div_ceil(8)) + SIG,
        DkgMsgKind::PrematureCommitment => {
            PREFIX + (COMPACT + size.div_ceil(8)) + PUBKEY + 32 + 2 * SIG
        }
    };
    (cap + SLACK).min(HARD_CEILING)
}

/// `CheckDKGMessageWireStructure`: whether `payload` frames a message of `kind` within
/// `params`' bounds and nothing follows it. A truncated or non-canonical read fails.
// Core: v24.0.0 src/llmq/net_dkg.cpp:164-185 CheckDKGMessageWireStructure
pub fn check_wire_structure(kind: DkgMsgKind, payload: &[u8], params: &LlmqParams) -> bool {
    let mut ds = Walk(payload);
    let framed = match kind {
        DkgMsgKind::Contribution => ds.contribution(params),
        DkgMsgKind::Complaint => ds.complaint(params),
        DkgMsgKind::Justification => ds.justification(params),
        DkgMsgKind::PrematureCommitment => ds.premature_commitment(params),
    };
    framed == Some(true) && ds.0.is_empty()
}

/// A read position over the payload; `None` is Core's thrown `ios_base::failure`.
struct Walk<'a>(&'a [u8]);

impl Walk<'_> {
    fn ignore(&mut self, n: usize) -> Option<()> {
        self.0 = self.0.get(n..)?;
        Some(())
    }

    fn compact(&mut self) -> Option<u64> {
        read_compact_size(&mut self.0, true).ok()
    }

    /// `ReadBoundedDynBitset`: a bit count of at most `max`, then its bytes, padding clear.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:89-98 ReadBoundedDynBitset
    // Core: v24.0.0 src/serialize.h:451-477 ReadFixedBitSet
    fn bitset(&mut self, max: usize) -> Option<u64> {
        let bits = self.compact()?;
        if bits > max as u64 {
            return None;
        }
        let nbytes = usize::try_from(bits.div_ceil(8)).ok()?;
        let bytes = self.0.get(..nbytes)?;
        self.ignore(nbytes)?;
        let rem = nbytes as u64 * 8 - bits;
        if rem != 0 && bytes[nbytes - 1] & !(0xffu8 >> rem) != 0 {
            return None;
        }
        Some(bits)
    }

    // Core: v24.0.0 src/llmq/net_dkg.cpp:100-121 CheckContributionWireStructure
    fn contribution(&mut self, params: &LlmqParams) -> Option<bool> {
        let threshold = usize::from(params.threshold);
        self.ignore(PREFIX)?;
        if self.compact()? != threshold as u64 {
            return Some(false);
        }
        self.ignore(threshold * PUBKEY)?;
        self.ignore(PUBKEY + 32)?;
        let blobs = self.compact()?;
        if blobs < u64::from(params.min_size) || blobs > u64::from(params.size) {
            return Some(false);
        }
        for _ in 0..blobs {
            let len = usize::try_from(self.compact()?).ok()?;
            self.ignore(len)?;
        }
        self.ignore(SIG)?;
        Some(true)
    }

    // Core: v24.0.0 src/llmq/net_dkg.cpp:123-135 CheckComplaintWireStructure
    fn complaint(&mut self, params: &LlmqParams) -> Option<bool> {
        let size = usize::from(params.size);
        self.ignore(PREFIX)?;
        let bad = self.bitset(size)?;
        let complain_for = self.bitset(size)?;
        if bad != complain_for {
            return Some(false);
        }
        self.ignore(SIG)?;
        Some(true)
    }

    // Core: v24.0.0 src/llmq/net_dkg.cpp:137-149 CheckJustificationWireStructure
    fn justification(&mut self, params: &LlmqParams) -> Option<bool> {
        self.ignore(PREFIX)?;
        let count = self.compact()?;
        if count > u64::from(params.size) {
            return Some(false);
        }
        self.ignore(usize::try_from(count).ok()? * (4 + SECKEY))?;
        self.ignore(SIG)?;
        Some(true)
    }

    // Core: v24.0.0 src/llmq/net_dkg.cpp:151-160 CheckPrematureCommitmentWireStructure
    fn premature_commitment(&mut self, params: &LlmqParams) -> Option<bool> {
        self.ignore(PREFIX)?;
        self.bitset(usize::from(params.size))?;
        self.ignore(PUBKEY + 32 + 2 * SIG)?;
        Some(true)
    }
}

#[cfg(test)]
mod tests;
