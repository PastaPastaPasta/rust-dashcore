//! Core's coins layer (A1 §2.4 `coins`): [`Coin`] and its coins-database serialization, the
//! amount and script compression it uses, the undo records, and [`CoinsView`].
//!
//! Every serialization here is identical at v23.1.8 and v24.0.0-rc.3 (`coins.h`, `compressor.*`,
//! `undo.h`; the pins differ only in `CCoinsViewErrorCatcher`'s error handling), so nothing takes
//! the release.
//!
//! - [`CoinRef`] and [`Coin`]: `Coin`, borrowed from its bytes without allocating, or owned;
//!   [`CoinRef::write`]/[`CoinRef::decode`] (`Coin::Serialize`, the database value) and
//!   [`CoinRef::write_undo`]/[`CoinRef::decode_undo`] (`TxInUndoFormatter`, one input's undo
//!   entry). [`coin_bytes`] writes a new coin's database value straight from its `TxOut`.
//! - [`compress`]: `CompressAmount`, `DecompressAmount`, `ScriptCompression` (with
//!   [`ScriptRef`], a script still in its compressed form) and `CScript::IsUnspendable`.
//! - [`TxUndo`], [`BlockUndo`], [`decode_block_undo`] and [`CoreUndoBytes`]: `CTxUndo` and
//!   `CBlockUndo`, and the bytes the rev files store; [`TxUndoRef`] and [`BlockUndoRef`] read and
//!   write them over borrowed coins.
//! - [`CoinsView`], [`CoinWrite`], [`CoinsBatch`] and [`MemoryCoins`]: `CCoinsView`'s reads, and
//!   the blind writes of `BatchWrite`; [`ViewOr`], the failure of a check that reads a view.
//! - [`MuHash3072`], [`tx_out_ser`] and [`HashSerialized2`]: the UTXO-set hashes of Core's coin
//!   statistics (`gettxoutsetinfo`, the coinstats index), `hash_serialized_2` with quirk 33.
//! - [`stats`]: the coinstats index's per-block totals and their reverse, `gettxoutsetinfo`'s
//!   `block_info`, `getblockstats`' fields and `hash_serialized_2` of a whole set.

mod coin;
pub mod compress;
mod muhash;
mod script;
pub mod stats;
mod undo;
mod utxo_hash;
mod view;

pub use coin::{Coin, CoinRef, MAX_COIN_HEIGHT, coin_bytes};
pub use compress::is_unspendable;
pub use muhash::MuHash3072;
pub use script::{CoinScript, ScriptRef};
pub use undo::{BlockUndo, BlockUndoRef, CoreUndoBytes, TxUndo, TxUndoRef, decode_block_undo};
pub use utxo_hash::{HashSerialized2, tx_out_ser};
pub use view::{
    CoinWrite, CoinsBatch, CoinsView, MAX_OUTPUTS_PER_BLOCK, MemoryCoins, ViewOr, access_by_txid,
};
