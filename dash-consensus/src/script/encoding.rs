//! Signature and public-key encoding checks (`script/interpreter.cpp:63-222`).

use super::ecdsa::check_low_s;
use super::error::ScriptError;
use super::flags::ScriptFlags;

/// `SIGHASH_ALL`, `SIGHASH_NONE`, `SIGHASH_SINGLE` and `SIGHASH_ANYONECANPAY` (`interpreter.h`).
pub const SIGHASH_ALL: i32 = 1;
pub const SIGHASH_NONE: i32 = 2;
pub const SIGHASH_SINGLE: i32 = 3;
pub const SIGHASH_ANYONECANPAY: i32 = 0x80;

// Core: script/interpreter.cpp:107 IsValidSignatureEncoding
/// BIP66 strict DER, plus the trailing hash-type byte:
/// `0x30 [total-length] 0x02 [R-length] [R] 0x02 [S-length] [S] [sighash]`, 9 to 73 bytes, R and S
/// positive, non-empty and minimally encoded. Consensus-critical since BIP66.
pub fn is_valid_signature_encoding(sig: &[u8]) -> bool {
    // Minimum and maximum size constraints.
    if sig.len() < 9 || sig.len() > 73 {
        return false;
    }
    // A signature is of type 0x30 (compound).
    if sig[0] != 0x30 {
        return false;
    }
    // Make sure the length covers the entire signature.
    if usize::from(sig[1]) != sig.len() - 3 {
        return false;
    }
    // Extract the length of the R element.
    let len_r = usize::from(sig[3]);
    // Make sure the length of the S element is still inside the signature.
    if 5 + len_r >= sig.len() {
        return false;
    }
    // Extract the length of the S element.
    let len_s = usize::from(sig[5 + len_r]);
    // Verify that the length of the signature matches the sum of the length of the elements.
    if len_r + len_s + 7 != sig.len() {
        return false;
    }
    // Check whether the R element is an integer.
    if sig[2] != 0x02 {
        return false;
    }
    // Zero-length integers are not allowed for R.
    if len_r == 0 {
        return false;
    }
    // Negative numbers are not allowed for R.
    if sig[4] & 0x80 != 0 {
        return false;
    }
    // Null bytes at the start of R are not allowed, unless R would otherwise be interpreted as a
    // negative number.
    if len_r > 1 && sig[4] == 0x00 && sig[5] & 0x80 == 0 {
        return false;
    }
    // Check whether the S element is an integer.
    if sig[len_r + 4] != 0x02 {
        return false;
    }
    // Zero-length integers are not allowed for S.
    if len_s == 0 {
        return false;
    }
    // Negative numbers are not allowed for S.
    if sig[len_r + 6] & 0x80 != 0 {
        return false;
    }
    // Null bytes at the start of S are not allowed, unless S would otherwise be interpreted as a
    // negative number.
    if len_s > 1 && sig[len_r + 6] == 0x00 && sig[len_r + 7] & 0x80 == 0 {
        return false;
    }
    true
}

// Core: script/interpreter.cpp:172 IsLowDERSignature
/// Strict DER, and S (checked without the hash-type byte) in the lower half of the order.
fn is_low_der_signature(sig: &[u8]) -> Result<(), ScriptError> {
    if !is_valid_signature_encoding(sig) {
        // census: unreachable (CheckSignatureEncoding checks DER first whenever LOW_S is set)
        return Err(ScriptError::SigDer);
    }
    // The hash-type byte follows the DER signature; CheckLowS parses the signature without it.
    if !check_low_s(&sig[..sig.len() - 1]) {
        return Err(ScriptError::SigHighS);
    }
    Ok(())
}

// Core: script/interpreter.cpp:189 IsDefinedHashtypeSignature
/// The hash type, without `SIGHASH_ANYONECANPAY`, is `ALL`, `NONE` or `SINGLE`.
fn is_defined_hashtype_signature(sig: &[u8]) -> bool {
    let Some(&last) = sig.last() else {
        // census: unreachable (CheckSignatureEncoding returns early on an empty signature)
        return false;
    };
    let hash_type = last & !(SIGHASH_ANYONECANPAY as u8);
    (SIGHASH_ALL as u8..=SIGHASH_SINGLE as u8).contains(&hash_type)
}

// Core: script/interpreter.cpp:200 CheckSignatureEncoding
/// The signature encoding rules the flags select. An empty signature always passes (it is the
/// compact way to provide a failing signature). Otherwise: strict DER when any of `DERSIG`,
/// `LOW_S` or `STRICTENC` is set; then low S under `LOW_S`; then a defined hash type under
/// `STRICTENC`.
pub fn check_signature_encoding(sig: &[u8], flags: ScriptFlags) -> Result<(), ScriptError> {
    if sig.is_empty() {
        return Ok(());
    }
    let strict = ScriptFlags::DERSIG | ScriptFlags::LOW_S | ScriptFlags::STRICTENC;
    if flags.intersects(strict) && !is_valid_signature_encoding(sig) {
        return Err(ScriptError::SigDer);
    }
    if flags.contains(ScriptFlags::LOW_S) {
        is_low_der_signature(sig)?;
    }
    if flags.contains(ScriptFlags::STRICTENC) && !is_defined_hashtype_signature(sig) {
        return Err(ScriptError::SigHashtype);
    }
    Ok(())
}

// Core: script/interpreter.cpp:63 IsCompressedOrUncompressedPubKey
/// 33 bytes starting 02/03, or 65 bytes starting 04. Hybrid keys (06/07) are rejected.
fn is_compressed_or_uncompressed_pubkey(pubkey: &[u8]) -> bool {
    // Non-canonical public key: too short.
    if pubkey.len() < 33 {
        return false;
    }
    match pubkey[0] {
        // Uncompressed: 65 bytes.
        0x04 => pubkey.len() == 65,
        // Compressed: 33 bytes.
        0x02 | 0x03 => pubkey.len() == 33,
        // Neither compressed nor uncompressed.
        _ => false,
    }
}

// Core: script/interpreter.cpp:217 CheckPubKeyEncoding
/// Under `STRICTENC` only: the key is compressed or uncompressed. It applies to every reached key,
/// even when the signature is empty.
pub fn check_pubkey_encoding(pubkey: &[u8], flags: ScriptFlags) -> Result<(), ScriptError> {
    if flags.contains(ScriptFlags::STRICTENC) && !is_compressed_or_uncompressed_pubkey(pubkey) {
        return Err(ScriptError::PubkeyType);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A strict-DER signature with one-byte R and S, plus the hash-type byte.
    fn sig(r: u8, s: u8, hash_type: u8) -> Vec<u8> {
        vec![0x30, 0x06, 0x02, 0x01, r, 0x02, 0x01, s, hash_type]
    }

    #[test]
    fn the_empty_signature_always_passes() {
        assert_eq!(check_signature_encoding(&[], ScriptFlags::from_bits(u32::MAX)), Ok(()));
    }

    #[test]
    fn checks_run_in_core_order() {
        let all = ScriptFlags::DERSIG | ScriptFlags::LOW_S | ScriptFlags::STRICTENC;
        // Not DER: SIG_DER before anything else.
        assert_eq!(check_signature_encoding(&[0x31; 9], all), Err(ScriptError::SigDer));
        // DER, low S, undefined hash type: SIG_HASHTYPE, and only under STRICTENC.
        assert_eq!(check_signature_encoding(&sig(1, 1, 0x04), all), Err(ScriptError::SigHashtype));
        assert_eq!(check_signature_encoding(&sig(1, 1, 0x04), ScriptFlags::DERSIG), Ok(()));
        assert_eq!(check_signature_encoding(&sig(1, 1, 0x81), all), Ok(()));
        // Without any of the three flags nothing is checked.
        assert_eq!(check_signature_encoding(&[0x31; 3], ScriptFlags::NULLFAIL), Ok(()));
    }

    #[test]
    fn der_shape_rules() {
        assert!(is_valid_signature_encoding(&sig(1, 1, 1)));
        // Negative R, padded R, padded S.
        assert!(!is_valid_signature_encoding(&sig(0x80, 1, 1)));
        let padded_r = [0x30, 0x07, 0x02, 0x02, 0x00, 0x01, 0x02, 0x01, 0x01, 0x01];
        assert!(!is_valid_signature_encoding(&padded_r));
        let needed_pad = [0x30, 0x07, 0x02, 0x02, 0x00, 0x80, 0x02, 0x01, 0x01, 0x01];
        assert!(is_valid_signature_encoding(&needed_pad));
        // Total length off by one.
        let mut wrong_len = sig(1, 1, 1);
        wrong_len[1] = 7;
        assert!(!is_valid_signature_encoding(&wrong_len));
        // Size limits.
        assert!(!is_valid_signature_encoding(&sig(1, 1, 1)[..8]));
        assert!(!is_valid_signature_encoding(&[0x30; 74]));
    }

    #[test]
    fn pubkey_encoding_rejects_hybrid_and_bad_lengths_under_strictenc() {
        let strict = ScriptFlags::STRICTENC;
        assert_eq!(check_pubkey_encoding(&[0x02; 33], strict), Ok(()));
        let mut uncompressed = vec![0x04; 65];
        assert_eq!(check_pubkey_encoding(&uncompressed, strict), Ok(()));
        uncompressed[0] = 0x06;
        assert_eq!(check_pubkey_encoding(&uncompressed, strict), Err(ScriptError::PubkeyType));
        assert_eq!(check_pubkey_encoding(&uncompressed, ScriptFlags::NONE), Ok(()));
        assert_eq!(check_pubkey_encoding(&[0x02; 34], strict), Err(ScriptError::PubkeyType));
        assert_eq!(check_pubkey_encoding(&[0x04; 33], strict), Err(ScriptError::PubkeyType));
        assert_eq!(check_pubkey_encoding(&[], strict), Err(ScriptError::PubkeyType));
    }
}
