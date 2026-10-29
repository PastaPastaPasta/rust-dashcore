use secp256k1::SecretKey;

use super::*;
use crate::descriptor::tests::{mainnet, regtest};
use crate::key_io::decode_secret;

// Core: src/test/util_tests.cpp:1724-1758
#[test]
fn core_message_sign_vector() {
    let secret = SecretKey::from_secret_bytes([
        0xD9, 0x7F, 0x51, 0x08, 0xF1, 0x1C, 0xDA, 0x6E, 0xEE, 0xBA, 0xAA, 0x42, 0x0F, 0xEF, 0x07,
        0x26, 0xB1, 0xF8, 0x98, 0x06, 0x0B, 0x98, 0x48, 0x9F, 0xA3, 0x09, 0x84, 0x63, 0xC0, 0x03,
        0x28, 0x66,
    ])
    .unwrap();
    let key = Key {
        secret,
        compressed: true,
    };
    assert_eq!(
        message_sign(&key, "Trust no one"),
        "IIOzMDkvw3GtLWXkeEYRRRH53MOLHM44sJ428Nu4NNacTPJTGcKesMJ+3s3OadYK34tpSQIhu922EviNNWTsiQg="
    );
}

// Core: src/test/util_tests.cpp:1760-1803
#[test]
fn core_message_verify_vectors() {
    let main = mainnet();
    let cases = [
        (
            "invalid address",
            "signature should be irrelevant",
            "message too",
            MessageVerification::InvalidAddress,
        ),
        (
            "7iRPy8FEHBzbChrktsG85YbDZ3SuiCxsNq",
            "signature should be irrelevant",
            "message too",
            MessageVerification::AddressNoKey,
        ),
        (
            "XuXS24zs2xP1vPynvNt14kWLa64csH8Mur",
            "invalid signature, not in base64 encoding",
            "message should be irrelevant",
            MessageVerification::MalformedSignature,
        ),
        (
            "XuXS24zs2xP1vPynvNt14kWLa64csH8Mur",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "message should be irrelevant",
            MessageVerification::PubkeyNotRecovered,
        ),
        (
            "XetGnWHsPXV9VSkWzB6Wn2KhZLD24gqa5j",
            "IPojfrX2dfPnH26UegfbGQQLrdK844DlHq5157/P6h57WyuS/Qsl+h/WSVGDF4MUi4rWSswW38oimDYfNNUBUOk=",
            "I never signed this",
            MessageVerification::NotSigned,
        ),
        (
            "XetGnWHsPXV9VSkWzB6Wn2KhZLD24gqa5j",
            "IIOzMDkvw3GtLWXkeEYRRRH53MOLHM44sJ428Nu4NNacTPJTGcKesMJ+3s3OadYK34tpSQIhu922EviNNWTsiQg=",
            "Trust no one",
            MessageVerification::Ok,
        ),
        (
            "XenV77v8QQ3rwjyCb3j2fCwfvgbkC4Vwaj",
            "IOACalWiJTLJ2U7wTKICx5mQ2tOAJ3to8dko8FMb2XSYbmvL+yMWedyfSfaK6V8jwoociyYx628nkXXnrOhPFIY=",
            "Trust me",
            MessageVerification::Ok,
        ),
    ];
    for (address, signature, message, want) in cases {
        assert_eq!(message_verify(address, signature, message, &main), want, "{address} {message}");
    }
}

/// The FT's regtest key signs to dashd's signature, which verifies against its P2PKH address.
// Core: test/functional/rpc_signmessagewithprivkey.py:30-43
#[test]
fn core_signmessagewithprivkey_vector() {
    let reg = regtest();
    let key = decode_secret("cU4zhap7nPJAWeMFu4j6jLrfPmqakDAzy8zn8Fhb3oEevdm4e5Lc", &reg).unwrap();
    let message = "This is just a test message";
    let signature = message_sign(&key, message);
    assert_eq!(
        signature,
        "ICzMhjIUmmXcPWy2+9nw01zQMawo+s5FIy6F7VMkL+TmIeNq1j3AMEuw075os29kh5KYLbysKkDlDD+EAqERBd4="
    );
    let address = "yeMpGzMj3rhtnz48XsfpB8itPHhHtgxLc3";
    assert_eq!(message_verify(address, &signature, message, &reg), MessageVerification::Ok);
    // An uncompressed key's signature names its uncompressed key: another address.
    let uncompressed = Key {
        compressed: false,
        ..key
    };
    let signature = message_sign(&uncompressed, message);
    assert_eq!(message_verify(address, &signature, message, &reg), MessageVerification::NotSigned);
}

/// `MessageHash` serializes the magic and the message as strings.
// Core: src/test/util_tests.cpp:1805-1820
#[test]
fn message_hash_prefixes_the_magic() {
    let mut prefixed = vec![MESSAGE_MAGIC.len() as u8];
    prefixed.extend_from_slice(MESSAGE_MAGIC.as_bytes());
    prefixed.extend_from_slice(b"\x03...");
    assert_eq!(message_hash("..."), sha256d::Hash::hash(&prefixed).to_byte_array());
    // A 253-byte message takes a three-byte length.
    let long = "x".repeat(253);
    let mut prefixed = vec![MESSAGE_MAGIC.len() as u8];
    prefixed.extend_from_slice(MESSAGE_MAGIC.as_bytes());
    prefixed.extend_from_slice(&[0xfd, 253, 0]);
    prefixed.extend_from_slice(long.as_bytes());
    assert_eq!(message_hash(&long), sha256d::Hash::hash(&prefixed).to_byte_array());
}

// Core: src/test/base64_tests.cpp:13-44
#[test]
fn core_base64_vectors() {
    let vectors = [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];
    for (input, output) in vectors {
        assert_eq!(encode_base64(input.as_bytes()), output);
        assert_eq!(decode_base64(output).as_deref(), Some(input.as_bytes()));
    }
    assert_eq!(encode_base64(&[0xff, 0x01, 0xff]), "/wH/");
    assert_eq!(decode_base64("invalid\0"), None);
    assert!(decode_base64("nQB/pZw=").is_some());
    assert_eq!(decode_base64("nQB/pZw=\0invalid"), None);
    assert_eq!(decode_base64("nQB/pZw=invalid\0"), None);
    // Not a multiple of 4; three '='; non-zero padding bits.
    assert_eq!(decode_base64("Zg="), None);
    assert_eq!(decode_base64("Z==="), None);
    assert_eq!(decode_base64("Zh=="), None);
}

/// A header byte below 27 still decodes as Core reads it: `(header - 27) & 3` and bit 2.
// Core: pubkey.cpp CPubKey::RecoverCompact
#[test]
fn recover_compact_reads_any_header_byte() {
    let reg = regtest();
    let key = decode_secret("cU4zhap7nPJAWeMFu4j6jLrfPmqakDAzy8zn8Fhb3oEevdm4e5Lc", &reg).unwrap();
    let mut sig = decode_base64(&message_sign(&key, "m")).unwrap();
    let address = "yeMpGzMj3rhtnz48XsfpB8itPHhHtgxLc3";
    // 27 + recid + 4 → the same recid and flag 32 bytes on, wrapping: still verifies.
    sig[0] = sig[0].wrapping_add(32);
    assert_eq!(message_verify(address, &encode_base64(&sig), "m", &reg), MessageVerification::Ok);
    // 64 bytes: not recovered.
    assert_eq!(
        message_verify(address, &encode_base64(&sig[..64]), "m", &reg),
        MessageVerification::PubkeyNotRecovered
    );
}

/// regtest dashd v24's `signmessagewithprivkey` signatures and its `verifymessage` verdicts on
/// 2,576 altered ones (every header byte, s negated, bit flips), recorded for SP0 (see that
/// directory's PROVENANCE.md): ours signs the same bytes and agrees on every verdict.
// Core: util/message.cpp:27-75, pubkey.cpp CPubKey::RecoverCompact
#[test]
fn signatures_and_verdicts_match_dashd_v24_transcript() {
    use crate::key_io::decode_secret;
    const KEY: &str = "cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";
    const ADDRESS: &str = "yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW";
    let reg = regtest();
    let key = decode_secret(KEY, &reg).unwrap();
    let unhex = |s: &str| crate::request::parse_hex(s).unwrap();
    let (mut signed, mut verdicts, mut valid) = (0, 0, 0);
    let transcript =
        include_str!("../../../dashd-masternode/tests/fixtures/dashd-v24/verifymessage.txt");
    for line in transcript.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let message = format!("{}{}{}", f[0], f[1], f[2]);
        let signature = encode_base64(&unhex(f[3]));
        match f[4] {
            "signed" => {
                assert_eq!(message_sign(&key, &message), signature, "{line}");
                signed += 1;
            }
            verdict => {
                let ours = message_verify(ADDRESS, &signature, &message, &reg);
                assert_eq!(ours == MessageVerification::Ok, verdict == "true", "{line}");
                verdicts += 1;
                valid += usize::from(verdict == "true");
            }
        }
    }
    assert_eq!((signed, verdicts, valid), (8, 2576, 264));
}
