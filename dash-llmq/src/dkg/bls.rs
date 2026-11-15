//! Core's BLS calls on the DKG path, over `dash_pkc::bls::core` (BS11b): the only module besides
//! [`super::wire`] that names dash-pkc (LQ4 §6). The rest of the crate takes the value types and
//! the functions from here, so a change of the base SDK's BLS API lands in these two files.

use std::collections::{BTreeMap, BTreeSet};

use dash_evo::types::ProTxHash;
use dash_pkc::bls::core::{
    self as core, BlsIesMultiBlobs as IesBlobs, public_key, secret_key_share, verify_vector,
};
use dash_pkc::bls::{BlsEagerPk, BlsSchemeSource};
use rand_core::CryptoRng;

pub use dash_evo::bls::{BlsCtx, BlsSchemeKind};
pub use dash_pkc::bls::core::{
    BlsIesMultiBlobs, BlsPkValue, BlsSigValue, BlsSkValue, BlsVerdict,
    CONTRIBUTION_VERIFY_BATCH_SIZE, ContributionVerdicts,
};
// Recovery uses Fr's ff traits and the public element API through these re-exports.
pub use dash_pkc::bls::{Fr, core::BlsSigElem};

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

/// `CBLSWorker::VerifyContributionShares(forId, vvecs, skShares, parallel, aggregated)`
/// (`bls/bls_worker.cpp:688-750`, v24.0.0): one verdict per contribution, checked in batches of
/// [`CONTRIBUTION_VERIFY_BATCH_SIZE`] by sums weighted with draws from `rng` when `aggregated`,
/// with a per-batch fallback; each contribution alone whenever the vectors fail
/// `VerifyVerificationVectors` together (`individually`, :706-736).
pub fn verify_contribution_shares(
    for_id: &[u8; 32],
    vvecs: &[&[BlsPkValue]],
    shares: &[&BlsSkValue],
    aggregated: bool,
    flag: BlsSchemeKind,
    rng: &mut dyn CryptoRng,
) -> ContributionVerdicts {
    let vvecs: Vec<Option<&[BlsPkValue]>> = vvecs.iter().copied().map(Some).collect();
    core::verify_contribution_shares(for_id, &vvecs, shares, aggregated, flag, &mut { rng })
}

/// `CBLSWorker::VerifyContributionShare` (`bls/bls_worker.cpp:752-776`, v24.0.0): one share
/// against its verification vector.
pub fn verify_contribution_share(
    for_id: &[u8; 32],
    vvec: &[BlsPkValue],
    share: &BlsSkValue,
    flag: BlsSchemeKind,
) -> bool {
    core::verify_contribution_share(for_id, Some(vvec), share, flag)
}

/// `CBLSWorker::BuildQuorumVerificationVector(vvecs)` (`bls/bls_worker.cpp`) under
/// `flag`: the element-wise sums of the vectors, or `None` when there is none or
/// `VerifyVerificationVectors` refuses them.
pub fn build_quorum_verification_vector(
    vvecs: &[&[BlsPkValue]],
    flag: BlsSchemeKind,
) -> Option<Vec<BlsPkValue>> {
    let vvecs: Vec<Option<&[BlsPkValue]>> = vvecs.iter().copied().map(Some).collect();
    core::build_quorum_verification_vector(&vvecs, flag)
}

/// `CBLSWorker::BuildPubKeyShare(vvec, id)` (`bls/bls_worker.cpp`): the public key
/// share `vvec` gives `id`, the invalid key when `PublicKeyShare` fails.
pub fn build_pub_key_share(vvec: &[BlsPkValue], id: &[u8; 32]) -> BlsPkValue {
    core::build_pub_key_share(vvec, id)
}

/// `CBLSWorker::AggregateSecretKeys(sks)` under `flag`: the sum in input order, the invalid
/// key when the list is empty or `VerifyVectorHelper` refuses it.
pub fn aggregate_secret_keys(sks: &[&BlsSkValue], flag: BlsSchemeKind) -> BlsSkValue {
    core::aggregate_secret_keys(sks, flag)
}

/// `CBLSWorker::FindVerificationVectorsWithSharedCoefficients(vvecs)`, which follows v24.0.0
/// (#8016; v24.0.0 `bls/bls_worker.cpp:807-824`), under `flag`: the indexes, in ascending order,
/// of the vectors holding a coefficient that another vector holds too.
///
/// A coefficient is keyed on its serialization under `flag`, which `GetHash()` hashes, so
/// invalid keys (all zeros) are one value. The first vector to hold a key owns it; a key a
/// vector repeats is not shared, and a missing vector is skipped.
pub fn vvecs_with_shared_coefficients(
    vvecs: &[Option<&[BlsPkValue]>],
    flag: BlsSchemeKind,
) -> BTreeSet<usize> {
    let mut owners = BTreeMap::new();
    let mut shared = BTreeSet::new();
    for (i, vvec) in vvecs.iter().enumerate() {
        for coefficient in vvec.unwrap_or_default() {
            let owner = *owners.entry(coefficient.to_bytes(flag)).or_insert(i);
            if owner != i {
                shared.extend([owner, i]);
            }
        }
    }
    shared
}

/// The wrapper's `operator==` on two public keys (`bls/bls.h:77-80`): the same validity and
/// equal elements.
pub fn public_keys_equal(a: &BlsPkValue, b: &BlsPkValue) -> bool {
    a.core_eq(b)
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

/// `MakeBatchVerificationWeight(rng)` with its default of 16 bytes
/// (`bls/bls_batchverifier.h:18-27`, v24.0.0): a key whose low 16 bytes are the first 16 of a
/// 32-byte draw from `rng`, drawn again while all zero.
pub fn make_batch_verification_weight(rng: &mut dyn CryptoRng) -> BlsSkValue {
    core::make_batch_verification_weight::<16>(&mut { rng })
}

/// `CBLSPublicKey::MulInsecure(factor)` (`bls/bls.cpp:208-213`, v24.0.0): `pk` times `factor`.
/// Core asserts that both are valid; here either one invalid gives the invalid key.
pub fn mul_public_key_insecure(pk: &BlsPkValue, factor: &BlsSkValue) -> BlsPkValue {
    core::mul_public_key_insecure(pk, factor)
}

/// `CBLSSignature::MulInsecure(factor)` (`bls/bls.cpp:303-308`, v24.0.0): `sig` times `factor`.
/// Core asserts that both are valid; here either one invalid gives the invalid signature.
pub fn mul_signature_insecure(sig: &BlsSigValue, factor: &BlsSkValue) -> BlsSigValue {
    core::mul_signature_insecure(sig, factor)
}

/// `CBLSSignature::AggregateSecure(sigs, pks, hash)` under `flag` (`bls/bls.cpp:257-287`): the
/// invalid signature for empty or unequal lists. Core does not read the hash.
pub fn aggregate_signatures_secure(
    sigs: &[&BlsSigValue],
    pks: &[&BlsPkValue],
    flag: BlsSchemeKind,
) -> BlsSigValue {
    core::aggregate_signatures_secure(sigs, pks, flag)
}

/// `CBLSSignature::Recover(sigs, ids)` (`bls/bls.cpp:360-391`): the signature the shares `sigs`
/// at `ids` give, `None` where Core's `Recover` returns false (fewer than two shares, an invalid
/// share, unequal lists, a zero or repeated id).
pub fn recover_signature(sigs: &[&BlsSigValue], ids: &[[u8; 32]]) -> Option<BlsSigValue> {
    let mut out = BlsSigValue::invalid();
    core::recover_signature(&mut out, sigs, ids).then_some(out)
}

/// A verification-vector key as the dkgdb gives it back: written with
/// `CBLSPublicKeyVersionWrapper(pk, false)`, so in the basic encoding (`l/dkgsessionmgr.cpp:125-132`),
/// and read back in the basic encoding (:174-178). Basic bytes that don't decode under the basic
/// scheme, all zeros included, come back as the invalid key.
pub fn stored_vvec_key(pk: &BlsPkValue) -> BlsPkValue {
    let bytes = pk.to_bytes(BlsSchemeKind::Basic);
    BlsEagerPk::decode(&bytes, BlsSchemeSource::Version(BlsSchemeKind::Basic))
        .map_or_else(|_| BlsPkValue::invalid(), BlsEagerPk::into_value)
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
pub fn make_new_key(rng: &mut impl CryptoRng) -> BlsSkValue {
    core::make_new_key(rng)
}

/// `CBLSSecretKey::Sign(hash, legacy)` (`bls/bls.cpp:125-142`): the signature under the
/// legacy scheme or the basic one, the invalid signature for an invalid key.
pub fn sign(sk: &BlsSkValue, hash: &[u8; 32], legacy: bool) -> BlsSigValue {
    core::sign(sk, hash, BlsSchemeKind::from_legacy(legacy))
}

/// `CBLSSecretKey::Sign(hash, legacy)` through dash-pkc's typed API: the operator signature of
/// the reference harness and the tests, equal to [`sign`]'s for a valid key. Production signs
/// through AM1's [`super::session::OperatorKey`].
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

/// `a.AggregateInsecure(error)` and `b.SubInsecure(error)` (`bls/bls.cpp:229-238`, :296-301):
/// the complementary signature errors the tests build.
#[cfg(test)]
pub(crate) fn add_and_subtract_error(
    a: &BlsSigValue,
    b: &BlsSigValue,
    error: &BlsSigValue,
) -> (BlsSigValue, BlsSigValue) {
    (core::aggregate_signatures_insecure_pair(a, error), core::sub_signatures_insecure(b, error))
}

#[cfg(test)]
mod tests;
