//! Quorum key material, persistence codecs, and the RPC-facing quorum projection.

mod info;
mod keys;
mod store;

pub use info::{MemberInfo, QuorumInfo, quorum_info};
pub use keys::{QuorumKeys, QuorumView, build_contributions, warm_members};
pub use store::{
    QuorumDbKey, StoredContributions, VvecReadError, decode_sk, decode_vvec, encode_sk,
    encode_vvec, from_store, load_or_build_contributions, quorum_db_key, to_store,
};

#[cfg(test)]
mod tests;
