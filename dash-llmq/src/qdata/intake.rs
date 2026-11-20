//! Core's handling of a `qdata` that passed the handler's gates and whose header was read: the
//! response checks, the verification vector, the encrypted contributions and the write, in
//! Core's order. Each effect is emitted as it happens and stays emitted when a later read throws.

use dash_consensus::primitives::{ReadErrorKind, read_compact_size};
use dash_evo::types::ProTxHash;
use dash_types::Numeric;

use super::source::{OperatorDecrypt, QuorumData, QuorumDataSource};
use super::tracker::{Key, Request, ResponseCheck, Tracker};
use super::{Effect, PeerId, QuorumRef};
use crate::dkg::bls::{BlsPkValue, BlsSchemeKind, BlsSkValue, aggregate_secret_keys};
use crate::dkg::wire::{BlsIesBlobBytes, QData};

/// A read of the `qdata` body that Core's stream read throws on, as `std::ios_base::failure`
/// constructed with `msg`. The handler turns it into its own throw through that type's
/// `ios_failure(msg)`, which adds libstdc++'s suffix to give Core's `what()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Throw {
    pub msg: &'static str,
}

impl Throw {
    pub const fn ios_failure(msg: &'static str) -> Self {
        Self {
            msg,
        }
    }
}

// Core: v24.0.0 src/streams.h:287 DataStream::read
const END_OF_DATA: &str = "DataStream::read(): end of data";
// Core: v24.0.0 src/bls/bls.h:197 CBLSWrapper::Unserialize
const MALLEABLE: &str = "malleable BLS object";

/// What [`intake`] reads.
pub struct IntakeCx<'a, S> {
    pub source: &'a S,
    pub tracker: &'a mut Tracker,
    /// The sender's verified proRegTx hash.
    pub requester: ProTxHash,
    /// The sender's id, for the log lines.
    pub peer: PeerId,
    /// The local operator key; None when this node runs no masternode (Core's `!m_nodeman`).
    pub operator: Option<&'a dyn OperatorDecrypt>,
    /// The scheme of the node's view, under which the BLS elements are read.
    pub scheme: BlsSchemeKind,
}

/// Processes `msg`, a `qdata` from a verified peer, emitting each effect into `fx` in order.
// Core: v24.0.0 src/llmq/net_quorum.cpp:213-270 NetQuorum::ProcessMessage
pub fn intake<S: QuorumDataSource>(
    cx: IntakeCx<'_, S>,
    msg: &QData,
    fx: &mut dyn FnMut(Effect),
) -> Result<(), Throw> {
    let mut response = Request::from(&msg.request());
    response.error = msg.error().to_base();
    let key = Key {
        pro_reg_tx: cx.requester,
        we_requested: true,
        quorum_hash: response.quorum_hash,
        llmq_type: response.llmq_type,
    };
    match cx.tracker.validate_response(&key, &response) {
        ResponseCheck::NotRequested => {
            fx(Effect::Score(10, "not requested"));
            return Ok(());
        }
        ResponseCheck::AlreadyReceived => {
            fx(Effect::Score(10, "already received"));
            return Ok(());
        }
        ResponseCheck::Mismatch => {
            fx(Effect::Score(10, "not like requested"));
            return Ok(());
        }
        ResponseCheck::Ok => {}
    }

    if response.error != Request::ERROR_NONE {
        fx(Effect::Log(format!(
            "NetQuorum::ProcessMessage -- qdata: Error {} ({}), from peer={}",
            char::from(response.error),
            error_string(response.error),
            cx.peer
        )));
        return Ok(());
    }

    let Some(quorum) = cx.source.cached_quorum(response.llmq_type, &response.quorum_hash) else {
        fx(Effect::Log(format!(
            "NetQuorum::ProcessMessage -- qdata: Quorum not found, from peer={}",
            cx.peer
        )));
        return Ok(());
    };
    let quorum_ref = QuorumRef {
        llmq_type: response.llmq_type,
        quorum_hash: response.quorum_hash,
    };
    let mut body: &[u8] = msg.data();

    if response.data_mask & Request::QUORUM_VERIFICATION_VECTOR != 0 {
        let expected = quorum.threshold();
        let vvec = read_vector_with_max_size(&mut body, expected, |b| read_pk(b, cx.scheme))?;
        let Some(vvec) = vvec.filter(|v| v.len() == expected) else {
            fx(Effect::Score(100, "invalid quorum verification vector size"));
            return Ok(());
        };
        if quorum.set_vvec(vvec, cx.scheme) {
            fx(Effect::QueueWarm(quorum_ref));
        } else {
            fx(Effect::Score(10, "invalid quorum verification vector"));
            return Ok(());
        }
    }

    if !process_contrib_qdata(&quorum, &response, &mut body, &cx, fx)? {
        return Ok(());
    }

    fx(Effect::Write(quorum_ref));
    Ok(())
}

/// Reads the encrypted contributions for the requested member and sets the aggregated secret
/// key share. Returns whether the handler goes on to write.
// Core: v24.0.0 src/llmq/net_quorum.cpp:303-355 NetQuorum::ProcessContribQDATA
fn process_contrib_qdata<S, Q: QuorumData>(
    quorum: &Q,
    request: &Request,
    body: &mut &[u8],
    cx: &IntakeCx<'_, S>,
    fx: &mut dyn FnMut(Effect),
) -> Result<bool, Throw> {
    if request.data_mask & Request::ENCRYPTED_CONTRIBUTIONS == 0 {
        return Ok(true);
    }
    let Some(operator) = cx.operator else {
        return Ok(true);
    };

    if quorum.vvec().is_none_or(|v| v.len() != quorum.threshold()) {
        fx(Effect::Log(format!(
            "NetQuorum::ProcessContribQDATA -- qdata: No valid quorum verification vector available, from peer={}",
            cx.peer
        )));
        return Ok(false);
    }

    let Some(member_idx) = quorum.member_index(&request.pro_tx) else {
        fx(Effect::Log(format!(
            "NetQuorum::ProcessContribQDATA -- qdata: Not a member of the quorum, from peer={}",
            cx.peer
        )));
        return Ok(false);
    };

    let expected = quorum.valid_count();
    let encrypted = read_vector_with_max_size(body, expected, |b| read_blob(b, cx.scheme))?;
    let Some(encrypted) = encrypted.filter(|v| v.len() == expected) else {
        fx(Effect::Score(100, "invalid encrypted contribution vector size"));
        return Ok(false);
    };

    let mut shares = Vec::with_capacity(encrypted.len());
    for blob in &encrypted {
        match operator.decrypt_sk(blob, member_idx, cx.scheme) {
            Some(sk) if sk.is_valid() => shares.push(sk),
            _ => {
                fx(Effect::Score(10, "invalid contribution share"));
                return Ok(false);
            }
        }
    }

    let refs: Vec<&BlsSkValue> = shares.iter().collect();
    let me = operator.pro_tx_hash().unwrap_or_else(|| ProTxHash::from([0; 32]));
    if !quorum.set_sk_share(aggregate_secret_keys(&refs, cx.scheme), &me) {
        fx(Effect::Score(10, "invalid secret key share received"));
        return Ok(false);
    }
    Ok(true)
}

// Core: v24.0.0 src/llmq/quorums.cpp:78-101 CQuorumDataRequest::GetErrorString
fn error_string(error: u8) -> &'static str {
    match error {
        Request::ERROR_NONE => "NONE",
        Request::ERROR_QUORUM_TYPE_INVALID => "QUORUM_TYPE_INVALID",
        Request::ERROR_QUORUM_BLOCK_NOT_FOUND => "QUORUM_BLOCK_NOT_FOUND",
        Request::ERROR_QUORUM_NOT_FOUND => "QUORUM_NOT_FOUND",
        Request::ERROR_MASTERNODE_IS_NO_MEMBER => "MASTERNODE_IS_NO_MEMBER",
        Request::ERROR_QUORUM_VERIFICATION_VECTOR_MISSING => "QUORUM_VERIFICATION_VECTOR_MISSING",
        Request::ERROR_ENCRYPTED_CONTRIBUTIONS_MISSING => "ENCRYPTED_CONTRIBUTIONS_MISSING",
        _ => "UNDEFINED",
    }
}

/// None when the count is over `max_size`, before any element is read. Unlike the wire
/// codec's raw readers, each element is read here with Core's scheme check and throw text.
// Core: v24.0.0 src/serialize.h:999-1009 UnserializeVectorWithMaxSize
fn read_vector_with_max_size<T>(
    input: &mut &[u8],
    max_size: usize,
    mut read: impl FnMut(&mut &[u8]) -> Result<T, Throw>,
) -> Result<Option<Vec<T>>, Throw> {
    let size = read_compact_size(input, true).map_err(read_throw)?;
    if size > max_size as u64 {
        return Ok(None);
    }
    (0..size).map(|_| read(input)).collect::<Result<Vec<_>, _>>().map(Some)
}

fn read_throw(kind: ReadErrorKind) -> Throw {
    Throw::ios_failure(match kind {
        ReadErrorKind::EndOfData => END_OF_DATA,
        ReadErrorKind::NonCanonicalCompactSize => "non-canonical ReadCompactSize()",
        ReadErrorKind::CompactSizeTooLarge => "ReadCompactSize(): size too large",
        ReadErrorKind::VarIntTooLarge => "ReadVarInt(): size too large",
    })
}

fn take<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8], Throw> {
    if input.len() < n {
        *input = &[];
        return Err(Throw::ios_failure(END_OF_DATA));
    }
    let (head, rest) = input.split_at(n);
    *input = rest;
    Ok(head)
}

fn take_array<const N: usize>(input: &mut &[u8]) -> Result<[u8; N], Throw> {
    let mut out = [0; N];
    out.copy_from_slice(take(input, N)?);
    Ok(out)
}

/// A public key read under `scheme`: all zeros, or a key canonical only under the other
/// scheme, is the invalid key; bytes canonical under neither throw.
// Core: v24.0.0 src/bls/bls.h:184-203 CBLSWrapper::Unserialize
fn read_pk(input: &mut &[u8], scheme: BlsSchemeKind) -> Result<BlsPkValue, Throw> {
    let raw = take_array::<48>(input)?;
    if let Some(pk) = BlsPkValue::from_canonical(&raw, scheme) {
        return Ok(pk);
    }
    if raw == [0; 48] || BlsPkValue::from_canonical(&raw, scheme.opposite()).is_some() {
        return Ok(BlsPkValue::invalid());
    }
    Err(Throw::ios_failure(MALLEABLE))
}

/// An encrypted contribution: the ephemeral key read under `scheme`, the IV seed and the
/// ciphertext.
// Core: v24.0.0 src/bls/bls_ies.h:25-28 CBLSIESEncryptedBlob::SERIALIZE_METHODS
fn read_blob(input: &mut &[u8], scheme: BlsSchemeKind) -> Result<BlsIesBlobBytes, Throw> {
    let ephemeral_pk = read_pk(input, scheme)?;
    let iv_seed = take_array::<32>(input)?;
    let len = read_compact_size(input, true).map_err(read_throw)?;
    let data = take(input, usize::try_from(len).unwrap_or(usize::MAX))?.to_vec();
    Ok(BlsIesBlobBytes::new(ephemeral_pk.to_bytes(scheme).into(), iv_seed, data))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_consensus::primitives::BlockHash;
    use dash_params::LlmqType;

    use super::*;
    use crate::dkg::bls::secret_to_public;
    use crate::dkg::wire::{QuorumDataError, QuorumDataRequest};
    use crate::qdata::fakes::{FakeOperator, FakeQuorum, FakeQuorumState, FakeSource};
    use crate::qdata::tracker::{Registration, Status};

    const NOW: i64 = 1_000_000;
    const PEER: PeerId = 7;
    const SCHEME: BlsSchemeKind = BlsSchemeKind::Basic;
    const VVEC: u16 = Request::QUORUM_VERIFICATION_VECTOR;
    const CONTRIB: u16 = Request::ENCRYPTED_CONTRIBUTIONS;

    const SENDER: u16 = 1;
    /// The quorum's members: valid, invalid, valid (this node), valid, valid. The threshold,
    /// the valid-member count and the size all differ, so a check on the wrong one fails.
    const MEMBER_A: u16 = 10;
    const MEMBER_B: u16 = 11;
    const ME: u16 = 12;
    const MEMBER_C: u16 = 13;
    const MEMBER_D: u16 = 14;
    const ME_INDEX: usize = 2;
    const OUTSIDER: u16 = 99;
    const THRESHOLD: u8 = 3;
    const VALID: u8 = 4;
    const SIZE: usize = 5;
    /// The contributions dealt to this node, one per valid member, and their shares.
    const BLOBS: [u8; VALID as usize] = [20, 21, 22, 23];
    const SHARES: [u8; VALID as usize] = [5, 6, 7, 8];
    const BLOB_1: u8 = BLOBS[0];
    const BLOB_2: u8 = BLOBS[1];
    const SHARE_1: u8 = SHARES[0];

    /// A body whose read throws at its first byte: a compact size cut short.
    const UNREADABLE: &[u8] = &[0xfd];

    fn raw(n: u16) -> [u8; 32] {
        let mut b = [0; 32];
        b[..2].copy_from_slice(&n.to_be_bytes());
        b[31] = 0x5a;
        b
    }

    fn pro(n: u16) -> ProTxHash {
        ProTxHash::from(raw(n))
    }

    fn quorum_hash() -> BlockHash {
        BlockHash::from(raw(1000))
    }

    fn sk(n: u8) -> BlsSkValue {
        let mut bytes = [0; 32];
        bytes[31] = n;
        BlsSkValue::from_canonical(&bytes, SCHEME).expect("canonical scalar")
    }

    fn sk_bytes(sk: &BlsSkValue) -> [u8; 32] {
        sk.to_bytes(SCHEME)
    }

    fn keys(n: u8) -> Vec<BlsPkValue> {
        (1..=n).map(|i| secret_to_public(&sk(i))).collect()
    }

    fn vvec() -> Vec<BlsPkValue> {
        keys(THRESHOLD)
    }

    fn share_sum(shares: &[u8]) -> BlsSkValue {
        sk(shares.iter().sum())
    }

    fn pk_bytes(pks: &[BlsPkValue], scheme: BlsSchemeKind) -> Vec<Vec<u8>> {
        pks.iter().map(|pk| pk.to_bytes(scheme).to_vec()).collect()
    }

    /// A vector on the wire: `count`, then `elements` as they are.
    fn vector(count: u8, elements: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![count];
        for e in elements {
            out.extend_from_slice(e);
        }
        out
    }

    fn vvec_body() -> Vec<u8> {
        vector(THRESHOLD, &pk_bytes(&vvec(), SCHEME))
    }

    fn ciphertext(n: u8) -> Vec<u8> {
        vec![n; 3]
    }

    /// An encrypted contribution's wire bytes: ephemeral key, IV seed, then the ciphertext.
    fn blob_bytes(n: u8) -> Vec<u8> {
        blob_bytes_in(n, SCHEME)
    }

    fn blob_bytes_in(n: u8, scheme: BlsSchemeKind) -> Vec<u8> {
        let mut out = secret_to_public(&sk(n)).to_bytes(scheme).to_vec();
        out.extend_from_slice(&[n; 32]);
        out.push(3);
        out.extend(ciphertext(n));
        out
    }

    fn contrib_body() -> Vec<u8> {
        vector(VALID, &BLOBS.map(blob_bytes))
    }

    fn concat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    fn quorum_state(vvec: Option<Vec<BlsPkValue>>) -> FakeQuorumState {
        FakeQuorumState {
            threshold: usize::from(THRESHOLD),
            members: vec![pro(MEMBER_A), pro(MEMBER_B), pro(ME), pro(MEMBER_C), pro(MEMBER_D)],
            valid: vec![true, false, true, true, true],
            vvec: vvec.map(Arc::new).into(),
            accept_vvec: true,
            accept_sk_share: true,
            ..FakeQuorumState::default()
        }
    }

    fn operator() -> FakeOperator {
        FakeOperator {
            pro_tx: Some(pro(ME)),
            shares: BLOBS
                .into_iter()
                .zip(SHARES)
                .map(|(blob, share)| ((ciphertext(blob), ME_INDEX), sk(share)))
                .collect(),
            ..FakeOperator::default()
        }
    }

    fn request(mask: u16, member: u16) -> Request {
        Request {
            llmq_type: LlmqType::LlmqTest,
            quorum_hash: quorum_hash(),
            data_mask: mask,
            pro_tx: pro(member),
            error: Request::ERROR_UNDEFINED,
        }
    }

    fn outbound_key() -> Key {
        Key {
            pro_reg_tx: pro(SENDER),
            we_requested: true,
            quorum_hash: quorum_hash(),
            llmq_type: LlmqType::LlmqTest,
        }
    }

    fn qdata(mask: u16, member: u16, body: Vec<u8>) -> QData {
        QData::new(QuorumDataRequest::from(&request(mask, member)), QuorumDataError::None, body)
    }

    fn warm_write() -> Vec<Effect> {
        vec![Effect::QueueWarm(quorum_ref()), Effect::Write(quorum_ref())]
    }

    struct Fx {
        source: FakeSource,
        quorum: FakeQuorum,
        tracker: Tracker,
        operator: Option<FakeOperator>,
        scheme: BlsSchemeKind,
    }

    impl Fx {
        /// A cached quorum, this node's operator key, and nothing requested yet.
        fn new(state: FakeQuorumState) -> Self {
            let quorum = FakeQuorum::from(state);
            let mut source = FakeSource::default();
            source.cached.insert((LlmqType::LlmqTest.to_base(), quorum_hash()), quorum.clone());
            Self {
                source,
                quorum,
                tracker: Tracker::new(),
                operator: Some(operator()),
                scheme: SCHEME,
            }
        }

        /// Registers our request for `mask` of `member`'s data, as the recovery poll does.
        fn asked(mut self, mask: u16, member: u16) -> Self {
            let accepted = self.tracker.register(outbound_key(), request(mask, member), NOW, true);
            assert_eq!(accepted, Registration::Accepted);
            self
        }

        fn run(&mut self, msg: &QData) -> (Vec<Effect>, Result<(), Throw>) {
            let mut effects = Vec::new();
            let result = intake(
                IntakeCx {
                    source: &self.source,
                    tracker: &mut self.tracker,
                    requester: pro(SENDER),
                    peer: PEER,
                    operator: self.operator.as_ref().map(|o| o as &dyn OperatorDecrypt),
                    scheme: self.scheme,
                },
                msg,
                &mut |e| effects.push(e),
            );
            (effects, result)
        }

        fn vvec_calls(&self) -> usize {
            self.quorum.state().set_vvec_calls.borrow().len()
        }

        fn decrypts(&self) -> Vec<(Vec<u8>, usize, BlsSchemeKind)> {
            self.operator.as_ref().map_or_else(Vec::new, |o| {
                o.calls.borrow().iter().map(|(b, i, s)| (b.to_bytes(), *i, *s)).collect()
            })
        }

        fn decrypt_calls(&self) -> usize {
            self.decrypts().len()
        }

        fn sk_share_calls(&self) -> Vec<([u8; 32], ProTxHash)> {
            let calls = self.quorum.state().set_sk_share_calls.borrow();
            calls.iter().map(|(sk, me)| (sk_bytes(sk), *me)).collect()
        }
    }

    fn quorum_ref() -> QuorumRef {
        QuorumRef {
            llmq_type: LlmqType::LlmqTest,
            quorum_hash: quorum_hash(),
        }
    }

    fn log(text: &str) -> Effect {
        Effect::Log(text.to_owned())
    }

    fn end_of_data() -> Result<(), Throw> {
        Err(Throw::ios_failure("DataStream::read(): end of data"))
    }

    fn malleable() -> Result<(), Throw> {
        Err(Throw::ios_failure("malleable BLS object"))
    }

    /// A key's bytes in the other scheme that are not canonical in [`SCHEME`].
    fn other_scheme_only() -> [u8; 48] {
        (1..=u8::MAX)
            .map(|n| secret_to_public(&sk(n)).to_bytes(SCHEME.opposite()))
            .find(|raw| BlsPkValue::from_canonical(raw, SCHEME).is_none())
            .expect("a key whose encodings differ")
    }

    /// Bytes canonical in neither scheme.
    fn unreadable_pk() -> Vec<u8> {
        let raw = [0xff; 48];
        assert!(BlsPkValue::from_canonical(&raw, SCHEME).is_none());
        assert!(BlsPkValue::from_canonical(&raw, SCHEME.opposite()).is_none());
        raw.to_vec()
    }

    #[test]
    fn the_fixture_separates_threshold_valid_count_and_size() {
        let q = FakeQuorum::from(quorum_state(None));
        assert_eq!((q.threshold(), q.valid_count(), q.size()), (3, 4, SIZE));
    }

    // The three response checks come before any body read: each body here throws if read.
    #[test]
    fn response_checks_score_10_before_the_body_is_read() {
        let mut fx = Fx::new(quorum_state(None));
        let msg = qdata(VVEC, ME, UNREADABLE.to_vec());
        assert_eq!(fx.run(&msg), (vec![Effect::Score(10, "not requested")], Ok(())));

        let mut fx = Fx::new(quorum_state(None)).asked(VVEC, ME);
        let mismatched = qdata(VVEC | CONTRIB, ME, UNREADABLE.to_vec());
        assert_eq!(fx.run(&mismatched), (vec![Effect::Score(10, "not like requested")], Ok(())));
        let other_member = qdata(VVEC, MEMBER_A, UNREADABLE.to_vec());
        assert_eq!(fx.run(&other_member), (vec![Effect::Score(10, "not like requested")], Ok(())));
        assert_eq!(fx.tracker.status(&outbound_key()), Status::Pending);

        assert_eq!(fx.run(&qdata(VVEC, ME, vvec_body())).1, Ok(()));
        assert_eq!(fx.run(&msg), (vec![Effect::Score(10, "already received")], Ok(())));
        assert_eq!(fx.vvec_calls(), 1);
    }

    #[test]
    fn a_registered_inbound_request_does_not_answer_for_a_response() {
        let mut fx = Fx::new(quorum_state(None));
        let inbound = Key {
            we_requested: false,
            ..outbound_key()
        };
        fx.tracker.register(inbound, request(VVEC, ME), NOW, false);
        let msg = qdata(VVEC, ME, UNREADABLE.to_vec());
        assert_eq!(fx.run(&msg), (vec![Effect::Score(10, "not requested")], Ok(())));
    }

    // Core prints the error byte with `%d` from a `uint8_t`-based enum, which tinyformat passes
    // to `operator<<` as the character; the logger then escapes control characters.
    #[test]
    fn an_error_response_logs_only_and_marks_the_request_processed() {
        let cases = [
            (QuorumDataError::QuorumTypeInvalid, "\u{1} (QUORUM_TYPE_INVALID)"),
            (QuorumDataError::QuorumBlockNotFound, "\u{2} (QUORUM_BLOCK_NOT_FOUND)"),
            (QuorumDataError::QuorumNotFound, "\u{3} (QUORUM_NOT_FOUND)"),
            (QuorumDataError::MasternodeIsNoMember, "\u{4} (MASTERNODE_IS_NO_MEMBER)"),
            (
                QuorumDataError::QuorumVerificationVectorMissing,
                "\u{5} (QUORUM_VERIFICATION_VECTOR_MISSING)",
            ),
            (
                QuorumDataError::EncryptedContributionsMissing,
                "\u{6} (ENCRYPTED_CONTRIBUTIONS_MISSING)",
            ),
            (QuorumDataError::Unknown(0x41), "A (UNDEFINED)"),
            (QuorumDataError::Unknown(0x0a), "\n (UNDEFINED)"),
            // Core writes the raw byte 0x80; a String holds it as U+0080.
            (QuorumDataError::Unknown(0x80), "\u{80} (UNDEFINED)"),
            (QuorumDataError::Undefined, "\u{ff} (UNDEFINED)"),
        ];
        for (error, shown) in cases {
            let mut fx = Fx::new(quorum_state(None)).asked(VVEC | CONTRIB, ME);
            let req = QuorumDataRequest::from(&request(VVEC | CONTRIB, ME));
            let msg = QData::new(req, error, UNREADABLE.to_vec());
            let line = format!("NetQuorum::ProcessMessage -- qdata: Error {shown}, from peer=7");
            assert_eq!(fx.run(&msg), (vec![log(&line)], Ok(())), "{error:?}");
            assert_eq!(fx.tracker.status(&outbound_key()), Status::Processed);
            assert_eq!((fx.vvec_calls(), fx.decrypt_calls()), (0, 0));
        }
    }

    // The error code is checked before the quorum is looked up (nq:231-241).
    #[test]
    fn an_error_response_for_a_quorum_not_cached_logs_the_error() {
        let mut fx = Fx::new(quorum_state(None)).asked(VVEC, ME);
        fx.source.cached.clear();
        let req = QuorumDataRequest::from(&request(VVEC, ME));
        let msg = QData::new(req, QuorumDataError::QuorumNotFound, UNREADABLE.to_vec());
        let line =
            "NetQuorum::ProcessMessage -- qdata: Error \u{3} (QUORUM_NOT_FOUND), from peer=7";
        assert_eq!(fx.run(&msg), (vec![log(line)], Ok(())));
    }

    #[test]
    fn a_quorum_not_cached_logs_unscored() {
        let mut fx = Fx::new(quorum_state(None)).asked(VVEC, ME);
        fx.source.cached.clear();
        let msg = qdata(VVEC, ME, UNREADABLE.to_vec());
        let line = "NetQuorum::ProcessMessage -- qdata: Quorum not found, from peer=7";
        assert_eq!(fx.run(&msg), (vec![log(line)], Ok(())));
        assert_eq!(fx.tracker.status(&outbound_key()), Status::Processed);
    }

    #[test]
    fn a_verification_vector_is_set_then_warmed_then_written() {
        let mut fx = Fx::new(quorum_state(None)).asked(VVEC, ME);
        let msg = qdata(VVEC, ME, vvec_body());
        assert_eq!(fx.run(&msg), (warm_write(), Ok(())));
        let calls = fx.quorum.state().set_vvec_calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(pk_bytes(&calls[0].0, SCHEME), pk_bytes(&vvec(), SCHEME));
        assert_eq!(calls[0].1, SCHEME);
    }

    // The count is refused before any element is read: the elements here throw if read. The
    // count is the valid-member count, within a bound on the wrong value.
    #[test]
    fn a_vvec_count_over_the_threshold_scores_100_without_reading_an_element() {
        let mut fx = Fx::new(quorum_state(None)).asked(VVEC, ME);
        let body = vector(THRESHOLD + 1, &vec![unreadable_pk(); usize::from(THRESHOLD + 1)]);
        let msg = qdata(VVEC, ME, body);
        let score = Effect::Score(100, "invalid quorum verification vector size");
        assert_eq!(fx.run(&msg), (vec![score], Ok(())));
        assert_eq!(fx.vvec_calls(), 0);
    }

    // An under-count is read in full first, so its elements' throws come before the score.
    #[test]
    fn a_vvec_count_under_the_threshold_scores_100_after_reading_its_elements() {
        let mut fx = Fx::new(quorum_state(None)).asked(VVEC, ME);
        let body = vector(1, &pk_bytes(&vvec()[..1], SCHEME));
        let msg = qdata(VVEC, ME, body);
        let score = Effect::Score(100, "invalid quorum verification vector size");
        assert_eq!(fx.run(&msg), (vec![score], Ok(())));
        assert_eq!(fx.vvec_calls(), 0);

        let mut fx = Fx::new(quorum_state(None)).asked(VVEC, ME);
        let msg = qdata(VVEC, ME, vector(1, &[unreadable_pk()]));
        assert_eq!(fx.run(&msg), (vec![], malleable()));
    }

    #[test]
    fn a_vvec_that_does_not_hash_to_the_commitment_scores_10() {
        let mut fx = Fx::new(FakeQuorumState {
            accept_vvec: false,
            ..quorum_state(None)
        })
        .asked(VVEC | CONTRIB, ME);
        let msg = qdata(VVEC | CONTRIB, ME, concat(&[&vvec_body(), &contrib_body()]));
        let score = Effect::Score(10, "invalid quorum verification vector");
        assert_eq!(fx.run(&msg), (vec![score], Ok(())));
        assert_eq!(fx.decrypt_calls(), 0);
    }

    #[test]
    fn vvec_elements_follow_cores_scheme_read() {
        let mut fx = Fx::new(quorum_state(None)).asked(VVEC, ME);
        let body = vector(THRESHOLD, &[vec![0; 48], other_scheme_only().to_vec(), vec![0; 48]]);
        let msg = qdata(VVEC, ME, body);
        assert_eq!(fx.run(&msg).1, Ok(()));
        let calls = fx.quorum.state().set_vvec_calls.borrow();
        assert!(
            calls[0].0.iter().all(|pk| !pk.is_valid()),
            "zeros and the other scheme read as the invalid key"
        );

        for (body, expected) in [
            (
                vector(THRESHOLD, &[pk_bytes(&vvec(), SCHEME)[0].clone(), unreadable_pk()]),
                malleable(),
            ),
            (
                vector(THRESHOLD, &[pk_bytes(&vvec(), SCHEME)[0].clone(), vec![0; 47]]),
                end_of_data(),
            ),
        ] {
            let mut fx = Fx::new(quorum_state(None)).asked(VVEC, ME);
            let msg = qdata(VVEC, ME, body);
            assert_eq!(fx.run(&msg), (vec![], expected));
            assert_eq!(fx.vvec_calls(), 0);
        }
    }

    #[test]
    fn a_vvec_count_read_throws_as_cores_read_compact_size() {
        for (body, what) in [
            (vec![], "DataStream::read(): end of data"),
            (vec![0xfd, 0x02, 0x00], "non-canonical ReadCompactSize()"),
            (vec![0xfe, 0x01, 0x00, 0x00, 0x02], "ReadCompactSize(): size too large"),
        ] {
            let mut fx = Fx::new(quorum_state(None)).asked(VVEC, ME);
            let msg = qdata(VVEC, ME, body);
            assert_eq!(fx.run(&msg), (vec![], Err(Throw::ios_failure(what))));
        }
    }

    #[test]
    fn a_vvec_and_contributions_set_the_aggregated_share_then_write() {
        let mut fx = Fx::new(quorum_state(None)).asked(VVEC | CONTRIB, ME);
        let body = concat(&[&vvec_body(), &contrib_body()]);
        let msg = qdata(VVEC | CONTRIB, ME, body);
        assert_eq!(fx.run(&msg), (warm_write(), Ok(())));

        let expected: Vec<_> =
            BLOBS.into_iter().map(|n| (blob_bytes(n), ME_INDEX, SCHEME)).collect();
        assert_eq!(fx.decrypts(), expected);
        assert_eq!(fx.sk_share_calls(), vec![(sk_bytes(&share_sum(&SHARES)), pro(ME))]);
    }

    // The contributions use the quorum's own vvec when the response carries none.
    #[test]
    fn contributions_alone_use_the_stored_vvec() {
        let mut fx = Fx::new(quorum_state(Some(vvec()))).asked(CONTRIB, ME);
        let msg = qdata(CONTRIB, ME, contrib_body());
        assert_eq!(fx.run(&msg), (vec![Effect::Write(quorum_ref())], Ok(())));
        assert_eq!(fx.sk_share_calls().len(), 1);
    }

    // Core's `!m_nodeman`: the contributions are neither read nor decrypted, and the write
    // still happens.
    #[test]
    fn without_an_operator_key_contributions_are_skipped_and_written() {
        let mut fx = Fx::new(quorum_state(None)).asked(VVEC | CONTRIB, ME);
        fx.operator = None;
        let msg = qdata(VVEC | CONTRIB, ME, concat(&[&vvec_body(), UNREADABLE]));
        assert_eq!(fx.run(&msg), (warm_write(), Ok(())));

        let mut fx = Fx::new(quorum_state(None)).asked(CONTRIB, ME);
        fx.operator = None;
        let msg = qdata(CONTRIB, ME, UNREADABLE.to_vec());
        assert_eq!(fx.run(&msg), (vec![Effect::Write(quorum_ref())], Ok(())));
        assert!(fx.sk_share_calls().is_empty());
    }

    #[test]
    fn contributions_without_a_valid_vvec_log_and_do_not_write() {
        let line = "NetQuorum::ProcessContribQDATA -- qdata: No valid quorum verification vector available, from peer=7";
        for stored in [None, Some(keys(1)), Some(keys(VALID))] {
            let mut fx = Fx::new(quorum_state(stored)).asked(CONTRIB, ME);
            let msg = qdata(CONTRIB, ME, UNREADABLE.to_vec());
            assert_eq!(fx.run(&msg), (vec![log(line)], Ok(())));
            assert_eq!(fx.decrypt_calls(), 0);
        }
    }

    // The stored vvec is checked before the membership (nq:314-326).
    #[test]
    fn contributions_for_a_non_member_without_a_vvec_log_the_vvec() {
        let mut fx = Fx::new(quorum_state(None)).asked(CONTRIB, OUTSIDER);
        let msg = qdata(CONTRIB, OUTSIDER, UNREADABLE.to_vec());
        let line = "NetQuorum::ProcessContribQDATA -- qdata: No valid quorum verification vector available, from peer=7";
        assert_eq!(fx.run(&msg), (vec![log(line)], Ok(())));
    }

    #[test]
    fn contributions_for_a_non_member_log_and_do_not_write() {
        let mut fx = Fx::new(quorum_state(None)).asked(VVEC | CONTRIB, OUTSIDER);
        let body = concat(&[&vvec_body(), UNREADABLE]);
        let msg = qdata(VVEC | CONTRIB, OUTSIDER, body);
        let line =
            "NetQuorum::ProcessContribQDATA -- qdata: Not a member of the quorum, from peer=7";
        assert_eq!(fx.run(&msg), (vec![Effect::QueueWarm(quorum_ref()), log(line)], Ok(())));
    }

    // An invalid member's contributions are still decrypted at its index, as Core's
    // `GetMemberIndex` counts every member.
    #[test]
    fn an_invalid_member_is_a_member() {
        let mut fx = Fx::new(quorum_state(Some(vvec()))).asked(CONTRIB, MEMBER_B);
        let msg = qdata(CONTRIB, MEMBER_B, contrib_body());
        let score = Effect::Score(10, "invalid contribution share");
        assert_eq!(fx.run(&msg), (vec![score], Ok(())));
        assert_eq!(fx.decrypts()[0].1, 1);
    }

    #[test]
    fn a_contribution_count_other_than_the_valid_members_scores_100() {
        let over = vector(VALID + 1, &vec![unreadable_pk(); usize::from(VALID + 1)]);
        let first: Vec<_> =
            BLOBS[..usize::from(THRESHOLD)].iter().map(|n| blob_bytes(*n)).collect();
        let at_threshold = vector(THRESHOLD, &first);
        let under = vector(1, &[blob_bytes(BLOB_1)]);
        for body in [over, at_threshold, under] {
            let mut fx = Fx::new(quorum_state(None)).asked(VVEC | CONTRIB, ME);
            let msg = qdata(VVEC | CONTRIB, ME, concat(&[&vvec_body(), &body]));
            let score = Effect::Score(100, "invalid encrypted contribution vector size");
            assert_eq!(fx.run(&msg), (vec![Effect::QueueWarm(quorum_ref()), score], Ok(())));
            assert_eq!(fx.decrypt_calls(), 0);
        }
    }

    #[test]
    fn an_undecryptable_or_invalid_share_scores_10() {
        for share in [None, Some(BlsSkValue::invalid())] {
            let mut fx = Fx::new(quorum_state(Some(vvec()))).asked(CONTRIB, ME);
            let op = fx.operator.as_mut().expect("operator");
            op.shares.remove(&(ciphertext(BLOB_1), ME_INDEX));
            if let Some(share) = share {
                op.shares.insert((ciphertext(BLOB_1), ME_INDEX), share);
            }
            let msg = qdata(CONTRIB, ME, contrib_body());
            let score = Effect::Score(10, "invalid contribution share");
            assert_eq!(fx.run(&msg), (vec![score], Ok(())));
            assert_eq!(fx.decrypt_calls(), 1, "the first bad share stops the loop");
            assert!(fx.sk_share_calls().is_empty());
        }
    }

    // v24.0.0 sums the shares dealt to one member without refusing equal ones.
    #[test]
    fn equal_shares_are_summed() {
        let mut fx = Fx::new(quorum_state(Some(vvec()))).asked(CONTRIB, ME);
        let op = fx.operator.as_mut().expect("operator");
        op.shares.insert((ciphertext(BLOB_2), ME_INDEX), sk(SHARE_1));
        let msg = qdata(CONTRIB, ME, contrib_body());
        assert_eq!(fx.run(&msg), (vec![Effect::Write(quorum_ref())], Ok(())));
        let dealt = [SHARE_1, SHARE_1, SHARES[2], SHARES[3]];
        assert_eq!(fx.sk_share_calls(), vec![(sk_bytes(&share_sum(&dealt)), pro(ME))]);
    }

    #[test]
    fn a_share_that_does_not_match_the_vvec_scores_10() {
        let mut fx = Fx::new(FakeQuorumState {
            accept_sk_share: false,
            ..quorum_state(Some(vvec()))
        })
        .asked(CONTRIB, ME);
        let msg = qdata(CONTRIB, ME, contrib_body());
        let score = Effect::Score(10, "invalid secret key share received");
        assert_eq!(fx.run(&msg), (vec![score], Ok(())));
    }

    // Core passes `m_nodeman->GetProTxHash()`, null before the registration is known.
    #[test]
    fn the_share_is_set_for_the_local_pro_tx() {
        let mut fx = Fx::new(quorum_state(Some(vvec()))).asked(CONTRIB, ME);
        fx.operator.as_mut().expect("operator").pro_tx = None;
        let msg = qdata(CONTRIB, ME, contrib_body());
        assert_eq!(fx.run(&msg).1, Ok(()));
        assert_eq!(fx.sk_share_calls()[0].1, ProTxHash::from([0; 32]));
    }

    // The warm-up queued after the vvec stays queued when the contributions' read throws.
    #[test]
    fn a_valid_vvec_then_truncated_contributions_warms_then_throws() {
        let mut contributions = contrib_body();
        contributions.pop();
        for (body, expected) in [
            (concat(&[&vvec_body(), &contributions]), end_of_data()),
            (
                concat(&[&vvec_body(), &vector(VALID, &[blob_bytes(BLOB_1), unreadable_pk()])]),
                malleable(),
            ),
            (
                concat(&[&vvec_body(), &[0xfd, 0x02, 0x00]]),
                Err(Throw::ios_failure("non-canonical ReadCompactSize()")),
            ),
            (
                concat(&[
                    &vvec_body(),
                    &[1],
                    &blob_bytes(BLOB_1)[..80],
                    &[0xfe, 0x01, 0x00, 0x00, 0x02],
                ]),
                Err(Throw::ios_failure("ReadCompactSize(): size too large")),
            ),
        ] {
            let mut fx = Fx::new(quorum_state(None)).asked(VVEC | CONTRIB, ME);
            let msg = qdata(VVEC | CONTRIB, ME, body);
            assert_eq!(fx.run(&msg), (vec![Effect::QueueWarm(quorum_ref())], expected));
            assert_eq!(fx.decrypt_calls(), 0);
            assert!(fx.sk_share_calls().is_empty());
        }
    }

    // An ephemeral key canonical only in the other scheme is read as the invalid key, and the
    // blob is handed on with that key.
    #[test]
    fn an_ephemeral_key_in_the_other_scheme_is_the_invalid_key() {
        let mut fx = Fx::new(quorum_state(Some(vvec()))).asked(CONTRIB, ME);
        let mut first = other_scheme_only().to_vec();
        first.extend_from_slice(&blob_bytes(BLOB_1)[48..]);
        let rest = BLOBS[1..].iter().map(|n| blob_bytes(*n));
        let blobs: Vec<_> = std::iter::once(first).chain(rest).collect();
        let msg = qdata(CONTRIB, ME, vector(VALID, &blobs));
        assert_eq!(fx.run(&msg).1, Ok(()));
        let op = fx.operator.as_ref().expect("operator");
        assert_eq!(op.calls.borrow()[0].0.ephemeral_pk().to_bytes(), [0u8; 48]);
    }

    // A view on the legacy scheme reads the vvec and the ephemeral keys in legacy bytes and
    // hands that scheme on.
    #[test]
    fn a_legacy_view_reads_legacy_elements() {
        let legacy = BlsSchemeKind::Legacy;
        let mut fx = Fx::new(quorum_state(None)).asked(VVEC | CONTRIB, ME);
        fx.scheme = legacy;
        let body = concat(&[
            &vector(THRESHOLD, &pk_bytes(&vvec(), legacy)),
            &vector(VALID, &BLOBS.map(|n| blob_bytes_in(n, legacy))),
        ]);
        let msg = qdata(VVEC | CONTRIB, ME, body);
        assert_eq!(fx.run(&msg), (warm_write(), Ok(())));
        {
            let calls = fx.quorum.state().set_vvec_calls.borrow();
            assert_eq!(pk_bytes(&calls[0].0, SCHEME), pk_bytes(&vvec(), SCHEME));
            assert_eq!(calls[0].1, legacy);
        }
        let expected: Vec<_> =
            BLOBS.into_iter().map(|n| (blob_bytes_in(n, legacy), ME_INDEX, legacy)).collect();
        assert_eq!(fx.decrypts(), expected);
    }

    #[test]
    fn trailing_bytes_are_ignored() {
        let mut fx = Fx::new(quorum_state(None)).asked(VVEC, ME);
        let msg = qdata(VVEC, ME, concat(&[&vvec_body(), &[1, 2, 3]]));
        assert_eq!(fx.run(&msg), (warm_write(), Ok(())));
    }

    #[test]
    fn an_empty_mask_writes() {
        let mut fx = Fx::new(quorum_state(None)).asked(0, ME);
        let msg = qdata(0, ME, UNREADABLE.to_vec());
        assert_eq!(fx.run(&msg), (vec![Effect::Write(quorum_ref())], Ok(())));
    }
}
