//! Secret byte buffers.

use core::fmt;

use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Bytes that must never be logged, displayed or left in memory: wiped on drop,
/// compared in constant time, and `Debug` shows only the length.
///
/// There is deliberately no `Display`, `Deref`, `AsRef` or conversion back into a
/// `Vec<u8>`; read the bytes with [`expose`](Self::expose). There is no serde
/// support either.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretBytes(Vec<u8>);

/// A BIP39 mnemonic passphrase (the "25th word"), as raw bytes.
pub type Passphrase = SecretBytes;

impl SecretBytes {
    /// Takes ownership of `bytes`; they are wiped when this value is dropped.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Copies `bytes`. The caller still owns (and must wipe) the source.
    pub fn from_slice(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }

    /// The secret bytes.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// Number of secret bytes (not secret itself).
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no secret bytes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Constant time in the contents; the lengths are not secret.
impl PartialEq for SecretBytes {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_slice().ct_eq(other.0.as_slice()).into()
    }
}

impl Eq for SecretBytes {}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes({} bytes)", self.0.len())
    }
}

/// Same layout as `Vec<u8>`: a length prefix, then the bytes.
#[cfg(feature = "bincode")]
impl bincode::Encode for SecretBytes {
    fn encode<E: bincode::enc::Encoder>(
        &self,
        encoder: &mut E,
    ) -> Result<(), bincode::error::EncodeError> {
        bincode::Encode::encode(self.0.as_slice(), encoder)
    }
}

/// Reads straight into a `SecretBytes`, so the bytes are wiped even when the read
/// fails part-way (decoding a `Vec<u8>` first would leave an unwiped copy).
#[cfg(feature = "bincode")]
impl<C> bincode::Decode<C> for SecretBytes {
    fn decode<D: bincode::de::Decoder<Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        let len: u64 = bincode::Decode::decode(decoder)?;
        let len = usize::try_from(len)
            .map_err(|_| bincode::error::DecodeError::OutsideUsizeRange(len))?;
        decoder.claim_container_read::<u8>(len)?;
        let mut secret = SecretBytes(vec![0u8; len]);
        bincode::de::read::Reader::read(decoder.reader(), &mut secret.0)?;
        Ok(secret)
    }
}

#[cfg(feature = "bincode")]
impl<'de, C> bincode::BorrowDecode<'de, C> for SecretBytes {
    fn borrow_decode<D: bincode::de::BorrowDecoder<'de, Context = C>>(
        decoder: &mut D,
    ) -> Result<Self, bincode::error::DecodeError> {
        bincode::Decode::decode(decoder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_shows_only_the_length() {
        let secret = SecretBytes::from_slice(b"hunter2");
        assert_eq!(format!("{secret:?}"), "SecretBytes(7 bytes)");
        assert_eq!(format!("{secret:#?}"), "SecretBytes(7 bytes)");
        assert!(!format!("{:?}", Some(&secret)).contains("hunter2"));
    }

    #[test]
    fn equality_and_accessors() {
        let a = SecretBytes::new(b"abc".to_vec());
        assert_eq!(a, SecretBytes::from_slice(b"abc"));
        assert_ne!(a, SecretBytes::from_slice(b"abd"));
        assert_ne!(a, SecretBytes::from_slice(b"abcd"));
        assert_eq!(a.expose(), b"abc");
        assert_eq!(a.len(), 3);
        assert!(!a.is_empty());
        assert!(SecretBytes::new(Vec::new()).is_empty());
    }

    #[test]
    fn zeroize_clears_the_bytes() {
        let mut secret = SecretBytes::from_slice(b"hunter2");
        secret.zeroize();
        assert!(secret.is_empty());
    }

    #[cfg(feature = "bincode")]
    #[test]
    fn codec_matches_vec_layout_and_round_trips() {
        let config = bincode::config::standard();
        for bytes in [Vec::new(), b"x".to_vec(), vec![0xA5; 300]] {
            let secret = SecretBytes::new(bytes.clone());
            let encoded = bincode::encode_to_vec(&secret, config).unwrap();
            assert_eq!(encoded, bincode::encode_to_vec(&bytes, config).unwrap());

            let (decoded, read): (SecretBytes, usize) =
                bincode::decode_from_slice(&encoded, config).unwrap();
            assert_eq!((decoded.expose(), read), (bytes.as_slice(), encoded.len()));

            let (borrowed, _): (SecretBytes, usize) =
                bincode::borrow_decode_from_slice(&encoded, config).unwrap();
            assert_eq!(borrowed, secret);
        }
    }

    #[cfg(feature = "bincode")]
    #[test]
    fn truncated_input_fails() {
        let config = bincode::config::standard();
        let encoded = bincode::encode_to_vec(SecretBytes::from_slice(b"hunter2"), config).unwrap();
        for cut in 0..encoded.len() {
            assert!(
                bincode::decode_from_slice::<SecretBytes, _>(&encoded[..cut], config).is_err(),
                "cut at {cut}"
            );
        }
    }
}
