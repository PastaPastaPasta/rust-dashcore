//! Deterministic basic-scheme BLS keys for test fixtures and node sessions.
//!
//! The only BLS key derivation of the fixtures: the recorder's operator keys (through the
//! `testkit-keys` CLI) and in-process session keys both come from [`derive_basic_bls_key`].
//! [`recover_quorum_sk`] recovers a regtest quorum's secret key from its members' shares.

use std::fmt;

use dash_pkc::bls::{
    BlsError, BlsPkBytes, BlsPublicKey, BlsScIetf, BlsSecretKey, BlsShareId, BlsSkBytes, BlsSkShare,
};
use dashcore_hashes::{HashEngine, sha256};

/// Domain tag of the derivation; bumping it is a breaking change to every image.
pub const BASIC_BLS_DOMAIN: &[u8] = b"dash-testkit/basic-bls/v1";

/// A basic-scheme BLS key pair from [`derive_basic_bls_key`], as base-sdk byte bags.
///
/// `sk` is the 32-byte big-endian scalar and `pk` the 48-byte serialized public key, the byte
/// orders Core's `bls generate` and `bls fromsecret` print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasicBlsKey {
    pub sk: BlsSkBytes,
    pub pk: BlsPkBytes,
}

/// The basic-scheme (IETF, post-V19) BLS key number `index` under `seed`.
///
/// `ikm = SHA-256(BASIC_BLS_DOMAIN ‖ seed ‖ index as 4 big-endian bytes)`, then IETF KeyGen.
pub fn derive_basic_bls_key(seed: &[u8; 32], index: u32) -> BasicBlsKey {
    let mut engine = sha256::Hash::engine();
    engine.input(BASIC_BLS_DOMAIN);
    engine.input(seed);
    engine.input(&index.to_be_bytes());
    let ikm = sha256::Hash::from_engine(engine);
    let sk = BlsSecretKey::<BlsScIetf>::from_ikm(ikm.as_byte_array())
        .expect("from_ikm fails only for ikm shorter than 32 bytes");
    BasicBlsKey {
        pk: sk.public_key().into(),
        sk: sk.into(),
    }
}

/// Why [`recover_quorum_sk`] refused. No variant holds a key or a share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuorumSkError {
    /// The threshold is below 2.
    ThresholdBelowTwo,
    /// Fewer shares than the threshold.
    TooFewShares {
        shares: usize,
        threshold: usize,
    },
    /// Two shares have the same id.
    DuplicateId,
    /// The quorum public key does not decode as a basic-scheme key.
    InvalidPublicKey,
    /// The share at this position of the input is not a valid secret key.
    InvalidShare(usize),
    /// Interpolation failed.
    Recover(BlsError),
    /// The two share subsets recover different keys, so the shares are not one quorum's.
    SubsetsDisagree,
    /// The recovered key times G is not the quorum public key.
    PublicKeyMismatch,
}

impl fmt::Display for QuorumSkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ThresholdBelowTwo => f.write_str("threshold is below 2"),
            Self::TooFewShares {
                shares,
                threshold,
            } => write!(f, "{shares} shares, fewer than the threshold {threshold}"),
            Self::DuplicateId => f.write_str("two shares have the same id"),
            Self::InvalidPublicKey => f.write_str("pk is not a basic-scheme BLS public key"),
            Self::InvalidShare(i) => write!(f, "share {i} is not a valid BLS secret key"),
            Self::Recover(err) => write!(f, "recovery failed: {err}"),
            Self::SubsetsDisagree => f.write_str(
                "two share subsets recover different keys (shares of different quorums)",
            ),
            Self::PublicKeyMismatch => f.write_str("recovered sk·G != quorum pk"),
        }
    }
}

impl std::error::Error for QuorumSkError {}

/// A quorum's secret key, recovered from its members' secret key shares, as Core's
/// `quorum info <type> <hash> true` gives them per member.
///
/// Each share's id is the member's proTxHash in Core's stored byte order (`CBLSId(proTxHash)`),
/// which is the reverse of the RPC hex: [`BlsShareId::from_hex`] of the RPC hex gives it.
///
/// The shares, sorted by their stored id bytes, recover once from the first `threshold` and, when
/// there are more, again from the last `threshold`. The two keys must be equal, and the key times
/// G must be `pk`. With more than `2 * threshold` shares the middle ones are in neither subset and
/// go unchecked; every regtest LLMQ has at most `2 * threshold` members.
pub fn recover_quorum_sk(
    pk: &BlsPkBytes,
    shares: &[(BlsShareId, BlsSkBytes)],
    threshold: usize,
) -> Result<BlsSkBytes, QuorumSkError> {
    if threshold < 2 {
        return Err(QuorumSkError::ThresholdBelowTwo);
    }
    if shares.len() < threshold {
        return Err(QuorumSkError::TooFewShares {
            shares: shares.len(),
            threshold,
        });
    }
    let pk =
        BlsPublicKey::<BlsScIetf>::try_from(*pk).map_err(|_| QuorumSkError::InvalidPublicKey)?;
    let mut sorted = shares
        .iter()
        .enumerate()
        .map(|(i, (id, sk))| {
            BlsSecretKey::<BlsScIetf>::try_from(sk.clone())
                .map(|sk| BlsSkShare::new(*id, sk))
                .map_err(|_| QuorumSkError::InvalidShare(i))
        })
        .collect::<Result<Vec<_>, _>>()?;
    sorted.sort_by(|a, b| a.id().as_bytes().cmp(b.id().as_bytes()));
    if sorted.windows(2).any(|w| w[0].id() == w[1].id()) {
        return Err(QuorumSkError::DuplicateId);
    }
    let recover = |subset: &[BlsSkShare<BlsScIetf>]| {
        BlsSecretKey::recover_shares(&subset.iter().collect::<Vec<_>>())
            .map_err(QuorumSkError::Recover)
    };
    let sk = recover(&sorted[..threshold])?;
    if sorted.len() > threshold && recover(&sorted[sorted.len() - threshold..])? != sk {
        return Err(QuorumSkError::SubsetsDisagree);
    }
    if sk.public_key() != pk {
        return Err(QuorumSkError::PublicKeyMismatch);
    }
    Ok(sk.into())
}
