//! BIP324's cipher suite, ported from Dash Core: `BIP324Cipher` (`bip324.cpp`), `FSChaCha20`
//! (`crypto/chacha20.cpp`), `FSChaCha20Poly1305` (`crypto/chacha20poly1305.cpp`) and
//! `CHKDF_HMAC_SHA256_L32` (`crypto/hkdf_sha256_32.cpp`).
//!
//! The primitives are RustCrypto's ChaCha20 and RFC 8439 ChaCha20-Poly1305; Core's AEAD is the
//! same construction (`AEADChaCha20Poly1305`), with the 96-bit nonce written as Core's
//! `Nonce96` pair: a little-endian `u32` then a little-endian `u64`.

use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use chacha20poly1305::aead::AeadInOut;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Tag};
use dash_p2p_core::Magic;
use dash_sha256::Sha256;
use secp256k1::ellswift::{ElligatorSwift, Party};
use zeroize::{Zeroize, Zeroizing};

pub use secp256k1::SecretKey;

/// Bytes of an ElligatorSwift public key, Core's `EllSwiftPubKey::size()`.
pub const ELLSWIFT_KEY_LEN: usize = 64;
/// Bytes of the session ID, Core's `BIP324Cipher::SESSION_ID_LEN`.
pub const SESSION_ID_LEN: usize = 32;
/// Bytes of a garbage terminator, Core's `BIP324Cipher::GARBAGE_TERMINATOR_LEN`.
pub const GARBAGE_TERMINATOR_LEN: usize = 16;
/// Packets per key of each cipher, Core's `BIP324Cipher::REKEY_INTERVAL`.
pub const REKEY_INTERVAL: u32 = 224;
/// Bytes of the encrypted length descriptor, Core's `BIP324Cipher::LENGTH_LEN`.
pub const LENGTH_LEN: usize = 3;
/// Bytes of the packet header (the ignore bit), Core's `BIP324Cipher::HEADER_LEN`.
pub const HEADER_LEN: usize = 1;
/// Bytes of the Poly1305 tag, Core's `FSChaCha20Poly1305::EXPANSION`.
pub const TAG_LEN: usize = 16;
/// Bytes a packet adds to its contents, Core's `BIP324Cipher::EXPANSION` (20).
pub const EXPANSION: usize = LENGTH_LEN + HEADER_LEN + TAG_LEN;
/// The header bit that marks a decoy packet, Core's `BIP324Cipher::IGNORE_BIT`.
pub const IGNORE_BIT: u8 = 0x80;

const KEY_LEN: usize = 32;
const BLOCK_LEN: u64 = 64;

/// Core's `Nonce96 {first, second}` as RFC 8439's 12 nonce bytes.
fn nonce96(first: u32, second: u64) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(&first.to_le_bytes());
    nonce[4..].copy_from_slice(&second.to_le_bytes());
    nonce
}

/// HMAC-SHA256 over the concatenation of `parts` (Core's `CHMAC_SHA256`).
fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut rkey = Zeroizing::new([0u8; 64]);
    if key.len() <= 64 {
        rkey[..key.len()].copy_from_slice(key);
    } else {
        rkey[..32].copy_from_slice(&dash_sha256::sha256(key));
    }
    let mut pad = Zeroizing::new([0u8; 64]);
    for (p, k) in pad.iter_mut().zip(rkey.iter()) {
        *p = k ^ 0x5c;
    }
    let mut outer = Sha256::new();
    outer.update(&pad[..]);
    for (p, k) in pad.iter_mut().zip(rkey.iter()) {
        *p = k ^ 0x36;
    }
    let mut inner = Sha256::new();
    inner.update(&pad[..]);
    for part in parts {
        inner.update(part);
    }
    let inner = Zeroizing::new(inner.finalize());
    outer.update(&inner[..]);
    outer.finalize()
}

/// HKDF-HMAC-SHA256 with 32-byte outputs, Core's `CHKDF_HMAC_SHA256_L32`.
struct HkdfL32 {
    prk: Zeroizing<[u8; 32]>,
}

impl HkdfL32 {
    fn new(ikm: &[u8], salt: &[u8]) -> Self {
        Self {
            prk: Zeroizing::new(hmac_sha256(salt, &[ikm])),
        }
    }

    fn expand32(&self, info: &str) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(hmac_sha256(&self.prk[..], &[info.as_bytes(), &[1]]))
    }
}

/// ChaCha20 with forward secrecy, Core's `FSChaCha20`: one continuous keystream per key, and a new
/// key drawn from that stream after every `rekey_interval` calls to [`Self::crypt`].
pub struct FsChaCha20 {
    stream: ChaCha20,
    rekey_interval: u32,
    chunk_counter: u32,
    rekey_counter: u64,
}

impl FsChaCha20 {
    /// A cipher at the start of `key`'s stream (nonce `{0, 0}`).
    pub fn new(key: &[u8; 32], rekey_interval: u32) -> Self {
        Self {
            stream: ChaCha20::new(key.into(), &nonce96(0, 0).into()),
            rekey_interval,
            chunk_counter: 0,
            rekey_counter: 0,
        }
    }

    /// Encrypts or decrypts `data` in place with the next keystream bytes, then rekeys if this was
    /// the interval's last call.
    pub fn crypt(&mut self, data: &mut [u8]) {
        self.stream.apply_keystream(data);
        self.chunk_counter += 1;
        if self.chunk_counter == self.rekey_interval {
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            self.stream.apply_keystream(&mut key[..]);
            self.rekey_counter += 1;
            self.stream = ChaCha20::new((&*key).into(), &nonce96(0, self.rekey_counter).into());
            self.chunk_counter = 0;
        }
    }
}

/// RFC 8439 ChaCha20-Poly1305 with forward secrecy, Core's `FSChaCha20Poly1305`: the nonce is the
/// packet counter and the rekey counter, and after `rekey_interval` packets the key becomes the
/// first 32 bytes of block 1 of the stream at nonce `{0xffffffff, rekey_counter}`.
pub struct FsChaCha20Poly1305 {
    key: Zeroizing<[u8; KEY_LEN]>,
    aead: ChaCha20Poly1305,
    rekey_interval: u32,
    packet_counter: u32,
    rekey_counter: u64,
}

impl FsChaCha20Poly1305 {
    /// A cipher whose first packet uses `key`.
    pub fn new(key: &[u8; 32], rekey_interval: u32) -> Self {
        Self {
            key: Zeroizing::new(*key),
            aead: ChaCha20Poly1305::new(key.into()),
            rekey_interval,
            packet_counter: 0,
            rekey_counter: 0,
        }
    }

    fn nonce(&self) -> [u8; 12] {
        nonce96(self.packet_counter, self.rekey_counter)
    }

    fn next_packet(&mut self) {
        self.packet_counter += 1;
        if self.packet_counter == self.rekey_interval {
            let mut stream =
                ChaCha20::new((&*self.key).into(), &nonce96(u32::MAX, self.rekey_counter).into());
            stream.seek(BLOCK_LEN);
            let mut key = Zeroizing::new([0u8; KEY_LEN]);
            stream.apply_keystream(&mut key[..]);
            self.aead = ChaCha20Poly1305::new((&*key).into());
            self.key = key;
            self.packet_counter = 0;
            self.rekey_counter += 1;
        }
    }

    /// Encrypts `plain` in place under `aad` and returns the tag.
    pub fn encrypt(&mut self, plain: &mut [u8], aad: &[u8]) -> [u8; TAG_LEN] {
        let tag = self
            .aead
            .encrypt_inout_detached(&self.nonce().into(), aad, plain.into())
            .unwrap_or_else(|_| unreachable!("BIP324 packets are far below RFC 8439's limit"));
        self.next_packet();
        tag.into()
    }

    /// Checks `tag` over `cipher` and `aad`, then decrypts `cipher` in place. Returns false, with
    /// `cipher` unchanged, when the tag does not match. The packet counts either way, as in Core.
    pub fn decrypt(&mut self, cipher: &mut [u8], aad: &[u8], tag: &[u8; TAG_LEN]) -> bool {
        let ok = self
            .aead
            .decrypt_inout_detached(&self.nonce().into(), aad, cipher.into(), &Tag::from(*tag))
            .is_ok();
        self.next_packet();
        ok
    }
}

/// The four ciphers, garbage terminators and session ID of one side of a BIP324 connection,
/// Core's `BIP324Cipher`.
pub struct Bip324Cipher {
    magic: Magic,
    /// Our private key until [`Self::initialize`]; wiped on drop.
    key: Option<Zeroizing<[u8; 32]>>,
    our_pubkey: [u8; ELLSWIFT_KEY_LEN],
    keys: Option<Keys>,
}

struct Keys {
    send_l: FsChaCha20,
    send_p: FsChaCha20Poly1305,
    recv_l: FsChaCha20,
    recv_p: FsChaCha20Poly1305,
    send_garbage_terminator: [u8; GARBAGE_TERMINATOR_LEN],
    recv_garbage_terminator: [u8; GARBAGE_TERMINATOR_LEN],
    session_id: [u8; SESSION_ID_LEN],
}

impl Bip324Cipher {
    /// A cipher for the network with message start `magic`, with our key `key` and its
    /// ElligatorSwift encoding drawn with the 32 bytes of entropy `ent32` (Core's
    /// `EllSwiftCreate`). The copies of `key` this crate keeps are wiped once used (best effort:
    /// secp256k1 takes keys by value, so temporaries may remain on the stack).
    pub fn new(magic: Magic, mut key: SecretKey, ent32: [u8; 32]) -> Self {
        let our_pubkey = ElligatorSwift::from_seckey(key, Some(ent32)).to_array();
        let cipher = Self::with_pubkey(magic, key, our_pubkey);
        key.non_secure_erase();
        cipher
    }

    /// A cipher with a given ElligatorSwift encoding of `key`'s public key (Core's test
    /// constructor; the BIP324 vectors fix the encoding).
    pub fn with_pubkey(
        magic: Magic,
        mut key: SecretKey,
        our_pubkey: [u8; ELLSWIFT_KEY_LEN],
    ) -> Self {
        let bytes = Zeroizing::new(key.to_secret_bytes());
        key.non_secure_erase();
        Self {
            magic,
            key: Some(bytes),
            our_pubkey,
            keys: None,
        }
    }

    /// Our ElligatorSwift public key.
    pub fn our_pubkey(&self) -> &[u8; ELLSWIFT_KEY_LEN] {
        &self.our_pubkey
    }

    /// Whether [`Self::initialize`] has run.
    pub fn is_initialized(&self) -> bool {
        self.keys.is_some()
    }

    /// Derives the ciphers from the peer's key, Core's `BIP324Cipher::Initialize`. The salt is
    /// `"bitcoin_v2_shared_secret"` followed by the network's message start, so a Dash network's
    /// magic yields Dash's keys. `self_decrypt` swaps the directions (Core's test hook). Our
    /// private key is wiped afterwards.
    ///
    /// # Panics
    ///
    /// If called twice.
    pub fn initialize(
        &mut self,
        their_pubkey: &[u8; ELLSWIFT_KEY_LEN],
        initiator: bool,
        self_decrypt: bool,
    ) {
        let bytes = self.key.take().expect("BIP324Cipher::initialize runs once");
        let mut key = SecretKey::from_secret_bytes(*bytes).expect("stored from a valid key");
        let ours = ElligatorSwift::from_array(self.our_pubkey);
        let theirs = ElligatorSwift::from_array(*their_pubkey);
        let (a, b, party) = if initiator {
            (ours, theirs, Party::Initiator)
        } else {
            (theirs, ours, Party::Responder)
        };
        let ecdh =
            Zeroizing::new(ElligatorSwift::shared_secret(a, b, key, party).to_secret_bytes());
        key.non_secure_erase();

        let mut salt = b"bitcoin_v2_shared_secret".to_vec();
        salt.extend_from_slice(self.magic.as_bytes());
        let hkdf = HkdfL32::new(&ecdh[..], &salt);

        let side = initiator != self_decrypt;
        let initiator_l = FsChaCha20::new(&hkdf.expand32("initiator_L"), REKEY_INTERVAL);
        let initiator_p = FsChaCha20Poly1305::new(&hkdf.expand32("initiator_P"), REKEY_INTERVAL);
        let responder_l = FsChaCha20::new(&hkdf.expand32("responder_L"), REKEY_INTERVAL);
        let responder_p = FsChaCha20Poly1305::new(&hkdf.expand32("responder_P"), REKEY_INTERVAL);
        let (send_l, send_p, recv_l, recv_p) = if side {
            (initiator_l, initiator_p, responder_l, responder_p)
        } else {
            (responder_l, responder_p, initiator_l, initiator_p)
        };

        let terminators = hkdf.expand32("garbage_terminators");
        let mut first = [0u8; GARBAGE_TERMINATOR_LEN];
        let mut last = [0u8; GARBAGE_TERMINATOR_LEN];
        first.copy_from_slice(&terminators[..GARBAGE_TERMINATOR_LEN]);
        last.copy_from_slice(&terminators[32 - GARBAGE_TERMINATOR_LEN..]);
        let (send_garbage_terminator, recv_garbage_terminator) = if initiator {
            (first, last)
        } else {
            (last, first)
        };

        self.keys = Some(Keys {
            send_l,
            send_p,
            recv_l,
            recv_p,
            send_garbage_terminator,
            recv_garbage_terminator,
            session_id: *hkdf.expand32("session_id"),
        });
    }

    fn keys(&self) -> &Keys {
        self.keys.as_ref().expect("BIP324 cipher not initialized")
    }

    fn keys_mut(&mut self) -> &mut Keys {
        self.keys.as_mut().expect("BIP324 cipher not initialized")
    }

    /// The session ID (for out-of-band verification and `getpeerinfo`).
    pub fn session_id(&self) -> &[u8; SESSION_ID_LEN] {
        &self.keys().session_id
    }

    /// The terminator we send after our garbage.
    pub fn send_garbage_terminator(&self) -> &[u8; GARBAGE_TERMINATOR_LEN] {
        &self.keys().send_garbage_terminator
    }

    /// The terminator that ends the peer's garbage.
    pub fn recv_garbage_terminator(&self) -> &[u8; GARBAGE_TERMINATOR_LEN] {
        &self.keys().recv_garbage_terminator
    }

    /// Writes the packet for `contents` into `output` (exactly `contents.len() + EXPANSION`
    /// bytes): the encrypted length, then the encrypted header and contents, then the tag. Core's
    /// `BIP324Cipher::Encrypt`.
    ///
    /// # Panics
    ///
    /// If `output` has the wrong length or `contents` is 2^24 bytes or longer.
    pub fn encrypt(&mut self, contents: &[u8], aad: &[u8], ignore: bool, output: &mut [u8]) {
        assert_eq!(output.len(), contents.len() + EXPANSION);
        output[LENGTH_LEN + HEADER_LEN..LENGTH_LEN + HEADER_LEN + contents.len()]
            .copy_from_slice(contents);
        self.encrypt_in_place(output, aad, ignore);
    }

    /// [`Self::encrypt`] for contents already in place: `packet` is the whole packet, with the
    /// contents at `LENGTH_LEN + HEADER_LEN` and room for the length, header and tag around them.
    ///
    /// # Panics
    ///
    /// If `packet` is shorter than [`EXPANSION`] or its contents are 2^24 bytes or longer.
    ///
    /// # Examples
    ///
    /// ```
    /// # use dash_p2p_transport::Magic;
    /// # use dash_p2p_transport::cipher::{Bip324Cipher, EXPANSION, SecretKey};
    /// # let magic = Magic::from_bytes([0xfc, 0xc1, 0xb7, 0xdc]);
    /// # let key = |n| SecretKey::from_secret_bytes([n; 32]).unwrap();
    /// let mut a = Bip324Cipher::new(magic, key(1), [2; 32]);
    /// let b = Bip324Cipher::new(magic, key(3), [4; 32]);
    /// a.initialize(b.our_pubkey(), true, false);
    /// let mut packet = [0; 4 + EXPANSION];
    /// a.encrypt_in_place(&mut packet, &[], false);
    /// ```
    #[cfg_attr(
        not(feature = "test-peer"),
        doc = r#"
Without the `test-peer` feature, a sender can't choose the header's reserved bits: this is the
example above with the test hook instead, and doesn't build.

```compile_fail
# use dash_p2p_transport::Magic;
# use dash_p2p_transport::cipher::{Bip324Cipher, EXPANSION, SecretKey};
# let magic = Magic::from_bytes([0xfc, 0xc1, 0xb7, 0xdc]);
# let key = |n| SecretKey::from_secret_bytes([n; 32]).unwrap();
let mut a = Bip324Cipher::new(magic, key(1), [2; 32]);
let b = Bip324Cipher::new(magic, key(3), [4; 32]);
a.initialize(b.our_pubkey(), true, false);
let mut packet = [0; 4 + EXPANSION];
a.encrypt_with_header(&mut packet, &[], 0x7f);
```"#
    )]
    pub fn encrypt_in_place(&mut self, packet: &mut [u8], aad: &[u8], ignore: bool) {
        let header = if ignore {
            IGNORE_BIT
        } else {
            0
        };
        self.encrypt_packet(packet, aad, header);
    }

    /// [`Self::encrypt_in_place`] with the whole header byte given, so that a test or fuzzing peer
    /// can set the bits BIP324 reserves (receivers ignore all but [`IGNORE_BIT`]). Built only
    /// with the `test-peer` feature (and for this crate's unit tests): senders keep the reserved
    /// bits zero.
    ///
    /// # Panics
    ///
    /// If `packet` is shorter than [`EXPANSION`] or its contents are 2^24 bytes or longer.
    #[cfg(any(test, feature = "test-peer"))]
    pub fn encrypt_with_header(&mut self, packet: &mut [u8], aad: &[u8], header: u8) {
        self.encrypt_packet(packet, aad, header);
    }

    fn encrypt_packet(&mut self, packet: &mut [u8], aad: &[u8], header: u8) {
        let contents_len =
            packet.len().checked_sub(EXPANSION).expect("packet shorter than EXPANSION");
        assert!(contents_len < 1 << 24);
        let keys = self.keys_mut();
        let len = contents_len as u32;
        packet[..LENGTH_LEN].copy_from_slice(&len.to_le_bytes()[..LENGTH_LEN]);
        keys.send_l.crypt(&mut packet[..LENGTH_LEN]);
        let body_end = LENGTH_LEN + HEADER_LEN + contents_len;
        packet[LENGTH_LEN] = header;
        let tag = keys.send_p.encrypt(&mut packet[LENGTH_LEN..body_end], aad);
        packet[body_end..].copy_from_slice(&tag);
    }

    /// Decrypts a packet's length descriptor, Core's `BIP324Cipher::DecryptLength`.
    pub fn decrypt_length(&mut self, input: &[u8; LENGTH_LEN]) -> u32 {
        let mut buf = *input;
        self.keys_mut().recv_l.crypt(&mut buf);
        u32::from_le_bytes([buf[0], buf[1], buf[2], 0])
    }

    /// Checks and decrypts a packet body in place (`input` is everything after the length
    /// descriptor: header, contents, tag). On success returns the ignore bit; the decrypted
    /// contents are `input[HEADER_LEN..input.len() - TAG_LEN]`. Core's `BIP324Cipher::Decrypt`.
    ///
    /// # Panics
    ///
    /// If `input` is shorter than the header and tag.
    pub fn decrypt(&mut self, input: &mut [u8], aad: &[u8]) -> Option<bool> {
        let body_len = input.len().checked_sub(TAG_LEN).expect("packet shorter than its tag");
        assert!(body_len >= HEADER_LEN);
        let (body, tag) = input.split_at_mut(body_len);
        let tag: [u8; TAG_LEN] = (&*tag).try_into().unwrap_or_else(|_| unreachable!());
        if !self.keys_mut().recv_p.decrypt(body, aad, &tag) {
            return None;
        }
        Some(body[0] & IGNORE_BIT == IGNORE_BIT)
    }
}

impl Drop for Bip324Cipher {
    fn drop(&mut self) {
        if let Some(keys) = self.keys.as_mut() {
            keys.session_id.zeroize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        // RFC 4231 test case 2 (Core's crypto_tests use the same vectors).
        let mac = hmac_sha256(b"Jefe", &[b"what do ya want ", b"for nothing?"]);
        assert_eq!(mac, hex32("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"));
    }

    #[test]
    fn hkdf_matches_rfc5869_case_1_first_block() {
        // RFC 5869 test case 1, first 32 bytes of the OKM (Core's crypto_tests `TestHKDF_SHA256_32`).
        let ikm = [0x0bu8; 22];
        let salt: Vec<u8> = (0u8..=0x0c).collect();
        let prk = hmac_sha256(&salt, &[&ikm]);
        let info: Vec<u8> = (0xf0u8..=0xf9).collect();
        let okm = hmac_sha256(&prk, &[&info, &[1]]);
        assert_eq!(okm, hex32("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf"));
    }

    #[test]
    fn private_key_is_dropped_by_initialize() {
        let magic = Magic::from_bytes([0xfc, 0xc1, 0xb7, 0xdc]);
        let key = |n| SecretKey::from_secret_bytes([n; 32]).unwrap();
        let mut a = Bip324Cipher::new(magic, key(1), [2; 32]);
        let b = Bip324Cipher::new(magic, key(3), [4; 32]);
        assert!(a.key.is_some());
        a.initialize(b.our_pubkey(), true, false);
        assert!(a.key.is_none() && a.is_initialized());
    }

    fn hex32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, o) in out.iter_mut().enumerate() {
            *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }
}
