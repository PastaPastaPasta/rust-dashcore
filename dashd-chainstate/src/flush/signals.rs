//! Core's `ChainStateFlushed` validation signal (E-b1.4): the port V hands it to. CH1-node/ND1's
//! `ValidationSignals` plugs in; until then it is [`NullSignals`].

use std::sync::Mutex;

use dash_chain::BlockRef;

/// Where V sends Core's `ChainStateFlushed` (the wallet's `WriteBestBlock`). V calls it from inside
/// its waits for W, so it must not block: it queues the signal (CH1-node §8.2).
pub trait FlushSignals: Send + Sync {
    // Core: validation.cpp:3111-3115 (GetMainSignals().ChainStateFlushed(m_chain.GetLocator()))
    /// A flush's B1 has returned Durable, and V has seen W release it (DEC-166). `tip` is the
    /// locator's tip: V's tip at the flush's call site, before the step (Core signals before
    /// `SetTip`). The port builds the locator from it. The signal lags, and never leads (D16): it
    /// names the flush's `new` or an ancestor, or at a disconnect site the disconnected block.
    fn chain_state_flushed(&self, tip: BlockRef);
}

/// The port before CH1-node/ND1: the signal goes nowhere.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullSignals;

impl FlushSignals for NullSignals {
    fn chain_state_flushed(&self, _: BlockRef) {}
}

/// A port that records the signals it was handed: the seam's and the tests'.
#[derive(Debug, Default)]
pub struct RecordingSignals {
    tips: Mutex<Vec<BlockRef>>,
}

impl RecordingSignals {
    /// The locator tips handed over so far, in order.
    pub fn tips(&self) -> Vec<BlockRef> {
        self.tips.lock().expect("unpoisoned").clone()
    }
}

impl FlushSignals for RecordingSignals {
    fn chain_state_flushed(&self, tip: BlockRef) {
        self.tips.lock().expect("unpoisoned").push(tip);
    }
}
