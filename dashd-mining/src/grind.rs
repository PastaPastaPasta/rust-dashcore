//! `GenerateBlock`'s proof-of-work loop (`rpc/mining.cpp:139-152`), up to, not including, its
//! `ProcessNewBlock`, which the caller makes.

use dash_consensus::Block;
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::Header;
use dash_num::CompactTarget;

/// How [`grind`] ended, decided in Core's order (`rpc/mining.cpp:148-153`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Grind {
    /// The header meets its target: submit the block.
    Found,
    /// `nNonce` reached `u32::MAX` with no proof found before it: Core returns true with no
    /// block, so `generateBlocks` assembles a new template and keeps the remaining tries. Decided
    /// even when that last nonce meets the target, which Core never checks.
    NonceExhausted,
    /// `max_tries` ran out, checked first: even a nonce that met the target on the last try
    /// loses.
    OutOfTries,
    /// `ShutdownRequested()`.
    Shutdown,
}

/// Sets `hashMerkleRoot` to `BlockMerkleRoot(block)`, then tries `nNonce` upward from its value
/// while `max_tries` lasts, the nonce is below `u32::MAX`, the header misses its target and
/// `shutdown` is false, spending one try per nonce passed. `max_tries` is the caller's: one
/// budget for every block of a `generate*` call.
pub fn grind(
    block: &mut Block,
    pow: &PowParams,
    max_tries: &mut u64,
    shutdown: &dyn Fn() -> bool,
) -> Grind {
    let mut raw = *block.header().raw();
    raw.merkle_root = block.merkle().0;
    let target = CompactTarget::new(raw.bits);
    let mut header = Header::new(raw);
    while *max_tries > 0
        && raw.nonce < u32::MAX
        && !check_proof_of_work(header.hash().as_bytes(), target, pow)
        && !shutdown()
    {
        raw.nonce += 1;
        *max_tries -= 1;
        header = Header::new(raw);
    }
    *block = Block::new(header, block.txs().to_vec());
    if *max_tries == 0 {
        Grind::OutOfTries
    } else if shutdown() {
        Grind::Shutdown
    } else if raw.nonce == u32::MAX {
        Grind::NonceExhausted
    } else {
        Grind::Found
    }
}

#[cfg(test)]
mod tests;
