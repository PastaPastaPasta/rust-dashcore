//! `CGovernanceVote`'s pure parts over BS9b's wire type [`GovVote`]: the network decode, hashes,
//! the signed message, both signature checks and `IsValid`.

use dash_dml::{MnList, OperatorKey};
use dash_evo::spork::{message_hash, recover_key_id};
use dash_pkc::bls::BlsSchemeKind;
use dash_pkc::bls::core::{BlsSigValue, BlsVerdict, verify_insecure};
use dash_primitives::GovVote;
use dash_types::codec::BaseCodec;
use dash_types::{CompactSize, Hashable, Numeric};

use crate::GovParams;
use crate::object::{outpoint_short, txid_bytes};
use crate::strings::hash_hex;

/// `vote_signal_enum_t`.
pub const SIGNAL_NONE: i32 = 0;
pub const SIGNAL_FUNDING: i32 = 1;
pub const SIGNAL_VALID: i32 = 2;
pub const SIGNAL_DELETE: i32 = 3;
pub const SIGNAL_ENDORSED: i32 = 4;
/// `VOTE_SIGNAL_UNKNOWN`: one past the last signal.
pub const SIGNAL_UNKNOWN: i32 = 5;

/// `vote_outcome_enum_t`.
pub const OUTCOME_NONE: i32 = 0;
pub const OUTCOME_YES: i32 = 1;
pub const OUTCOME_NO: i32 = 2;
pub const OUTCOME_ABSTAIN: i32 = 3;
/// `VOTE_OUTCOME_UNKNOWN`.
pub const OUTCOME_UNKNOWN: i32 = 4;

/// `CGovernanceVote::COMPACT_SIG_SIZE` and `BLS_SIG_SIZE`.
// Core: governance/vote.h:71-72
pub const COMPACT_SIG_SIZE: usize = 65;
pub const BLS_SIG_SIZE: usize = 96;

/// Where `vchSig`'s CompactSize starts: the outpoint (36), parent hash (32), outcome and signal
/// (4 each) and time (8).
const SIG_OFFSET: usize = 84;

/// A network `govobjvote`: Core's reader, which takes a signature of 65 or 96 bytes only
/// ("bad governance vote signature size"). Bytes after the vote are not read.
// Core: governance/vote.h:190-207
pub fn decode_network(bytes: &[u8]) -> Option<GovVote> {
    if !sig_count_within_limit(bytes) {
        return None;
    }
    let vote = GovVote::decode(&mut &bytes[..]).ok()?;
    matches!(vote.sig().len(), COMPACT_SIG_SIZE | BLS_SIG_SIZE).then_some(vote)
}

/// `LIMITED_VECTOR(obj.vchSig, BLS_SIG_SIZE)`: the count is read (and range-checked) and bounded
/// before any signature byte is, so an oversized vector is never allocated.
// Core: serialize.h LimitedVectorFormatter; governance/vote.h:199
fn sig_count_within_limit(bytes: &[u8]) -> bool {
    let Some(mut sig) = bytes.get(SIG_OFFSET..) else {
        return false;
    };
    CompactSize::decode(&mut sig).is_ok_and(|n| n.to_base() <= BLS_SIG_SIZE as u64)
}

/// `nVoteSignal` as Core's `int`.
pub fn signal(vote: &GovVote) -> i32 {
    vote.signal().to_base() as i32
}

/// `nVoteOutcome` as Core's `int`.
pub fn outcome(vote: &GovVote) -> i32 {
    vote.outcome().to_base() as i32
}

/// `GetHash()`: internal order.
// Core: governance/vote.cpp:145-156
pub fn hash(vote: &GovVote) -> [u8; 32] {
    *vote.hash().as_bytes()
}

/// `GetSignatureHash()`.
pub fn signature_hash(vote: &GovVote) -> [u8; 32] {
    *vote.signature_hash().as_bytes()
}

/// `GetSignatureString()`: what an ECDSA voting key signs off testnet.
// Core: governance/vote.h:166-172
pub fn signature_string(vote: &GovVote) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        outpoint_short(&vote.masternode_outpoint()),
        hash_hex(&txid_bytes(&vote.parent_hash())),
        signal(vote),
        outcome(vote),
        vote.time()
    )
}

/// `ConvertOutcomeToString`.
// Core: governance/vote.cpp:42-56
pub fn outcome_to_string(outcome: i32) -> &'static str {
    match outcome {
        OUTCOME_NONE => "none",
        OUTCOME_YES => "yes",
        OUTCOME_NO => "no",
        OUTCOME_ABSTAIN => "abstain",
        _ => "error",
    }
}

/// `ConvertSignalToString`.
// Core: governance/vote.cpp:58-72
pub fn signal_to_string(signal: i32) -> &'static str {
    match signal {
        SIGNAL_FUNDING => "funding",
        SIGNAL_VALID => "valid",
        SIGNAL_DELETE => "delete",
        SIGNAL_ENDORSED => "endorsed",
        _ => "none",
    }
}

/// `ConvertVoteOutcome`: `none` for an unknown string.
// Core: governance/vote.cpp:75-90
pub fn convert_outcome(s: &str) -> i32 {
    match s {
        "yes" => OUTCOME_YES,
        "no" => OUTCOME_NO,
        "abstain" => OUTCOME_ABSTAIN,
        _ => OUTCOME_NONE,
    }
}

/// `ConvertVoteSignal`: `none` for an unknown string.
// Core: governance/vote.cpp:92-105
pub fn convert_signal(s: &str) -> i32 {
    match s {
        "funding" => SIGNAL_FUNDING,
        "valid" => SIGNAL_VALID,
        "delete" => SIGNAL_DELETE,
        "endorsed" => SIGNAL_ENDORSED,
        _ => SIGNAL_NONE,
    }
}

/// `CheckSignature(keyID)`: a compact signature over the signature hash on testnet, over the
/// signed message elsewhere (notes D8).
// Core: governance/vote.cpp:158-190
pub fn check_voting_signature(vote: &GovVote, key_id: &[u8; 20], p: &GovParams) -> bool {
    let hash = if p.is_testnet() {
        signature_hash(vote)
    } else {
        message_hash(&signature_string(vote))
    };
    recover_key_id(&hash, vote.sig()).as_ref() == Some(key_id)
}

/// `CheckSignature(pubKey)`: the operator's basic-scheme BLS signature over the signature hash.
// Core: governance/vote.cpp:192-209
pub fn check_operator_signature(vote: &GovVote, key: &OperatorKey) -> bool {
    let sig = BlsSigValue::from_set_bytes_basic(vote.sig());
    verify_insecure(&sig, &key.get(), &signature_hash(vote), BlsSchemeKind::Basic)
        == BlsVerdict::Valid
}

/// The `+ 1h` in `IsValid`: how far ahead of the adjusted clock a vote may be.
// Core: governance/vote.cpp:213
pub const MAX_TIME_AHEAD: i64 = 60 * 60;

/// Whether `IsValid` reads the list: the time, signal and outcome checks pass.
// Core: governance/vote.cpp:213-228
pub fn passes_static_checks(vote: &GovVote, adjusted_now: i64) -> bool {
    vote.time() <= adjusted_now.saturating_add(MAX_TIME_AHEAD)
        && (SIGNAL_NONE..SIGNAL_UNKNOWN).contains(&signal(vote))
        && (OUTCOME_NONE..OUTCOME_UNKNOWN).contains(&outcome(vote))
}

/// The key a `CheckSignature` runs against: the voting key id or the operator key.
#[derive(Clone, Debug, PartialEq)]
pub enum SigKey {
    Voting([u8; 20]),
    Operator(Box<OperatorKey>),
}

impl SigKey {
    /// `CheckSignature(key)`.
    pub fn verify(&self, vote: &GovVote, p: &GovParams) -> bool {
        match self {
            SigKey::Voting(key_id) => check_voting_signature(vote, key_id, p),
            SigKey::Operator(key) => check_operator_signature(vote, key),
        }
    }
}

/// `CGovernanceVote::SignatureMemo`: a stored vote's last verdict, with the key, signature hash
/// and signature bytes it was computed over. One slot: a store replaces it. Core keys the slot
/// by a hash of the three; this keeps them, and compares an operator key by its point, as Core's
/// basic-scheme encoding does.
// Core: governance/vote.h v24.0.2:86-122; governance/vote.cpp v24.0.2:22-45, 164-207
#[derive(Clone, Debug, Default)]
pub struct SigMemo(Option<(SigKey, [u8; 32], Vec<u8>, bool)>);

impl SigMemo {
    /// `Lookup`: the verdict when this exact check already ran.
    pub fn lookup(&self, key: &SigKey, vote: &GovVote) -> Option<bool> {
        let (k, sig_hash, sig, valid) = self.0.as_ref()?;
        (k == key && *sig_hash == signature_hash(vote) && sig[..] == vote.sig()[..])
            .then_some(*valid)
    }

    /// `Store`.
    pub fn store(&mut self, key: SigKey, vote: &GovVote, valid: bool) {
        self.0 = Some((key, signature_hash(vote), vote.sig().to_vec(), valid));
    }
}

/// `IsValid` before the signature: `None` when the time, signal or outcome check fails or the
/// masternode is not in `list`, else the key the signature must check against.
// Core: governance/vote.cpp v24.0.2:211-242
pub fn signature_key(
    vote: &GovVote,
    list: &MnList,
    use_voting_key: bool,
    adjusted_now: i64,
) -> Option<SigKey> {
    if !passes_static_checks(vote, adjusted_now) {
        return None;
    }
    let dmn = list.get_by_collateral(&vote.masternode_outpoint())?;
    Some(if use_voting_key {
        SigKey::Voting(*dmn.cold.key_id_voting.as_bytes())
    } else {
        SigKey::Operator(Box::new(dmn.cold.pub_key_operator.clone()))
    })
}

/// `IsValid(tip_mn_list, useVotingKey)` at `GetAdjustedTime()` = `adjusted_now`.
// Core: governance/vote.cpp:211-242
pub fn is_valid(
    vote: &GovVote,
    list: &MnList,
    use_voting_key: bool,
    adjusted_now: i64,
    p: &GovParams,
) -> bool {
    signature_key(vote, list, use_voting_key, adjusted_now).is_some_and(|key| key.verify(vote, p))
}

/// `IsValidForUnknownParent`: a funding vote may carry either key, every other only the
/// operator's.
// Core: governance/vote.cpp:244-250
pub fn is_valid_for_unknown_parent(
    vote: &GovVote,
    list: &MnList,
    adjusted_now: i64,
    p: &GovParams,
) -> bool {
    (signal(vote) == SIGNAL_FUNDING && is_valid(vote, list, true, adjusted_now, p))
        || is_valid(vote, list, false, adjusted_now, p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_primitives::{OutPoint, TxHash, VoteOutcome, VoteSignal};

    fn vote(sig_len: usize) -> GovVote {
        GovVote::new(
            OutPoint::new(TxHash::from_lendian([1; 32]), 3),
            TxHash::from_lendian([2; 32]),
            VoteOutcome::new(1),
            VoteSignal::new(7),
            -5,
            vec![0; sig_len],
        )
    }

    #[test]
    fn the_memo_answers_only_the_check_it_stored_and_keeps_one_slot() {
        let (a, b) = (SigKey::Voting([1; 20]), SigKey::Voting([2; 20]));
        let v = vote(65);
        let mut memo = SigMemo::default();
        assert_eq!(memo.lookup(&a, &v), None);
        memo.store(a.clone(), &v, true);
        assert_eq!(memo.lookup(&a, &v), Some(true));
        let with = |time: i64, sig: Vec<u8>| {
            GovVote::new(
                v.masternode_outpoint(),
                v.parent_hash(),
                v.outcome(),
                v.signal(),
                time,
                sig,
            )
        };
        assert_eq!(memo.lookup(&a, &with(v.time(), vec![1; 65])), None, "another signature");
        assert_eq!(memo.lookup(&a, &with(v.time() + 1, vec![0; 65])), None, "another hash");
        assert_eq!(memo.lookup(&b, &v), None, "another key");
        memo.store(b.clone(), &v, false);
        assert_eq!((memo.lookup(&b, &v), memo.lookup(&a, &v)), (Some(false), None));
    }

    #[test]
    fn network_signature_sizes() {
        for (len, ok) in [(0, false), (64, false), (65, true), (96, true), (97, false)] {
            let mut bytes = Vec::new();
            vote(len).encode(&mut bytes);
            bytes.push(0xff); // not read
            assert_eq!(decode_network(&bytes).is_some(), ok, "{len}");
        }
    }

    #[test]
    fn signature_offset() {
        let mut bytes = Vec::new();
        vote(0).encode(&mut bytes);
        assert_eq!(bytes.len(), SIG_OFFSET + 1);
        assert_eq!(bytes[SIG_OFFSET], 0);
    }

    /// The `LIMITED_VECTOR` bound rejects on the count alone, independently of the final 65/96
    /// check: count 97 with a full payload, and huge counts with (almost) nothing behind them.
    #[test]
    fn signature_count_bound() {
        let mut full = Vec::new();
        vote(97).encode(&mut full);
        assert!(!sig_count_within_limit(&full));
        assert!(decode_network(&full).is_none());

        let mut at_limit = Vec::new();
        vote(96).encode(&mut at_limit);
        assert!(sig_count_within_limit(&at_limit));
        // The bound only reads the count: 96 with no payload passes it, then fails the decode.
        assert!(sig_count_within_limit(&at_limit[..SIG_OFFSET + 1]));
        assert!(decode_network(&at_limit[..SIG_OFFSET + 1]).is_none());
        assert!(!sig_count_within_limit(&at_limit[..SIG_OFFSET]));

        let prefix = &at_limit[..SIG_OFFSET];
        for count in [
            &[0xfd, 0x61, 0x00][..],                                 // 97
            &[0xfe, 0x01, 0x00, 0x00, 0x02],                         // MAX_SIZE + 1
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], // u64::MAX
            &[0xfd, 0x60, 0x00],                                     // 96, non-canonical
        ] {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(count);
            bytes.extend_from_slice(&[0; 97]);
            assert!(!sig_count_within_limit(&bytes), "{count:02x?}");
            assert!(decode_network(&bytes).is_none(), "{count:02x?}");
        }
    }

    /// `Time() > GetAdjustedTime() + 1h` fails (`vote.cpp:213-218`): an hour ahead is valid, a
    /// second more is not.
    #[test]
    fn time_bound_is_inclusive() {
        let now = 1_700_000_000;
        let v = |time| {
            let base = vote(96);
            GovVote::new(
                base.masternode_outpoint(),
                base.parent_hash(),
                VoteOutcome::new(OUTCOME_YES as u32),
                VoteSignal::new(SIGNAL_FUNDING as u32),
                time,
                base.sig().clone(),
            )
        };
        assert!(passes_static_checks(&v(now + 3600), now));
        assert!(!passes_static_checks(&v(now + 3601), now));
        assert!(passes_static_checks(&v(i64::MIN), now));
    }

    #[test]
    fn strings() {
        let v = vote(65);
        assert_eq!(
            signature_string(&v),
            format!("{}-3|{}|7|1|-5", "01".repeat(32), "02".repeat(32))
        );
        assert_eq!(signal_to_string(7), "none");
        assert_eq!(outcome_to_string(-1), "error");
        assert_eq!(convert_signal("x"), SIGNAL_NONE);
        assert!(!passes_static_checks(&v, 0));
    }
}
