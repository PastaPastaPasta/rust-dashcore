//! The pure decision half of Core's MNAUTH intake.

use dash_evo::types::ProTxHash;
use dash_p2p_core::{MnAuth, ServiceFlags};
use dash_pkc::bls::{BlsPkBytes, BlsPublicKey, BlsScChia, BlsScIetf, BlsSigBytes, BlsSignature};
use dashd_p2p::processing::seams::OperatorKeyRef;

use super::sign_hash;
use crate::mnconn::deterministic_outbound;

/// The facts read from the connection when its `mnauth` arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerFacts {
    pub id: i64,
    pub verified: Option<[u8; 32]>,
    pub services: ServiceFlags,
    pub inbound: bool,
    pub probe: bool,
    pub sent_challenge: [u8; 32],
    pub version: i32,
    /// `fDisconnect` when the message is processed.
    pub disconnect: bool,
}

/// A peer that passes `ForEachNode`'s default filter.
// Core: net.h:1432; net.cpp:4874 at v24.0.0
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OtherPeer {
    pub id: i64,
    pub verified: Option<[u8; 32]>,
    pub inbound: bool,
}

/// The input to [`decide`].
#[derive(Clone, Debug)]
pub struct Input {
    pub msg: MnAuth,
    pub peer: PeerFacts,
    /// `GetMN(proRegTxHash)`'s operator key at the tip list.
    pub op: Option<OperatorKeyRef>,
    /// The process scheme at that list.
    pub bls_legacy: bool,
    /// Our proTxHash, when we are an active masternode with one.
    pub me: Option<[u8; 32]>,
    /// The peers in table order, the new peer included.
    pub others: Vec<OtherPeer>,
    /// `GetTime<std::chrono::seconds>()`.
    pub now: i64,
}

/// A side effect the handler applies in the order given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    SetLastOutboundSuccess {
        pro: [u8; 32],
        now: i64,
    },
    MarkProbe(i64),
    Disconnect(i64),
}

/// A `NET_NETCONN` log line.
pub type LogLine = String;

/// Core's MNAUTH outcome before `TryMarkVerified`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decision {
    pub score: Option<(u32, &'static str)>,
    pub log: Vec<LogLine>,
    pub effects: Vec<Effect>,
    pub grant: bool,
}

/// Core's read throws: the signature is canonical in neither scheme.
// Core: bls/bls.h:186-203 at v24.0.0
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MalleableSig;

impl Decision {
    fn score(score: u32, reason: &'static str) -> Self {
        Self {
            score: Some((score, reason)),
            ..Self::default()
        }
    }
}

/// `CBLSSignature::Unserialize` under the process scheme: `Ok(true)` for a valid object,
/// `Ok(false)` for an invalid one (null, or canonical only in the other scheme).
// Core: bls/bls.h:98-121, :186-203 at v24.0.0
fn read_sig(sig: &BlsSigBytes, legacy: bool) -> Result<bool, MalleableSig> {
    let canonical = |legacy: bool| {
        if legacy {
            sig.is_canonical::<BlsScChia>()
        } else {
            sig.is_canonical::<BlsScIetf>()
        }
    };
    if sig.is_null() {
        Ok(false)
    } else if canonical(legacy) {
        Ok(true)
    } else if canonical(!legacy) {
        Ok(false)
    } else {
        Err(MalleableSig)
    }
}

/// The stored operator key, decode, Core parity.
// Core: evo/mnauth.cpp:93; bls/bls.h:98-121, :495-513 at v24.0.0
fn operator_key(op: &OperatorKeyRef) -> Option<BlsPublicKey<BlsScIetf>> {
    let stored = BlsPkBytes::from_bytes(op.stored);
    if op.stored_legacy {
        stored.to::<BlsScChia>().ok()?.to_scheme::<BlsScIetf>().ok()
    } else {
        stored.to::<BlsScIetf>().ok()
    }
}

/// `pubKey` serialized under the process scheme.
// Core: evo/mnauth.cpp:95; bls/bls.h:177-180 at v24.0.0
fn process_bytes(key: &BlsPublicKey<BlsScIetf>, legacy: bool) -> Option<[u8; 48]> {
    if legacy {
        Some(key.to_scheme::<BlsScChia>().ok()?.to_bytes())
    } else {
        Some(key.to_bytes())
    }
}

/// The signature read under the process scheme, for a basic-scheme check.
// Core: evo/mnauth.cpp:63, :98 at v24.0.0
fn basic_sig(sig: &BlsSigBytes, legacy: bool) -> Option<BlsSignature<BlsScIetf>> {
    if legacy {
        sig.to::<BlsScChia>().ok()?.to_scheme::<BlsScIetf>().ok()
    } else {
        sig.to::<BlsScIetf>().ok()
    }
}

// Core: evo/mnauth.cpp:93-98 at v24.0.0
fn verify(msg: &MnAuth, peer: &PeerFacts, op: &OperatorKeyRef, legacy: bool) -> bool {
    let Some(key) = operator_key(op) else {
        return false;
    };
    let Some(pk_ser) = process_bytes(&key, legacy) else {
        return false;
    };
    let Some(sig) = basic_sig(&msg.sig(), legacy) else {
        return false;
    };
    let hash = sign_hash(&pk_ser, &peer.sent_challenge, !peer.inbound, peer.version);
    key.verify_digest(&hash, &sig).is_ok()
}

/// Core's MNAUTH checks and ordered effects up to `TryMarkVerified`. `Err` is the read's throw.
// Core: evo/mnauth.cpp:63-156 at v24.0.0
pub fn decide(input: Input) -> Result<Decision, MalleableSig> {
    let Input {
        msg,
        peer,
        op,
        bls_legacy,
        me,
        others,
        now,
    } = input;

    // Core: evo/mnauth.cpp:62-63 at v24.0.0
    let sig_valid = read_sig(&msg.sig(), bls_legacy)?;
    let pro: [u8; 32] = msg.pro_reg_tx_hash().into();
    let shown = ProTxHash::from(pro);

    // Core: evo/mnauth.cpp:65-68 at v24.0.0
    if peer.verified.is_some() {
        return Ok(Decision::score(100, "duplicate mnauth"));
    }
    // Core: evo/mnauth.cpp:70-73 at v24.0.0
    if !peer.services.has(ServiceFlags::NODE_NETWORK | ServiceFlags::NODE_BLOOM) {
        return Ok(Decision::score(100, "mnauth from a node with invalid services"));
    }
    // Core: evo/mnauth.cpp:75-77 at v24.0.0
    if pro == [0; 32] {
        return Ok(Decision::score(100, "empty mnauth proRegTxHash"));
    }
    // Core: evo/mnauth.cpp:79-83 at v24.0.0; an invalid object prints as zeros (bls.h:122-128)
    if !sig_valid {
        let mut d = Decision::score(100, "invalid mnauth signature");
        d.log.push(format!(
            "CMNAuth::ProcessMessage -- invalid mnauth for protx={shown} with sig={}",
            "0".repeat(192)
        ));
        return Ok(d);
    }
    // Core: evo/mnauth.cpp:85-91 at v24.0.0
    let Some(op) = op else {
        return Ok(Decision::score(10, "missing mnauth masternode"));
    };
    // Core: evo/mnauth.cpp:96 at v24.0.0
    let mut d = Decision::default();
    d.log.push(format!(
        "CMNAuth::ProcessMessage -- constructed signHash for nVersion {}, peer={}",
        peer.version, peer.id
    ));
    // Core: evo/mnauth.cpp:98-102 at v24.0.0
    if !verify(&msg, &peer, &op, bls_legacy) {
        d.score = Some((10, "mnauth signature verification failed"));
        return Ok(d);
    }

    // Core: evo/mnauth.cpp:104-112 at v24.0.0
    if !peer.inbound {
        d.effects.push(Effect::SetLastOutboundSuccess {
            pro,
            now,
        });
        if peer.probe {
            d.log.push(format!(
                "CMNAuth::ProcessMessage -- Masternode probe successful for {shown}, disconnecting. peer={}",
                peer.id
            ));
            d.effects.push(Effect::Disconnect(peer.id));
            return Ok(d);
        }
    }

    // Core: evo/mnauth.cpp:116-152 at v24.0.0
    let mut self_disconnected = peer.disconnect;
    for other in others {
        if self_disconnected {
            break;
        }
        if other.verified != Some(pro) {
            continue;
        }
        let Some(me) = me else {
            d.log.push(format!(
                "CMNAuth::ProcessMessage -- Masternode {shown} has already verified as peer {}, dropping new connection. peer={}",
                other.id, peer.id
            ));
            d.effects.push(Effect::Disconnect(peer.id));
            self_disconnected = true;
            continue;
        };
        let outbound = deterministic_outbound(me, pro);
        d.log.push(format!(
            "CMNAuth::ProcessMessage -- Masternode {shown} has already verified as peer {}, deterministicOutbound={}. peer={}",
            other.id,
            ProTxHash::from(outbound),
            peer.id
        ));
        if outbound == me {
            if other.inbound {
                d.log.push(format!(
                    "CMNAuth::ProcessMessage -- marking old inbound for dropping it later, peer={}",
                    other.id
                ));
                d.effects.push(Effect::MarkProbe(other.id));
            } else if peer.inbound {
                d.log.push(format!(
                    "CMNAuth::ProcessMessage -- marking new inbound for dropping it later, peer={}",
                    peer.id
                ));
                d.effects.push(Effect::MarkProbe(peer.id));
            }
        } else if !other.inbound {
            d.log.push(format!(
                "CMNAuth::ProcessMessage -- dropping old outbound, peer={}",
                other.id
            ));
            d.effects.push(Effect::Disconnect(other.id));
        } else if !peer.inbound {
            d.log.push(format!(
                "CMNAuth::ProcessMessage -- dropping new outbound, peer={}",
                peer.id
            ));
            d.effects.push(Effect::Disconnect(peer.id));
            self_disconnected = true;
        }
    }
    // Core: evo/mnauth.cpp:154-156 at v24.0.0
    d.grant = !self_disconnected;
    Ok(d)
}

#[cfg(test)]
mod tests {
    use dash_args::ArgsManager;
    use dash_consensus::hash::sha256d;
    use dash_pkc::bls::BlsSchemeKind;
    use dash_primitives::TxHash;
    use serde_json::Value;

    use super::*;
    use crate::active::OperatorKey;
    use crate::active::key::parse;

    // Public regtest v0.0.4 network-mn-0 key, also recorded by A1.
    const SECRET: &str = "6fa2f4c370d6603b6e850babbcca53f4f8af3289a20189ba4ff274defebdf1bb";
    const PRO: [u8; 32] = [7; 32];
    const CHALLENGE: [u8; 32] = [0x11; 32];
    const NOW: i64 = 1_700_000_063;
    const SERVICES: ServiceFlags = ServiceFlags::new(1 | 1 << 2);

    fn key() -> OperatorKey {
        let args = ArgsManager::new();
        args.force_set_arg("-masternodeblsprivkey", SECRET);
        parse(&args).unwrap().unwrap()
    }

    fn scheme(legacy: bool) -> BlsSchemeKind {
        BlsSchemeKind::from_legacy(legacy)
    }

    fn op(key: &OperatorKey, stored_legacy: bool) -> OperatorKeyRef {
        let stored = key.pubkey().to_bytes(scheme(stored_legacy));
        OperatorKeyRef {
            stored,
            stored_legacy,
            pubkey_hash: sha256d(&stored),
        }
    }

    /// The `mnauth` `key` sends over a connection it sees as `signer_inbound`, as `PushMNAUTH`
    /// signs it under the process scheme `legacy`.
    fn signed(
        key: &OperatorKey,
        pro: [u8; 32],
        challenge: [u8; 32],
        signer_inbound: bool,
        version: i32,
        legacy: bool,
    ) -> MnAuth {
        let pk = key.pubkey().to_bytes(scheme(legacy));
        let hash = sign_hash(&pk, &challenge, signer_inbound, version);
        let sig = key.sign(&hash, false).to_bytes(scheme(legacy));
        MnAuth::new(TxHash::from(pro), BlsSigBytes::from_bytes(sig))
    }

    fn with_sig(msg: &MnAuth, sig: [u8; 96]) -> MnAuth {
        MnAuth::new(msg.pro_reg_tx_hash(), BlsSigBytes::from_bytes(sig))
    }

    fn peer(id: i64, inbound: bool) -> PeerFacts {
        PeerFacts {
            id,
            verified: None,
            services: SERVICES,
            inbound,
            probe: false,
            sent_challenge: CHALLENGE,
            version: 70242,
            disconnect: false,
        }
    }

    /// A valid input: `key`'s `mnauth` for `PRO` from a peer we see as `inbound`, basic scheme.
    fn input(key: &OperatorKey, inbound: bool) -> Input {
        Input {
            msg: signed(key, PRO, CHALLENGE, !inbound, 70242, false),
            peer: peer(9, inbound),
            op: Some(op(key, false)),
            bls_legacy: false,
            me: None,
            others: vec![OtherPeer {
                id: 9,
                verified: None,
                inbound,
            }],
            now: NOW,
        }
    }

    fn other(id: i64, inbound: bool) -> OtherPeer {
        OtherPeer {
            id,
            verified: Some(PRO),
            inbound,
        }
    }

    /// A proTxHash `me` for which `DeterministicOutboundConnection(me, PRO)` is `me` when `ours`.
    fn me(ours: bool) -> [u8; 32] {
        (1..=255u8)
            .map(|b| [b; 32])
            .find(|m| *m != PRO && (deterministic_outbound(*m, PRO) == *m) == ours)
            .unwrap()
    }

    fn line(text: &str) -> String {
        format!("CMNAuth::ProcessMessage -- {text}")
    }

    fn signhash_line(id: i64) -> String {
        signhash_line_at(70242, id)
    }

    fn signhash_line_at(version: i32, id: i64) -> String {
        line(&format!("constructed signHash for nVersion {version}, peer={id}"))
    }

    fn decide_ok(input: Input) -> Decision {
        decide(input).unwrap()
    }

    fn scores(score: u32, reason: &'static str) -> Option<(u32, &'static str)> {
        Some((score, reason))
    }

    // Core: evo/mnauth.cpp:62-102; bls/bls.h:186-203 at v24.0.0
    #[test]
    fn mnauth_checks_score_in_cores_order() {
        let key = key();
        let valid = input(&key, true);
        let basic_sig = valid.msg.sig().to_bytes();
        let legacy_sig = key
            .sign(
                &sign_hash(&key.pubkey().to_bytes(scheme(false)), &CHALLENGE, false, 70242),
                false,
            )
            .to_bytes(scheme(true));
        assert!(
            !BlsSigBytes::from_bytes(legacy_sig).is_canonical::<BlsScIetf>(),
            "a legacy encoding the basic read refuses"
        );
        let shown = ProTxHash::from(PRO);
        let zero_sig_line =
            line(&format!("invalid mnauth for protx={shown} with sig={}", "0".repeat(192)));

        type Case = (&'static str, Box<dyn Fn(&mut Input)>, Option<(u32, &'static str)>);
        let cases: Vec<Case> = vec![
            (
                "duplicate",
                Box::new(|i| i.peer.verified = Some([1; 32])),
                scores(100, "duplicate mnauth"),
            ),
            (
                "no NODE_BLOOM",
                Box::new(|i| i.peer.services = ServiceFlags::NODE_NETWORK),
                scores(100, "mnauth from a node with invalid services"),
            ),
            (
                "no NODE_NETWORK",
                Box::new(|i| i.peer.services = ServiceFlags::NODE_BLOOM),
                scores(100, "mnauth from a node with invalid services"),
            ),
            (
                "null proTxHash",
                Box::new(|i| i.msg = MnAuth::new(TxHash::from([0; 32]), i.msg.sig())),
                scores(100, "empty mnauth proRegTxHash"),
            ),
            (
                "null signature",
                Box::new(|i| i.msg = with_sig(&i.msg, [0; 96])),
                scores(100, "invalid mnauth signature"),
            ),
            (
                "signature only canonical in the other scheme",
                Box::new(move |i| i.msg = with_sig(&i.msg, legacy_sig)),
                scores(100, "invalid mnauth signature"),
            ),
            (
                "unknown masternode",
                Box::new(|i| i.op = None),
                scores(10, "missing mnauth masternode"),
            ),
            (
                "wrong challenge",
                Box::new(|i| i.peer.sent_challenge = [0x22; 32]),
                scores(10, "mnauth signature verification failed"),
            ),
            (
                "wrong direction",
                Box::new(|i| i.peer.inbound = false),
                scores(10, "mnauth signature verification failed"),
            ),
            (
                "wrong version",
                Box::new(|i| i.peer.version = 70241),
                scores(10, "mnauth signature verification failed"),
            ),
            (
                "invalid stored key",
                Box::new(|i| {
                    i.op = Some(OperatorKeyRef {
                        stored: [0; 48],
                        ..i.op.unwrap()
                    })
                }),
                scores(10, "mnauth signature verification failed"),
            ),
            (
                "the duplicate check first",
                Box::new(|i| {
                    i.peer.verified = Some([1; 32]);
                    i.peer.services = ServiceFlags::NONE;
                    i.msg = with_sig(&MnAuth::new(TxHash::from([0; 32]), i.msg.sig()), [0; 96]);
                    i.op = None;
                }),
                scores(100, "duplicate mnauth"),
            ),
            (
                "services before the proTxHash",
                Box::new(|i| {
                    i.peer.services = ServiceFlags::NONE;
                    i.msg = MnAuth::new(TxHash::from([0; 32]), i.msg.sig());
                }),
                scores(100, "mnauth from a node with invalid services"),
            ),
            (
                "services before the signature",
                Box::new(|i| {
                    i.peer.services = ServiceFlags::NONE;
                    i.msg = with_sig(&i.msg, [0; 96]);
                }),
                scores(100, "mnauth from a node with invalid services"),
            ),
            (
                "the proTxHash before the signature",
                Box::new(|i| {
                    i.msg = with_sig(&MnAuth::new(TxHash::from([0; 32]), i.msg.sig()), [0; 96]);
                }),
                scores(100, "empty mnauth proRegTxHash"),
            ),
            (
                "an invalid signature before the list",
                Box::new(|i| {
                    i.msg = with_sig(&i.msg, [0; 96]);
                    i.op = None;
                }),
                scores(100, "invalid mnauth signature"),
            ),
        ];
        for (name, edit, want) in cases {
            let mut i = input(&key, true);
            edit(&mut i);
            let (id, version) = (i.peer.id, i.peer.version);
            let d = decide_ok(i);
            assert_eq!(d.score, want, "{name}");
            assert!(!d.grant && d.effects.is_empty(), "{name}");
            let log = match want.unwrap().1 {
                "invalid mnauth signature" => vec![zero_sig_line.clone()],
                "mnauth signature verification failed" => vec![signhash_line_at(version, id)],
                _ => vec![],
            };
            assert_eq!(d.log, log, "{name}");
        }
        let d = decide_ok(input(&key, true));
        assert_eq!(BlsSigBytes::from_bytes(basic_sig), input(&key, true).msg.sig());
        assert_eq!(
            d,
            Decision {
                score: None,
                log: vec![signhash_line(9)],
                effects: vec![],
                grant: true,
            }
        );
    }

    // Core: bls/bls.h:186-203; evo/mnauth.cpp:62-68 at v24.0.0
    #[test]
    fn a_signature_canonical_in_neither_scheme_throws_before_any_check() {
        let key = key();
        for bytes in [[0xff; 96], [0xc0; 96]] {
            for legacy in [false, true] {
                let mut i = input(&key, true);
                i.msg = with_sig(&i.msg, bytes);
                i.bls_legacy = legacy;
                i.peer.verified = Some([1; 32]);
                assert_eq!(decide(i), Err(MalleableSig), "{legacy}");
            }
        }
    }

    /// The point at infinity, as `G2Element::SerializeToArray` writes it in either scheme.
    // Core: dashbls/src/elements.cpp:406-410 at v24.0.0
    fn identity_sig() -> [u8; 96] {
        let mut bytes = [0; 96];
        bytes[0] = 0xc0;
        bytes
    }

    // Core: bls/bls.h:111-115, :186-203; dashbls/src/elements.cpp:262-283 at v24.0.0
    #[test]
    fn the_identity_signature_throws_under_both_process_schemes() {
        let key = key();
        let identity = BlsSigBytes::from_bytes(identity_sig());
        assert!(!identity.is_null());
        assert!(!identity.is_canonical::<BlsScIetf>());
        assert!(!identity.is_canonical::<BlsScChia>());
        for legacy in [false, true] {
            let mut i = input(&key, true);
            i.msg = with_sig(&i.msg, identity_sig());
            i.bls_legacy = legacy;
            assert_eq!(decide(i), Err(MalleableSig), "bls_legacy={legacy}");
        }
    }

    // Core: evo/mnauth.cpp:104-112, :154-156 at v24.0.0
    #[test]
    fn outbound_success_is_recorded_and_a_probe_is_dropped() {
        let key = key();
        let d = decide_ok(input(&key, false));
        let success = Effect::SetLastOutboundSuccess {
            pro: PRO,
            now: NOW,
        };
        assert_eq!((d.effects.clone(), d.grant), (vec![success], true));
        let mut probe = input(&key, false);
        probe.peer.probe = true;
        let d = decide_ok(probe);
        assert_eq!(d.effects, vec![success, Effect::Disconnect(9)]);
        assert!(!d.grant);
        assert_eq!(
            d.log,
            vec![
                signhash_line(9),
                line(&format!(
                    "Masternode probe successful for {}, disconnecting. peer=9",
                    ProTxHash::from(PRO)
                )),
            ]
        );
        let mut inbound_probe = input(&key, true);
        inbound_probe.peer.probe = true;
        let d = decide_ok(inbound_probe);
        assert_eq!((d.effects, d.grant), (vec![], true));
    }

    fn walk(
        key: &OperatorKey,
        inbound: bool,
        me: Option<[u8; 32]>,
        others: &[OtherPeer],
    ) -> Decision {
        let mut i = input(key, inbound);
        i.me = me;
        i.others = others.to_vec();
        i.others.push(OtherPeer {
            id: 9,
            verified: None,
            inbound,
        });
        decide_ok(i)
    }

    fn already(other: i64, me: Option<[u8; 32]>) -> String {
        let shown = ProTxHash::from(PRO);
        match me {
            Some(me) => line(&format!(
                "Masternode {shown} has already verified as peer {other}, deterministicOutbound={}. peer=9",
                ProTxHash::from(deterministic_outbound(me, PRO))
            )),
            None => line(&format!(
                "Masternode {shown} has already verified as peer {other}, dropping new connection. peer=9"
            )),
        }
    }

    // Core: evo/mnauth.cpp:116-156 at v24.0.0
    #[test]
    fn the_dedup_walk_follows_cores_branches() {
        let key = key();
        let ours = Some(me(true));
        let theirs = Some(me(false));
        let success = Effect::SetLastOutboundSuccess {
            pro: PRO,
            now: NOW,
        };
        // (self inbound, me, other inbound, effects after any outbound success, extra line, grant)
        type Case = (bool, Option<[u8; 32]>, bool, Vec<Effect>, Option<String>, bool);
        let cases: [Case; 9] = [
            (true, None, true, vec![Effect::Disconnect(9)], None, false),
            (false, None, false, vec![Effect::Disconnect(9)], None, false),
            (
                true,
                ours,
                true,
                vec![Effect::MarkProbe(3)],
                Some(line("marking old inbound for dropping it later, peer=3")),
                true,
            ),
            (
                false,
                ours,
                true,
                vec![Effect::MarkProbe(3)],
                Some(line("marking old inbound for dropping it later, peer=3")),
                true,
            ),
            (
                true,
                ours,
                false,
                vec![Effect::MarkProbe(9)],
                Some(line("marking new inbound for dropping it later, peer=9")),
                true,
            ),
            (false, ours, false, vec![], None, true),
            (
                true,
                theirs,
                false,
                vec![Effect::Disconnect(3)],
                Some(line("dropping old outbound, peer=3")),
                true,
            ),
            (
                false,
                theirs,
                true,
                vec![Effect::Disconnect(9)],
                Some(line("dropping new outbound, peer=9")),
                false,
            ),
            (true, theirs, true, vec![], None, true),
        ];
        for (inbound, me, other_inbound, effects, extra, grant) in cases {
            let d = walk(&key, inbound, me, &[other(3, other_inbound)]);
            let mut want_effects = if inbound {
                vec![]
            } else {
                vec![success]
            };
            want_effects.extend(effects);
            let mut log = vec![signhash_line(9), already(3, me)];
            log.extend(extra);
            let case = format!("inbound={inbound} me={me:?} other_inbound={other_inbound}");
            assert_eq!(d.effects, want_effects, "{case}");
            assert_eq!(d.log, log, "{case}");
            assert_eq!(d.grant, grant, "{case}");
            assert_eq!(d.score, None, "{case}");
        }
    }

    // Core: evo/mnauth.cpp:117-120, :137-144 at v24.0.0
    #[test]
    fn the_walk_stops_once_the_new_peer_is_dropped_and_keeps_earlier_effects() {
        let key = key();
        let theirs = Some(me(false));
        let success = Effect::SetLastOutboundSuccess {
            pro: PRO,
            now: NOW,
        };
        let d = walk(&key, false, theirs, &[other(2, false), other(3, true), other(4, false)]);
        assert_eq!(d.effects, vec![success, Effect::Disconnect(2), Effect::Disconnect(9)]);
        assert_eq!(
            d.log,
            vec![
                signhash_line(9),
                already(2, theirs),
                line("dropping old outbound, peer=2"),
                already(3, theirs),
                line("dropping new outbound, peer=9"),
            ]
        );
        assert!(!d.grant);

        let d = walk(&key, true, None, &[other(2, true), other(3, false)]);
        assert_eq!(d.effects, vec![Effect::Disconnect(9)]);
        assert_eq!(d.log, vec![signhash_line(9), already(2, None)]);

        let ours = Some(me(true));
        let d = walk(&key, true, ours, &[other(2, true), other(3, false), other(4, true)]);
        assert_eq!(
            d.effects,
            vec![Effect::MarkProbe(2), Effect::MarkProbe(9), Effect::MarkProbe(4)]
        );
        assert!(d.grant, "marking the new inbound as a probe keeps the walk going");

        let d = walk(
            &key,
            true,
            theirs,
            &[OtherPeer {
                id: 2,
                verified: Some([8; 32]),
                inbound: false,
            }],
        );
        assert_eq!((d.effects, d.log, d.grant), (vec![], vec![signhash_line(9)], true));
    }

    // Core: evo/mnauth.cpp:117-120, :154-156 at v24.0.0
    #[test]
    fn a_new_peer_already_disconnecting_walks_nothing_and_is_not_granted() {
        let key = key();
        let mut i = input(&key, false);
        i.peer.disconnect = true;
        i.others = vec![other(2, false)];
        let d = decide_ok(i);
        assert_eq!(
            d.effects,
            vec![Effect::SetLastOutboundSuccess {
                pro: PRO,
                now: NOW,
            }]
        );
        assert_eq!(d.log, vec![signhash_line(9)]);
        assert!(!d.grant);
    }

    // Core: evo/mnauth.cpp:93-98; bls/bls.h:98-121, :177-180, :495-513 at v24.0.0
    #[test]
    fn a_stored_key_decodes_under_its_own_scheme_and_signs_under_the_process_scheme() {
        let key = key();
        assert_ne!(key.pubkey().to_bytes(scheme(true)), key.pubkey().to_bytes(scheme(false)));
        for (stored_legacy, process_legacy) in
            [(true, false), (false, false), (true, true), (false, true)]
        {
            for inbound in [false, true] {
                let mut i = input(&key, inbound);
                i.msg = signed(&key, PRO, CHALLENGE, !inbound, 70242, process_legacy);
                i.op = Some(op(&key, stored_legacy));
                i.bls_legacy = process_legacy;
                let d = decide_ok(i.clone());
                let case = format!("stored_legacy={stored_legacy} process_legacy={process_legacy}");
                assert_eq!((d.score, d.grant), (None, true), "{case}");
                i.bls_legacy = !process_legacy;
                assert_ne!(decide(i).map(|d| d.grant), Ok(true), "{case}: other process scheme");
            }
        }
    }

    fn decode_hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    // Core: evo/mnauth.cpp:19-49, :93-98 at v24.0.0
    #[test]
    fn core_v24_mnauth_payloads_verify_on_the_receiving_side() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/mnauth/core-v24.json"))
                .unwrap();
        let key = key();
        for record in fixture["records"].as_array().unwrap() {
            let payload = decode_hex(record["payload"].as_str().unwrap());
            let msg = <MnAuth as dash_types::codec::BaseCodec>::decode(&mut &payload[..]).unwrap();
            let stored: [u8; 48] =
                decode_hex(record["public_key"].as_str().unwrap()).try_into().unwrap();
            assert_eq!(stored, key.pubkey().to_bytes(BlsSchemeKind::Basic));
            let signer_inbound = record["direction"] == "inbound";
            let mut i = input(&key, !signer_inbound);
            i.msg = msg;
            i.peer.sent_challenge =
                decode_hex(record["challenge"].as_str().unwrap()).try_into().unwrap();
            i.peer.version = record["version"].as_i64().unwrap() as i32;
            i.op = Some(OperatorKeyRef {
                stored,
                stored_legacy: false,
                pubkey_hash: sha256d(&stored),
            });
            let name = record["name"].as_str().unwrap();
            let d = decide_ok(i.clone());
            assert_eq!((d.score, d.grant), (None, true), "{name}");
            i.peer.inbound = signer_inbound;
            assert_eq!(
                decide_ok(i).score,
                scores(10, "mnauth signature verification failed"),
                "{name}: the signer's own direction"
            );
        }
    }
}
