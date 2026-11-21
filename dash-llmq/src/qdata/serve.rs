//! Core's answer to a `qgetdata` that passed the handler's gates: the `qdata` reply, if any,
//! and the peer scores, in Core's order (the reply goes out before any score).

use dash_consensus::primitives::{BlockHash, CoreWrite, TxHash, write_bytes, write_compact_size};
use dash_evo::types::ProTxHash;

use super::source::{EncryptedContributions, QuorumData, QuorumDataSource};
use super::tracker::{Key, Registration, Request, TrackerAccess};
use crate::dkg::bls::BlsSchemeKind;
use crate::dkg::wire::{QData, QuorumDataError, QuorumDataRequest, QuorumHash};

/// A `qgetdata` as read off the wire. Its error is UNDEFINED: the request carries none.
// Core: v24.0.0 src/llmq/quorums.h:117-132 CQuorumDataRequest::SERIALIZE_METHODS
impl From<&QuorumDataRequest> for Request {
    fn from(r: &QuorumDataRequest) -> Self {
        Self {
            llmq_type: r.llmq_type(),
            quorum_hash: BlockHash::from(*r.quorum_hash().as_bytes()),
            data_mask: r.data_mask(),
            pro_tx: ProTxHash::from(*r.pro_tx_hash().as_bytes()),
            error: Self::ERROR_UNDEFINED,
        }
    }
}

/// The request's wire form, without its error.
impl From<&Request> for QuorumDataRequest {
    fn from(r: &Request) -> Self {
        Self::new(
            r.llmq_type,
            QuorumHash::from(*r.quorum_hash.as_bytes()),
            r.data_mask,
            TxHash::from(*r.pro_tx.as_bytes()),
        )
    }
}

/// What [`serve`] reads.
pub struct ServeCx<'a, S, E, T> {
    pub source: &'a S,
    /// The data request map, reached only for the registration.
    pub tracker: &'a mut T,
    /// The requester's verified proRegTx hash; null for an unverified `qwatch` peer.
    pub requester: ProTxHash,
    pub now: i64,
    /// Whether this node runs a masternode (Core's `m_nodeman`).
    pub local_mn: bool,
    /// The dkgdb reader.
    pub contributions: &'a E,
    /// The scheme of the node's view, under which the verification vector is written.
    pub scheme: BlsSchemeKind,
}

/// [`serve`]'s result: the `qdata` to push, then the scores to apply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServeOut {
    pub reply: Option<QData>,
    pub scores: Vec<(i32, &'static str)>,
}

/// Answers `req` from a peer that passed the `qgetdata` gates. `Err` is the quorum build's read
/// error, which Core throws out of the handler: no reply and no score, with the request still
/// registered.
// Core: v24.0.0 src/llmq/net_quorum.cpp:94-201 NetQuorum::ProcessMessage
pub fn serve<S: QuorumDataSource, E: EncryptedContributions, T: TrackerAccess>(
    cx: ServeCx<'_, S, E, T>,
    req: &QuorumDataRequest,
) -> Result<ServeOut, S::Error> {
    let mut out = ServeOut::default();
    let request = Request::from(req);
    let llmq_type = request.llmq_type;
    let quorum_hash = request.quorum_hash;

    if !cx.source.llmq_registered(llmq_type) {
        send_qdata(&mut out, req, QuorumDataError::QuorumTypeInvalid, false, Vec::new());
        out.scores.push((100, "invalid llmqType in QGETDATA"));
        return Ok(out);
    }

    if !cx.source.active_block(&quorum_hash) {
        send_qdata(&mut out, req, QuorumDataError::QuorumBlockNotFound, false, Vec::new());
        return Ok(out);
    }

    let key = Key {
        pro_reg_tx: cx.requester,
        we_requested: false,
        quorum_hash,
        llmq_type,
    };
    let registration = cx.tracker.with_tracker(|t| t.register(key, request, cx.now, false));
    let request_limit_exceeded = match registration {
        Registration::RequesterLimitExceeded => {
            out.scores.push((25, "too many quorum data requests"));
            return Ok(out);
        }
        Registration::CapacityExhausted => return Ok(out),
        Registration::RateLimited => true,
        Registration::Accepted => false,
    };

    let Some(quorum) = cx.source.quorum(llmq_type, &quorum_hash)? else {
        if send_qdata(
            &mut out,
            req,
            QuorumDataError::QuorumNotFound,
            request_limit_exceeded,
            Vec::new(),
        ) {
            out.scores.push((25, "request limit exceeded"));
        }
        return Ok(out);
    };

    let mut body = Vec::new();

    if request.data_mask & Request::QUORUM_VERIFICATION_VECTOR != 0 {
        let Some(vvec) = quorum.vvec() else {
            if send_qdata(
                &mut out,
                req,
                QuorumDataError::QuorumVerificationVectorMissing,
                request_limit_exceeded,
                Vec::new(),
            ) {
                out.scores.push((25, "request limit exceeded"));
            }
            return Ok(out);
        };
        write_compact_size(vvec.len() as u64, &mut body);
        for pk in vvec.iter() {
            body.extend_from_slice(&pk.to_bytes(cx.scheme));
        }
    }

    let (misbehave_contrib, contrib_error) = process_contrib_qgetdata(
        &mut body,
        &quorum,
        &request,
        cx.local_mn,
        cx.contributions,
        cx.scheme,
    );

    let misbehave_qdata = match contrib_error {
        Some(error) => send_qdata(&mut out, req, error, request_limit_exceeded, Vec::new()),
        None => send_qdata(&mut out, req, QuorumDataError::None, request_limit_exceeded, body),
    };

    if request_limit_exceeded && (misbehave_contrib || misbehave_qdata) {
        out.scores.push((25, "request limit exceeded"));
    }
    Ok(out)
}

/// Sets the reply to `req` with `error` and `body`. Returns whether the request limit counts as
/// misbehavior for that error.
// Core: v24.0.0 src/llmq/net_quorum.cpp:96-119 NetQuorum::ProcessMessage sendQDATA
fn send_qdata(
    out: &mut ServeOut,
    req: &QuorumDataRequest,
    error: QuorumDataError,
    request_limit_exceeded: bool,
    body: Vec<u8>,
) -> bool {
    let misbehave = match error {
        QuorumDataError::QuorumVerificationVectorMissing
        | QuorumDataError::EncryptedContributionsMissing => false,
        _ => request_limit_exceeded,
    };
    out.reply = Some(QData::new(*req, error, body));
    misbehave
}

/// Appends the requested member's encrypted contributions to `body`. Returns whether the
/// request counts as misbehavior, and the error the reply carries instead of the data.
// Core: v24.0.0 src/llmq/net_quorum.cpp:273-301 NetQuorum::ProcessContribQGETDATA
fn process_contrib_qgetdata<Q: QuorumData, E: EncryptedContributions>(
    body: &mut Vec<u8>,
    quorum: &Q,
    request: &Request,
    local_mn: bool,
    contributions: &E,
    scheme: BlsSchemeKind,
) -> (bool, Option<QuorumDataError>) {
    if request.data_mask & Request::ENCRYPTED_CONTRIBUTIONS == 0 {
        return (false, None);
    }
    if !local_mn {
        return (true, Some(QuorumDataError::EncryptedContributionsMissing));
    }
    if quorum.member_index(&request.pro_tx).is_none() {
        return (true, Some(QuorumDataError::MasternodeIsNoMember));
    }
    let Some(encrypted) = contributions.get(
        request.llmq_type,
        &request.quorum_hash,
        quorum.valid(),
        &request.pro_tx,
        scheme,
    ) else {
        return (true, Some(QuorumDataError::EncryptedContributionsMissing));
    };
    write_compact_size(encrypted.len() as u64, body);
    for blob in &encrypted {
        body.write(blob.ephemeral_pk().as_bytes());
        body.write(blob.iv_seed());
        write_bytes(blob.data(), body);
    }
    (false, None)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_params::LlmqType;
    use dash_types::Numeric;
    use dash_types::codec::BaseCodec;

    use super::*;
    use crate::dkg::bls::{BlsPkValue, BlsSkValue, secret_to_public};
    use crate::dkg::wire::BlsIesBlobBytes;
    use crate::qdata::fakes::{
        FakeContributions, FakeQuorum, FakeQuorumState, FakeReadError, FakeSource,
    };
    use crate::qdata::tracker::{
        EXPIRATION_TIMEOUT, MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER, Tracker,
    };

    const NOW: i64 = 1_000_000;
    const LIMIT: (i32, &str) = (25, "request limit exceeded");
    const VVEC: u16 = Request::QUORUM_VERIFICATION_VECTOR;
    const CONTRIB: u16 = Request::ENCRYPTED_CONTRIBUTIONS;

    /// The quorum's block, an active block with no mined commitment, and a block off the
    /// active chain.
    const QUORUM_BLOCK: u16 = 1;
    const NO_QUORUM_BLOCK: u16 = 2;
    const OFF_CHAIN_BLOCK: u16 = 3;

    fn raw(n: u16) -> [u8; 32] {
        let mut b = [0; 32];
        b[..2].copy_from_slice(&n.to_be_bytes());
        b[31] = 0xa5;
        b
    }

    fn block(n: u16) -> BlockHash {
        BlockHash::from(raw(n))
    }

    fn pro(n: u16) -> ProTxHash {
        ProTxHash::from(raw(n))
    }

    fn null() -> ProTxHash {
        ProTxHash::from([0; 32])
    }

    const REQUESTER: u16 = 1;
    /// A valid member with dkgdb rows, an invalid member with rows, a valid member without
    /// rows, and a masternode outside the quorum.
    const VALID_MEMBER: u16 = 10;
    const INVALID_MEMBER: u16 = 11;
    const MEMBER_WITHOUT_ROWS: u16 = 12;
    const OUTSIDER: u16 = 99;

    fn sk(n: u8) -> BlsSkValue {
        let mut bytes = [0; 32];
        bytes[31] = n;
        BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).expect("canonical scalar")
    }

    fn vvec() -> Vec<BlsPkValue> {
        vec![secret_to_public(&sk(1)), secret_to_public(&sk(2))]
    }

    fn vvec_body(scheme: BlsSchemeKind) -> Vec<u8> {
        let mut out = vec![2];
        for pk in vvec() {
            out.extend_from_slice(&pk.to_bytes(scheme));
        }
        out
    }

    /// An encrypted contribution's wire bytes: ephemeral key, IV seed, then a 3-byte ciphertext.
    fn blob_bytes(n: u8) -> Vec<u8> {
        let mut out = secret_to_public(&sk(n)).to_bytes(BlsSchemeKind::Basic).to_vec();
        out.extend_from_slice(&[n; 32]);
        out.extend_from_slice(&[3, n, n, n]);
        out
    }

    fn blob(n: u8) -> BlsIesBlobBytes {
        BlsIesBlobBytes::decode(&mut &blob_bytes(n)[..]).expect("blob bytes")
    }

    fn contrib_body(member: u16) -> Vec<u8> {
        let (a, b) = rows_of(member);
        let mut out = vec![2];
        out.extend(blob_bytes(a));
        out.extend(blob_bytes(b));
        out
    }

    fn rows_of(member: u16) -> (u8, u8) {
        match member {
            VALID_MEMBER => (20, 21),
            INVALID_MEMBER => (30, 31),
            _ => unreachable!("no rows"),
        }
    }

    fn valid_bits() -> Vec<bool> {
        vec![true, false, true]
    }

    fn test_type() -> LlmqType {
        LlmqType::LlmqTest
    }

    struct Fx {
        source: FakeSource,
        dkgdb: FakeContributions,
        tracker: Tracker,
        local_mn: bool,
        scheme: BlsSchemeKind,
    }

    impl Fx {
        fn new(has_vvec: bool) -> Self {
            let quorum = FakeQuorum::from(FakeQuorumState {
                threshold: 2,
                members: vec![pro(VALID_MEMBER), pro(INVALID_MEMBER), pro(MEMBER_WITHOUT_ROWS)],
                valid: valid_bits(),
                vvec: has_vvec.then(|| Arc::new(vvec())).into(),
                ..FakeQuorumState::default()
            });
            let mut source = FakeSource::default();
            source.registered.insert(test_type().to_base());
            source.active.extend([block(QUORUM_BLOCK), block(NO_QUORUM_BLOCK)]);
            source.quorums.insert((test_type().to_base(), block(QUORUM_BLOCK)), quorum);
            let mut dkgdb = FakeContributions::default();
            for member in [VALID_MEMBER, INVALID_MEMBER] {
                let (a, b) = rows_of(member);
                dkgdb.rows.insert(
                    (test_type().to_base(), block(QUORUM_BLOCK), pro(member)),
                    vec![blob(a), blob(b)],
                );
            }
            Self {
                source,
                dkgdb,
                tracker: Tracker::new(),
                local_mn: true,
                scheme: BlsSchemeKind::Basic,
            }
        }

        fn try_serve_at(
            &mut self,
            requester: ProTxHash,
            req: &QuorumDataRequest,
            now: i64,
        ) -> Result<ServeOut, FakeReadError> {
            serve(
                ServeCx {
                    source: &self.source,
                    tracker: &mut self.tracker,
                    requester,
                    now,
                    local_mn: self.local_mn,
                    contributions: &self.dkgdb,
                    scheme: self.scheme,
                },
                req,
            )
        }

        fn serve_at(
            &mut self,
            requester: ProTxHash,
            req: &QuorumDataRequest,
            now: i64,
        ) -> ServeOut {
            self.try_serve_at(requester, req, now).expect("no read error")
        }

        fn serve(&mut self, req: &QuorumDataRequest) -> ServeOut {
            self.serve_at(pro(REQUESTER), req, NOW)
        }

        /// Registers `req` from the requester a second earlier, so that serving it is
        /// rate-limited.
        fn prime(&mut self, req: &QuorumDataRequest) {
            let r = Request::from(req);
            assert_eq!(
                self.tracker.register(key(pro(REQUESTER), &r), r, NOW - 1, false),
                Registration::Accepted
            );
        }

        fn lookups(&self) -> usize {
            self.source.quorum_lookups.borrow().len()
        }
    }

    fn key(requester: ProTxHash, r: &Request) -> Key {
        Key {
            pro_reg_tx: requester,
            we_requested: false,
            quorum_hash: r.quorum_hash,
            llmq_type: r.llmq_type,
        }
    }

    fn req_of(
        llmq_type: LlmqType,
        block_n: u16,
        mask: u16,
        pro_tx: ProTxHash,
    ) -> QuorumDataRequest {
        QuorumDataRequest::from(&Request {
            llmq_type,
            quorum_hash: block(block_n),
            data_mask: mask,
            pro_tx,
            error: Request::ERROR_UNDEFINED,
        })
    }

    fn req(block_n: u16, mask: u16, pro_tx: u16) -> QuorumDataRequest {
        req_of(test_type(), block_n, mask, pro(pro_tx))
    }

    fn reply_of(out: &ServeOut) -> &QData {
        out.reply.as_ref().expect("a reply")
    }

    #[derive(Clone, Copy, Debug)]
    enum Body {
        Empty,
        Vvec,
        Contrib(u16),
        Both(u16),
    }

    impl Body {
        fn bytes(self) -> Vec<u8> {
            match self {
                Self::Empty => Vec::new(),
                Self::Vvec => vvec_body(BlsSchemeKind::Basic),
                Self::Contrib(member) => contrib_body(member),
                Self::Both(member) => {
                    [vvec_body(BlsSchemeKind::Basic), contrib_body(member)].concat()
                }
            }
        }
    }

    struct Case {
        name: &'static str,
        block: u16,
        mask: u16,
        pro_tx: u16,
        local_mn: bool,
        has_vvec: bool,
        error: QuorumDataError,
        body: Body,
        /// Whether the dkgdb is read, for `pro_tx`.
        reads_dkgdb: bool,
        scores_free: &'static [(i32, &'static str)],
        scores_limited: &'static [(i32, &'static str)],
    }

    const CASES: &[Case] = &[
        Case {
            name: "quorum not found",
            block: NO_QUORUM_BLOCK,
            mask: VVEC,
            pro_tx: VALID_MEMBER,
            local_mn: true,
            has_vvec: true,
            error: QuorumDataError::QuorumNotFound,
            body: Body::Empty,
            reads_dkgdb: false,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "vvec missing",
            block: QUORUM_BLOCK,
            mask: VVEC,
            pro_tx: VALID_MEMBER,
            local_mn: true,
            has_vvec: false,
            error: QuorumDataError::QuorumVerificationVectorMissing,
            body: Body::Empty,
            reads_dkgdb: false,
            scores_free: &[],
            scores_limited: &[],
        },
        Case {
            name: "vvec missing stops before the contributions",
            block: QUORUM_BLOCK,
            mask: VVEC | CONTRIB,
            pro_tx: VALID_MEMBER,
            local_mn: true,
            has_vvec: false,
            error: QuorumDataError::QuorumVerificationVectorMissing,
            body: Body::Empty,
            reads_dkgdb: false,
            scores_free: &[],
            scores_limited: &[],
        },
        Case {
            name: "contributions missing: no local masternode",
            block: QUORUM_BLOCK,
            mask: CONTRIB,
            pro_tx: VALID_MEMBER,
            local_mn: false,
            has_vvec: true,
            error: QuorumDataError::EncryptedContributionsMissing,
            body: Body::Empty,
            reads_dkgdb: false,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "contributions missing: no local masternode, vvec dropped",
            block: QUORUM_BLOCK,
            mask: VVEC | CONTRIB,
            pro_tx: VALID_MEMBER,
            local_mn: false,
            has_vvec: true,
            error: QuorumDataError::EncryptedContributionsMissing,
            body: Body::Empty,
            reads_dkgdb: false,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "contributions missing: no dkgdb rows",
            block: QUORUM_BLOCK,
            mask: CONTRIB,
            pro_tx: MEMBER_WITHOUT_ROWS,
            local_mn: true,
            has_vvec: true,
            error: QuorumDataError::EncryptedContributionsMissing,
            body: Body::Empty,
            reads_dkgdb: true,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "contributions missing: no local masternode, requester outside",
            block: QUORUM_BLOCK,
            mask: CONTRIB,
            pro_tx: OUTSIDER,
            local_mn: false,
            has_vvec: true,
            error: QuorumDataError::EncryptedContributionsMissing,
            body: Body::Empty,
            reads_dkgdb: false,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "contributions missing: no dkgdb rows, vvec dropped",
            block: QUORUM_BLOCK,
            mask: VVEC | CONTRIB,
            pro_tx: MEMBER_WITHOUT_ROWS,
            local_mn: true,
            has_vvec: true,
            error: QuorumDataError::EncryptedContributionsMissing,
            body: Body::Empty,
            reads_dkgdb: true,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "masternode is no member",
            block: QUORUM_BLOCK,
            mask: CONTRIB,
            pro_tx: OUTSIDER,
            local_mn: true,
            has_vvec: true,
            error: QuorumDataError::MasternodeIsNoMember,
            body: Body::Empty,
            reads_dkgdb: false,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "masternode is no member, vvec dropped",
            block: QUORUM_BLOCK,
            mask: VVEC | CONTRIB,
            pro_tx: OUTSIDER,
            local_mn: true,
            has_vvec: true,
            error: QuorumDataError::MasternodeIsNoMember,
            body: Body::Empty,
            reads_dkgdb: false,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "vvec only",
            block: QUORUM_BLOCK,
            mask: VVEC,
            pro_tx: OUTSIDER,
            local_mn: false,
            has_vvec: true,
            error: QuorumDataError::None,
            body: Body::Vvec,
            reads_dkgdb: false,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "contributions only",
            block: QUORUM_BLOCK,
            mask: CONTRIB,
            pro_tx: VALID_MEMBER,
            local_mn: true,
            has_vvec: false,
            error: QuorumDataError::None,
            body: Body::Contrib(VALID_MEMBER),
            reads_dkgdb: true,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "contributions of an invalid member",
            block: QUORUM_BLOCK,
            mask: CONTRIB,
            pro_tx: INVALID_MEMBER,
            local_mn: true,
            has_vvec: true,
            error: QuorumDataError::None,
            body: Body::Contrib(INVALID_MEMBER),
            reads_dkgdb: true,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "vvec then contributions",
            block: QUORUM_BLOCK,
            mask: VVEC | CONTRIB,
            pro_tx: VALID_MEMBER,
            local_mn: true,
            has_vvec: true,
            error: QuorumDataError::None,
            body: Body::Both(VALID_MEMBER),
            reads_dkgdb: true,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
        Case {
            name: "empty mask",
            block: QUORUM_BLOCK,
            mask: 0,
            pro_tx: VALID_MEMBER,
            local_mn: true,
            has_vvec: true,
            error: QuorumDataError::None,
            body: Body::Empty,
            reads_dkgdb: false,
            scores_free: &[],
            scores_limited: &[LIMIT],
        },
    ];

    #[test]
    fn every_branch_free_and_rate_limited() {
        for case in CASES {
            for limited in [false, true] {
                let name = format!("{} (limited: {limited})", case.name);
                let mut fx = Fx::new(case.has_vvec);
                fx.local_mn = case.local_mn;
                let q = req(case.block, case.mask, case.pro_tx);
                if limited {
                    fx.prime(&q);
                }

                let out = fx.serve(&q);

                let reply = reply_of(&out);
                assert_eq!(reply.request(), q, "{name}");
                assert_eq!(reply.error(), case.error, "{name}");
                assert_eq!(reply.data(), &case.body.bytes(), "{name}");
                let expected = if limited {
                    case.scores_limited
                } else {
                    case.scores_free
                };
                assert_eq!(out.scores, expected, "{name}");

                let calls = fx.dkgdb.calls.borrow();
                if case.reads_dkgdb {
                    assert_eq!(
                        *calls,
                        [(
                            test_type().to_base(),
                            block(case.block),
                            valid_bits(),
                            pro(case.pro_tx),
                            BlsSchemeKind::Basic
                        )],
                        "{name}"
                    );
                } else {
                    assert!(calls.is_empty(), "{name}");
                }

                let r = Request::from(&q);
                let entry = fx.tracker.entry(&key(pro(REQUESTER), &r)).expect("tracked");
                assert_eq!(
                    entry.time,
                    if limited {
                        NOW - 1
                    } else {
                        NOW
                    },
                    "{name}"
                );
                assert_eq!(entry.req.error, Request::ERROR_UNDEFINED, "{name}");
                assert!(!entry.processed, "{name}");
                assert_eq!(fx.tracker.inbound_count(), 1, "{name}");
            }
        }
    }

    /// A `qdata`'s wire bytes as Core writes them: the 67-byte request, the error, the body.
    fn qdata_bytes(block_n: u16, mask: u16, pro_tx: u16, error: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![test_type().to_base()];
        out.extend_from_slice(&raw(block_n));
        out.extend_from_slice(&mask.to_le_bytes());
        out.extend_from_slice(&raw(pro_tx));
        out.push(error);
        out.extend_from_slice(body);
        out
    }

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "the test pins the wire bytes with base-sdk's encoder"
    )]
    fn reply_is_the_request_the_error_then_the_body_on_the_wire() {
        for (pro_tx, error, body) in
            [(VALID_MEMBER, 0, Body::Both(VALID_MEMBER).bytes()), (OUTSIDER, 4, Vec::new())]
        {
            let mut fx = Fx::new(true);
            let out = fx.serve(&req(QUORUM_BLOCK, VVEC | CONTRIB, pro_tx));
            let wire = qdata_bytes(QUORUM_BLOCK, VVEC | CONTRIB, pro_tx, error, &body);
            let mut cursor = &wire[..];
            assert_eq!(QData::decode(&mut cursor).expect("qdata"), *reply_of(&out));
            assert!(cursor.is_empty());
            let mut encoded = Vec::new();
            reply_of(&out).encode(&mut encoded);
            assert_eq!(encoded, wire);
        }
    }

    #[test]
    fn an_invalid_vvec_element_is_written_as_48_zero_bytes() {
        for scheme in [BlsSchemeKind::Legacy, BlsSchemeKind::Basic] {
            let mut fx = Fx::new(true);
            fx.scheme = scheme;
            let quorum = &fx.source.quorums[&(test_type().to_base(), block(QUORUM_BLOCK))];
            *quorum.state().vvec.borrow_mut() =
                Some(Arc::new(vec![secret_to_public(&sk(1)), BlsPkValue::invalid()]));

            let out = fx.serve(&req(QUORUM_BLOCK, VVEC, OUTSIDER));

            let mut expected = vec![2];
            expected.extend_from_slice(&secret_to_public(&sk(1)).to_bytes(scheme));
            expected.extend_from_slice(&[0; 48]);
            assert_eq!(reply_of(&out).error(), QuorumDataError::None, "{scheme:?}");
            assert_eq!(reply_of(&out).data(), &expected, "{scheme:?}");
        }
    }

    #[test]
    fn vvec_is_written_under_the_given_scheme() {
        assert_ne!(vvec_body(BlsSchemeKind::Legacy), vvec_body(BlsSchemeKind::Basic));
        for scheme in [BlsSchemeKind::Legacy, BlsSchemeKind::Basic] {
            let mut fx = Fx::new(true);
            fx.scheme = scheme;
            let out = fx.serve(&req(QUORUM_BLOCK, VVEC | CONTRIB, VALID_MEMBER));
            let expected = [vvec_body(scheme), contrib_body(VALID_MEMBER)].concat();
            assert_eq!(reply_of(&out).data(), &expected, "{scheme:?}");
            assert_eq!(fx.dkgdb.calls.borrow()[0].4, scheme, "{scheme:?}");
        }
    }

    #[test]
    fn unregistered_type_replies_then_scores_100_untracked() {
        let unregistered = LlmqType::from_base(105);
        for (block_n, limited) in
            [(QUORUM_BLOCK, false), (QUORUM_BLOCK, true), (OFF_CHAIN_BLOCK, false)]
        {
            let mut fx = Fx::new(true);
            let q = req_of(unregistered, block_n, VVEC, pro(VALID_MEMBER));
            if limited {
                fx.prime(&q);
            }
            let before = fx.tracker.inbound_count();

            let out = fx.serve(&q);

            let reply = reply_of(&out);
            assert_eq!(reply.request(), q);
            assert_eq!(reply.error(), QuorumDataError::QuorumTypeInvalid);
            assert!(reply.data().is_empty());
            assert_eq!(out.scores, [(100, "invalid llmqType in QGETDATA")]);
            assert_eq!(fx.tracker.inbound_count(), before);
            assert_eq!(fx.lookups(), 0);
        }
    }

    #[test]
    fn block_not_on_the_active_chain_replies_unscored_untracked() {
        for limited in [false, true] {
            let mut fx = Fx::new(true);
            let q = req(OFF_CHAIN_BLOCK, VVEC, VALID_MEMBER);
            if limited {
                fx.prime(&q);
            }
            let before = fx.tracker.inbound_count();

            let out = fx.serve(&q);

            let reply = reply_of(&out);
            assert_eq!(reply.request(), q);
            assert_eq!(reply.error(), QuorumDataError::QuorumBlockNotFound);
            assert!(reply.data().is_empty());
            assert!(out.scores.is_empty());
            assert_eq!(fx.tracker.inbound_count(), before);
            assert_eq!(
                fx.tracker.entry(&key(pro(REQUESTER), &Request::from(&q))).is_some(),
                limited
            );
            assert_eq!(fx.lookups(), 0);
        }
    }

    /// Fills `requester`'s bucket with keys for other quorum hashes.
    fn fill(fx: &mut Fx, requester: ProTxHash, first_block: u16) {
        for n in 0..MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER as u16 {
            let r = Request::from(&req(first_block + n, VVEC, VALID_MEMBER));
            assert_eq!(
                fx.tracker.register(key(requester, &r), r, NOW, false),
                Registration::Accepted
            );
        }
    }

    #[test]
    fn requester_limit_scores_25_without_a_reply() {
        let mut fx = Fx::new(true);
        fill(&mut fx, pro(REQUESTER), 1000);
        let q = req(QUORUM_BLOCK, VVEC, VALID_MEMBER);

        let out = fx.serve(&q);

        assert_eq!(out.reply, None);
        assert_eq!(out.scores, [(25, "too many quorum data requests")]);
        assert_eq!(fx.tracker.inbound_count(), MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER);
        assert!(fx.tracker.entry(&key(pro(REQUESTER), &Request::from(&q))).is_none());
        assert_eq!(fx.lookups(), 0);
    }

    #[test]
    fn capacity_exhausted_is_silent() {
        let mut fx = Fx::new(true);
        fill(&mut fx, null(), 1000);
        let q = req(QUORUM_BLOCK, VVEC | CONTRIB, VALID_MEMBER);

        let out = fx.serve_at(null(), &q, NOW);

        assert_eq!(out, ServeOut::default());
        assert_eq!(fx.tracker.inbound_count(), MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER);
        assert_eq!(fx.tracker.inbound_count_for(&null()), MAX_INBOUND_DATA_REQUESTS_PER_REQUESTER);
        assert!(fx.tracker.entry(&key(null(), &Request::from(&q))).is_none());
        assert_eq!(fx.lookups(), 0);
        assert!(fx.dkgdb.calls.borrow().is_empty());
    }

    #[test]
    fn unverified_qwatch_requests_share_the_null_identity() {
        let mut fx = Fx::new(true);
        let q = req(QUORUM_BLOCK, VVEC, VALID_MEMBER);

        let out = fx.serve_at(null(), &q, NOW);

        assert_eq!(reply_of(&out).error(), QuorumDataError::None);
        assert!(fx.tracker.entry(&key(null(), &Request::from(&q))).is_some());
        assert_eq!(fx.tracker.inbound_count_for(&null()), 1);
    }

    #[test]
    fn registration_has_no_expiry_bias() {
        let q = req(NO_QUORUM_BLOCK, VVEC, VALID_MEMBER);
        for (delay, scores) in [(EXPIRATION_TIMEOUT - 1, vec![LIMIT]), (EXPIRATION_TIMEOUT, vec![])]
        {
            let mut fx = Fx::new(true);
            assert!(fx.serve(&q).scores.is_empty());
            let out = fx.serve_at(pro(REQUESTER), &q, NOW + delay);
            assert_eq!(out.scores, scores, "+{delay} s");
        }
    }

    #[test]
    fn quorum_read_error_has_no_reply_or_score_and_stays_registered() {
        for limited in [false, true] {
            let mut fx = Fx::new(true);
            fx.source.read_errors.insert((test_type().to_base(), block(QUORUM_BLOCK)));
            let q = req(QUORUM_BLOCK, VVEC | CONTRIB, VALID_MEMBER);
            if limited {
                fx.prime(&q);
            }

            let result = fx.try_serve_at(pro(REQUESTER), &q, NOW);

            assert_eq!(result, Err(FakeReadError), "limited: {limited}");
            let entry =
                fx.tracker.entry(&key(pro(REQUESTER), &Request::from(&q))).expect("tracked");
            assert_eq!(
                entry.time,
                if limited {
                    NOW - 1
                } else {
                    NOW
                }
            );
            assert_eq!(fx.tracker.inbound_count(), 1);
            assert_eq!(fx.lookups(), 1);
            assert!(fx.dkgdb.calls.borrow().is_empty());
        }
    }

    #[test]
    fn wire_request_converts_both_ways() {
        let wire = QuorumDataRequest::new(
            test_type(),
            QuorumHash::from(raw(7)),
            VVEC | CONTRIB,
            TxHash::from(raw(8)),
        );
        let r = Request::from(&wire);
        assert_eq!(r.llmq_type, test_type());
        assert_eq!(r.quorum_hash, block(7));
        assert_eq!(r.data_mask, VVEC | CONTRIB);
        assert_eq!(r.pro_tx, pro(8));
        assert_eq!(r.error, Request::ERROR_UNDEFINED);
        assert_eq!(QuorumDataRequest::from(&r), wire);
    }
}
