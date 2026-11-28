//! Core's `getblocktemplate` statics (`rpc/mining.cpp:756`, `:809-834` at v24.0.2) as one
//! per-engine cache behind the GBT mutex.

use dash_primitives::BlockHash;

use crate::{BlockTemplate, BlockTemplateSource, MiningMempool, TemplateError, TemplateTxs};

/// `CScript() << OP_TRUE`, the cached template's coinbase script (`rpc/mining.cpp:823`).
const OP_TRUE_SCRIPT: [u8; 1] = [0x51];

/// `pindexPrev`, `nTransactionsUpdatedLast`, `time_start` and `pblocktemplate`.
#[derive(Default)]
pub(super) struct GbtCache {
    /// `pindexPrev`: the tip the cached template was built on; `None` before the first build
    /// and after a failed one.
    pub(super) prev: Option<BlockHash>,
    pub(super) tx_updated_last: u32,
    pub(super) time_start: i64,
    pub(super) template: Option<Cached>,
}

/// `pblocktemplate`, with the header fields the calls edit in place on `pblock`.
pub(super) struct Cached {
    pub(super) template: BlockTemplate,
    pub(super) version: i32,
    pub(super) time: u32,
    pub(super) bits: u32,
    pub(super) nonce: u32,
}

impl Cached {
    fn new(template: BlockTemplate) -> Cached {
        let header = *template.block.header();
        Cached {
            template,
            version: header.version(),
            time: header.time(),
            bits: header.bits(),
            nonce: header.nonce(),
        }
    }
}

impl GbtCache {
    /// The cache check and rebuild (`rpc/mining.cpp:812-833`) at V's tip `tip` and `GetTime()`
    /// `now`: the cached template, rebuilt when the tip moved, or when the mempool changed and
    /// more than 5 seconds passed since the last build. Each read of the update counter is
    /// `GetTransactionsUpdated()`'s lock-free load.
    ///
    /// # Errors
    ///
    /// `CreateNewBlock`'s, leaving `prev` cleared so the next call builds again.
    pub(super) fn refresh(
        &mut self,
        tip: BlockHash,
        mempool: &dyn MiningMempool,
        source: &dyn BlockTemplateSource,
        now: i64,
    ) -> Result<(), TemplateError> {
        // Core: v24.0.2 rpc/mining.cpp:812-813
        if self.prev != Some(tip)
            || (mempool.transactions_updated() != self.tx_updated_last && now - self.time_start > 5)
        {
            // :815-820
            self.prev = None;
            self.tx_updated_last = mempool.transactions_updated();
            self.time_start = now;
            // :823-828
            let built = source.create_new_block(&OP_TRUE_SCRIPT, TemplateTxs::Mempool)?;
            // :831
            self.prev = Some(built.prev);
            self.template = Some(Cached::new(built));
        }
        Ok(())
    }

    /// The template a successful [`Self::refresh`] left.
    pub(super) fn cached(&mut self) -> &mut Cached {
        self.template.as_mut().expect("a refreshed cache holds its template")
    }
}
