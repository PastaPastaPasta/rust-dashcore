//! The chain reads LQ1 makes, through a view with an associated error (SB0 §4.1.1; EV addendum
//! §2.2): the node's complete ancestry never errs, and an anchored SPV client's view errs for a
//! block it should hold but does not.

use std::fmt;
use std::marker::PhantomData;

use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};

/// The chain reads LQ1 makes: bases, cycle bases, cycle predecessors, work blocks.
///
/// **Contract** (EV addendum M2). `ancestor(id, h)` is `Ok(None)` only when `h > height(id)`
/// (Core's null `GetAncestor`); `prev(id)` is `Ok(None)` only at actual genesis. A block the view
/// should hold but does not (below an anchored client's floor, not yet fetched) is `Err`, never
/// `Ok(None)`: a view never answers one of Core's normal-absence values for input it merely lacks
/// (EV addendum §2.1). Heights below zero are never passed: callers compute them in `i64` and
/// take Core's null-pointer branch without a view call.
pub trait LlmqChain {
    /// The view's error, the call's one error type.
    type Error;
    /// The block's header fields.
    fn header(&self, id: BlockId) -> Result<HeaderCtx, Self::Error>;
    /// `GetAncestor(height)`; `Ok(None)` only when `height` is above `id`'s height.
    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, Self::Error>;
    /// `pprev`; `Ok(None)` only at genesis.
    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, Self::Error>;
}

/// The node's complete ancestry as an [`LlmqChain`]. Never errs; `E` is the call's error type, so
/// one error type flows through each call (EV addendum §2.2).
pub struct CompleteChain<'a, E> {
    chain: &'a dyn ChainAncestry,
    _e: PhantomData<fn() -> E>,
}

impl<'a, E> CompleteChain<'a, E> {
    /// Wraps the node's block index.
    pub fn new(chain: &'a dyn ChainAncestry) -> Self {
        CompleteChain {
            chain,
            _e: PhantomData,
        }
    }
}

impl<E> Clone for CompleteChain<'_, E> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<E> Copy for CompleteChain<'_, E> {}

impl<E> LlmqChain for CompleteChain<'_, E> {
    type Error = E;

    fn header(&self, id: BlockId) -> Result<HeaderCtx, E> {
        Ok(self.chain.header(id))
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, E> {
        Ok(self.chain.ancestor(id, height))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, E> {
        Ok(self.chain.prev(id))
    }
}

/// An invariant of complete ancestry does not hold: corruption, never a panic. The mined-commitment
/// reader reports "a mined commitment's base is above the block that mined it" with it, without
/// naming a store error (EV addendum §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrokenInvariant(pub &'static str);

impl fmt::Display for BrokenInvariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for BrokenInvariant {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::MemChain;

    #[test]
    fn complete_chain_reads_as_the_block_index() {
        let mut chain = MemChain::new();
        let genesis = chain.genesis();
        let tip = chain.extend(genesis, 5);
        let fork = chain.extend(chain.ancestor(tip, 2).unwrap(), 2);
        let view = CompleteChain::<'_, BrokenInvariant>::new(&chain);
        for id in [genesis, tip, fork] {
            assert_eq!(view.header(id), Ok(chain.header(id)));
            assert_eq!(view.prev(id), Ok(chain.prev(id)));
            for h in 0..=6 {
                assert_eq!(view.ancestor(id, h), Ok(chain.ancestor(id, h)));
            }
        }
        // Core's null values, and only these, are `Ok(None)`.
        assert_eq!(view.prev(genesis), Ok(None));
        assert_eq!(view.ancestor(tip, 6), Ok(None));
        assert_eq!(view.ancestor(fork, 5), Ok(None));
        assert_eq!(view.ancestor(fork, 4), Ok(Some(fork)));
    }
}
