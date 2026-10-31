//! Type 7's L0 (EV1 §6.1, EV1-5b-2), and the quorum world type 9's share.

use std::convert::Infallible;

use dash_consensus::BlockId;
use dash_consensus::hash::sha256d;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_evo::locks::sign_hash;
use dash_pkc::bls::{BlsEagerSig, BlsSchemeKind};
use dash_primitives::{LlmqType, MnHardFork, QuorumHash, TxType};

use super::super::quorum::{QuorumKey, SeamError, TestQuorums};
use super::super::reasons::*;
use super::super::testctx::{Fixture, MemIndex, Seams, special};
use super::super::{CheckResult, SpecialTxFault};
use super::check_mnhf_tx;
use crate::creditpool::RangesSet;
use crate::testkit::{MemChain, operator_key, operator_sig};
use dash_consensus::primitives::Tx;

const C: TxValidationResult = TxValidationResult::Consensus;

/// A chain to check on: `pindexPrev` at height 200, a quorum block on it at 150, a block at 150 on
/// a fork, and the quorums of one key each the manager holds.
pub(crate) struct QuorumWorld {
    pub fx: Fixture,
    pub chain: MemChain,
    pub prev: Option<BlockId>,
    /// On the chain at 150.
    pub base: BlockId,
    /// Off the chain at 150.
    pub fork: BlockId,
    pub index: MemIndex,
    pub quorums: TestQuorums,
    pub mtp: i64,
    pub bound: bool,
    pub unlock: Option<RangesSet>,
}

impl QuorumWorld {
    pub(crate) fn new() -> QuorumWorld {
        let mut chain = MemChain::new();
        let below = chain.extend(chain.genesis(), 149);
        let base = chain.child(below);
        let prev = chain.extend(base, 50);
        let fork = chain.child(below);
        let index = MemIndex::of(&chain, fork);
        let mut fx = Fixture::v24();
        fx.check_sigs = true;
        QuorumWorld {
            fx,
            chain,
            prev: Some(prev),
            base,
            fork,
            index,
            quorums: TestQuorums::default(),
            mtp: 0,
            bound: true,
            unlock: None,
        }
    }

    /// The quorum hash of `block`.
    pub(crate) fn hash_of(&self, block: BlockId) -> QuorumHash {
        QuorumHash::from(*self.chain.hash(block).as_bytes())
    }

    /// The manager holds a quorum of `llmq_type` at `block`, keyed by [`operator_key`]`(key)`
    /// (newer quorums go after older ones).
    pub(crate) fn hold(&mut self, llmq_type: LlmqType, block: BlockId, key: u8) -> QuorumHash {
        let quorum_hash = self.hash_of(block);
        self.quorums.quorums.push((
            llmq_type,
            QuorumKey {
                quorum_hash,
                public_key: operator_key(key, BlsSchemeKind::Basic),
            },
        ));
        quorum_hash
    }

    pub(crate) fn seams(&self) -> Seams<'_> {
        Seams {
            chain: &self.chain,
            prev: self.prev,
            mtp: self.mtp,
            index: &self.index,
            bound: self.bound,
            unlock: self.unlock.as_ref().map(|u| u as _),
        }
    }

    pub(crate) fn check(&mut self, tx: &Tx) -> CheckResult<Infallible> {
        let at = Seams {
            chain: &self.chain,
            prev: self.prev,
            mtp: self.mtp,
            index: &self.index,
            bound: self.bound,
            unlock: self.unlock.as_ref().map(|u| u as _),
        };
        self.fx.check_at(tx, at, &mut self.quorums)
    }

    pub(crate) fn mnhf_type(&self) -> LlmqType {
        self.fx.params().consensus.llmq_type_mnhf
    }

    /// A verdict, or a panic naming the fault.
    pub(crate) fn verdict(&mut self, tx: &Tx) -> Result<(), (TxValidationResult, String)> {
        match self.check(tx) {
            Ok(v) => v.map_err(|e| (e.result(), e.reject_reason().to_owned())),
            Err(fault) => panic!("a fault: {fault:?}"),
        }
    }
}

/// A valid signature to stand in before signing.
fn placeholder() -> BlsEagerSig {
    operator_sig(0, &[0; 32])
}

fn mnhf_tx(p: &MnHardFork) -> Tx {
    special(TxType::MnhfSignal, vec![], vec![], p)
}

/// The signal for `bit` on the quorum at `quorum`, signed by `key` over Core's `SignHash`.
fn signed(w: &QuorumWorld, bit: u8, quorum: QuorumHash, key: u8) -> (MnHardFork, Tx) {
    let mut p = MnHardFork {
        version: 1,
        version_bit: bit,
        quorum_hash: quorum,
        sig: placeholder(),
    };
    let msg = sha256d(&p.sig_hash_bytes(mnhf_tx(&p).raw()));
    let hash = sign_hash(w.mnhf_type(), &quorum, p.request_id().as_bytes(), &msg);
    p.sig = operator_sig(key, &hash);
    let tx = mnhf_tx(&p);
    (p, tx)
}

/// A bit no deployment uses: `IsValidMNActivation` holds.
const FREE_BIT: u8 = 5;

/// The world with the MNHF quorum at `base` held under key 1, and its signal for [`FREE_BIT`].
fn ready() -> (QuorumWorld, MnHardFork, Tx) {
    let mut w = QuorumWorld::new();
    let t = w.mnhf_type();
    let q = w.hold(t, w.base, 1);
    let (p, tx) = signed(&w, FREE_BIT, q, 1);
    (w, p, tx)
}

#[track_caller]
fn assert_reject(w: &mut QuorumWorld, tx: &Tx, reason: &str) {
    assert_eq!(w.verdict(tx), Err((C, reason.to_owned())));
}

/// A signal signed by the quorum it names, on the chain, for a bit that may activate this way:
/// accepted, after one `GetQuorum` of `llmqTypeMnhf`.
#[test]
fn mnhf_accepts_a_signed_signal() {
    let (mut w, _, tx) = ready();
    assert_eq!(w.verdict(&tx), Ok(()));
    assert_eq!(w.quorums.calls, vec![("get", w.mnhf_type(), 0)]);
}

/// Steps 2 and 3: a payload that does not decode whole, then the trivial checks.
#[test]
fn mnhf_rejects_the_payload_then_the_trivial_checks() {
    let (mut w, p, tx) = ready();
    let mut bytes = tx.raw().extra_payload.clone();
    bytes.push(0);
    let trailing = super::super::testctx::tx_of(TxType::MnhfSignal, vec![], vec![], bytes);
    assert_reject(&mut w, &trailing, BAD_MNHF_PAYLOAD);
    assert_reject(&mut w, &mnhf_tx_bytes(&[]), BAD_MNHF_PAYLOAD);
    for version in [0, 2, 255] {
        let q = MnHardFork {
            version,
            ..p.clone()
        };
        assert_reject(&mut w, &mnhf_tx(&q), BAD_MNHF_VERSION);
    }
    for bit in [29, 255] {
        let q = MnHardFork {
            version_bit: bit,
            ..p.clone()
        };
        assert_reject(&mut w, &mnhf_tx(&q), BAD_MNHF_NBIT_OUT_OF_BOUNDS);
    }
    // Core: evo/mnhftx.cpp:94-104 checks the version before the bit.
    let both = MnHardFork {
        version: 0,
        version_bit: 29,
        ..p
    };
    assert_reject(&mut w, &mnhf_tx(&both), BAD_MNHF_VERSION);
    assert!(w.quorums.calls.is_empty());
}

fn mnhf_tx_bytes(bytes: &[u8]) -> Tx {
    super::super::testctx::tx_of(TxType::MnhfSignal, vec![], vec![], bytes.to_vec())
}

/// Step 4 (`evo_mnhf_tests.cpp`'s "non EHF fork"): a bit a deployment without `useEHF` uses,
/// inside its window, is refused; outside the window, or by an EHF deployment, it passes on.
#[test]
fn mnhf_rejects_a_non_ehf_bit_in_its_window() {
    let (mut w, _, _) = ready();
    let deployments = &w.fx.params().consensus.deployments;
    let plain = deployments.iter().find(|d| !d.use_ehf).expect("a deployment without useEHF");
    let (bit, start) = (plain.bit as u8, plain.start_time.max(0));
    let ehf = deployments.iter().find(|d| d.use_ehf).expect("an EHF deployment");
    let (ehf_bit, ehf_start) = (ehf.bit as u8, ehf.start_time.max(0));
    let q = w.hash_of(w.base);
    w.mtp = start;
    let (_, tx) = signed(&w, bit, q, 1);
    assert_reject(&mut w, &tx, BAD_MNHF_NON_EHF);
    w.mtp = ehf_start;
    let (_, tx) = signed(&w, ehf_bit, q, 1);
    assert_eq!(w.verdict(&tx), Ok(()));
}

/// Step 5 (`evo_mnhf_tests.cpp`'s "wrong quorum"): an unknown quorum hash, one on a fork, and
/// one above `pindexPrev` (no ancestor there) are each `bad-mnhf-quorum-hash`, before any quorum
/// read.
#[test]
fn mnhf_rejects_a_quorum_hash_off_the_chain() {
    let (mut w, _, _) = ready();
    let unknown = QuorumHash::from([7; 32]);
    let fork = w.hash_of(w.fork);
    let prev = w.prev.expect("a prev");
    let above = w.chain.child(prev);
    w.index = MemIndex::of(&w.chain, above);
    let above = w.hash_of(above);
    for q in [unknown, fork, above] {
        let (_, tx) = signed(&w, FREE_BIT, q, 1);
        assert_reject(&mut w, &tx, BAD_MNHF_QUORUM_HASH);
    }
    assert!(w.quorums.calls.is_empty());
}

/// Step 6: a quorum on the chain the manager does not hold, or holds under another type.
#[test]
fn mnhf_rejects_a_missing_quorum() {
    let mut w = QuorumWorld::new();
    let q = w.hash_of(w.base);
    let (_, tx) = signed(&w, FREE_BIT, q, 1);
    assert_reject(&mut w, &tx, BAD_MNHF_MISSING_QUORUM);
    let other = w.fx.params().consensus.llmq_type_platform;
    assert_ne!(other, w.mnhf_type());
    w.hold(other, w.base, 1);
    assert_reject(&mut w, &tx, BAD_MNHF_MISSING_QUORUM);
}

/// Step 7: another key's signature, a signature over another transaction (the outputs are in
/// `msgHash`) and over another request id (the bit) are `bad-mnhf-invalid`, and so they are with
/// `check_sigs` off: type 7 always verifies (§6.1).
#[test]
fn mnhf_verifies_the_signature_always() {
    for check_sigs in [true, false] {
        let (mut w, p, _) = ready();
        w.fx.check_sigs = check_sigs;
        let (_, other_key) = signed(&w, FREE_BIT, p.quorum_hash, 2);
        assert_reject(&mut w, &other_key, BAD_MNHF_INVALID);
        let paying = special(
            TxType::MnhfSignal,
            vec![],
            vec![super::super::testctx::txout(1, vec![0x51])],
            &p,
        );
        assert_reject(&mut w, &paying, BAD_MNHF_INVALID);
        let (signed_6, _) = signed(&w, FREE_BIT + 1, p.quorum_hash, 1);
        let moved = MnHardFork {
            version_bit: FREE_BIT,
            ..signed_6
        };
        assert_reject(&mut w, &mnhf_tx(&moved), BAD_MNHF_INVALID);
    }
}

/// The type step (`bad-mnhf-type`) is unreachable through the dispatcher, which sends only type
/// 7 here and only special-version transactions past its payload check; a direct call shows it.
#[test]
fn mnhf_type_step_direct() {
    let (w, p, _) = ready();
    let mut quorums = TestQuorums::default();
    let wrong = special(TxType::AssetLock, vec![], vec![], &p);
    let got =
        w.fx.with_seams(w.seams(), &mut quorums, |cx| check_mnhf_tx(&wrong, wrong.payload(), cx));
    assert_eq!(
        got.map(|v| v.map_err(|e| e.reject_reason().to_owned())),
        Ok(Err(BAD_MNHF_TYPE.to_owned()))
    );
}

/// Faults: no `pindexPrev` past the trivial checks (Core dereferences it), the seams unbound
/// where the quorum hash is read, and the manager's errors. Rejects before each still come.
#[test]
fn mnhf_faults_without_prev_or_seams() {
    // No pindexPrev means DIP3 is not active after it, so through the dispatcher the gate
    // answers first; the type's check shows the fault, after the trivial checks.
    let (mut w, p, tx) = ready();
    w.prev = None;
    let gate = w.verdict(&tx);
    assert_eq!(gate, Err((TxValidationResult::BadSpecial, BAD_TX_TYPE_DIP3_INACTIVE.to_owned())));
    let v0 = mnhf_tx(&MnHardFork {
        version: 0,
        ..p.clone()
    });
    let mut quorums = TestQuorums::default();
    let direct = |tx: &Tx, quorums: &mut TestQuorums| {
        w.fx.with_seams(w.seams(), quorums, |cx| check_mnhf_tx(tx, tx.payload(), cx))
    };
    let got = direct(&tx, &mut quorums);
    assert!(matches!(got, Err(SpecialTxFault::NoPrev(TxType::MnhfSignal))), "{got:?}");
    let got = direct(&v0, &mut quorums).map(|v| v.map_err(|e| e.reject_reason().to_owned()));
    assert_eq!(got, Ok(Err(BAD_MNHF_VERSION.to_owned())));

    let (mut w, _, tx) = ready();
    w.bound = false;
    assert!(matches!(w.check(&tx), Err(SpecialTxFault::Unbound(TxType::MnhfSignal))));
    assert_reject(&mut w, &v0, BAD_MNHF_VERSION);

    let (w, _, tx) = ready();
    struct Broken;
    impl super::super::quorum::QuorumLookup for Broken {
        fn get_quorum(
            &mut self,
            _: LlmqType,
            _: &QuorumHash,
            _: &mut dyn dash_evo::effect::EffectSink,
        ) -> Result<Option<QuorumKey>, SeamError> {
            Err(SeamError::Store(crate::store::StoreError::Read {
                what: "quorum".into(),
            }))
        }

        fn scan_quorums(
            &mut self,
            _: LlmqType,
            _: BlockId,
            _: usize,
            _: &mut super::super::quorum::QuorumScan,
            _: &mut dyn dash_evo::effect::EffectSink,
        ) -> Result<(), SeamError> {
            Err(SeamError::Store(crate::store::StoreError::Read {
                what: "quorum".into(),
            }))
        }
    }
    let got = w.fx.check_at(&tx, w.seams(), &mut Broken);
    assert!(matches!(got, Err(SpecialTxFault::Quorum(SeamError::Store(_)))), "{got:?}");
}

// --- Census vectors (`specialtx::tests::CASES`): one per reject site of `CheckMNHFTx` ---

type Verdict = Result<(), TxValidationError>;

/// `ready`'s world after `edit`, checking what `tx` makes of the ready signal.
fn vector(edit: impl FnOnce(&mut QuorumWorld, MnHardFork, Tx) -> Tx) -> Verdict {
    let (mut w, p, tx) = ready();
    let tx = edit(&mut w, p, tx);
    w.check(&tx).expect("no fault")
}

pub(crate) fn payload() -> Verdict {
    vector(|_, _, _| mnhf_tx_bytes(&[]))
}

pub(crate) fn version() -> Verdict {
    vector(|_, p, _| {
        mnhf_tx(&MnHardFork {
            version: 2,
            ..p
        })
    })
}

pub(crate) fn nbit() -> Verdict {
    vector(|_, p, _| {
        mnhf_tx(&MnHardFork {
            version_bit: 29,
            ..p
        })
    })
}

pub(crate) fn non_ehf() -> Verdict {
    vector(|w, p, _| {
        let deployments = &w.fx.params().consensus.deployments;
        let plain = deployments.iter().find(|d| !d.use_ehf).expect("a deployment without useEHF");
        let (bit, start) = (plain.bit as u8, plain.start_time.max(0));
        w.mtp = start;
        signed(w, bit, p.quorum_hash, 1).1
    })
}

pub(crate) fn quorum_hash_unknown() -> Verdict {
    vector(|w, _, _| signed(w, FREE_BIT, QuorumHash::from([7; 32]), 1).1)
}

pub(crate) fn quorum_hash_off_chain() -> Verdict {
    vector(|w, _, _| signed(w, FREE_BIT, w.hash_of(w.fork), 1).1)
}

pub(crate) fn missing_quorum() -> Verdict {
    vector(|w, p, tx| {
        w.quorums.quorums.retain(|(_, q)| q.quorum_hash != p.quorum_hash);
        tx
    })
}

pub(crate) fn invalid() -> Verdict {
    vector(|w, p, _| signed(w, FREE_BIT, p.quorum_hash, 2).1)
}
