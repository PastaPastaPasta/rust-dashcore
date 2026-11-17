use dash_consensus::hash::sha256d;

#[cfg(feature = "net")]
use std::sync::Arc;

#[cfg(feature = "net")]
use dash_p2p_core::MnAuth;
#[cfg(feature = "net")]
use dash_pkc::bls::{BlsSchemeKind, BlsSigBytes};
#[cfg(feature = "net")]
use dash_primitives::TxHash;
#[cfg(feature = "net")]
use dashd_p2p::processing::seams::{MnAuthChallenge, MnAuthSigner};

#[cfg(feature = "net")]
use crate::active::ActiveMasternode;
#[cfg(feature = "net")]
use crate::tip::TipMnList;

/// Returns Core's MNAUTH sign hash.
// Core: evo/mnauth.cpp:40; bls/bls.h:177-180 at v24.0.0
pub fn sign_hash(pk_ser: &[u8; 48], challenge: &[u8; 32], inbound: bool, version: i32) -> [u8; 32] {
    let mut bytes = [0u8; 48 + 32 + 1 + 4];
    bytes[..48].copy_from_slice(pk_ser);
    bytes[48..80].copy_from_slice(challenge);
    bytes[80] = u8::from(inbound);
    bytes[81..].copy_from_slice(&version.to_le_bytes());
    sha256d(&bytes)
}

#[cfg(feature = "net")]
/// W1 installs `MnAuthSigning::new(active, tip)` as the processor's signer.
pub struct MnAuthSigning {
    active: Arc<ActiveMasternode>,
    tip: TipMnList,
}

#[cfg(feature = "net")]
impl MnAuthSigning {
    pub fn new(active: Arc<ActiveMasternode>, tip: TipMnList) -> Self {
        Self {
            active,
            tip,
        }
    }
}

#[cfg(feature = "net")]
impl MnAuthSigner for MnAuthSigning {
    // Core: evo/mnauth.cpp:19-49 at v24.0.0
    fn mnauth_payload(&self, challenge: &MnAuthChallenge) -> Option<MnAuth> {
        let pro_tx_hash = self.active.st.read().pro_tx_hash?;
        if challenge.received == [0; 32] {
            return None;
        }

        let process = self.tip.load();
        let legacy = process.bls.is_legacy();
        let scheme = BlsSchemeKind::from_legacy(legacy);
        let public_key = self.active.key.pubkey().to_bytes(scheme);
        let hash =
            sign_hash(&public_key, &challenge.received, challenge.inbound, challenge.our_version);
        let signature = self.active.key.sign(&hash, false).to_bytes(scheme);
        Some(MnAuth::new(TxHash::from(pro_tx_hash), BlsSigBytes::from_bytes(signature)))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "net")]
    use std::sync::Arc;

    #[cfg(feature = "net")]
    use dash_args::ArgsManager;
    #[cfg(feature = "net")]
    use dash_consensus::BlsCtx;
    #[cfg(feature = "net")]
    use dash_dml::MnList;
    #[cfg(feature = "net")]
    use dash_pkc::bls::core::{BlsVerdict, verify_insecure};
    #[cfg(feature = "net")]
    use dash_types::codec::BaseCodec;
    #[cfg(feature = "net")]
    use serde_json::Value;

    use super::*;
    #[cfg(feature = "net")]
    use crate::active::key::parse;
    #[cfg(feature = "net")]
    use crate::active::{ActiveMasternode, ActiveState};
    #[cfg(feature = "net")]
    use crate::tip::{TipListRef, TipMnList};

    #[cfg(feature = "net")]
    // Public regtest v0.0.4 network-mn-0 key, also recorded by A1.
    const SECRET: &str = "6fa2f4c370d6603b6e850babbcca53f4f8af3289a20189ba4ff274defebdf1bb";

    #[cfg(feature = "net")]
    fn key() -> crate::active::OperatorKey {
        let args = ArgsManager::new();
        args.force_set_arg("-masternodeblsprivkey", SECRET);
        parse(&args).unwrap().unwrap()
    }

    #[cfg(feature = "net")]
    fn signer(bls: BlsCtx) -> (Arc<ActiveMasternode>, MnAuthSigning) {
        let active = Arc::new(ActiveMasternode::new(key()));
        let tip = TipMnList::fixed(TipListRef {
            list: Arc::new(MnList::default_pre_dip3()),
            bls,
        });
        let signing = MnAuthSigning::new(Arc::clone(&active), tip);
        (active, signing)
    }

    #[test]
    fn sign_hash_serializes_the_challenge_tuple() {
        let public_key = [1; 48];
        let challenge = [2; 32];
        let got = sign_hash(&public_key, &challenge, true, 70242);
        let mut serialized = Vec::new();
        serialized.extend_from_slice(&public_key);
        serialized.extend_from_slice(&challenge);
        serialized.push(1);
        serialized.extend_from_slice(&70242i32.to_le_bytes());
        assert_eq!(got, sha256d(&serialized));
        assert_eq!(
            got,
            [
                0x3e, 0xfc, 0x0a, 0x0f, 0x55, 0xa7, 0x87, 0x98, 0xfb, 0x9e, 0xda, 0x76, 0xa5, 0x65,
                0x1b, 0xdb, 0xff, 0x5c, 0x7d, 0x93, 0xb4, 0x1d, 0x05, 0x63, 0x74, 0x8d, 0x35, 0xde,
                0x5c, 0xc9, 0x6c, 0xbe,
            ]
        );
    }

    #[cfg(feature = "net")]
    #[test]
    fn mnauth_payload_requires_a_protx_and_non_null_challenge() {
        let (active, signing) = signer(BlsCtx::BASIC);
        let challenge = MnAuthChallenge {
            received: [1; 32],
            inbound: false,
            our_version: 70242,
        };
        assert_eq!(signing.mnauth_payload(&challenge), None);
        assert_eq!(
            signing.mnauth_payload(&MnAuthChallenge {
                received: [0; 32],
                ..challenge
            }),
            None
        );
        *active.st.write() = ActiveState {
            pro_tx_hash: Some([7; 32]),
            ..ActiveState::default()
        };
        assert_eq!(
            signing.mnauth_payload(&MnAuthChallenge {
                received: [0; 32],
                ..challenge
            }),
            None
        );
    }

    #[cfg(feature = "net")]
    #[test]
    fn mnauth_payload_uses_the_tip_scheme_for_key_and_signature_serialization() {
        for (bls, scheme) in
            [(BlsCtx::LEGACY, BlsSchemeKind::Legacy), (BlsCtx::BASIC, BlsSchemeKind::Basic)]
        {
            let (active, signing) = signer(bls);
            let public_key = active.pubkey();
            *active.st.write() = ActiveState {
                pro_tx_hash: Some([7; 32]),
                ..ActiveState::default()
            };
            let challenge = MnAuthChallenge {
                received: [3; 32],
                inbound: true,
                our_version: 70240,
            };
            let message = signing.mnauth_payload(&challenge).unwrap();
            assert_eq!(message.pro_reg_tx_hash(), [7; 32].into());
            let signature_bytes = message.sig().to_bytes();
            assert_ne!(signature_bytes, [0; 96]);
            let mut encoded = Vec::new();
            message.encode(&mut encoded);
            assert_eq!(encoded.len(), 128);
            assert_eq!(&encoded[..32], &[7; 32]);
            assert_eq!(&encoded[32..], signature_bytes);
            let public_key_bytes = public_key.to_bytes(scheme);
            let hash = sign_hash(
                &public_key_bytes,
                &challenge.received,
                challenge.inbound,
                challenge.our_version,
            );
            let signature =
                dash_pkc::bls::core::BlsSigValue::from_canonical(&signature_bytes, scheme).unwrap();
            assert_eq!(
                verify_insecure(&signature, &public_key, &hash, BlsSchemeKind::Basic),
                BlsVerdict::Valid
            );
        }
    }

    fn decode_hex(text: &str) -> Vec<u8> {
        assert_eq!(text.len() % 2, 0);
        text.as_bytes()
            .chunks(2)
            .map(|pair| {
                assert_eq!(pair.len(), 2);
                let digit = |b: u8| match b {
                    b'0'..=b'9' => b - b'0',
                    b'a'..=b'f' => b - b'a' + 10,
                    b'A'..=b'F' => b - b'A' + 10,
                    _ => panic!("invalid fixture hex"),
                };
                digit(pair[0]) << 4 | digit(pair[1])
            })
            .collect()
    }

    #[cfg(feature = "net")]
    // Core: evo/mnauth.cpp:19-49; bls/bls.h:177-180 at v24.0.0
    #[test]
    fn core_v24_mnauth_wire_vectors_match() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/mnauth/core-v24.json"))
                .unwrap();
        let key = key();
        let public_key = key.pubkey().to_bytes(BlsSchemeKind::Basic);
        let active = Arc::new(ActiveMasternode::new(key));
        let tip = TipMnList::fixed(TipListRef {
            list: Arc::new(MnList::default_pre_dip3()),
            bls: BlsCtx::BASIC,
        });
        let signing = MnAuthSigning::new(Arc::clone(&active), tip);
        for record in fixture["records"].as_array().unwrap() {
            assert_eq!(record["process_scheme"], "basic");
            assert_eq!(decode_hex(record["public_key"].as_str().unwrap()), public_key);
            let mut pro_tx_hash = decode_hex(record["pro_tx_hash"].as_str().unwrap());
            pro_tx_hash.reverse();
            let pro_tx_hash: [u8; 32] = pro_tx_hash.try_into().unwrap();
            *active.st.write() = ActiveState {
                pro_tx_hash: Some(pro_tx_hash),
                ..ActiveState::default()
            };
            let challenge: [u8; 32] =
                decode_hex(record["challenge"].as_str().unwrap()).try_into().unwrap();
            let inbound = record["direction"] == "inbound";
            let our_version = record["version"].as_i64().unwrap() as i32;
            let message = signing
                .mnauth_payload(&MnAuthChallenge {
                    received: challenge,
                    inbound,
                    our_version,
                })
                .unwrap();
            let mut encoded = Vec::new();
            message.encode(&mut encoded);
            assert_eq!(encoded, decode_hex(record["payload"].as_str().unwrap()));
        }
    }
}
