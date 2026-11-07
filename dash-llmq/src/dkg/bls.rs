//! Core's BLS calls on the DKG path, over `dash_pkc::bls::core` (BS11b): the only module besides
//! [`super::wire`] that names dash-pkc (LQ4 §6). The rest of the crate takes the value types and
//! the functions from here, so a change of the base SDK's BLS API lands in these two files.

use dash_evo::types::ProTxHash;
use dash_pkc::bls::core::{
    self as core, BlsIesMultiBlobs as IesBlobs, public_key, secret_key_share, verify_vector,
};

pub use dash_evo::bls::{BlsCtx, BlsSchemeKind};
pub use dash_pkc::bls::core::{
    BlsIesMultiBlobs, BlsPkValue, BlsSigValue, BlsSkValue, BlsVerdict,
    CONTRIBUTION_VERIFY_BATCH_SIZE,
};

/// The length of an IES IV seed (`CBLSIESMultiRecipientBlobs::ivSeed`, a `uint256`).
pub const IV_SEED_LEN: usize = 32;

/// `CBLSId(proTxHash)`: a member's id is its proTxHash's internal bytes (`l/dkgsession.cpp:68`).
pub fn member_id(pro_tx: &ProTxHash) -> [u8; 32] {
    *pro_tx.as_bytes()
}

/// `CBLSWorker::GenerateContributions` (`bls/bls_worker.cpp:78-118`) from coefficients already
/// drawn: the verification vector `vvec[j] = coefficients[j]·G1`, and each id's secret share,
/// the polynomial at the id. `None` when a share fails, as it does for every id when there are
/// fewer than 2 coefficients.
///
/// Core draws the coefficients inside the call (`MakeNewKey`, :86); here the session draws
/// them through [`super::rng::DkgRng`] first, so the draw order is the session's to state.
pub fn generate_contributions(
    coefficients: &[BlsSkValue],
    ids: &[[u8; 32]],
) -> Option<(Vec<BlsPkValue>, Vec<BlsSkValue>)> {
    let vvec = coefficients.iter().map(public_key).collect();
    let msk: Vec<&BlsSkValue> = coefficients.iter().collect();
    let mut shares = Vec::with_capacity(ids.len());
    for id in ids {
        let mut share = BlsSkValue::invalid();
        if !secret_key_share(&mut share, &msk, id) {
            return None;
        }
        shares.push(share);
    }
    Some((vvec, shares))
}

/// `CBLSWorker::VerifyVerificationVector` (`bls/bls_worker.cpp:747-750`) under `flag`.
pub fn verify_verification_vector(vvec: &[BlsPkValue], flag: BlsSchemeKind) -> bool {
    verify_vector(&vvec.iter().collect::<Vec<_>>(), flag)
}

/// `CBLSWorker::VerifyContributionShares(forId, vvecs, skShares)` with Core's defaults
/// (`aggregated = true`; `bls/bls_worker.cpp:686-719`): one verdict per contribution, checked
/// in aggregated batches of [`CONTRIBUTION_VERIFY_BATCH_SIZE`] with a per-batch fallback.
pub fn verify_contribution_shares(
    for_id: &[u8; 32],
    vvecs: &[&[BlsPkValue]],
    shares: &[&BlsSkValue],
    flag: BlsSchemeKind,
) -> Vec<bool> {
    let vvecs: Vec<Option<&[BlsPkValue]>> = vvecs.iter().copied().map(Some).collect();
    core::verify_contribution_shares(for_id, &vvecs, shares, true, flag)
}

/// `CBLSWorker::VerifyContributionShare` (`bls/bls_worker.cpp:721-745`): one share against its
/// verification vector.
pub fn verify_contribution_share(
    for_id: &[u8; 32],
    vvec: &[BlsPkValue],
    share: &BlsSkValue,
    flag: BlsSchemeKind,
) -> bool {
    core::verify_contribution_share(for_id, Some(vvec), share, flag)
}

/// `CBLSSignature::VerifyInsecure(pk, hash)` under `flag`.
pub fn verify_insecure(
    sig: &BlsSigValue,
    pk: &BlsPkValue,
    hash: &[u8; 32],
    flag: BlsSchemeKind,
) -> bool {
    core::verify_insecure(sig, pk, hash, flag).is_valid()
}

/// `aggSig.VerifyInsecureAggregated(pks, hashes)` under `flag`, over the aggregate Core builds
/// with `AggregateInsecure` (`l/net_dkg.cpp:259-263`).
pub fn verify_insecure_aggregated(
    sigs: &[&BlsSigValue],
    pks: &[&BlsPkValue],
    hashes: &[[u8; 32]],
    flag: BlsSchemeKind,
) -> bool {
    let agg = core::aggregate_signatures_insecure(sigs);
    core::verify_insecure_aggregated(&agg, pks, hashes, flag).is_valid()
}

/// `CBLSSecretKey::GetPublicKey`.
pub fn secret_to_public(sk: &BlsSkValue) -> BlsPkValue {
    public_key(sk)
}

/// The ephemeral key and IV seed `CBLSIESMultiRecipientBlobs::InitEncrypt` draws
/// (`bls/bls_ies.cpp:59-71`).
#[derive(Clone, Debug)]
pub struct IesSeed {
    /// `ephemeralSecretKey.MakeNewKey()`.
    pub ephemeral_sk: BlsSkValue,
    /// `GetStrongRandBytes(ivSeed)`.
    pub iv_seed: [u8; IV_SEED_LEN],
}

/// `InitEncrypt` with `seed`, then `Encrypt(i, pk_i, sk_i)` for each recipient in order, each
/// secret key serialized under `flag` as Core's `ds << obj` does (`a/dkgsession.cpp:84-95`).
/// `false` when a recipient's encryption fails; the blobs after it are empty.
pub fn encrypt_shares(
    seed: &IesSeed,
    recipients: &[(&BlsPkValue, &BlsSkValue)],
    flag: BlsSchemeKind,
) -> (BlsIesMultiBlobs, bool) {
    let plaintexts: Vec<[u8; 32]> = recipients.iter().map(|(_, sk)| sk.to_bytes(flag)).collect();
    let recipients: Vec<(&BlsPkValue, &[u8])> = recipients
        .iter()
        .zip(&plaintexts)
        .map(|((pk, _), plaintext)| (*pk, &plaintext[..]))
        .collect();
    IesBlobs::encrypt_with(&seed.ephemeral_sk, &seed.iv_seed, &recipients)
}

/// Whether `Encrypt` to `pk` can succeed: Core's `DHKeyExchange` refuses an invalid key
/// (`bls/bls.cpp:208-220`), and a 32-byte secret key always fills whole AES blocks.
pub fn can_encrypt_to(pk: &BlsPkValue) -> bool {
    pk.is_valid()
}

/// `CBLSIESMultiRecipientObjects<CBLSSecretKey>::Decrypt(idx, sk, out)` (`bls/bls_ies.h`): the
/// blob at `idx` decrypted with `sk` and read as a secret key under `flag`.
pub fn decrypt_share(
    blobs: &BlsIesMultiBlobs,
    idx: usize,
    sk: &BlsSkValue,
    flag: BlsSchemeKind,
) -> Option<BlsSkValue> {
    blobs.decrypt_secret_key(idx, sk, flag)
}

/// A secret key drawn as `CBLSSecretKey::MakeNewKey` draws it (`bls/bls.cpp`).
pub fn make_new_key(rng: &mut impl rand_core::CryptoRng) -> BlsSkValue {
    core::make_new_key(rng)
}

/// `CBLSSecretKey::Sign(hash, legacy)` through dash-pkc's typed API, until BS11c's
/// `core::sign` exists (P-1): the operator signature of the reference harness and the tests.
/// Production signs through AM1's [`super::session::OperatorKey`].
#[cfg(any(test, feature = "reference"))]
pub fn sign_stand_in(sk: &BlsSkValue, hash: &[u8; 32], legacy: bool) -> BlsSigValue {
    use dash_pkc::bls::{BlsScChia, BlsScIetf, BlsSecretKey, BlsSigId};
    let bytes = sk.to_bytes(BlsSchemeKind::Basic);
    let signed = if legacy {
        BlsSecretKey::<BlsScChia>::from_bytes(&bytes).map(|key| key.sign_digest(hash).to_bytes())
    } else {
        BlsSecretKey::<BlsScIetf>::from_bytes(&bytes)
            .map(|key| key.sign_with(hash, BlsSigId::Basic).to_bytes())
    };
    let scheme = BlsSchemeKind::from_legacy(legacy);
    signed
        .ok()
        .and_then(|sig| BlsSigValue::from_canonical(&sig, scheme))
        .unwrap_or_else(BlsSigValue::invalid)
}

#[cfg(test)]
mod tests;
