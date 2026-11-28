//! Scripted doubles for the signer tests. [`FakeSignShares`] is a [`QuorumSigning`] that records
//! every sign request, answers queries from scripted maps, and delivers recovered signatures to
//! listeners in registration order, as the signing runtime does; it models the non-platform
//! types the signers use, and refuses a no-wait-marked caller of the two calls that may wait on
//! V, as the runtime does. [`FakeIsLockView`] scripts [`IsLockView`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dash_consensus::primitives::TxHash;
use dash_evo::locks::wire::RecoveredSig;
use dash_llmq::signing::api::{
    ListenerId, QuorumSigning, RecoveredSigListener, RecoveredSigResult, SelectedQuorum,
    SignRequest, SigningFault,
};
use dash_params::LlmqType;
use dashd_chainstate::chain::no_wait_marked;
use parking_lot::Mutex;

use crate::signing::IsLockView;

type Key = (LlmqType, [u8; 32]);

#[derive(Default)]
struct State {
    sign_requests: Vec<SignRequest>,
    sign_result: Option<Result<bool, SigningFault>>,
    selected: HashMap<Key, SelectedQuorum>,
    recovered: HashMap<Key, RecoveredSig>,
    sessions: HashSet<[u8; 32]>,
    votes: HashMap<Key, [u8; 32]>,
    truncated: Vec<Key>,
    fault: Option<SigningFault>,
    listeners: Vec<(ListenerId, Arc<dyn RecoveredSigListener>)>,
    next_listener: ListenerId,
}

/// Records sign requests and serves scripted recovered signatures, votes and quorums.
#[derive(Default)]
pub struct FakeSignShares {
    state: Mutex<State>,
}

impl FakeSignShares {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Every `async_sign_if_member` request so far, in call order.
    pub fn sign_requests(&self) -> Vec<SignRequest> {
        self.state.lock().sign_requests.clone()
    }

    /// Overrides `async_sign_if_member`: it then returns `result` without modelling votes.
    pub fn set_sign_result(&self, result: Result<bool, SigningFault>) {
        self.state.lock().sign_result = Some(result);
    }

    /// Makes every query fail with `fault`, or clears that with `None`.
    pub fn set_fault(&self, fault: Option<SigningFault>) {
        self.state.lock().fault = fault;
    }

    pub fn set_selected_quorum(&self, t: LlmqType, id: [u8; 32], q: SelectedQuorum) {
        self.state.lock().selected.insert((t, id), q);
    }

    /// Stores `sig` as recovered without notifying listeners.
    pub fn insert_recovered(&self, sig: RecoveredSig) {
        let mut s = self.state.lock();
        s.sessions.insert(sig.sign_hash());
        s.recovered.insert((sig.llmq_type, sig.id), sig);
    }

    pub fn insert_session(&self, sign_hash: [u8; 32]) {
        self.state.lock().sessions.insert(sign_hash);
    }

    pub fn set_vote(&self, t: LlmqType, id: [u8; 32], msg: [u8; 32]) {
        self.state.lock().votes.insert((t, id), msg);
    }

    /// Every `truncate_recovered_sig` call so far, in call order.
    pub fn truncated(&self) -> Vec<(LlmqType, [u8; 32])> {
        self.state.lock().truncated.clone()
    }

    pub fn listener_count(&self) -> usize {
        self.state.lock().listeners.len()
    }

    /// Stores `sig` and calls every listener in registration order with no lock held, as
    /// `ProcessRecoveredSig` does; returns each listener's result in that order. A sig for an id
    /// that already has one is rejected: nothing is stored and no listener runs.
    // Core: v24.0.2 src/llmq/signing.cpp:596-608 ProcessRecoveredSig
    pub fn deliver(&self, sig: RecoveredSig) -> Vec<RecoveredSigResult> {
        let listeners: Vec<_> = {
            let mut s = self.state.lock();
            let key = (sig.llmq_type, sig.id);
            if s.recovered.contains_key(&key) {
                return Vec::new();
            }
            s.sessions.insert(sig.sign_hash());
            s.recovered.insert(key, sig.clone());
            s.listeners.iter().map(|(_, l)| Arc::clone(l)).collect()
        };
        listeners.iter().map(|l| l.handle_new_recovered_sig(&sig)).collect()
    }

    fn check(&self, s: &State) -> Result<(), SigningFault> {
        s.fault.clone().map_or(Ok(()), Err)
    }
}

impl QuorumSigning for FakeSignShares {
    /// As a quorum member: refuses a conflicting or repeated vote unless allowed, reports a
    /// recovered id as signed, and otherwise records the vote.
    // Core: v24.0.2 src/llmq/signing_shares.cpp:832-867 AsyncSignIfMember
    fn async_sign_if_member(&self, req: &SignRequest) -> Result<bool, SigningFault> {
        debug_assert!(!no_wait_marked(), "async_sign_if_member may wait on V");
        let mut s = self.state.lock();
        s.sign_requests.push(req.clone());
        if let Some(result) = s.sign_result.clone() {
            return result;
        }
        let key = (req.llmq_type, req.id);
        let mut voted = false;
        if let Some(prev) = s.votes.get(&key).copied() {
            voted = true;
            if prev != req.msg_hash {
                if !req.allow_diff_msg {
                    return Ok(false);
                }
                voted = false;
            } else if !req.allow_resign {
                return Ok(false);
            }
        }
        if s.recovered.contains_key(&key) {
            return Ok(true);
        }
        if !voted {
            s.votes.insert(key, req.msg_hash);
        }
        Ok(true)
    }

    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
    ) -> Result<Option<SelectedQuorum>, SigningFault> {
        debug_assert!(!no_wait_marked(), "select_quorum_for_signing may wait on V");
        let s = self.state.lock();
        self.check(&s)?;
        Ok(s.selected.get(&(t, *id)).copied())
    }

    fn has_recovered_sig(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        let s = self.state.lock();
        self.check(&s)?;
        Ok(s.recovered.get(&(t, *id)).is_some_and(|r| r.msg_hash == *msg))
    }

    fn has_recovered_sig_for_id(&self, t: LlmqType, id: &[u8; 32]) -> Result<bool, SigningFault> {
        let s = self.state.lock();
        self.check(&s)?;
        Ok(s.recovered.contains_key(&(t, *id)))
    }

    fn has_recovered_sig_for_session(&self, sign_hash: &[u8; 32]) -> Result<bool, SigningFault> {
        let s = self.state.lock();
        self.check(&s)?;
        Ok(s.sessions.contains(sign_hash))
    }

    fn has_recovered_sig_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        _msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        self.has_recovered_sig_for_id(t, id)
    }

    fn get_recovered_sig(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, SigningFault> {
        let s = self.state.lock();
        self.check(&s)?;
        Ok(s.recovered.get(&(t, *id)).filter(|r| r.msg_hash == *msg).cloned())
    }

    fn is_conflicting(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        let s = self.state.lock();
        self.check(&s)?;
        Ok(s.recovered.get(&(t, *id)).is_some_and(|r| r.msg_hash != *msg))
    }

    fn get_vote_for_id(
        &self,
        t: LlmqType,
        id: &[u8; 32],
    ) -> Result<Option<[u8; 32]>, SigningFault> {
        let s = self.state.lock();
        self.check(&s)?;
        Ok(s.votes.get(&(t, *id)).copied())
    }

    fn truncate_recovered_sig(&self, t: LlmqType, id: &[u8; 32]) -> Result<(), SigningFault> {
        let mut s = self.state.lock();
        self.check(&s)?;
        s.recovered.remove(&(t, *id));
        s.truncated.push((t, *id));
        Ok(())
    }

    fn register_listener(&self, l: Arc<dyn RecoveredSigListener>) -> ListenerId {
        let mut s = self.state.lock();
        let id = s.next_listener;
        s.next_listener += 1;
        s.listeners.push((id, l));
        id
    }

    fn unregister_listener(&self, id: ListenerId) {
        self.state.lock().listeners.retain(|(l, _)| *l != id);
    }
}

/// InstantSend on or off, and a set of locked txids.
#[derive(Default)]
pub struct FakeIsLockView {
    disabled: Mutex<bool>,
    locked: Mutex<HashSet<TxHash>>,
}

impl FakeIsLockView {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set_enabled(&self, enabled: bool) {
        *self.disabled.lock() = !enabled;
    }

    pub fn lock(&self, txid: TxHash) {
        self.locked.lock().insert(txid);
    }
}

impl IsLockView for FakeIsLockView {
    fn is_enabled(&self) -> bool {
        !*self.disabled.lock()
    }

    fn is_locked(&self, txid: &TxHash) -> dash_kv::Result<bool> {
        Ok(self.is_enabled() && self.locked.lock().contains(txid))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recsig(id: u8, msg: u8) -> RecoveredSig {
        let mut bytes = vec![100u8];
        bytes.extend([7u8; 32]);
        bytes.extend([id; 32]);
        bytes.extend([msg; 32]);
        bytes.extend([0u8; 96]);
        RecoveredSig::decode(&mut bytes.as_slice()).expect("a full recovered sig")
    }

    struct Tag(&'static str, Arc<Mutex<Vec<&'static str>>>);

    impl RecoveredSigListener for Tag {
        fn handle_new_recovered_sig(&self, _: &RecoveredSig) -> RecoveredSigResult {
            self.1.lock().push(self.0);
            RecoveredSigResult::Nothing
        }
    }

    #[test]
    fn delivers_in_registration_order_and_skips_unregistered() {
        let fake = FakeSignShares::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let cl = fake.register_listener(Arc::new(Tag("cl", Arc::clone(&seen))));
        fake.register_listener(Arc::new(Tag("is", Arc::clone(&seen))));
        fake.deliver(recsig(1, 1));
        fake.unregister_listener(cl);
        fake.deliver(recsig(2, 2));
        assert_eq!(*seen.lock(), ["cl", "is", "is"]);
        assert_eq!(fake.listener_count(), 1);
    }

    struct Reenter(Arc<FakeSignShares>, Mutex<Option<ListenerId>>);

    impl RecoveredSigListener for Reenter {
        fn handle_new_recovered_sig(&self, sig: &RecoveredSig) -> RecoveredSigResult {
            assert!(!self.0.is_conflicting(sig.llmq_type, &sig.id, &sig.msg_hash).unwrap());
            if let Some(id) = self.1.lock().take() {
                self.0.unregister_listener(id);
            }
            RecoveredSigResult::Nothing
        }
    }

    #[test]
    fn a_listener_may_call_back_into_the_fake() {
        let fake = FakeSignShares::new();
        let listener = Arc::new(Reenter(Arc::clone(&fake), Mutex::new(None)));
        let id = fake.register_listener(listener.clone());
        *listener.1.lock() = Some(id);
        assert_eq!(fake.deliver(recsig(1, 1)), [RecoveredSigResult::Nothing]);
        assert_eq!(fake.listener_count(), 0);
    }

    #[test]
    fn a_second_sig_for_a_recovered_id_is_rejected() {
        let fake = FakeSignShares::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        fake.register_listener(Arc::new(Tag("cl", Arc::clone(&seen))));
        let first = recsig(1, 1);
        let t = first.llmq_type;
        fake.deliver(first.clone());
        assert!(fake.deliver(recsig(1, 2)).is_empty());
        assert_eq!(*seen.lock(), ["cl"]);
        assert_eq!(fake.get_recovered_sig(t, &[1; 32], &[1; 32]).unwrap(), Some(first.clone()));
        assert!(fake.has_recovered_sig_for_session(&first.sign_hash()).unwrap());
    }

    fn request(id: u8, msg: u8, allow_resign: bool, allow_diff_msg: bool) -> SignRequest {
        SignRequest {
            llmq_type: recsig(id, msg).llmq_type,
            id: [id; 32],
            msg_hash: [msg; 32],
            quorum_hash: None,
            allow_resign,
            allow_diff_msg,
        }
    }

    #[test]
    fn signing_records_a_vote_and_refuses_a_conflicting_one() {
        let fake = FakeSignShares::new();
        let t = recsig(1, 1).llmq_type;
        assert!(fake.async_sign_if_member(&request(1, 1, false, false)).unwrap());
        assert_eq!(fake.get_vote_for_id(t, &[1; 32]).unwrap(), Some([1; 32]));
        assert!(!fake.async_sign_if_member(&request(1, 2, false, false)).unwrap());
        assert!(!fake.async_sign_if_member(&request(1, 1, false, false)).unwrap());
        assert!(fake.async_sign_if_member(&request(1, 1, true, false)).unwrap());
        assert!(fake.async_sign_if_member(&request(1, 2, false, true)).unwrap());
        assert_eq!(fake.get_vote_for_id(t, &[1; 32]).unwrap(), Some([2; 32]));
        assert_eq!(fake.sign_requests().len(), 5);
    }

    #[test]
    #[should_panic(expected = "may wait on V")]
    fn signing_from_a_no_wait_thread_is_refused() {
        let fake = FakeSignShares::new();
        let _marked = dashd_chainstate::chain::NoWaitGuard::set();
        let _ = fake.async_sign_if_member(&request(1, 1, false, false));
    }

    #[test]
    fn a_disabled_view_reports_nothing_locked() {
        let view = FakeIsLockView::new();
        let txid = TxHash::from([5; 32]);
        view.lock(txid);
        assert!(view.is_locked(&txid).unwrap());
        view.set_enabled(false);
        assert!(!view.is_locked(&txid).unwrap());
    }

    #[test]
    fn a_recovered_sig_for_another_message_conflicts() {
        let fake = FakeSignShares::new();
        let sig = recsig(1, 1);
        let t = sig.llmq_type;
        fake.insert_recovered(sig);
        assert!(fake.is_conflicting(t, &[1; 32], &[2; 32]).unwrap());
        assert!(!fake.is_conflicting(t, &[1; 32], &[1; 32]).unwrap());
        assert!(!fake.is_conflicting(t, &[3; 32], &[2; 32]).unwrap());
    }
}
