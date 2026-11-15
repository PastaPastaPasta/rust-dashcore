//! Known-answer tests from Dash Core v24.0.0-rc.3's own unit tests (see the provenance note in
//! `data/core_cipher_vectors.rs`): BIP324 packets under Dash's mainnet salt, and the
//! forward-secure ChaCha20 and ChaCha20-Poly1305 ciphers.

mod common;

#[path = "data/core_cipher_vectors.rs"]
mod vectors;

use common::mainnet;
use dash_p2p_transport::cipher::{
    Bip324Cipher, EXPANSION, FsChaCha20, FsChaCha20Poly1305, LENGTH_LEN, REKEY_INTERVAL, SecretKey,
    TAG_LEN,
};
use vectors::{BIP324_PACKET, FS_CHACHA20, FS_CHACHA20_POLY1305, PacketVector};

fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2));
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn arr<const N: usize>(s: &str) -> [u8; N] {
    unhex(s).try_into().unwrap()
}

fn cipher(v: &PacketVector, initiating: bool, self_decrypt: bool) -> Bip324Cipher {
    let key = SecretKey::from_secret_bytes(arr(v.in_priv_ours)).unwrap();
    let ours = arr(v.in_ellswift_ours);
    let mut c = Bip324Cipher::with_pubkey(mainnet(), key, ours);
    assert!(!c.is_initialized());
    assert_eq!(c.our_pubkey(), &ours);
    c.initialize(&arr(v.in_ellswift_theirs), initiating, self_decrypt);
    assert!(c.is_initialized());
    c
}

/// Feeds `dummy` (a packet our side encrypted) to a self-decrypting cipher to advance it.
fn skip(c: &mut Bip324Cipher, dummy: &[u8]) {
    c.decrypt_length(dummy[..LENGTH_LEN].try_into().unwrap());
    c.decrypt(&mut dummy[LENGTH_LEN..].to_vec(), &[]);
}

/// Core's `TestBIP324PacketVector`, with its random damage replaced by fixed positions.
#[test]
fn bip324_packet_vectors() {
    for (n, v) in BIP324_PACKET.iter().enumerate() {
        let mut c = cipher(v, v.in_initiating, false);
        assert_eq!(c.session_id()[..], unhex(v.out_session_id), "vector {n}");
        assert_eq!(c.send_garbage_terminator()[..], unhex(v.mid_send_garbage_terminator));
        assert_eq!(c.recv_garbage_terminator()[..], unhex(v.mid_recv_garbage_terminator));

        let dummies: Vec<Vec<u8>> = (0..v.in_idx)
            .map(|_| {
                let mut d = vec![0; EXPANSION];
                c.encrypt(&[], &[], true, &mut d);
                d
            })
            .collect();
        let contents = unhex(v.in_contents).repeat(v.in_multiply as usize);
        let aad = unhex(v.in_aad);
        let mut ciphertext = vec![0; contents.len() + EXPANSION];
        c.encrypt(&contents, &aad, v.in_ignore, &mut ciphertext);
        if v.out_ciphertext.is_empty() {
            assert!(ciphertext.ends_with(&unhex(v.out_ciphertext_endswith)), "vector {n}");
        } else {
            assert_eq!(ciphertext, unhex(v.out_ciphertext), "vector {n}");
        }

        // error: 0 none, 1 wrong side, 2..=9 a bit flip in the ciphertext, 10 a bit flip in the
        // aad, 11 an extra zero byte of aad, 12 the wrong packet index.
        for error in 0..=12u32 {
            let mut d = cipher(v, (error == 1) ^ v.in_initiating, true);
            assert_eq!(d.session_id()[..] == unhex(v.out_session_id)[..], error != 1);
            assert_eq!(
                d.send_garbage_terminator()[..] == unhex(v.mid_send_garbage_terminator)[..],
                error != 1
            );
            if v.in_idx == 0 && error == 12 {
                continue;
            }
            let dec_idx = if error == 12 {
                v.in_idx ^ (1 << (n % 16))
            } else {
                v.in_idx
            };
            for i in 0..dec_idx {
                skip(
                    &mut d,
                    &dummies[if i < v.in_idx {
                        i as usize
                    } else {
                        0
                    }],
                );
            }
            let mut to_decrypt = ciphertext.clone();
            if (2..=9).contains(&error) {
                let pos = (error as usize * 7919 + n) % to_decrypt.len();
                to_decrypt[pos] ^= 1 << (error - 2);
            }
            let len = d.decrypt_length(to_decrypt[..LENGTH_LEN].try_into().unwrap()) as usize;
            to_decrypt.resize(len + EXPANSION, 0);
            let mut dec_aad = aad.clone();
            if error == 10 {
                if aad.is_empty() {
                    continue;
                }
                let pos = n % dec_aad.len();
                dec_aad[pos] ^= 1 << (n % 8);
            }
            if error == 11 {
                dec_aad.push(0);
            }
            let res = d.decrypt(&mut to_decrypt[LENGTH_LEN..], &dec_aad);
            assert_eq!(res.is_some(), error == 0, "vector {n} error {error}");
            if let Some(ignore) = res {
                assert_eq!(&to_decrypt[LENGTH_LEN + 1..len + LENGTH_LEN + 1], &contents[..]);
                assert_eq!(ignore, v.in_ignore);
            }
        }
    }
}

/// Core's `TestFSChaCha20`: the stream matches plain ChaCha20 (zero nonce) for `rekey_interval`
/// crypts, and the first crypt after rotation gives the recorded ciphertext.
#[test]
fn fschacha20_vectors() {
    use chacha20::ChaCha20;
    use chacha20::cipher::{KeyIvInit, StreamCipher};
    for &(plain, key, interval, after) in FS_CHACHA20 {
        let (plain, key) = (unhex(plain), arr(key));
        let mut fs = FsChaCha20::new(&key, interval);
        let mut c20 = ChaCha20::new(&key.into(), &[0; 12].into());
        let mut out = plain.clone();
        for _ in 0..interval {
            let mut want = plain.clone();
            c20.apply_keystream(&mut want);
            out.copy_from_slice(&plain);
            fs.crypt(&mut out);
            assert_eq!(out, want);
        }
        out.copy_from_slice(&plain);
        fs.crypt(&mut out);
        assert_eq!(out, unhex(after));
    }
}

/// Core's `TestFSChaCha20Poly1305`: seek with `msg_idx` empty packets, then encrypt and decrypt.
#[test]
fn fschacha20poly1305_vectors() {
    for &(plain, aad, key, msg_idx, expected) in FS_CHACHA20_POLY1305 {
        let (plain, aad, key, expected) = (unhex(plain), unhex(aad), arr(key), unhex(expected));
        let mut enc = FsChaCha20Poly1305::new(&key, REKEY_INTERVAL);
        let mut dec = FsChaCha20Poly1305::new(&key, REKEY_INTERVAL);
        for _ in 0..msg_idx {
            let tag = enc.encrypt(&mut [], &[]);
            assert!(dec.decrypt(&mut [], &[], &tag));
        }
        let mut buf = plain.clone();
        let tag = enc.encrypt(&mut buf, &aad);
        buf.extend_from_slice(&tag);
        assert_eq!(buf, expected);
        let (body, tag) = buf.split_at_mut(plain.len());
        assert!(dec.decrypt(body, &aad, &<[u8; TAG_LEN]>::try_from(&*tag).unwrap()));
        assert_eq!(body, &plain[..]);
    }
}
