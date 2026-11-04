//! Compact-block reconstruction (oracle.h "compact blocks", ABI 34): net_processing's
//! `PartiallyDownloadedBlock` over a mempool the caller describes.

use super::*;

/// `ReadStatus` (blockencodings.h).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReadStatus {
    Ok,
    /// Invalid object, the peer is sending bogus crap.
    Invalid,
    /// Failed to process the object.
    Failed,
}

impl ReadStatus {
    fn from_raw(status: i32) -> Option<Self> {
        match status {
            0 => Some(Self::Ok),
            1 => Some(Self::Invalid),
            2 => Some(Self::Failed),
            _ => None,
        }
    }
}

/// What `InitData` and `FillBlock` returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactFill {
    pub init: ReadStatus,
    /// None when `InitData` was not [`ReadStatus::Ok`] (`FillBlock` then does not run).
    pub fill: Option<ReadStatus>,
    /// The reconstructed block (network serialization) when `FillBlock` returned
    /// [`ReadStatus::Ok`].
    pub block: Option<Vec<u8>>,
}

fn tx_list(txs: &[Vec<u8>]) -> Vec<u8> {
    let mut list = Vec::new();
    write_compact_size(&mut list, txs.len() as u64);
    txs.iter().for_each(|tx| list.extend(tx));
    list
}

impl Oracle {
    /// A compact block's reconstruction as net_processing runs it
    /// (`oracle_partially_downloaded_block`): `InitData(cmpct, extra_txs)` over a mempool of
    /// `pool_txs` (scanned in that order; their txids must be distinct, else
    /// [`ErrorKind::Argument`]), then `FillBlock` with the transactions of `blocktxn` (a
    /// `blocktxn` payload, whose block hash is not read; None for none, as for a compact block
    /// `InitData` completed). Pool and extra transactions are keyed by their instance hashes, as
    /// net_processing keys them. Each argument must decode whole. Reads no chain state.
    pub fn partially_downloaded_block(
        &self,
        cmpct: &[u8],
        pool_txs: &[Vec<u8>],
        extra_txs: &[Vec<u8>],
        blocktxn: Option<&[u8]>,
    ) -> Result<CompactFill> {
        let (pool, extra) = (tx_list(pool_txs), tx_list(extra_txs));
        let blocktxn = blocktxn.unwrap_or_default();
        self.shared(|| {
            let (mut init, mut fill) = (-1, -1);
            let block = self.with_buffer(
                cmpct.len() * 2 + pool.len() + extra.len() + blocktxn.len(),
                |out, len| unsafe {
                    (self.api.partially_downloaded_block)(
                        cmpct.as_ptr(),
                        cmpct.len(),
                        pool.as_ptr(),
                        pool.len(),
                        extra.as_ptr(),
                        extra.len(),
                        blocktxn.as_ptr(),
                        blocktxn.len(),
                        &mut init,
                        &mut fill,
                        out,
                        len,
                    )
                },
            )?;
            let init = ReadStatus::from_raw(init).expect("the shim's InitData status");
            let fill = (fill != -1)
                .then(|| ReadStatus::from_raw(fill).expect("the shim's FillBlock status"));
            Ok(CompactFill {
                init,
                block: (fill == Some(ReadStatus::Ok)).then_some(block),
                fill,
            })
        })
    }
}
