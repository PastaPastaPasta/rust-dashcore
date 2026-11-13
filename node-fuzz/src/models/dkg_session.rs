//! `dkg_session` (LQ4 §4.3 T5, v1 and v2): whole quorums of dash-llmq's reference harness
//! ([`Network`]) run by an action script, each run checked against seven invariants.
//!
//! The input:
//! - byte 0, the member set: 0-127 `llmq_test` 3:2; 128-191 `llmq_test_dip0024` (4, rotated);
//!   192-247 `llmq_test` overridden to 5:3; 248-255 `llmq_test` overridden to 12 members,
//!   minSize 7 (`llmq_devnet`'s sizes). The fuzzer reaches the 32-share pending verify by
//!   repeated contributions at these sets. A 33:20 override, which reaches it with no action, is
//!   test-only and runs in the tests' release replays (68-118 CPU-s each, with the cache-off run);
//! - byte 1, flags: bit 0 the base is before V19 (the sessions sign under the legacy scheme), bit 1
//!   the process-wide scheme starts legacy, bits 2-3 a rotated set's quorum index (modulo its
//!   active quorum count);
//! - bytes 2-9, the seed (little-endian) of each member's randomness and batch weights;
//! - then at most [`MAX_ACTIONS`] actions, each an op byte (modulo 18) and its operand bytes, a
//!   missing operand reading 0. `node` and `member` are taken modulo the member count, `pos`
//!   modulo the node's queue length (an empty queue makes the action a no-op):
//!   - 0 advance: the current phase's queues are drained in pops of each node's pop size, then
//!     the next start function runs on every member in member order and its message is sent
//!     (Contribute, Complain, Justify, Commit; the fifth advance finalizes);
//!   - 1 pop (node, max): one batch of at most `max % 8 + 1`;
//!   - 2 drop (node, pos); 3 duplicate (node, pos); 4 swap (node, a, b);
//!   - 5 flip (node, pos, offset u16 LE, mode): mode bit 7 truncates the payload at the offset,
//!     else the byte at the offset is XORed with `mode | 1`;
//!   - 6 pop size (node, size): the node's drain pops take `size % 8 + 1`;
//!   - 7 equivocate (member, offset u16 LE, xor): the member's last message of this phase with a
//!     byte before its signature XORed with `xor | 1`, re-signed with its operator key, sent to
//!     every queue;
//!   - 8 send mode (member, phase, mode, mask): how the member's message of `phase % 4` is sent:
//!     `mode % 3` 0 to every queue, 1 to its own queue alone, 2 as a modified sender (a
//!     contribution whose shares for the members in `mask` (bit `j % 8`) are fresh keys, when its
//!     contribution-omit rate is 0; a premature commitment whose `sig` is re-signed over its sign
//!     hash; otherwise as 0);
//!   - 9 rate (member, type, rate): `SimError::ALL[type % 9]` at rate 0 or 100 (bit 0) for a
//!     byte below 128, else `rate % 101`;
//!   - 10 lie type (member, type): the member's commit-lie type draw answers `type % 5` (the last
//!     such action counts, from the start);
//!   - 11 scheme: the process-wide scheme flips;
//!   - 12 connections (member, sporks, facts): the member's `ConnSnapshot` for Complain: SPORK_23
//!     and SPORK_21 each 0, 1 or 2 (`sporks % 3`, `sporks / 3 % 3`), one fully connected node per
//!     member from a byte mixed from `facts`;
//!   - 13 misuse (member, which): a start function called out of phase order (repeated, or two
//!     ahead);
//!   - 14 missing row (node, who): the node's `qdkg_V` row of `who` is removed before its first
//!     dkgdb read in Commit;
//!   - 15 perturb (node, pos, field): in Commit, the `sig` (field 0) or `quorumSig` (field 1) of a
//!     queued premature commitment becomes bytes that still decode: under the basic scheme a valid
//!     signature over another hash, under the legacy scheme the object the bytes with byte 5 + 1
//!     (or those with bit 7 of byte 0 clear) decode to;
//!   - 16 finalizers (mask): member `i` finalizes iff bit `i % 8` is set (all by default);
//!   - 17 clear (node): the node's queue is dropped.
//!
//!   After the script, the remaining advances run. Each member's randomness is a [`ForcedRng`]
//!   over [`Source`] (honest coefficients from the member set's material, every other draw from
//!   the seed); its batch weights a [`SeededRng`] of the seed, so an input replays.
//!
//! Invariants (the field names a divergence):
//! 1. `inv1_fault`: no `DkgFault` but `PhaseOrder`, and that only from a scripted misuse.
//! 2. Per finalizer and per `validMembers` group of its accepted premature commitments with at
//!    least `minSize` entries, from the reference verifier and `aggregate_signatures_secure` and
//!    `recover_signature` alone: `inv2a_shape`, `inv2b_emitted` (one final for the key exactly
//!    when the assembled commitment verifies, and each emitted final verifies and is byte-equal
//!    to it), `inv2c_agreement`.
//! 3. `inv3_store`: each row against the node's receives up to its write, modelled from the
//!    popped bytes, the node's operator key and its recorded decisions. The harness sees each
//!    receive start (the preamble's `received…` status update of its member) and how many rows
//!    the call had written by then; the modelled receives must start in that order, and a row
//!    belongs to the last receive started before it, else to the call. No later start bounds a
//!    pop's last receive, so it is taken to run until the pop returns. `qdkg_V` only as a
//!    processed contribution's write in its own receive; in a pending verify, per entry pushed
//!    since the last one of a member neither bad nor complained about, one `qdkg_E` row of its
//!    last processed contribution and, when the valid share our blob of it decrypted to
//!    verifies under the scheme of the verify's call, one `qdkg_S` row of that share, in the
//!    receive or start that ran the verify. A member whose checked share fails is complained
//!    about once that verify's rows are written, so a later verify writes nothing for it.
//!    `qdkg_S` also for the exact share a justification revealed to us that Core accepts
//!    (complained-for indexes only, the share verifying under the receive's scheme), once, in
//!    the receive that accepted it, so in receive order and not in a later step.
//!    Of Core's bad marks, those from second messages, votes, repeated complaints and
//!    justifications are modelled; the others can only leave a member good here.
//! 4. `inv4_scores`: `Own` never scored; a decode failure scores per message, a bad batch
//!    signature at most once per peer and batch.
//! 5. `inv5_relay`: no relay of a premature commitment that fails full verification; a liar's
//!    own lied premature commitment is neither relayed nor served.
//! 6. `inv6_draws`: each step's draw tags in §2.9's order under its gate.
//! 7. `inv7_cache_twin`: the run with the session cache off gives the same events, invs, scores,
//!    store rows and finals, but where a node's cache key was used under both schemes, under
//!    another scheme than a Commit start that used a key but sent nothing, or after its rows
//!    changed ([`Run::cache_exceptions`]). Such a node is still compared on its scores, rows,
//!    the lines of every step but its Commit-phase receives and its finalize, its invs but those
//!    of premature commitments with an excepted key, and its finals of the other keys.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};

use dash_evo::llmq::{LlmqCommitment, commitment_version};
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{
    BlsCtx, BlsIesMultiBlobs, BlsPkValue, BlsSchemeKind, BlsSigValue, BlsSkValue, IesSeed,
    aggregate_signatures_secure, build_pub_key_share, build_quorum_verification_vector,
    make_new_key, member_id, public_keys_equal, recover_signature, stored_vvec_key,
    verify_contribution_share, verify_insecure, verify_verification_vector,
};
use dash_llmq::dkg::conn::{
    ConnFacts, ConnSnapshot, MIN_MASTERNODE_PROTO_VERSION, NODE_COMPACT_FILTERS, eval_spork,
};
use dash_llmq::dkg::reference::{
    DrawLog, DrawTag, ForcedRng, MemStore, Network, NetworkSetup, Node, Queued, RecordingDebug,
    StoreRow, TestMember, TestOperatorKey, quorum, seeded_rng,
};
use dash_llmq::dkg::rng::{CoreRng, DkgRng, KeyDraw, RangeDraw, SeededRng};
use dash_llmq::dkg::session::{
    BatchOutcome, DkgDebugSink, DkgEnv, DkgEvent, DkgFault, DkgInv, DkgMsgKind, DkgSession,
    DkgStore, FinalVerifier, MemberStatusBit, Misbehaviour, MissingVvec, OperatorKey, Sender,
    SerialRunner, SessionStatusBit, VerifiedContributions,
};
use dash_llmq::dkg::sim::{SimError, SimErrorRates};
use dash_llmq::dkg::wire::{
    Complaint, Contribution, Justification, PrematureCommitment, QuorumHash,
    final_commitment_bytes, vvec_hash,
};
use dash_params::{ChainParamsBuilder, LlmqParams, LlmqType, Network as Chain, Release};
use dash_pkc::bls::{BlsEagerPk, BlsEagerSig};
use dash_primitives::{DynBitset, commitment_bls_scheme};
use node_l1::Divergence;

const MAX_ACTIONS: usize = 128;
const MAX_QUEUE: usize = 64;
const OPS: u8 = 18;
const SIG_LEN: usize = 96;
const PHASES: [DkgMsgKind; 4] = [
    DkgMsgKind::Contribution,
    DkgMsgKind::Complaint,
    DkgMsgKind::Justification,
    DkgMsgKind::PrematureCommitment,
];

const INV1: &str = "inv1_fault";
const INV2A: &str = "inv2a_shape";
const INV2B: &str = "inv2b_emitted";
const INV2C: &str = "inv2c_agreement";
const INV3: &str = "inv3_store";
const INV4: &str = "inv4_scores";
const INV5: &str = "inv5_relay";
const INV6: &str = "inv6_draws";
const INV7: &str = "inv7_cache_twin";
/// Every field, in the order [`failures`] reports them.
const FIELDS: [&str; 9] = [INV1, INV2A, INV2B, INV2C, INV3, INV4, INV5, INV6, INV7];

/// A member set: its number (it offsets the key seed and scopes the memo), type and parameters,
/// the network seed its operator keys come from, and each member's honest coefficients.
struct Material {
    set: usize,
    t: LlmqType,
    params: LlmqParams,
    seed: u64,
    rotated: bool,
    keys: Vec<TestOperatorKey>,
    coefficients: Vec<Vec<BlsSkValue>>,
}

/// `t`'s parameters on regtest, else on devnet, as the reference verifier reads them.
#[allow(clippy::expect_used)] // Core's parameters, constants; the types used here all have an entry
fn chain_llmq(t: LlmqType) -> LlmqParams {
    [Chain::Regtest, Chain::Devnet]
        .into_iter()
        .find_map(|chain| {
            let setup = ChainParamsBuilder::new(chain, Release::V24_0).build().ok()?;
            setup.consensus.llmq(t).copied()
        })
        .expect("regtest or devnet runs the type")
}

fn overridden(size: u16, min_size: u16, threshold: u16, bad_votes: u16) -> LlmqParams {
    LlmqParams {
        size,
        min_size,
        threshold,
        dkg_bad_votes_threshold: bad_votes,
        ..chain_llmq(LlmqType::LlmqTest)
    }
}

fn material(set: usize, t: LlmqType, params: LlmqParams, rotated: bool) -> Material {
    let seed = 0xd5e0 + set as u64;
    let n = usize::from(params.size);
    let (_, keys) = quorum(n, seed);
    let coefficients = (0..n)
        .map(|i| {
            let mut rng = SeededRng::new(seed ^ 0xc0ef_0000 ^ ((i as u64) << 40));
            (0..params.threshold).map(|_| make_new_key(&mut rng)).collect()
        })
        .collect();
    Material {
        set,
        t,
        params,
        seed,
        rotated,
        keys,
        coefficients,
    }
}

static MATERIAL: [LazyLock<Material>; 4] = [
    LazyLock::new(|| material(0, LlmqType::LlmqTest, chain_llmq(LlmqType::LlmqTest), false)),
    LazyLock::new(|| {
        let t = LlmqType::LlmqTestDip0024;
        material(1, t, chain_llmq(t), true)
    }),
    LazyLock::new(|| {
        let d = chain_llmq(LlmqType::LlmqDevnet);
        let params = overridden(d.size, d.min_size, d.threshold, d.dkg_bad_votes_threshold);
        material(2, LlmqType::LlmqTest, params, false)
    }),
    LazyLock::new(|| material(3, LlmqType::LlmqTest, overridden(5, 3, 3, 3), false)),
];

/// The member set byte 0 names, the costly ones rarely.
fn set_index(b: u8) -> usize {
    match b {
        0..=127 => 0,
        128..=191 => 1,
        192..=247 => 3,
        248..=255 => 2,
    }
}

/// Builds every member set before the first input, so that no input's coverage includes it.
pub(super) fn start() {
    for m in &MATERIAL {
        LazyLock::force(m);
    }
}

type Decisions = Arc<Mutex<Vec<bool>>>;

/// A member's script-driven randomness: its honest coefficients from the material, every other
/// draw Core's over a [`SeededRng`] of the input's seed. It keeps each `rand_bool` answer.
struct Source {
    coefficients: &'static [BlsSkValue],
    rng: CoreRng<SeededRng>,
    decisions: Decisions,
}

impl DkgRng for Source {
    fn new_key(&mut self, why: KeyDraw) -> BlsSkValue {
        match why {
            KeyDraw::Coefficient(j) => {
                self.coefficients.get(j as usize).cloned().unwrap_or_else(|| self.rng.new_key(why))
            }
            _ => self.rng.new_key(why),
        }
    }

    fn ies_seed(&mut self, recipients: usize) -> IesSeed {
        self.rng.ies_seed(recipients)
    }

    fn rand_bool(&mut self, percent: u8, why: SimError) -> bool {
        let answer = self.rng.rand_bool(percent, why);
        lock(&self.decisions).push(answer);
        answer
    }

    fn rand_range(&mut self, n: u64, why: RangeDraw) -> u64 {
        self.rng.rand_range(n, why)
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Advance,
    Pop {
        node: u8,
        max: u8,
    },
    Drop {
        node: u8,
        pos: u8,
    },
    Dup {
        node: u8,
        pos: u8,
    },
    Swap {
        node: u8,
        a: u8,
        b: u8,
    },
    Flip {
        node: u8,
        pos: u8,
        offset: u16,
        mode: u8,
    },
    PopSize {
        node: u8,
        size: u8,
    },
    Equivocate {
        member: u8,
        offset: u16,
        xor: u8,
    },
    Mode {
        member: u8,
        phase: u8,
        mode: u8,
        mask: u8,
    },
    Rate {
        member: u8,
        kind: u8,
        rate: u8,
    },
    LieType {
        member: u8,
        lie: u8,
    },
    Scheme,
    Conn {
        member: u8,
        sporks: u8,
        facts: u8,
    },
    Misuse {
        member: u8,
        which: u8,
    },
    MissingRow {
        node: u8,
        who: u8,
    },
    Perturb {
        node: u8,
        pos: u8,
        field: u8,
    },
    Finalizers {
        mask: u8,
    },
    Clear {
        node: u8,
    },
}

impl Action {
    fn decode(op: u8, b: impl Fn(usize) -> u8) -> (Self, usize) {
        let w = |k: usize| u16::from_le_bytes([b(k), b(k + 1)]);
        match op % OPS {
            0 => (Self::Advance, 0),
            1 => (
                Self::Pop {
                    node: b(0),
                    max: b(1),
                },
                2,
            ),
            2 => (
                Self::Drop {
                    node: b(0),
                    pos: b(1),
                },
                2,
            ),
            3 => (
                Self::Dup {
                    node: b(0),
                    pos: b(1),
                },
                2,
            ),
            4 => (
                Self::Swap {
                    node: b(0),
                    a: b(1),
                    b: b(2),
                },
                3,
            ),
            5 => (
                Self::Flip {
                    node: b(0),
                    pos: b(1),
                    offset: w(2),
                    mode: b(4),
                },
                5,
            ),
            6 => (
                Self::PopSize {
                    node: b(0),
                    size: b(1),
                },
                2,
            ),
            7 => (
                Self::Equivocate {
                    member: b(0),
                    offset: w(1),
                    xor: b(3),
                },
                4,
            ),
            8 => (
                Self::Mode {
                    member: b(0),
                    phase: b(1),
                    mode: b(2),
                    mask: b(3),
                },
                4,
            ),
            9 => (
                Self::Rate {
                    member: b(0),
                    kind: b(1),
                    rate: b(2),
                },
                3,
            ),
            10 => (
                Self::LieType {
                    member: b(0),
                    lie: b(1),
                },
                2,
            ),
            11 => (Self::Scheme, 0),
            12 => (
                Self::Conn {
                    member: b(0),
                    sporks: b(1),
                    facts: b(2),
                },
                3,
            ),
            13 => (
                Self::Misuse {
                    member: b(0),
                    which: b(1),
                },
                2,
            ),
            14 => (
                Self::MissingRow {
                    node: b(0),
                    who: b(1),
                },
                2,
            ),
            15 => (
                Self::Perturb {
                    node: b(0),
                    pos: b(1),
                    field: b(2),
                },
                3,
            ),
            16 => (
                Self::Finalizers {
                    mask: b(0),
                },
                1,
            ),
            _ => (
                Self::Clear {
                    node: b(0),
                },
                1,
            ),
        }
    }

    #[cfg(test)]
    fn encode(self, out: &mut Vec<u8>) {
        let (op, args): (u8, Vec<u8>) = match self {
            Self::Advance => (0, vec![]),
            Self::Pop {
                node,
                max,
            } => (1, vec![node, max]),
            Self::Drop {
                node,
                pos,
            } => (2, vec![node, pos]),
            Self::Dup {
                node,
                pos,
            } => (3, vec![node, pos]),
            Self::Swap {
                node,
                a,
                b,
            } => (4, vec![node, a, b]),
            Self::Flip {
                node,
                pos,
                offset,
                mode,
            } => {
                let [lo, hi] = offset.to_le_bytes();
                (5, vec![node, pos, lo, hi, mode])
            }
            Self::PopSize {
                node,
                size,
            } => (6, vec![node, size]),
            Self::Equivocate {
                member,
                offset,
                xor,
            } => {
                let [lo, hi] = offset.to_le_bytes();
                (7, vec![member, lo, hi, xor])
            }
            Self::Mode {
                member,
                phase,
                mode,
                mask,
            } => (8, vec![member, phase, mode, mask]),
            Self::Rate {
                member,
                kind,
                rate,
            } => (9, vec![member, kind, rate]),
            Self::LieType {
                member,
                lie,
            } => (10, vec![member, lie]),
            Self::Scheme => (11, vec![]),
            Self::Conn {
                member,
                sporks,
                facts,
            } => (12, vec![member, sporks, facts]),
            Self::Misuse {
                member,
                which,
            } => (13, vec![member, which]),
            Self::MissingRow {
                node,
                who,
            } => (14, vec![node, who]),
            Self::Perturb {
                node,
                pos,
                field,
            } => (15, vec![node, pos, field]),
            Self::Finalizers {
                mask,
            } => (16, vec![mask]),
            Self::Clear {
                node,
            } => (17, vec![node]),
        };
        out.push(op);
        out.extend(args);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Script {
    set: u8,
    flags: u8,
    seed: u64,
    actions: Vec<Action>,
}

impl Script {
    fn parse(data: &[u8]) -> Self {
        let byte = |i: usize| data.get(i).copied().unwrap_or(0);
        let seed: [u8; 8] = std::array::from_fn(|k| byte(2 + k));
        let mut actions = Vec::new();
        let mut at = 10;
        while at < data.len() && actions.len() < MAX_ACTIONS {
            let (action, used) = Action::decode(data[at], |k| byte(at + 1 + k));
            actions.push(action);
            at += 1 + used;
        }
        Self {
            set: byte(0),
            flags: byte(1),
            seed: u64::from_le_bytes(seed),
            actions,
        }
    }

    #[cfg(test)]
    fn bytes(&self) -> Vec<u8> {
        let mut out = vec![self.set, self.flags];
        out.extend(self.seed.to_le_bytes());
        for a in &self.actions {
            a.encode(&mut out);
        }
        out
    }
}

/// A rate byte below 128 sets 0 or 100 by its low bit, any other `b % 101`.
fn rate_of(b: u8) -> i32 {
    match b {
        0..=127 => i32::from(b & 1) * 100,
        _ => i32::from(b % 101),
    }
}

/// A pop-size or pop-limit byte as a batch size, 1 to 8.
fn pop_len(b: u8) -> usize {
    usize::from(b % 8) + 1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendMode {
    Mesh,
    Own,
    Modified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepKind {
    Contribute,
    Complain,
    Justify,
    Commit,
    Finalize,
    Pop(DkgMsgKind),
    Misuse,
}

/// One call into a node's session: what it drew, decided and logged.
struct Step {
    kind: StepKind,
    tags: Range<usize>,
    bools: Range<usize>,
    events: Range<usize>,
    rates: SimErrorRates,
    /// The process-wide scheme during the call, as [`scheme_id`].
    scheme: u8,
    /// The draws a modified sender made after the session's: they end `tags`.
    suffix: Vec<DrawTag>,
    /// The dkgdb rows the call wrote, in order.
    rows: Vec<Row>,
    /// Each receive's start in the call, as [`watched`] saw it.
    starts: Vec<Start>,
}

/// A receive's start as the harness sees it: the preamble's `received…` status update of the
/// sender's member (`l/dkgsession.cpp:697-764`), and how many dkgdb rows the call had written
/// before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Start {
    who: usize,
    bit: MemberStatusBit,
    rows: usize,
}

/// A node's dkgdb during one call, counting its writes.
struct CountedStore<'a> {
    inner: &'a mut MemStore,
    writes: &'a Cell<usize>,
}

impl CountedStore<'_> {
    fn count(&self) {
        self.writes.set(self.writes.get() + 1);
    }
}

impl DkgStore for CountedStore<'_> {
    fn write_vvec(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, vvec: &[BlsPkValue]) {
        self.count();
        self.inner.write_vvec(t, base, who, vvec);
    }

    fn write_sk(&mut self, t: LlmqType, base: &QuorumHash, who: &ProTxHash, sk: &BlsSkValue) {
        self.count();
        self.inner.write_sk(t, base, who, sk);
    }

    fn write_encrypted(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        who: &ProTxHash,
        blobs: &BlsIesMultiBlobs,
    ) {
        self.count();
        self.inner.write_encrypted(t, base, who, blobs);
    }

    fn verified_contributions(
        &mut self,
        t: LlmqType,
        base: &QuorumHash,
        valid: &[bool],
    ) -> Result<VerifiedContributions, MissingVvec> {
        self.inner.verified_contributions(t, base, valid)
    }
}

/// A node's debug sink during one call, noting each receive's [`Start`].
struct StartLog<'a> {
    inner: &'a mut RecordingDebug,
    writes: &'a Cell<usize>,
    starts: Vec<Start>,
}

impl DkgDebugSink for StartLog<'_> {
    fn init_local_session_status(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        base: &QuorumHash,
        base_height: i32,
    ) {
        self.inner.init_local_session_status(t, quorum_index, base, base_height);
    }

    fn update_local_session_status(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        bit: SessionStatusBit,
    ) {
        self.inner.update_local_session_status(t, quorum_index, bit);
    }

    fn update_local_member_status(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        idx: usize,
        bit: MemberStatusBit,
    ) {
        if matches!(
            bit,
            MemberStatusBit::ReceivedContribution
                | MemberStatusBit::ReceivedComplaint
                | MemberStatusBit::ReceivedJustification
        ) {
            self.starts.push(Start {
                who: idx,
                bit,
                rows: self.writes.get(),
            });
        }
        self.inner.update_local_member_status(t, quorum_index, idx, bit);
    }

    fn add_local_member_complaint(
        &mut self,
        t: LlmqType,
        quorum_index: i32,
        idx: usize,
        from: usize,
    ) {
        self.inner.add_local_member_complaint(t, quorum_index, idx, from);
    }
}

/// The preamble's status bit of a `kind` receive; a premature commitment has no preamble.
fn received_bit(kind: DkgMsgKind) -> Option<MemberStatusBit> {
    match kind {
        DkgMsgKind::Contribution => Some(MemberStatusBit::ReceivedContribution),
        DkgMsgKind::Complaint => Some(MemberStatusBit::ReceivedComplaint),
        DkgMsgKind::Justification => Some(MemberStatusBit::ReceivedJustification),
        _ => None,
    }
}

/// [`Node::step`], with the store and debug sink seen through [`CountedStore`] and
/// [`StartLog`]: the result and each receive's [`Start`]. It repeats that function's
/// environment and event collection, so a change there must be made here too.
fn watched<R>(
    node: &mut Node,
    bls: BlsCtx,
    f: impl FnOnce(&mut DkgSession<TestMember>, &mut DkgEnv<'_>) -> R,
) -> (R, Vec<Start>) {
    let Node {
        session,
        key,
        store,
        debug,
        sim,
        rng,
        verify,
        weights,
        events,
        ..
    } = node;
    let writes = Cell::new(0);
    let mut store = CountedStore {
        inner: store,
        writes: &writes,
    };
    let mut debug = StartLog {
        inner: debug,
        writes: &writes,
        starts: Vec::new(),
    };
    let mut env = DkgEnv {
        bls,
        rng: &mut **rng,
        weights: &mut **weights,
        key,
        store: &mut store,
        debug: &mut debug,
        sim,
        runner: &SerialRunner,
        verify,
    };
    let out = f(session, &mut env);
    events.extend(session.take_events());
    (out, debug.starts)
}

/// One dkgdb write: its type, base and row.
type Row = (LlmqType, QuorumHash, StoreRow);

struct PopRecord {
    phase: DkgMsgKind,
    bls: BlsCtx,
    batch: Vec<Queued>,
    outcome: BatchOutcome<usize>,
}

struct FinalRecord {
    bls: BlsCtx,
    finals: Vec<LlmqCommitment>,
}

struct Run {
    m: &'static Material,
    net: Network,
    stage: u8,
    logs: Vec<DrawLog>,
    decisions: Vec<Decisions>,
    modes: Vec<[(SendMode, u8); 4]>,
    pop_size: Vec<usize>,
    conn: Vec<ConnSnapshot>,
    missing: Vec<Vec<usize>>,
    finalizers: u8,
    /// Each member's last message of the current phase, as sent.
    sent: Vec<Option<Vec<u8>>>,
    steps: Vec<Vec<Step>>,
    pops: Vec<Vec<PopRecord>>,
    finals: Vec<Option<FinalRecord>>,
    faults: usize,
    /// The session hash and `validMembers` of each member's premature commitment as its session
    /// built it.
    own_pcommit: Vec<Option<([u8; 32], Vec<bool>)>>,
    removed: Vec<Vec<Row>>,
    memo: Memo,
}

fn scheme_id(bls: BlsCtx) -> u8 {
    u8::from(bls.scheme() == BlsSchemeKind::Legacy)
}

/// The scheme a [`scheme_id`] names.
fn scheme_kind(id: u8) -> BlsSchemeKind {
    if id == 1 {
        BlsSchemeKind::Legacy
    } else {
        BlsSchemeKind::Basic
    }
}

/// A node's draw, decision, event and row counts before a call.
type Mark = (usize, usize, usize, usize);

/// A node's excepted cache keys, and each received premature commitment's key by session hash.
type Exceptions = (BTreeSet<Vec<bool>>, BTreeMap<[u8; 32], Vec<bool>>);

impl Run {
    #[cfg(test)]
    fn play(script: &Script, cache: bool) -> Self {
        Self::play_in(&MATERIAL[set_index(script.set)], script, cache)
    }

    /// `script` on member set `m`, whatever its byte 0 names.
    fn play_in(m: &'static Material, script: &Script, cache: bool) -> Self {
        let n = usize::from(m.params.size);
        let legacy_base = script.flags & 1 != 0;
        let base = if m.rotated {
            let count = i32::from(m.params.signing_active_quorum_count.max(1));
            NetworkSetup::rotated(&m.params, i32::from(script.flags >> 2 & 3) % count)
        } else {
            NetworkSetup::new(&m.params)
        };
        let setup = NetworkSetup {
            v19_after_base: !legacy_base,
            ..base
        };
        let mut net = Network::with_setup(
            m.t,
            &m.params,
            n,
            m.seed,
            || Box::new(SeededRng::new(0)),
            seeded_rng,
            setup,
        );
        if script.flags & 2 != 0 {
            net.set_bls(BlsCtx::LEGACY);
        }
        let mut lie_types = vec![None; n];
        for a in &script.actions {
            if let Action::LieType {
                member,
                lie,
            } = *a
            {
                lie_types[usize::from(member) % n] = Some(u64::from(lie % 5));
            }
        }
        let (mut logs, mut decisions) = (Vec::new(), Vec::new());
        for (i, node) in net.nodes.iter_mut().enumerate() {
            if !cache {
                node.session = node.session.clone().without_cache();
            }
            let source = Source {
                coefficients: &m.coefficients[i],
                rng: CoreRng(SeededRng::new(script.seed ^ ((i as u64 + 1) << 48))),
                decisions: Decisions::default(),
            };
            decisions.push(source.decisions.clone());
            let rng = ForcedRng::new(source);
            let rng = match lie_types[i] {
                Some(k) => rng.with_commit_lie_type(k),
                None => rng,
            };
            logs.push(rng.log());
            node.rng = Box::new(rng);
            node.weights = Box::new(SeededRng::new(script.seed ^ 0x5eed ^ ((i as u64) << 40)));
        }
        let mut run = Self {
            m,
            net,
            stage: 0,
            logs,
            decisions,
            modes: vec![[(SendMode::Mesh, 0); 4]; n],
            pop_size: vec![8; n],
            conn: vec![ConnSnapshot::default(); n],
            missing: vec![Vec::new(); n],
            finalizers: 0xff,
            sent: vec![None; n],
            steps: (0..n).map(|_| Vec::new()).collect(),
            pops: (0..n).map(|_| Vec::new()).collect(),
            finals: (0..n).map(|_| None).collect(),
            faults: 0,
            own_pcommit: vec![None; n],
            removed: vec![Vec::new(); n],
            memo: Memo::new((m.set, script.flags)),
        };
        for a in &script.actions {
            run.act(*a);
        }
        while run.stage < 5 {
            run.advance();
        }
        run
    }

    fn n(&self) -> usize {
        self.net.nodes.len()
    }

    fn is_test(&self) -> bool {
        self.m.t == LlmqType::LlmqTest
    }

    fn act(&mut self, a: Action) {
        let n = self.n();
        let at = |b: u8| usize::from(b) % n;
        match a {
            Action::Advance => self.advance(),
            Action::Pop {
                node,
                max,
            } => {
                self.pop(at(node), pop_len(max));
            }
            Action::Drop {
                node,
                pos,
            } => {
                let q = &mut self.net.nodes[at(node)].queue;
                if !q.is_empty() {
                    q.remove(usize::from(pos) % q.len());
                }
            }
            Action::Dup {
                node,
                pos,
            } => {
                let q = &mut self.net.nodes[at(node)].queue;
                if !q.is_empty() && q.len() < MAX_QUEUE {
                    let k = usize::from(pos) % q.len();
                    let e = q[k].clone();
                    q.insert(k, e);
                }
            }
            Action::Swap {
                node,
                a,
                b,
            } => {
                let q = &mut self.net.nodes[at(node)].queue;
                if !q.is_empty() {
                    let len = q.len();
                    q.swap(usize::from(a) % len, usize::from(b) % len);
                }
            }
            Action::Flip {
                node,
                pos,
                offset,
                mode,
            } => {
                let q = &mut self.net.nodes[at(node)].queue;
                if q.is_empty() {
                    return;
                }
                let k = usize::from(pos) % q.len();
                let mut bytes = q[k].1.to_vec();
                if bytes.is_empty() {
                    return;
                }
                let o = usize::from(offset) % bytes.len();
                if mode & 0x80 != 0 {
                    bytes.truncate(o);
                } else {
                    bytes[o] ^= mode | 1;
                }
                q[k].1 = bytes.into();
            }
            Action::PopSize {
                node,
                size,
            } => self.pop_size[at(node)] = pop_len(size),
            Action::Equivocate {
                member,
                offset,
                xor,
            } => self.equivocate(at(member), offset, xor),
            Action::Mode {
                member,
                phase,
                mode,
                mask,
            } => {
                let mode = match mode % 3 {
                    0 => SendMode::Mesh,
                    1 => SendMode::Own,
                    _ => SendMode::Modified,
                };
                self.modes[at(member)][usize::from(phase % 4)] = (mode, mask);
            }
            Action::Rate {
                member,
                kind,
                rate,
            } => {
                let kind = SimError::ALL[usize::from(kind) % SimError::ALL.len()];
                let _ = self.net.nodes[at(member)].sim.set(kind, rate_of(rate));
            }
            Action::LieType {
                ..
            } => {}
            Action::Scheme => {
                let next = if self.net.bls.scheme() == BlsSchemeKind::Legacy {
                    BlsCtx::BASIC
                } else {
                    BlsCtx::LEGACY
                };
                self.net.set_bls(next);
            }
            Action::Conn {
                member,
                sporks,
                facts,
            } => {
                self.conn[at(member)] = self.snapshot(sporks, facts);
            }
            Action::Misuse {
                member,
                which,
            } => self.misuse(at(member), which),
            Action::MissingRow {
                node,
                who,
            } => self.missing[at(node)].push(at(who)),
            Action::Perturb {
                node,
                pos,
                field,
            } => self.perturb(at(node), pos, field),
            Action::Finalizers {
                mask,
            } => self.finalizers = mask,
            Action::Clear {
                node,
            } => self.net.nodes[at(node)].queue.clear(),
        }
    }

    fn snapshot(&self, sporks: u8, facts: u8) -> ConnSnapshot {
        let spork = |v: u8| i64::from(v % 3);
        let t = self.m.t;
        let mut snap = ConnSnapshot {
            pose_enabled: eval_spork(t, spork(sporks)),
            all_members_connected: eval_spork(t, spork(sporks / 3)),
            ..ConnSnapshot::default()
        };
        for (j, member) in self.net.members.iter().enumerate() {
            let f = facts.rotate_left(j as u32 % 8) ^ (j as u8).wrapping_mul(0x9d);
            let who = member.pro_tx_hash;
            if f & 1 != 0 {
                snap.nodes.push(ConnFacts {
                    verified_pro_tx: if f & 0x40 != 0 {
                        ProTxHash::from([0; 32])
                    } else {
                        who
                    },
                    version: MIN_MASTERNODE_PROTO_VERSION - i32::from(f >> 1 & 1),
                    masternode_connection: f & 4 != 0,
                    relay_txs: f & 8 != 0,
                    services: if f & 16 != 0 {
                        NODE_COMPACT_FILTERS
                    } else {
                        0
                    },
                });
            }
            if f & 0xa0 == 0xa0 {
                snap.outbound_failed_too_many.insert(who);
            }
            if f & 0x82 == 0x80 {
                snap.platform_banned.insert(who);
            }
        }
        snap
    }

    /// The node's draw, decision, event and row counts.
    fn mark(&self, i: usize) -> Mark {
        let node = &self.net.nodes[i];
        (
            lock(&self.logs[i]).len(),
            lock(&self.decisions[i]).len(),
            node.events.len(),
            node.store.rows.len(),
        )
    }

    fn record(
        &mut self,
        i: usize,
        kind: StepKind,
        from: Mark,
        suffix: Vec<DrawTag>,
        starts: Vec<Start>,
    ) {
        let to = self.mark(i);
        let node = &self.net.nodes[i];
        let step = Step {
            kind,
            tags: from.0..to.0,
            bools: from.1..to.1,
            events: from.2..to.2,
            rates: node.sim,
            scheme: scheme_id(self.net.bls),
            suffix,
            rows: node.store.rows.get(from.3..to.3).map(<[Row]>::to_vec).unwrap_or_default(),
            starts,
        };
        self.steps[i].push(step);
    }

    /// `f` on node `i`'s session, recorded as a `kind` step.
    fn stepped<T>(
        &mut self,
        i: usize,
        kind: StepKind,
        f: impl FnOnce(&mut DkgSession<TestMember>, &mut DkgEnv<'_>) -> T,
    ) -> T {
        let (bls, from) = (self.net.bls, self.mark(i));
        let (out, starts) = watched(&mut self.net.nodes[i], bls, f);
        self.record(i, kind, from, Vec::new(), starts);
        out
    }

    /// One batch of at most `max` messages at node `i`, as [`dash_llmq::dkg::reference::Node::pop`]
    /// with the outcome kept.
    fn pop(&mut self, i: usize, max: usize) -> bool {
        let (bls, phase) = (self.net.bls, self.net.phase);
        if self.net.nodes[i].queue.is_empty() || self.stage == 0 || self.stage > 4 {
            return false;
        }
        let from = self.mark(i);
        let node = &mut self.net.nodes[i];
        let take = node.queue.len().min(max);
        let popped: Vec<Queued> = node.queue.drain(..take).collect();
        let batch: Vec<(Sender<usize>, &[u8])> = popped.iter().map(|(s, p)| (*s, &p[..])).collect();
        let (outcome, starts) = watched(node, bls, |s, env| s.process_batch(phase, &batch, env));
        node.misbehaving.extend(outcome.misbehaving.iter().map(|s| s.misbehaviour));
        node.relayed.extend(outcome.relay.iter().map(|r| r.inv));
        node.events.extend(outcome.events.iter().cloned());
        self.record(i, StepKind::Pop(phase), from, Vec::new(), starts);
        self.pops[i].push(PopRecord {
            phase,
            bls,
            batch: popped,
            outcome,
        });
        true
    }

    fn drain(&mut self) {
        for i in 0..self.n() {
            while self.pop(i, self.pop_size[i]) {}
        }
    }

    fn deliver(&mut self, i: usize, payload: Vec<u8>, mode: SendMode) {
        match mode {
            SendMode::Own => {
                let own: Arc<[u8]> = payload.as_slice().into();
                self.net.nodes[i].queue.push_back((Sender::Own, own));
            }
            SendMode::Mesh | SendMode::Modified => self.net.send(i, &payload),
        }
        self.sent[i] = Some(payload);
    }

    fn advance(&mut self) {
        let n = self.n();
        let bls = self.net.bls;
        let scheme = bls.scheme();
        match self.stage {
            0 => {
                self.sent = vec![None; n];
                for i in 0..n {
                    let (mode, mask) = self.modes[i][0];
                    let omits = self.is_test()
                        && self.net.nodes[i].sim.percent(SimError::ContributionOmit) > 0;
                    let from = self.mark(i);
                    if mode == SendMode::Modified && !omits {
                        let lie_to: BTreeSet<usize> =
                            (0..n).filter(|&j| j != i && mask >> (j % 8) & 1 == 1).collect();
                        let qc = self.net.contribute_modified(i, &lie_to);
                        let mut suffix: Vec<DrawTag> = lie_to
                            .iter()
                            .map(|&j| DrawTag::Key(KeyDraw::ContributionLie(j as u32)))
                            .collect();
                        suffix.push(DrawTag::Ies(n));
                        self.record(i, StepKind::Contribute, from, suffix, Vec::new());
                        self.sent[i] = Some(qc.payload(scheme));
                    } else {
                        match self.stepped(i, StepKind::Contribute, |s, env| s.contribute(env)) {
                            Ok(Some(qc)) => self.deliver(i, qc.payload(scheme), mode),
                            Ok(None) => {}
                            Err(_) => self.faults += 1,
                        }
                    }
                }
                self.stage = 1;
            }
            1..=3 => {
                self.drain();
                let phase = PHASES[usize::from(self.stage)];
                self.net.enter(phase);
                self.sent = vec![None; n];
                for i in 0..n {
                    let mode = self.modes[i][usize::from(self.stage)].0;
                    let out = match self.stage {
                        1 => {
                            let conn = self.conn[i].clone();
                            self.stepped(i, StepKind::Complain, |s, env| {
                                s.verify_and_complain(&conn, env)
                            })
                            .map(|qc| qc.map(|qc| qc.payload(scheme)))
                        }
                        2 => self
                            .stepped(i, StepKind::Justify, |s, env| s.verify_and_justify(env))
                            .map(|qj| qj.map(|qj| qj.payload(scheme))),
                        _ => {
                            self.remove_rows(i);
                            let qc = self
                                .stepped(i, StepKind::Commit, |s, env| s.verify_and_commit(env));
                            Ok(qc.map(|qc| {
                                self.own_pcommit[i] =
                                    Some((*qc.session_hash(), qc.valid_members()));
                                qc.payload(scheme)
                            }))
                        }
                    };
                    match out {
                        Ok(Some(mut payload)) => {
                            if mode == SendMode::Modified && self.stage == 3 {
                                self.resign(i, DkgMsgKind::PrematureCommitment, &mut payload);
                            }
                            self.deliver(i, payload, mode);
                        }
                        Ok(None) => {}
                        Err(_) => self.faults += 1,
                    }
                }
                self.stage += 1;
            }
            4 => {
                self.drain();
                for i in 0..n {
                    if self.finalizers >> (i % 8) & 1 == 0 {
                        continue;
                    }
                    let finals =
                        self.stepped(i, StepKind::Finalize, |s, env| s.finalize_commitments(env));
                    self.finals[i] = Some(FinalRecord {
                        bls,
                        finals,
                    });
                }
                self.stage = 5;
            }
            _ => {}
        }
    }

    fn remove_rows(&mut self, i: usize) {
        let node = &mut self.net.nodes[i];
        if self.missing[i].is_empty() || !node.store.reads.is_empty() {
            return;
        }
        let gone: BTreeSet<ProTxHash> =
            self.missing[i].iter().map(|&j| self.net.members[j].pro_tx_hash).collect();
        let (removed, kept) = std::mem::take(&mut node.store.rows)
            .into_iter()
            .partition(|row| matches!(&row.2, StoreRow::Vvec { who, .. } if gone.contains(who)));
        node.store.rows = kept;
        self.removed[i] = removed;
    }

    /// `payload`'s last [`SIG_LEN`] bytes re-signed by member `i` over its sign hash under the
    /// current scheme; `false` when it does not decode.
    fn resign(&self, i: usize, kind: DkgMsgKind, payload: &mut [u8]) -> bool {
        let (params, bls) = (&self.net.params, self.net.bls);
        let Some(hash) = sign_hash(kind, payload, params, bls) else {
            return false;
        };
        let node = &self.net.nodes[i];
        let sig = node.key.sign(&hash, node.session.signs_legacy());
        let len = payload.len();
        payload[len - SIG_LEN..].copy_from_slice(&sig.to_bytes(bls.scheme()));
        true
    }

    fn equivocate(&mut self, i: usize, offset: u16, xor: u8) {
        if !(1..=4).contains(&self.stage) {
            return;
        }
        let Some(mut payload) = self.sent[i].clone() else {
            return;
        };
        if payload.len() <= SIG_LEN {
            return;
        }
        let body = payload.len() - SIG_LEN;
        payload[usize::from(offset) % body] ^= xor | 1;
        if self.resign(i, self.net.phase, &mut payload) {
            self.net.send(i, &payload);
        }
    }

    fn misuse(&mut self, i: usize, which: u8) {
        // A start function that is not the next one: Core's handler never makes these calls.
        let call = match (self.stage, which % 2) {
            (1, 0) | (2.., 1) => 0,
            (0, _) | (2, 0) => 1,
            _ => 2,
        };
        let conn = ConnSnapshot::default();
        let out = self.stepped(i, StepKind::Misuse, |s, env| match call {
            0 => s.contribute(env).map(|_| ()),
            1 => s.verify_and_complain(&conn, env).map(|_| ()),
            _ => s.verify_and_justify(env).map(|_| ()),
        });
        if out != Err(DkgFault::PhaseOrder) {
            self.faults += 1;
        }
    }

    fn perturb(&mut self, i: usize, pos: u8, field: u8) {
        if self.stage != 4 {
            return;
        }
        let scheme = self.net.bls.scheme();
        if self.net.nodes[i].queue.is_empty() {
            return;
        }
        let other = self.m.keys[0].sign(&[0x5a; 32], scheme == BlsSchemeKind::Legacy);
        let q = &mut self.net.nodes[i].queue;
        let k = usize::from(pos) % q.len();
        let mut bytes = q[k].1.to_vec();
        let back = SIG_LEN * (1 + usize::from(field % 2));
        if bytes.len() < back {
            return;
        }
        let at = bytes.len() - back;
        let field_bytes = &mut bytes[at..at + SIG_LEN];
        let new = match scheme {
            BlsSchemeKind::Legacy => {
                let mut b = [0u8; SIG_LEN];
                b.copy_from_slice(field_bytes);
                b[5] = b[5].wrapping_add(1);
                let decoded = BlsSigValue::from_canonical(&b, scheme).or_else(|| {
                    if b[0] & 0x80 == 0 {
                        return None;
                    }
                    b[0] &= 0x7f;
                    BlsSigValue::from_canonical(&b, scheme)
                });
                let Some(v) = decoded else {
                    return;
                };
                v.to_bytes(scheme)
            }
            _ => other.to_bytes(scheme),
        };
        field_bytes.copy_from_slice(&new);
        q[k].1 = bytes.into();
    }

    fn member_index(&self, pro_tx: &ProTxHash) -> Option<usize> {
        self.net.members.iter().position(|m| m.pro_tx_hash == *pro_tx)
    }
}

/// A `kind` message's member, signature and sign hash, when `payload` reads as one under `bls`.
fn signed_by(
    kind: DkgMsgKind,
    payload: &[u8],
    p: &LlmqParams,
    bls: BlsCtx,
) -> Option<(ProTxHash, BlsSigValue, [u8; 32])> {
    macro_rules! parts {
        ($m:expr) => {
            $m.ok().map(|m| (*m.pro_tx_hash(), m.sig().clone(), *m.sign_hash()))
        };
    }
    match kind {
        DkgMsgKind::Contribution => parts!(Contribution::read(payload, p, bls)),
        DkgMsgKind::Complaint => parts!(Complaint::read(payload, p, bls)),
        DkgMsgKind::Justification => parts!(Justification::read(payload, p, bls)),
        _ => parts!(PrematureCommitment::read(payload, p, bls)),
    }
}

fn sign_hash(kind: DkgMsgKind, payload: &[u8], p: &LlmqParams, bls: BlsCtx) -> Option<[u8; 32]> {
    signed_by(kind, payload, p, bls).map(|(_, _, hash)| hash)
}

/// A final commitment's version, its keys' scheme and its `quorumIndex` field, as
/// `FinalizeCommitments` sets them.
fn final_header(setup: &NetworkSetup) -> (u16, BlsSchemeKind, Option<i16>) {
    let version = commitment_version(setup.rotation_enabled, setup.v19_after_base);
    // `int` to `int16_t`, as Core assigns it.
    let index = if setup.rotation_enabled {
        setup.quorum_index as i16
    } else {
        0
    };
    (version, commitment_bls_scheme(version), matches!(version, 2 | 4).then_some(index))
}

fn bits(set: &DynBitset) -> Vec<bool> {
    (0..set.num_bits()).map(|i| set.get(i) == Some(true)).collect()
}

fn bitset(bits: &[bool]) -> Option<DynBitset> {
    let mut data = vec![0u8; bits.len().div_ceil(8)];
    for i in (0..bits.len()).filter(|&i| bits[i]) {
        data[i / 8] |= 1 << (i % 8);
    }
    DynBitset::new(bits.len() as u64, data).ok()
}

impl Run {
    fn inv2(&self, out: &mut BTreeSet<&'static str>) {
        let params = &self.net.params;
        let setup = &self.net.setup;
        let min = usize::from(params.min_size);
        let (version, scheme, quorum_index) = final_header(setup);
        // Per finalizer: each group's accepted entries (session hash and the scheme it was
        // accepted under), the finalize scheme, and its finals' bytes.
        type View = BTreeMap<Vec<bool>, (Vec<([u8; 32], u8)>, u8, Vec<Vec<u8>>)>;
        let mut views: Vec<View> = Vec::new();
        // Equal accepted entries, received under equal schemes, assemble the same final at every
        // finalizer: each reconstruction and each verdict is computed once a run, by the finalize
        // scheme.
        type Built = BTreeMap<(u8, Vec<([u8; 32], u8)>), Option<Vec<u8>>>;
        let mut built: Built = BTreeMap::new();
        let mut verdicts: BTreeMap<(u8, Vec<u8>), bool> = BTreeMap::new();
        for (i, rec) in self.finals.iter().enumerate() {
            let Some(rec) = rec else {
                continue;
            };
            let flag = rec.bls.scheme();
            let session = &self.net.nodes[i].session;
            let mut full: BTreeMap<[u8; 32], bool> = BTreeMap::new();
            let mut accepted_under: BTreeMap<[u8; 32], u8> = BTreeMap::new();
            for pop in self.pops[i].iter().filter(|p| p.phase == DkgMsgKind::PrematureCommitment) {
                for (hash, skipped) in relays(&pop.outcome) {
                    let e = full.entry(hash).or_insert(true);
                    *e &= !skipped && pop.bls.scheme() == flag;
                    accepted_under.entry(hash).or_insert(scheme_id(pop.bls));
                }
            }
            let mut groups: BTreeMap<Vec<bool>, Vec<(&[u8; 32], &PrematureCommitment)>> =
                BTreeMap::new();
            for hash in full.keys() {
                if let Some(qc) = session.get_premature_commitment(hash) {
                    groups.entry(qc.valid_members()).or_default().push((hash, qc));
                }
            }
            let mut emitted: BTreeMap<Vec<bool>, Vec<&LlmqCommitment>> = BTreeMap::new();
            for qc in &rec.finals {
                emitted.entry(bits(qc.valid_members())).or_default().push(qc);
            }
            for key in emitted.keys() {
                if groups.get(key).is_none_or(|g| g.len() < min) {
                    out.insert(INV2A);
                }
            }
            let mut verifier = setup.verifier(self.net.llmq_type, params, &self.net.members);
            verifier.bls = rec.bls;
            let at = scheme_id(rec.bls);
            let mut verify = |qc: &LlmqCommitment, bytes: Vec<u8>| {
                *verdicts
                    .entry((at, bytes))
                    .or_insert_with(|| verifier.verify(&setup.base.hash, qc))
            };
            let mut view = View::new();
            for (key, group) in &groups {
                if group.len() < min {
                    continue;
                }
                let first = group[0].1;
                let pk = first.quorum_public_key().to_bytes(flag);
                let matching: Vec<&PrematureCommitment> = group
                    .iter()
                    .map(|(_, qc)| *qc)
                    .filter(|qc| {
                        qc.quorum_public_key().to_bytes(flag) == pk
                            && qc.quorum_vvec_hash() == first.quorum_vvec_hash()
                    })
                    .collect();
                let mut signers = vec![false; usize::from(params.size)];
                for qc in &matching {
                    if let Some(idx) = self.member_index(qc.pro_tx_hash()) {
                        signers[idx] = true;
                    }
                }
                let finals = emitted.get(key).map(Vec::as_slice).unwrap_or_default();
                for e in finals {
                    let shape = bits(e.valid_members()) == *key
                        && e.quorum_public_key().to_bytes(scheme)
                            == first.quorum_public_key().to_bytes(scheme)
                        && e.quorum_vvec_hash().as_bytes() == first.quorum_vvec_hash()
                        && bits(e.signers()) == signers
                        && e.version() == version
                        && e.quorum_index() == quorum_index
                        && e.llmq_type() == self.net.llmq_type
                        && e.quorum_hash() == setup.base.hash;
                    if !shape {
                        out.insert(INV2A);
                    }
                }
                let hashes: Vec<([u8; 32], u8)> = matching
                    .iter()
                    .map(|qc| {
                        let hash = *qc.session_hash();
                        (hash, accepted_under.get(&hash).copied().unwrap_or(u8::MAX))
                    })
                    .collect();
                let assembled = built
                    .entry((at, hashes))
                    .or_insert_with(|| {
                        let qc = self.assemble(first, &matching, key, &signers, rec.bls)?;
                        let bytes = final_commitment_bytes(&qc);
                        verify(&qc, bytes.clone()).then_some(bytes)
                    })
                    .clone();
                // Core appends the final it verified (`a/dkgsession.cpp:723-731`): one per key,
                // and those bytes.
                if finals.len() != usize::from(assembled.is_some()) {
                    out.insert(INV2B);
                }
                for e in finals {
                    let bytes = final_commitment_bytes(e);
                    if assembled.as_ref() != Some(&bytes) || !verify(e, bytes) {
                        out.insert(INV2B);
                    }
                }
                let live = group.iter().all(|(h, _)| full.get(*h) == Some(&true));
                if live && finals.is_empty() {
                    out.insert(INV2B);
                }
                let hashes = group
                    .iter()
                    .map(|(h, _)| (**h, accepted_under.get(*h).copied().unwrap_or(u8::MAX)))
                    .collect();
                let bytes = finals.iter().map(|qc| final_commitment_bytes(qc)).collect();
                view.insert(key.clone(), (hashes, scheme_id(rec.bls), bytes));
            }
            views.push(view);
        }
        for (k, a) in views.iter().enumerate() {
            for b in &views[k + 1..] {
                for (key, (hashes, flag, finals)) in a {
                    if let Some((h2, f2, finals2)) = b.get(key)
                        && hashes == h2
                        && flag == f2
                        && finals != finals2
                    {
                        out.insert(INV2C);
                    }
                }
            }
        }
    }

    /// The group's final commitment from `matching`, as `FinalizeCommitments` builds it; `None`
    /// when the recovery fails.
    fn assemble(
        &self,
        first: &PrematureCommitment,
        matching: &[&PrematureCommitment],
        key: &[bool],
        signers: &[bool],
        bls: BlsCtx,
    ) -> Option<LlmqCommitment> {
        let (mut sigs, mut pks, mut ids, mut shares) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for qc in matching {
            let idx = self.member_index(qc.pro_tx_hash())?;
            sigs.push(qc.sig());
            pks.push(&self.net.members[idx].operator_pk);
            ids.push(member_id(qc.pro_tx_hash()));
            shares.push(qc.quorum_sig());
        }
        let members_sig = aggregate_signatures_secure(&sigs, &pks, bls.scheme());
        let quorum_sig = recover_signature(&shares, &ids)?;
        let (version, scheme, quorum_index) = final_header(&self.net.setup);
        Some(LlmqCommitment::new(
            version,
            self.net.llmq_type,
            *first.quorum_hash(),
            quorum_index,
            bitset(signers)?,
            bitset(key)?,
            BlsEagerPk::new(first.quorum_public_key().clone(), scheme),
            (*first.quorum_vvec_hash()).into(),
            BlsEagerSig::new(quorum_sig, scheme),
            BlsEagerSig::new(members_sig, scheme),
        ))
    }

    /// Each dkgdb row node `i` writes is one Core writes, given what the node had received when
    /// it wrote it (v24.0.0 `l/dkgsession.cpp:213-272`, :326-356, :430-485;
    /// `a/dkgsession.cpp:145-212`, :373-388).
    /// The receives are modelled from the popped bytes, the node's operator key and its recorded
    /// decisions, and must start as the harness saw them ([`Step::starts`]). A row belongs to
    /// the last receive that started before it was written: a `qdkg_V` row or a justified
    /// `qdkg_S` row is that receive's own write, and a pending verify's rows are those of the
    /// receive or start that ran it.
    fn inv3(&self, out: &mut BTreeSet<&'static str>) {
        for i in 0..self.n() {
            let me = member_id(&self.net.members[i].pro_tx_hash);
            let mut rx = Received::new(self.n());
            for (step, pop) in self.steps_with_pops(i) {
                // A pending verify checks each share under the scheme of the call it runs in.
                let scheme = scheme_kind(step.scheme);
                let check = |p: &Processed| {
                    p.share.as_ref().is_some_and(|sk| {
                        verify_contribution_share(&me, p.read.msg.vvec(), sk, scheme)
                    })
                };
                rx.settle();
                let mut receives = Vec::new();
                match (step.kind, pop) {
                    (StepKind::Pop(DkgMsgKind::Contribution), Some(pop)) => {
                        receives = self.contribution_receipts(i, pop, step, &mut rx);
                    }
                    (StepKind::Pop(DkgMsgKind::Complaint), Some(pop)) => {
                        receives = self.complaint_receipts(pop, &mut rx);
                    }
                    (StepKind::Pop(DkgMsgKind::Justification), Some(pop)) => {
                        receives = self.justification_receipts(i, pop, &mut rx);
                    }
                    (StepKind::Complain, _) => rx.verify(&check),
                    (StepKind::Justify, _) => {
                        rx.justify_marks(usize::from(self.net.params.dkg_bad_votes_threshold));
                    }
                    _ => {}
                }
                let bit = match step.kind {
                    StepKind::Pop(kind) => received_bit(kind),
                    _ => None,
                };
                if !receives
                    .iter()
                    .map(|(who, _)| (*who, bit))
                    .eq(step.starts.iter().map(|s| (s.who, Some(s.bit))))
                {
                    out.insert(INV3);
                    continue;
                }
                let mut receives = receives
                    .into_iter()
                    .zip(&step.starts)
                    .map(|((_, receipt), start)| (receipt, start.rows))
                    .peekable();
                let mut open: Option<Receipt> = None;
                for (r, row) in step.rows.iter().enumerate() {
                    while let Some((receipt, _)) = receives.next_if(|(_, rows)| *rows <= r) {
                        if let Some(prev) = open.take() {
                            rx.apply(prev, &check);
                        }
                        rx.settle();
                        open = receipt;
                    }
                    let own = open.take_if(|p| p.writes(&row.2));
                    let local = own.is_some();
                    if let Some(p) = own {
                        rx.apply(p, &check);
                    }
                    if !self.row_ok(&mut rx, row, local) {
                        out.insert(INV3);
                    }
                }
                open.into_iter()
                    .chain(receives.filter_map(|(receipt, _)| receipt))
                    .for_each(|p| rx.apply(p, &check));
            }
        }
    }

    /// The text of each line node `i` logged in `step`.
    fn lines<'a>(&'a self, i: usize, step: &Step) -> impl Iterator<Item = &'a str> {
        self.net.nodes[i].events[step.events.clone()].iter().map(|l| l.text.as_str())
    }

    /// Node `i`'s steps in order, each pop step with its record.
    fn steps_with_pops(&self, i: usize) -> impl Iterator<Item = (&Step, Option<&PopRecord>)> {
        let mut pops = self.pops[i].iter();
        self.steps[i].iter().map(move |step| {
            let pop = match step.kind {
                StepKind::Pop(_) => pops.next(),
                _ => None,
            };
            (step, pop)
        })
    }

    /// The contribution receives of a pop at node `i` past the preamble, in order
    /// (`l/dkgsession.cpp:219-236`, :697-764): a member's first distinct contribution is
    /// processed (an equal one again), its second is not and marks it bad, later ones are
    /// dropped. A processed one is complained about when our blob does not decrypt to a valid
    /// key, or on a complain-lie, answered by the step's recorded decisions in order.
    fn contribution_receipts(
        &self,
        i: usize,
        pop: &PopRecord,
        step: &Step,
        rx: &mut Received,
    ) -> Vec<Receive> {
        let flag = pop.bls.scheme();
        let decisions = lock(&self.decisions[i]);
        let mut answers = decisions.get(step.bools.clone()).unwrap_or_default().iter().copied();
        let lies = self.is_test() && step.rates.percent(SimError::ComplainLie) > 0;
        let key = &self.net.nodes[i].key;
        let mut out = Vec::new();
        for read in reached(&pop.batch, |p| self.read_contribution(p, pop.bls), &rx.contributions) {
            let j = read.who;
            match preamble(&mut rx.contributions[j], read.hash) {
                Preamble::Drop => continue,
                Preamble::Second => {
                    out.push((j, Some(Receipt::Second(j))));
                    continue;
                }
                Preamble::Process => {}
            }
            let share = key.decrypt_share(read.msg.blobs(), i, flag).filter(BlsSkValue::is_valid);
            let lied = share.is_some() && j != i && lies && answers.next().unwrap_or(false);
            let complained = share.is_none() || lied;
            out.push((
                j,
                Some(Receipt::Processed(
                    Processed {
                        read,
                        share,
                    },
                    complained,
                )),
            ));
        }
        out
    }

    /// The complaint receives of a pop (`l/dkgsession.cpp:326-356`): past the preamble, each
    /// `badMembers` bit is a vote against that member and each `complainForMembers` bit a
    /// complaint about it from the sender.
    fn complaint_receipts(&self, pop: &PopRecord, rx: &mut Received) -> Vec<Receive> {
        let mut out = Vec::new();
        for read in reached(&pop.batch, |p| self.read_complaint(p, pop.bls), &rx.complaints) {
            let j = read.who;
            let second = match preamble(&mut rx.complaints[j], read.hash) {
                Preamble::Drop => continue,
                Preamble::Second => true,
                Preamble::Process => false,
            };
            out.push((j, None));
            if second {
                rx.bad[j] = true;
                continue;
            }
            for k in 0..self.n() {
                if read.msg.bad_member(k) {
                    rx.votes[k].insert(j);
                }
                if read.msg.complain_for(k) {
                    rx.complaints_from[k].insert(j);
                }
            }
        }
        out
    }

    /// The justification receives of a pop at node `i` (`l/dkgsession.cpp:430-485`): past the
    /// preamble, from a member not bad whose every revealed index complained about it, the
    /// revealed share for us is accepted when it verifies against the vector of the member's
    /// last processed contribution under the receive's scheme, and our complaint is then
    /// settled. Each accepted share is a receipt that authorizes its one `qdkg_S` write inside
    /// that receive. A share for us that does not verify marks the member bad; those for others
    /// are not checked, which can only leave a member good that Core marks bad.
    fn justification_receipts(&self, i: usize, pop: &PopRecord, rx: &mut Received) -> Vec<Receive> {
        let me = member_id(&self.net.members[i].pro_tx_hash);
        let scheme = pop.bls.scheme();
        let mut out = Vec::new();
        for read in reached(&pop.batch, |p| self.read_justification(p, pop.bls), &rx.justifications)
        {
            let j = read.who;
            let second = match preamble(&mut rx.justifications[j], read.hash) {
                Preamble::Drop => continue,
                Preamble::Second => true,
                Preamble::Process => false,
            };
            let at = out.len();
            out.push((j, None));
            if second {
                rx.bad[j] = true;
                continue;
            }
            let unasked = read.msg.keys().iter().any(|(index, _)| {
                usize::try_from(*index).is_ok_and(|k| !rx.complaints_from[j].contains(&k))
            });
            if rx.bad[j] || unasked {
                rx.bad[j] = true;
                continue;
            }
            let ours = read.msg.keys().iter().find(|(index, _)| usize::try_from(*index) == Ok(i));
            let Some((_, sk)) = ours else {
                continue;
            };
            let vvec = rx.processed[j].last().map(|p| p.read.msg.vvec());
            if vvec.is_some_and(|v| verify_contribution_share(&me, v, sk, scheme)) {
                out[at].1 = Some(Receipt::Justified {
                    who: *read.msg.pro_tx_hash(),
                    sk: sk.clone(),
                });
                rx.complaints_from[j].remove(&i);
            } else {
                rx.bad[j] = true;
            }
        }
        out
    }

    /// Whether the node may write `row` having received `rx`, where `local` says the row is the
    /// one write ([`Receipt::writes`]) of the receive it was written in: a `qdkg_V` row only so;
    /// in a pending verify (the Complain start, or inside the receive whose push made 32
    /// pending), per pending entry of a member neither bad nor complained about, one `qdkg_E`
    /// row of its last processed contribution's blobs and, when that contribution's share
    /// passed the verify, one `qdkg_S` row of that share; and a justified `qdkg_S` row only so.
    /// A pending row uses up its entry.
    fn row_ok(&self, rx: &mut Received, (t, base, row): &Row, local: bool) -> bool {
        let (StoreRow::Vvec {
            who,
            ..
        }
        | StoreRow::Sk {
            who,
            ..
        }
        | StoreRow::Encrypted {
            who,
            ..
        }) = row;
        let Some(j) = self.member_index(who) else {
            return false;
        };
        if *t != self.net.llmq_type || *base != self.net.setup.base.hash {
            return false;
        }
        let last = rx.processed[j].last();
        let left = rx.left.entry(j).or_default();
        let take = |n: &mut usize| {
            let some = *n > 0;
            *n -= usize::from(some);
            some
        };
        match row {
            StoreRow::Vvec {
                ..
            } => local,
            StoreRow::Sk {
                sk,
                ..
            } if local => sk.is_valid(),
            StoreRow::Encrypted {
                blobs,
                ..
            } => last.is_some_and(|p| p.read.msg.blobs() == blobs) && take(&mut left.0),
            StoreRow::Sk {
                sk,
                ..
            } => {
                sk.is_valid()
                    && last.is_some_and(|p| p.share.as_ref() == Some(sk))
                    && take(&mut left.1)
            }
        }
    }

    fn inv4(&self, out: &mut BTreeSet<&'static str>) {
        for (i, pops) in self.pops.iter().enumerate() {
            for pop in pops {
                let mut failed: BTreeMap<usize, usize> = BTreeMap::new();
                let mut read: BTreeMap<usize, usize> = BTreeMap::new();
                let mut unsigned: BTreeSet<usize> = BTreeSet::new();
                for (sender, payload) in &pop.batch {
                    if let Sender::Peer(j) = sender {
                        let verdict = self.signed(pop.phase, payload, pop.bls);
                        let tally = if verdict.is_some() {
                            &mut read
                        } else {
                            &mut failed
                        };
                        *tally.entry(*j).or_default() += 1;
                        if verdict == Some(false) {
                            unsigned.insert(*j);
                        }
                    }
                }
                let (mut decode, mut ban, mut bad) = (
                    BTreeMap::<usize, usize>::new(),
                    BTreeMap::<usize, usize>::new(),
                    BTreeMap::<usize, usize>::new(),
                );
                for s in &pop.outcome.misbehaving {
                    let (peer, tally) = match s.misbehaviour {
                        Misbehaviour::Decode {
                            peer,
                            ..
                        } => (peer, &mut decode),
                        Misbehaviour::PreVerifyBan {
                            peer,
                            ..
                        } => (peer, &mut ban),
                        Misbehaviour::BadSignature {
                            peer,
                        } => (peer, &mut bad),
                    };
                    *tally.entry(peer).or_default() += 1;
                    let sent = failed.contains_key(&peer) || read.contains_key(&peer);
                    if peer == i || !sent {
                        out.insert(INV4);
                    }
                }
                // A bad batch signature is scored only for a peer one of whose messages carries a
                // signature its member's operator key does not verify; the converse would need
                // PreVerify's verdicts.
                let consistent = decode == failed
                    && bad.iter().all(|(p, &c)| c <= 1 && unsigned.contains(p))
                    && ban.iter().all(|(p, c)| *c <= read.get(p).copied().unwrap_or(0));
                if !consistent {
                    out.insert(INV4);
                }
            }
        }
    }

    fn inv5(&self, out: &mut BTreeSet<&'static str>) {
        let n = self.n();
        let mut full = FullCheck::new(self.memo.scope);
        for i in 0..n {
            let node = &self.net.nodes[i];
            let mut rows: BTreeMap<usize, &Vec<BlsPkValue>> = BTreeMap::new();
            for (_, _, row) in &node.store.rows {
                if let StoreRow::Vvec {
                    who,
                    vvec,
                } = row
                    && let Some(j) = self.member_index(who)
                {
                    rows.insert(j, vvec);
                }
            }
            for pop in self.pops[i].iter().filter(|p| p.phase == DkgMsgKind::PrematureCommitment) {
                let flag = pop.bls.scheme();
                let read: BTreeMap<[u8; 32], PrematureCommitment> = pop
                    .batch
                    .iter()
                    .filter_map(|(_, payload)| {
                        PrematureCommitment::read(payload, &self.net.params, pop.bls).ok()
                    })
                    .map(|qc| (*qc.session_hash(), qc))
                    .collect();
                for (hash, _) in relays(&pop.outcome) {
                    let Some(qc) = read.get(&hash) else {
                        out.insert(INV5);
                        continue;
                    };
                    if !full.verifies(qc, &rows, n, flag) {
                        out.insert(INV5);
                    }
                }
            }
            let lied = self.steps[i].iter().filter(|s| s.kind == StepKind::Commit).any(|s| {
                node.events[s.events.clone()]
                    .iter()
                    .any(|e| e.text.starts_with("lying on commitment"))
            });
            if lied && let Some((hash, _)) = &self.own_pcommit[i] {
                let relayed = node.relayed.iter().any(|inv| inv.hash == *hash);
                if relayed || node.session.get_premature_commitment(hash).is_some() {
                    out.insert(INV5);
                }
            }
        }
    }

    fn inv6(&self, out: &mut BTreeSet<&'static str>) {
        let n = self.n();
        let threshold = u32::from(self.net.params.threshold);
        let index: BTreeMap<String, usize> = self
            .net
            .members
            .iter()
            .enumerate()
            .map(|(i, m)| (m.pro_tx_hash.to_string(), i))
            .collect();
        for i in 0..n {
            let log = lock(&self.logs[i]).clone();
            let decisions = lock(&self.decisions[i]).clone();
            let events = &self.net.nodes[i].events;
            let mut end = 0;
            for step in &self.steps[i] {
                if step.tags.start != end {
                    out.insert(INV6);
                }
                end = step.tags.end;
                let mut e = Expect {
                    tags: Vec::new(),
                    decisions: &decisions[step.bools.clone()],
                    at: 0,
                    test: self.is_test(),
                    rates: step.rates,
                    ok: true,
                };
                let lines = &events[step.events.clone()];
                let has = |text: &str| lines.iter().any(|l| l.text.contains(text));
                match step.kind {
                    StepKind::Contribute => {
                        if !e.gate(SimError::ContributionClone) {
                            for j in 0..threshold {
                                e.tags.push(DrawTag::Key(KeyDraw::Coefficient(j)));
                            }
                        }
                        if !has("GenerateContributions failed")
                            && !e.gate(SimError::ContributionOmit)
                        {
                            e.tags.push(DrawTag::Ies(n));
                            for j in (0..n).filter(|&j| j != i) {
                                if e.gate(SimError::ContributionLie) {
                                    e.tags.push(DrawTag::Key(KeyDraw::ContributionLie(j as u32)));
                                }
                                e.gate(SimError::ContributionInvalid);
                            }
                        }
                        e.tags.extend(step.suffix.iter().copied());
                    }
                    StepKind::Justify if has("sending justification") => {
                        for l in lines {
                            let Some(who) = l.text.strip_prefix("justifying for ") else {
                                continue;
                            };
                            let j = index.get(who).copied().unwrap_or(usize::MAX);
                            if j != i && e.gate(SimError::JustifyLie) {
                                e.tags.push(DrawTag::Key(KeyDraw::JustifyLie(j as u32)));
                            }
                        }
                        e.gate(SimError::JustifyOmit);
                    }
                    StepKind::Commit if !has("not enough valid members") => {
                        let failed =
                            ["failed to get valid", "failed to build"].iter().any(|t| has(t));
                        if !e.gate(SimError::CommitOmit) && !failed && e.gate(SimError::CommitLie) {
                            e.tags.push(DrawTag::Range(RangeDraw::CommitLieType, 5));
                            if has("lieType=0") {
                                e.tags.push(DrawTag::Key(KeyDraw::CommitLieKey));
                            }
                        }
                    }
                    StepKind::Pop(DkgMsgKind::Contribution) => {
                        let mut records: Vec<Option<u32>> = Vec::new();
                        for l in lines {
                            if l.record.is_some() && !records.contains(&l.record) {
                                records.push(l.record);
                            }
                        }
                        for r in records {
                            let rec: Vec<&DkgEvent> =
                                lines.iter().filter(|l| l.record == r).collect();
                            let sender = rec.iter().find_map(|l| {
                                l.text
                                    .strip_prefix("received contribution from ")
                                    .and_then(|w| index.get(w))
                            });
                            let decided = rec.iter().any(|l| {
                                l.text == "decrypted our contribution share"
                                    || l.text.starts_with("lying/complaining for")
                            });
                            if sender.is_some_and(|&j| j != i) && decided {
                                e.gate(SimError::ComplainLie);
                            }
                        }
                    }
                    _ => {}
                }
                if !e.ok || e.at != e.decisions.len() || log[step.tags.clone()] != e.tags[..] {
                    out.insert(INV6);
                }
            }
            if end != log.len() {
                out.insert(INV6);
            }
        }
    }
}

/// The tags §2.9 says a step draws, built from the decisions it made.
struct Expect<'a> {
    tags: Vec<DrawTag>,
    decisions: &'a [bool],
    at: usize,
    test: bool,
    rates: SimErrorRates,
    ok: bool,
}

impl Expect<'_> {
    /// `ShouldSimulateError(kind)`: no draw for a type other than `llmq_test` or at rate 0.
    fn gate(&mut self, kind: SimError) -> bool {
        let rate = self.rates.percent(kind);
        if !self.test || rate == 0 {
            return false;
        }
        self.tags.push(DrawTag::Bool(kind, rate));
        let answer = self.decisions.get(self.at).copied();
        self.at += 1;
        answer.unwrap_or_else(|| {
            self.ok = false;
            false
        })
    }
}

/// Each premature commitment the outcome relays, with whether its receive skipped full
/// verification.
fn relays(outcome: &BatchOutcome<usize>) -> Vec<([u8; 32], bool)> {
    outcome
        .relay
        .iter()
        .filter(|r| r.inv.kind == DkgMsgKind::PrematureCommitment)
        .map(|r| {
            let record =
                r.at.checked_sub(1).and_then(|k| outcome.events.get(k)).and_then(|e| e.record);
            // A receive whose record cannot be found counts as skipped: no liveness claim.
            let skipped = record.is_none()
                || outcome
                    .events
                    .iter()
                    .any(|e| e.record == record && e.text.contains("skipping full verification"));
            (r.inv.hash, skipped)
        })
        .collect()
}

/// The member set (its index) and the flags byte: with the bytes and the scheme, every input
/// of a memoised read or check.
type Scope = (usize, u8);

/// The valid members' `qdkg_V` rows a full check reads, in member order.
type Rows<'a> = Vec<&'a [BlsPkValue]>;

/// A quorum verification vector.
type Vector = Vec<BlsPkValue>;

/// Values kept by the rows they were computed from.
type ByRows<'a, T> = Vec<(Rows<'a>, T)>;

/// `ReceiveMessage`'s full check, its quorum verification vectors and verdicts kept by the rows
/// they came from: the nodes' rows are mostly equal, so each is computed once a run.
struct FullCheck<'a> {
    scope: Scope,
    /// By scope, `validMembers` and scheme, then by the rows.
    vectors: BTreeMap<(Scope, Vec<bool>, u8), ByRows<'a, Option<Vector>>>,
    /// By scope, scheme and the message's bytes under it, then by the rows.
    verdicts: BTreeMap<(Scope, u8, Vec<u8>), ByRows<'a, bool>>,
}

impl<'a> FullCheck<'a> {
    fn new(scope: Scope) -> Self {
        Self {
            scope,
            vectors: BTreeMap::new(),
            verdicts: BTreeMap::new(),
        }
    }

    /// The full check of `qc` over the `qdkg_V` rows, or `true` where it is skipped (a valid
    /// member without a row, or vectors that do not build).
    fn verifies(
        &mut self,
        qc: &PrematureCommitment,
        rows: &BTreeMap<usize, &'a Vec<BlsPkValue>>,
        n: usize,
        flag: BlsSchemeKind,
    ) -> bool {
        let valid = qc.valid_members();
        let mut used: Rows<'a> = Vec::new();
        for j in (0..n).filter(|&j| valid.get(j).copied().unwrap_or(false)) {
            let Some(vvec) = rows.get(&j) else {
                return true;
            };
            used.push(vvec.as_slice());
        }
        let flag_id = u8::from(flag == BlsSchemeKind::Legacy);
        let key = (self.scope, flag_id, qc.payload(flag));
        if let Some((_, v)) = self.verdicts.get(&key).and_then(|e| e.iter().find(|e| e.0 == used)) {
            return *v;
        }
        let vectors = self.vectors.entry((self.scope, valid, flag_id)).or_default();
        let at = vectors.iter().position(|e| e.0 == used).unwrap_or_else(|| {
            let stored: Vec<Vec<BlsPkValue>> =
                used.iter().map(|v| v.iter().map(stored_vvec_key).collect()).collect();
            let refs: Vec<&[BlsPkValue]> = stored.iter().map(Vec::as_slice).collect();
            vectors.push((used.clone(), build_quorum_verification_vector(&refs, flag)));
            vectors.len() - 1
        });
        let verdict = vectors[at].1.as_ref().is_none_or(|v| {
            let share = build_pub_key_share(v, &member_id(qc.pro_tx_hash()));
            v.first().is_some_and(|f| public_keys_equal(f, qc.quorum_public_key()))
                && vvec_hash(v, flag) == *qc.quorum_vvec_hash()
                && share.is_valid()
                && verify_insecure(qc.quorum_sig(), &share, qc.sign_hash(), flag)
        });
        self.verdicts.entry(key).or_default().push((used, verdict));
        verdict
    }
}

/// What the checks read from the popped payloads, kept by scope, phase, scheme and the bytes:
/// every queue gets a sent payload, so each is read once a run.
struct Memo {
    scope: Scope,
    /// By scope, phase, scheme and bytes.
    reads: RefCell<BTreeMap<ReadKey, Option<bool>>>,
    /// By scope, scheme and bytes, one per message kind.
    contributions: Reads<Contribution>,
    complaints: Reads<Complaint>,
    justifications: Reads<Justification>,
    commitments: Reads<PrematureCommitment>,
}

type ReadKey = (Scope, u8, u8, Arc<[u8]>);
type PayloadKey = (Scope, u8, Arc<[u8]>);
/// The messages [`Run::admit`] read.
type Reads<M> = RefCell<BTreeMap<PayloadKey, Option<Arc<Read<M>>>>>;

fn phase_id(kind: DkgMsgKind) -> u8 {
    PHASES.iter().position(|p| *p == kind).unwrap_or(PHASES.len()) as u8
}

/// The value `cache` holds for `key`, else `f`'s, kept.
fn memoised<K: Ord, V: Clone>(cache: &RefCell<BTreeMap<K, V>>, key: K, f: impl FnOnce() -> V) -> V {
    if let Some(v) = cache.borrow().get(&key) {
        return v.clone();
    }
    let v = f();
    cache.borrow_mut().insert(key, v.clone());
    v
}

impl Memo {
    fn new(scope: Scope) -> Self {
        Self {
            scope,
            reads: RefCell::default(),
            contributions: RefCell::default(),
            complaints: RefCell::default(),
            justifications: RefCell::default(),
            commitments: RefCell::default(),
        }
    }

    /// `None` when `payload` does not read as a `kind` message under `bls`, else whether its
    /// signature verifies under its member's operator key: `f`'s answer.
    fn reads(
        &self,
        kind: DkgMsgKind,
        payload: &Arc<[u8]>,
        bls: BlsCtx,
        f: impl FnOnce() -> Option<bool>,
    ) -> Option<bool> {
        let key = (self.scope, phase_id(kind), scheme_id(bls), payload.clone());
        memoised(&self.reads, key, f)
    }
}

/// The fields `PreVerifyMessage` starts with, and the session hash.
trait Message: Sized {
    const KIND: DkgMsgKind;
    fn decode(payload: &[u8], p: &LlmqParams, bls: BlsCtx) -> Option<Self>;
    fn header(&self) -> (&QuorumHash, &ProTxHash, [u8; 32]);
}

macro_rules! message {
    ($msg:ty, $kind:expr) => {
        impl Message for $msg {
            const KIND: DkgMsgKind = $kind;
            fn decode(payload: &[u8], p: &LlmqParams, bls: BlsCtx) -> Option<Self> {
                <$msg>::read(payload, p, bls).ok()
            }
            fn header(&self) -> (&QuorumHash, &ProTxHash, [u8; 32]) {
                (self.quorum_hash(), self.pro_tx_hash(), *self.session_hash())
            }
        }
    };
}

message!(Contribution, DkgMsgKind::Contribution);
message!(Complaint, DkgMsgKind::Complaint);
message!(Justification, DkgMsgKind::Justification);
message!(PrematureCommitment, DkgMsgKind::PrematureCommitment);

/// A popped message that reads and passes `PreVerifyMessage`'s checks before the count: its
/// member, its session hash, whether its signature verifies under the member's operator key,
/// and the message.
struct Read<M> {
    who: usize,
    hash: [u8; 32],
    signed: bool,
    msg: M,
}

/// The messages of one pop that reach `ReceiveMessage`, in batch order (`l/net_dkg.cpp:404-458`):
/// those `read` admits while their member had fewer than two in `seen` before the batch, from a
/// sender none of whose admitted messages fails its signature. A batch whose aggregate signature
/// verifies though a message's own does not is read as the latter; the actions cannot build one.
fn reached<M>(
    batch: &[Queued],
    read: impl Fn(&Arc<[u8]>) -> Option<Arc<Read<M>>>,
    seen: &[BTreeSet<[u8; 32]>],
) -> Vec<Arc<Read<M>>> {
    let admitted: Vec<(Sender<usize>, Arc<Read<M>>)> = batch
        .iter()
        .filter_map(|(sender, payload)| {
            read(payload).filter(|r| seen[r.who].len() < 2).map(|r| (*sender, r))
        })
        .collect();
    let bad: BTreeSet<Sender<usize>> =
        admitted.iter().filter(|(_, r)| !r.signed).map(|(sender, _)| *sender).collect();
    admitted.into_iter().filter(|(sender, _)| !bad.contains(sender)).map(|(_, r)| r).collect()
}

/// The receive preamble's count of a member's distinct messages of one kind
/// (`l/dkgsession.cpp:697-764`).
enum Preamble {
    /// The first distinct message, or an equal one again: processed.
    Process,
    /// The second distinct message: not processed, the member marked bad.
    Second,
    /// Two distinct messages already: dropped.
    Drop,
}

fn preamble(seen: &mut BTreeSet<[u8; 32]>, hash: [u8; 32]) -> Preamble {
    if seen.len() >= 2 {
        return Preamble::Drop;
    }
    seen.insert(hash);
    if seen.len() > 1 {
        Preamble::Second
    } else {
        Preamble::Process
    }
}

/// A contribution a node processed, and the share our blob decrypts to when that is a valid key.
struct Processed {
    read: Arc<Read<Contribution>>,
    share: Option<BlsSkValue>,
}

/// A modelled receive in a pop: its sender's member, and its receipt when it writes or marks.
type Receive = (usize, Option<Receipt>);

/// What one receive in a pop does that the store check orders against the pop's rows.
enum Receipt {
    /// A processed contribution, and whether the node complained about it.
    Processed(Processed, bool),
    /// The member's second distinct contribution: not processed, the member marked bad.
    Second(usize),
    /// An accepted justification's share for us from `who`.
    Justified {
        who: ProTxHash,
        sk: BlsSkValue,
    },
}

impl Receipt {
    /// Whether `row` is this receive's own write: a processed contribution's `qdkg_V` row, or
    /// an accepted justification's `qdkg_S` row.
    fn writes(&self, row: &StoreRow) -> bool {
        match (self, row) {
            (
                Self::Processed(p, _),
                StoreRow::Vvec {
                    who,
                    vvec,
                },
            ) => p.read.msg.pro_tx_hash() == who && p.read.msg.vvec() == vvec.as_slice(),
            (
                Self::Justified {
                    who: from,
                    sk: share,
                },
                StoreRow::Sk {
                    who,
                    sk,
                },
            ) => from == who && share == sk,
            _ => false,
        }
    }
}

/// What a node's session holds from its receives, per member, as the store check models it.
struct Received {
    /// The preamble's sets: distinct contributions, complaints and justifications received.
    contributions: Vec<BTreeSet<[u8; 32]>>,
    complaints: Vec<BTreeSet<[u8; 32]>>,
    justifications: Vec<BTreeSet<[u8; 32]>>,
    processed: Vec<Vec<Processed>>,
    /// Marked bad by a second distinct message, by the Justify start, or by a justification.
    /// Core's other marks are not modelled, which only leaves members good here.
    bad: Vec<bool>,
    /// Complained about at a receive, or after a pending verify its share failed.
    complained: Vec<bool>,
    /// `badMemberVotes` and `complaintsFromOthers`, as member indexes.
    votes: Vec<BTreeSet<usize>>,
    complaints_from: Vec<BTreeSet<usize>>,
    /// The pending entries pushed since the last pending verify, in order.
    pushed: Vec<usize>,
    /// While a pending verify's rows are written: per member the `qdkg_E` and `qdkg_S` rows it
    /// may still write. Then the members whose checked shares failed.
    left: BTreeMap<usize, (usize, usize)>,
    failing: BTreeSet<usize>,
}

impl Received {
    fn new(n: usize) -> Self {
        Self {
            contributions: vec![BTreeSet::new(); n],
            complaints: vec![BTreeSet::new(); n],
            justifications: vec![BTreeSet::new(); n],
            processed: (0..n).map(|_| Vec::new()).collect(),
            bad: vec![false; n],
            complained: vec![false; n],
            votes: vec![BTreeSet::new(); n],
            complaints_from: vec![BTreeSet::new(); n],
            pushed: Vec::new(),
            left: BTreeMap::new(),
            failing: BTreeSet::new(),
        }
    }

    /// A receive: a processed contribution not complained about is pushed, and the 32nd push
    /// runs the pending verify inside it (`l/dkgsession.cpp:268-272`).
    fn apply(&mut self, receipt: Receipt, check: &dyn Fn(&Processed) -> bool) {
        self.settle();
        match receipt {
            Receipt::Second(j) => self.bad[j] = true,
            Receipt::Processed(p, complained) => {
                let j = p.read.who;
                self.complained[j] |= complained;
                self.processed[j].push(p);
                if !complained {
                    self.pushed.push(j);
                    if self.pushed.len() >= 32 {
                        self.verify(check);
                    }
                }
            }
            Receipt::Justified {
                ..
            } => {}
        }
    }

    /// `VerifyPendingContributions` over the entries pushed since the last one
    /// (`a/dkgsession.cpp:145-212`): each entry of a member neither bad nor complained about
    /// writes a `qdkg_E` row, and a `qdkg_S` row when `check` passes the share of the member's
    /// last processed contribution (`check` runs under the scheme of the verify's call). It
    /// assumes one verdict per entry: Core's return on a verdict count that does not match
    /// (`a/dkgsession.cpp:190-193`) is not modelled.
    fn verify(&mut self, check: &dyn Fn(&Processed) -> bool) {
        self.settle();
        let mut verdicts = BTreeMap::new();
        for j in std::mem::take(&mut self.pushed) {
            if self.bad[j] || self.complained[j] {
                continue;
            }
            let passes =
                *verdicts.entry(j).or_insert_with(|| self.processed[j].last().is_some_and(check));
            let left = self.left.entry(j).or_default();
            left.0 += 1;
            if passes {
                left.1 += 1;
            } else {
                self.failing.insert(j);
            }
        }
    }

    /// Ends a pending verify once its rows are written: each checked member whose share failed
    /// is complained about (`a/dkgsession.cpp:195-203`), so a later verify skips it.
    fn settle(&mut self) {
        self.left.clear();
        for j in std::mem::take(&mut self.failing) {
            self.complained[j] = true;
        }
    }

    /// `VerifyAndJustify`'s marks (`a/dkgsession.cpp:373-388`): a member with `threshold` votes
    /// against it, or more than one complaint, is bad.
    fn justify_marks(&mut self, threshold: usize) {
        for m in 0..self.bad.len() {
            if self.votes[m].len() >= threshold || self.complaints[m].len() > 1 {
                self.bad[m] = true;
            }
        }
    }
}

impl Run {
    /// Whether `payload`, read as a `kind` message under `bls`, carries a signature its member's
    /// operator key verifies; `None` when it does not read.
    fn signed(&self, kind: DkgMsgKind, payload: &Arc<[u8]>, bls: BlsCtx) -> Option<bool> {
        self.memo.reads(kind, payload, bls, || {
            let (who, sig, hash) = signed_by(kind, payload, &self.net.params, bls)?;
            let pk = self.member_index(&who).map(|m| &self.net.members[m].operator_pk);
            Some(pk.is_some_and(|pk| verify_insecure(&sig, pk, &hash, bls.scheme())))
        })
    }

    /// `payload` read as an `M` under `bls`, when its quorum is ours, its sender a member and
    /// `checks` pass.
    fn admit<M: Message>(
        &self,
        reads: &Reads<M>,
        payload: &Arc<[u8]>,
        bls: BlsCtx,
        checks: impl FnOnce(&M) -> bool,
    ) -> Option<Arc<Read<M>>> {
        memoised(reads, (self.memo.scope, scheme_id(bls), payload.clone()), || {
            let msg = M::decode(payload, &self.net.params, bls)?;
            let (quorum, who, hash) = msg.header();
            let ours = *quorum == self.net.setup.base.hash;
            let who = self.member_index(who).filter(|_| ours && checks(&msg))?;
            let signed = self.signed(M::KIND, payload, bls) == Some(true);
            Some(Arc::new(Read {
                who,
                hash,
                signed,
                msg,
            }))
        })
    }

    /// `PreVerifyMessage(const CDKGContribution&)` (`l/dkgsession.cpp:158-211`).
    fn read_contribution(
        &self,
        payload: &Arc<[u8]>,
        bls: BlsCtx,
    ) -> Option<Arc<Read<Contribution>>> {
        let (n, threshold) = (self.n(), usize::from(self.net.params.threshold));
        self.admit(&self.memo.contributions, payload, bls, |qc: &Contribution| {
            qc.sig().is_valid()
                && qc.blobs().blobs.len() == n
                && qc.vvec().len() == threshold
                && verify_verification_vector(qc.vvec(), bls.scheme())
        })
    }

    /// `PreVerifyMessage(const CDKGComplaint&)` (`l/dkgsession.cpp:277-325`): both bitsets'
    /// sizes, the compact sizes after the payload's 65-byte prefix, are `size`.
    fn read_complaint(&self, payload: &Arc<[u8]>, bls: BlsCtx) -> Option<Arc<Read<Complaint>>> {
        let size = usize::from(self.net.params.size);
        let sizes = || {
            let bad = usize::from(*payload.get(65)?);
            let complain = usize::from(*payload.get(66 + bad.div_ceil(8))?);
            (bad < 253 && complain < 253).then_some((bad, complain))
        };
        self.admit(&self.memo.complaints, payload, bls, |qc: &Complaint| {
            qc.sig().is_valid() && sizes() == Some((size, size))
        })
    }

    /// `PreVerifyMessage(const CDKGJustification&)` (`l/dkgsession.cpp:366-429`).
    fn read_justification(
        &self,
        payload: &Arc<[u8]>,
        bls: BlsCtx,
    ) -> Option<Arc<Read<Justification>>> {
        let n = self.n();
        self.admit(&self.memo.justifications, payload, bls, |qj: &Justification| {
            let mut indexes = BTreeSet::new();
            qj.sig().is_valid()
                && !qj.keys().is_empty()
                && qj.keys().iter().all(|(index, sk)| {
                    usize::try_from(*index).is_ok_and(|x| x < n)
                        && indexes.insert(*index)
                        && sk.is_valid()
                })
        })
    }

    /// `PreVerifyMessage(const CDKGPrematureCommitment&)` (`l/dkgsession.cpp:503-562`).
    fn read_commitment(
        &self,
        payload: &Arc<[u8]>,
        bls: BlsCtx,
    ) -> Option<Arc<Read<PrematureCommitment>>> {
        let n = self.n();
        let (size, min) =
            (usize::from(self.net.params.size), usize::from(self.net.params.min_size));
        self.admit(&self.memo.commitments, payload, bls, |qc: &PrematureCommitment| {
            let valid = qc.valid_members();
            valid.len() == size
                && valid.iter().filter(|v| **v).count() >= min
                && qc.sig().is_valid()
                && qc.quorum_sig().is_valid()
                && !valid.iter().skip(n).any(|v| *v)
        })
    }

    /// The cache keys (`validMembers`) node `i`'s session cache may answer with a value a
    /// cache-off run does not compute (PLAN invariant 7's exception), and the key of each
    /// premature commitment the node received, by session hash.
    ///
    /// A key is used by the node's Commit start (its own `validMembers`; a key unknown here when
    /// it used one but sent nothing: it logged that the vector or its own share failed to build)
    /// and by each premature commitment that reaches `ReceiveMessage` with a
    /// `qdkg_V` row for every valid member (`a/dkgsession.cpp:556-571`,
    /// `l/dkgsession.cpp:577-612`). A key is excepted when it is used under both schemes, under
    /// another scheme than the unknown key's, or when a row of one of its members is written
    /// after its first use.
    fn cache_exceptions(&self, i: usize) -> Exceptions {
        let with_row: BTreeSet<&ProTxHash> = self.net.nodes[i]
            .store
            .rows
            .iter()
            .filter_map(|(_, _, row)| match row {
                StoreRow::Vvec {
                    who,
                    ..
                } => Some(who),
                _ => None,
            })
            .collect();
        let readable = |key: &[bool]| {
            self.net.members.iter().zip(key).all(|(m, v)| !v || with_row.contains(&m.pro_tx_hash))
        };
        let mut uses: BTreeMap<Vec<bool>, (usize, BTreeSet<u8>)> = BTreeMap::new();
        let mut unknown = None;
        let mut writes: Vec<(usize, usize)> = Vec::new();
        let mut keys = BTreeMap::new();
        let mut seen = vec![BTreeSet::new(); self.n()];
        for (k, (step, pop)) in self.steps_with_pops(i).enumerate() {
            for (_, _, row) in &step.rows {
                if let StoreRow::Vvec {
                    who,
                    ..
                }
                | StoreRow::Sk {
                    who,
                    ..
                } = row
                    && let Some(j) = self.member_index(who)
                {
                    writes.push((k, j));
                }
            }
            let mut used = Vec::new();
            match (step.kind, pop) {
                (StepKind::Commit, _) => match &self.own_pcommit[i] {
                    Some((_, valid)) => used.push(valid.clone()),
                    None if self.lines(i, step).any(|l| {
                        l.contains("failed to build quorum verification vector")
                            || l.contains("failed to build own secret share")
                    }) =>
                    {
                        unknown = Some(step.scheme)
                    }
                    None => {}
                },
                (StepKind::Pop(DkgMsgKind::PrematureCommitment), Some(pop)) => {
                    for read in reached(&pop.batch, |p| self.read_commitment(p, pop.bls), &seen) {
                        seen[read.who].insert(read.hash);
                        let valid = read.msg.valid_members();
                        keys.insert(read.hash, valid.clone());
                        if readable(&valid) {
                            used.push(valid);
                        }
                    }
                }
                _ => {}
            }
            for key in used {
                uses.entry(key).or_insert_with(|| (k, BTreeSet::new())).1.insert(step.scheme);
            }
        }
        let excepted = uses
            .into_iter()
            .filter(|(key, (first, schemes))| {
                schemes.len() > 1
                    || unknown.is_some_and(|s| schemes.iter().any(|x| *x != s))
                    || writes.iter().any(|(k, j)| k > first && key.get(*j) == Some(&true))
            })
            .map(|(key, _)| key)
            .collect();
        (excepted, keys)
    }

    /// Node `i`'s finals by `validMembers`, as bytes, but those of `except`'s keys.
    fn finals_by_key(
        &self,
        i: usize,
        except: &BTreeSet<Vec<bool>>,
    ) -> Option<BTreeMap<Vec<bool>, Vec<Vec<u8>>>> {
        let rec = self.finals[i].as_ref()?;
        let mut out: BTreeMap<Vec<bool>, Vec<Vec<u8>>> = BTreeMap::new();
        for qc in &rec.finals {
            let key = bits(qc.valid_members());
            if !except.contains(&key) {
                out.entry(key).or_default().push(final_commitment_bytes(qc));
            }
        }
        Some(out)
    }

    /// Whether the cache-off `twin` differs from this run outside [`Self::cache_exceptions`]: a
    /// node with an excepted key is compared on all but its Commit-phase receive and finalize
    /// lines, its invs of premature commitments with an excepted key and its finals of those
    /// keys.
    fn twin_differs(&self, twin: &Self) -> bool {
        self.twin_differs_except(twin, |i| self.cache_exceptions(i))
    }

    /// [`Self::twin_differs`] with node `i`'s exceptions given by `exceptions(i)`.
    fn twin_differs_except(&self, twin: &Self, exceptions: impl Fn(usize) -> Exceptions) -> bool {
        let rows = |run: &Self, i: usize| -> Vec<Row> {
            run.net.nodes[i].store.rows.iter().chain(&run.removed[i]).cloned().collect()
        };
        (0..self.n()).any(|i| {
            let (a, b) = (&self.net.nodes[i], &twin.net.nodes[i]);
            if a.misbehaving != b.misbehaving || rows(self, i) != rows(twin, i) {
                return true;
            }
            let (except, keys) = exceptions(i);
            if except.is_empty() {
                let finals = |run: &Self| run.finals[i].as_ref().map(|f| f.finals.clone());
                return a.events != b.events
                    || a.relayed != b.relayed
                    || finals(self) != finals(twin);
            }
            let kept = |inv: &&DkgInv| {
                inv.kind != DkgMsgKind::PrematureCommitment
                    || keys.get(&inv.hash).is_none_or(|k| !except.contains(k))
            };
            let (sa, sb) = (&self.steps[i], &twin.steps[i]);
            let lines = |s: &Step, t: &Step| {
                s.kind != t.kind
                    || !matches!(
                        s.kind,
                        StepKind::Pop(DkgMsgKind::PrematureCommitment) | StepKind::Finalize
                    ) && a.events.get(s.events.clone()) != b.events.get(t.events.clone())
            };
            sa.len() != sb.len()
                || sa.iter().zip(sb).any(|(s, t)| lines(s, t))
                || !a.relayed.iter().filter(kept).eq(b.relayed.iter().filter(kept))
                || self.finals_by_key(i, &except) != twin.finals_by_key(i, &except)
        })
    }
}

/// Every invariant the input breaks, in [`FIELDS`] order.
fn failures(data: &[u8]) -> Vec<&'static str> {
    let script = Script::parse(data);
    failures_in(&MATERIAL[set_index(script.set)], &script)
}

fn failures_in(m: &'static Material, script: &Script) -> Vec<&'static str> {
    let run = Run::play_in(m, script, true);
    let mut out = BTreeSet::new();
    if run.faults > 0 {
        out.insert(INV1);
    }
    run.inv2(&mut out);
    run.inv3(&mut out);
    run.inv4(&mut out);
    run.inv5(&mut out);
    run.inv6(&mut out);
    if run.twin_differs(&Run::play_in(m, script, false)) {
        out.insert(INV7);
    }
    FIELDS.into_iter().filter(|f| out.contains(f)).collect()
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    match failures(data).first() {
        Some(field) => Err(Divergence {
            field,
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)] // fixed seeds and the repository's seed files
mod tests {
    use super::*;

    const SEED: u64 = 0x5eed_00c9;
    const S3: u8 = 0;
    const DIP24: u8 = 128;
    const S5: u8 = 192;
    const S12: u8 = 248;
    /// The base before V19, and the process-wide scheme starting legacy.
    const LEGACY: u8 = 3;
    const COMMIT_PHASE: u8 = 3;

    fn script(set: u8, flags: u8, actions: Vec<Action>) -> Script {
        Script {
            set,
            flags,
            seed: SEED,
            actions,
        }
    }

    fn kind(k: SimError) -> u8 {
        SimError::ALL.iter().position(|x| *x == k).unwrap_or(0) as u8
    }

    fn rate(member: u8, kind: u8, rate: u8) -> Action {
        Action::Rate {
            member,
            kind,
            rate,
        }
    }

    /// Member `member` at rate 100 for each of `kinds`.
    fn rates(member: u8, kinds: &[SimError]) -> Vec<Action> {
        kinds.iter().map(|k| rate(member, kind(*k), 1)).collect()
    }

    /// Every premature commitment node `i` popped in Commit, as first read: by session hash.
    fn popped(run: &Run, i: usize) -> BTreeMap<[u8; 32], PrematureCommitment> {
        let mut out = BTreeMap::new();
        for pop in run.pops[i].iter().filter(|p| p.phase == DkgMsgKind::PrematureCommitment) {
            for (_, payload) in &pop.batch {
                if let Ok(qc) = PrematureCommitment::read(payload, &run.net.params, pop.bls) {
                    out.entry(*qc.session_hash()).or_insert(qc);
                }
            }
        }
        out
    }

    fn advances(n: usize) -> Vec<Action> {
        vec![Action::Advance; n]
    }

    /// Each row of the C7 matrix, as the erring member's rates there (each call the FT made up to
    /// that row).
    const ROWS: [(&str, &[SimError]); 9] = {
        use SimError::*;
        [
            ("honest", &[]),
            ("contribution-omit", &[ContributionOmit]),
            ("contribution-lie", &[ContributionLie]),
            ("contribution-invalid", &[ContributionLie, ContributionInvalid]),
            ("justify-omit", &[ContributionLie, JustifyOmit]),
            ("justify-lie", &[ContributionLie, JustifyLie]),
            ("complain-lie", &[ComplainLie]),
            ("commit-omit", &[CommitOmit]),
            ("commit-lie", &[CommitLie]),
        ]
    };

    /// The commit-lie row with lie type `lie` at `member`, whose premature commitment reaches its
    /// own queue alone, as C7 mines it.
    fn commit_lie(member: u8, lie: u8) -> Vec<Action> {
        let mut a = rates(member, &[SimError::CommitLie]);
        a.push(Action::LieType {
            member,
            lie,
        });
        a.push(Action::Mode {
            member,
            phase: COMMIT_PHASE,
            mode: 1,
            mask: 0,
        });
        a
    }

    /// The node missing a `qdkg_V` row and the three liars of `group-missing-vvec-row`: the
    /// highest-hash premature commitment the node holds is from the member that does not lie,
    /// the lowest from a liar.
    fn missing_row_case() -> (u8, Vec<u8>) {
        for node in 0..5u8 {
            for honest in (0..5).filter(|&j| j != node) {
                let liars: Vec<u8> = (0..5).filter(|&j| j != node && j != honest).collect();
                let run = Run::play(&missing_row_script(node, &liars), true);
                let popped = popped(&run, usize::from(node));
                let by = |h: Option<(&[u8; 32], &PrematureCommitment)>| {
                    h.and_then(|(_, qc)| run.member_index(qc.pro_tx_hash()))
                };
                let (low, high) = (by(popped.iter().next()), by(popped.iter().next_back()));
                let honest = usize::from(honest);
                if high == Some(honest) && low.is_some_and(|l| l != honest) {
                    return (node, liars);
                }
            }
        }
        panic!("no member set orders the commitments as needed");
    }

    fn missing_row_script(node: u8, liars: &[u8]) -> Script {
        let mut a = vec![Action::MissingRow {
            node,
            who: (node + 1) % 5,
        }];
        for &m in liars {
            a.extend(rates(m, &[SimError::CommitLie]));
            a.push(Action::LieType {
                member: m,
                lie: 1,
            });
        }
        script(S5, 0, a)
    }

    /// Every seed: one per C7 matrix row, the finals and groups the invariants judge, and one
    /// per action.
    fn seed_scripts() -> Vec<(String, Script)> {
        let mut out: Vec<(String, Script)> = Vec::new();
        for (row, (name, kinds)) in ROWS.iter().enumerate() {
            let member = (row % 3) as u8;
            if *name == "commit-lie" {
                for lie in 0..5 {
                    out.push((
                        format!("row-commit-lie-type-{lie}"),
                        script(S3, 0, commit_lie(member, lie)),
                    ));
                }
                for lie in [3, 4] {
                    let name = format!("row-commit-lie-legacy-type-{lie}");
                    out.push((name, script(S3, LEGACY, commit_lie(member, lie))));
                }
            } else {
                out.push((format!("row-{name}"), script(S3, 0, rates(member, kinds))));
            }
        }
        let clone =
            [rates(0, &[SimError::ContributionClone]), rates(1, &[SimError::ContributionClone])];
        out.push(("row-contribution-clone".into(), script(S5, 0, clone.concat())));

        let all: Vec<SimError> = SimError::ALL.to_vec();
        out.push(("rates-all-on-rotated".into(), script(DIP24, 4, rates(0, &all))));
        let kinds = 0..SimError::ALL.len() as u8;
        let zero = kinds.clone().map(|k| rate(0, k, 0)).collect();
        out.push(("rates-all-zero".into(), script(S3, 0, zero)));
        let high = vec![rate(1, kind(SimError::CommitOmit), 201)];
        out.push(("rate-high-byte-commit-omit".into(), script(S3, 0, high)));
        let partial = kinds.map(|k| rate(2, k, 150)).collect();
        out.push(("rates-partial".into(), script(S5, 0, partial)));
        let mut raised = advances(3);
        raised.extend(rates(1, &[SimError::CommitOmit]));
        out.push(("rate-raised-before-commit".into(), script(S3, 0, raised)));
        let mut lowered = rates(1, &[SimError::CommitOmit]);
        lowered.extend(advances(3));
        lowered.push(rate(1, kind(SimError::CommitOmit), 0));
        out.push(("rate-lowered-before-commit".into(), script(S3, 0, lowered)));
        let mut mid_pop = advances(1);
        mid_pop.push(Action::Pop {
            node: 1,
            max: 0,
        });
        mid_pop.extend(rates(1, &[SimError::ComplainLie]));
        out.push(("rate-raised-between-pops".into(), script(S3, 0, mid_pop)));
        let mut drained = advances(1);
        drained.extend((0..3).map(|node| Action::Pop {
            node,
            max: 7,
        }));
        drained.extend(rates(1, &[SimError::ComplainLie]));
        out.push(("rate-raised-after-contribution-drain".into(), script(S3, 0, drained)));

        let mut split = advances(4);
        split.extend([
            Action::Drop {
                node: 0,
                pos: 2,
            },
            Action::Drop {
                node: 1,
                pos: 0,
            },
            Action::Drop {
                node: 2,
                pos: 2,
            },
            Action::Advance,
        ]);
        out.push(("final-split-01-12-01".into(), script(S3, 0, split)));
        let mut straddle = advances(4);
        straddle.extend([
            Action::Pop {
                node: 0,
                max: 1,
            },
            Action::Scheme,
            Action::Advance,
        ]);
        out.push(("final-scheme-straddle".into(), script(S3, LEGACY, straddle)));
        let mut truncated_pop = advances(4);
        truncated_pop.extend([
            Action::Flip {
                node: 0,
                pos: 0,
                offset: 10,
                mode: 0x80,
            },
            Action::Scheme,
            Action::Pop {
                node: 0,
                max: 0,
            },
            Action::Scheme,
        ]);
        out.push(("commit-truncated-pop-scheme-flipped".into(), script(S3, 0, truncated_pop)));
        let (node, liars) = missing_row_case();
        out.push(("group-missing-vvec-row".into(), missing_row_script(node, &liars)));

        let mut truncated = advances(1);
        truncated.push(Action::Flip {
            node: 0,
            pos: 1,
            offset: 10,
            mode: 0x80,
        });
        out.push(("queue-truncated-contribution".into(), script(S3, 0, truncated)));
        let mut flipped = advances(1);
        flipped.push(Action::Flip {
            node: 2,
            pos: 0,
            offset: 300,
            mode: 4,
        });
        out.push(("queue-flipped-contribution".into(), script(S3, 0, flipped)));
        let mut queue = vec![
            Action::PopSize {
                node: 0,
                size: 0,
            },
            Action::Advance,
        ];
        queue.extend([
            Action::Dup {
                node: 0,
                pos: 1,
            },
            Action::Swap {
                node: 0,
                a: 0,
                b: 2,
            },
            Action::Pop {
                node: 0,
                max: 1,
            },
            Action::Drop {
                node: 1,
                pos: 1,
            },
        ]);
        out.push(("queue-dup-swap-drop".into(), script(S3, 0, queue)));
        let mut cleared = advances(4);
        cleared.push(Action::Clear {
            node: 2,
        });
        out.push(("queue-cleared-commit".into(), script(S3, 0, cleared)));

        for (lie, mode) in [(0, 2), (2, 2), (3, 2)] {
            let mut a = commit_lie(0, lie);
            a.push(Action::Mode {
                member: 0,
                phase: COMMIT_PHASE,
                mode,
                mask: 0,
            });
            out.push((format!("commit-lie-type-{lie}-resigned-sent"), script(S3, 0, a)));
        }
        for field in [0, 1] {
            let mut a = advances(4);
            a.push(Action::Perturb {
                node: 1,
                pos: 0,
                field,
            });
            out.push((format!("commit-perturbed-field-{field}"), script(S3, 0, a.clone())));
            out.push((format!("commit-perturbed-field-{field}-legacy"), script(S3, LEGACY, a)));
        }
        let modified = vec![Action::Mode {
            member: 1,
            phase: 0,
            mode: 2,
            mask: 0b101,
        }];
        out.push(("contribution-modified-sender".into(), script(S3, 0, modified)));

        let mut eq = advances(1);
        eq.push(Action::Equivocate {
            member: 1,
            offset: 260,
            xor: 2,
        });
        out.push(("equivocate-contribution".into(), script(S3, 0, eq)));
        let mut eq = advances(4);
        eq.push(Action::Equivocate {
            member: 2,
            offset: 120,
            xor: 1,
        });
        out.push(("equivocate-commitment".into(), script(S3, 0, eq)));
        let conn = vec![
            Action::Conn {
                member: 0,
                sporks: 4,
                facts: 0x5b,
            },
            Action::Conn {
                member: 1,
                sporks: 1,
                facts: 0xe3,
            },
        ];
        out.push(("conn-snapshots".into(), script(S3, 0, conn)));
        let misuse = vec![
            Action::Misuse {
                member: 0,
                which: 0,
            },
            Action::Advance,
            Action::Misuse {
                member: 1,
                which: 0,
            },
            Action::Advance,
            Action::Advance,
            Action::Misuse {
                member: 2,
                which: 1,
            },
        ];
        out.push(("misuse-out-of-order".into(), script(S3, 0, misuse)));
        let mut flip = advances(1);
        flip.push(Action::Scheme);
        out.push(("scheme-flip-after-contribute".into(), script(S3, 0, flip)));
        let mut repeated = rates(0, &[SimError::ContributionLie]);
        repeated.push(Action::Advance);
        repeated.resize(
            34,
            Action::Dup {
                node: 1,
                pos: 0,
            },
        );
        out.push(("contribution-lie-duplicated-32".into(), script(S3, 0, repeated)));
        let straddle = vec![
            Action::Advance,
            Action::Pop {
                node: 1,
                max: 7,
            },
            Action::Scheme,
        ];
        out.push(("contributions-popped-then-scheme-flip".into(), script(S5, 0, straddle)));
        out.push((
            "finalizers-subset".into(),
            script(
                S3,
                0,
                vec![Action::Finalizers {
                    mask: 0b101,
                }],
            ),
        ));
        out.push(("rotated-index-1".into(), script(DIP24, 4, vec![])));
        out.push(("rotated-legacy".into(), script(DIP24, LEGACY, vec![])));
        out.push(("set-12-honest".into(), script(S12, 0, vec![])));
        out
    }

    static SEEDS: LazyLock<Vec<(String, Script)>> = LazyLock::new(seed_scripts);

    fn seed(name: &str) -> Script {
        SEEDS.iter().find(|(n, _)| n == name).expect("the seed").1.clone()
    }

    fn dir() -> String {
        format!("{}/seeds/dkg_session", env!("CARGO_MANIFEST_DIR"))
    }

    /// Writes the seed files from [`seed_scripts`].
    #[test]
    #[ignore = "writes node-fuzz/seeds/dkg_session"]
    fn write_seeds() {
        std::fs::create_dir_all(dir()).expect("the seed directory");
        for (name, s) in SEEDS.iter() {
            std::fs::write(format!("{}/{name}", dir()), s.bytes()).expect("a seed file");
        }
    }

    #[test]
    fn seeds_are_the_helpers() {
        let names: BTreeSet<String> = std::fs::read_dir(dir())
            .expect("the seed directory")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let want: BTreeSet<String> = SEEDS.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, want);
        for (name, s) in SEEDS.iter() {
            let data = std::fs::read(format!("{}/{name}", dir())).expect("a seed file");
            assert_eq!(data, s.bytes(), "{name}");
            assert_eq!(Script::parse(&data), *s, "{name}");
        }
    }

    #[test]
    fn seeds_hold() {
        for (name, s) in SEEDS.iter() {
            assert_eq!(failures(&s.bytes()), Vec::<&str>::new(), "{name}");
        }
    }

    /// `llmq_test` overridden to 33:20, which reaches the 32-share pending verify with no action.
    /// No byte 0 names it.
    static SET_33: LazyLock<Material> =
        LazyLock::new(|| material(4, LlmqType::LlmqTest, overridden(33, 20, 20, 20), false));
    /// The action cap of the 33-member scripts.
    const MAX_ACTIONS_33: usize = 4;

    /// `actions` on [`SET_33`]; byte 0 is not read.
    fn failures_33(actions: Vec<Action>) -> Vec<&'static str> {
        failures_in(&SET_33, &script(S3, 0, actions))
    }

    /// The honest 33-member run, which reaches the 32-share pending verify: about a minute of CPU
    /// in release, far more in a test build.
    #[test]
    #[ignore = "a 33-member run: minutes in a release test build"]
    fn seed_33_holds() {
        assert_eq!(failures_33(vec![]), Vec::<&str>::new());
    }

    /// Long 33-member scripts, every member finalizing, each [`MAX_ACTIONS_33`] actions: that
    /// many members complain-lying about every other (so each other member justifies to them),
    /// lying in their contributions to every other (each pending verify fails for them, then
    /// they justify to all), as modified senders of fresh shares to every other, or half of each
    /// lie; or contribution duplicates at one receiver; or contribution equivocations.
    fn long_33(fill: &str) -> Vec<Action> {
        let liars = |k: SimError, members: Range<usize>| -> Vec<Action> {
            members.map(|m| rate(m as u8, kind(k), 1)).collect()
        };
        let all = 0..MAX_ACTIONS_33;
        match fill {
            "complain-lies" => liars(SimError::ComplainLie, all),
            "contribution-lies" => liars(SimError::ContributionLie, all),
            "modified-senders" => all
                .map(|m| Action::Mode {
                    member: m as u8,
                    phase: 0,
                    mode: 2,
                    mask: 0xff,
                })
                .collect(),
            "mixed-lies" => {
                let half = MAX_ACTIONS_33 / 2;
                let mut a = liars(SimError::ComplainLie, 0..half);
                a.extend(liars(SimError::ContributionLie, half..MAX_ACTIONS_33));
                a
            }
            "contribution-dups" | "contribution-equivocations" => {
                let mut a = advances(1);
                a.extend(all.skip(1).map(|k| match fill {
                    "contribution-dups" => Action::Dup {
                        node: 0,
                        pos: 0,
                    },
                    _ => Action::Equivocate {
                        member: k as u8,
                        offset: 1200 + k as u16,
                        xor: 1,
                    },
                }));
                a
            }
            other => panic!("unknown fill {other}"),
        }
    }

    #[test]
    #[ignore = "a 33-member run: minutes in a release test build"]
    fn long_33_complain_lies_hold() {
        assert_eq!(failures_33(long_33("complain-lies")), Vec::<&str>::new());
    }

    #[test]
    #[ignore = "a 33-member run: minutes in a release test build"]
    fn long_33_contribution_lies_hold() {
        assert_eq!(failures_33(long_33("contribution-lies")), Vec::<&str>::new());
    }

    #[test]
    #[ignore = "a 33-member run: minutes in a release test build"]
    fn long_33_modified_senders_hold() {
        assert_eq!(failures_33(long_33("modified-senders")), Vec::<&str>::new());
    }

    #[test]
    #[ignore = "a 33-member run: minutes in a release test build"]
    fn long_33_mixed_lies_hold() {
        assert_eq!(failures_33(long_33("mixed-lies")), Vec::<&str>::new());
    }

    #[test]
    #[ignore = "a 33-member run: minutes in a release test build"]
    fn long_33_contribution_dups_hold() {
        assert_eq!(failures_33(long_33("contribution-dups")), Vec::<&str>::new());
    }

    #[test]
    #[ignore = "a 33-member run: minutes in a release test build"]
    fn long_33_contribution_equivocations_hold() {
        assert_eq!(failures_33(long_33("contribution-equivocations")), Vec::<&str>::new());
    }

    /// Byte flips of the seeds after byte 0 (a fixed xorshift).
    #[test]
    fn mutated_seeds_hold() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for (name, s) in SEEDS.iter() {
            let seed = s.bytes();
            for _ in 0..2 {
                let mut data = seed.clone();
                for _ in 0..3 {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    let at = 1 + (state as usize) % (data.len() - 1);
                    data[at] ^= (state >> 32) as u8;
                }
                assert_eq!(failures(&data), Vec::<&str>::new(), "{name} mutated: {data:?}");
            }
        }
    }

    /// Node 1 receives member 0's lying contribution 33 times: the 32nd push's verify writes a
    /// `qdkg_E` row per pending entry, fails and is complained about, so the Complain start's
    /// verify skips the 33rd copy.
    #[test]
    fn verify_after_failed_one_skips_the_member() {
        let run = Run::play(&seed("contribution-lie-duplicated-32"), true);
        let encrypted = |kind: StepKind| -> Vec<usize> {
            run.steps[1]
                .iter()
                .filter(|s| s.kind == kind)
                .flat_map(|s| &s.rows)
                .filter_map(|(_, _, row)| match row {
                    StoreRow::Encrypted {
                        who,
                        ..
                    } => run.member_index(who),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(encrypted(StepKind::Pop(DkgMsgKind::Contribution)), vec![0; 32]);
        assert_eq!(encrypted(StepKind::Complain), vec![1, 2]);
    }

    /// The fuzz input space reaches the 32-share pending verify at a small set: the seed file,
    /// read as the target reads it, runs on 3 members, and node 1 verifies 32 pending
    /// contributions inside a contribution receive.
    #[test]
    fn small_set_reaches_the_32_share_verify() {
        let data = std::fs::read(format!("{}/contribution-lie-duplicated-32", dir()))
            .expect("a seed file");
        let run = Run::play(&Script::parse(&data), true);
        assert_eq!(run.n(), 3);
        let events = &run.net.nodes[1].events;
        let verified: Vec<String> = run.steps[1]
            .iter()
            .filter(|s| s.kind == StepKind::Pop(DkgMsgKind::Contribution))
            .flat_map(|s| &events[s.events.clone()])
            .map(ToString::to_string)
            .filter(|l| l.contains("pending contributions"))
            .collect();
        assert_eq!(verified.len(), 1, "{verified:?}");
        assert!(verified[0].ends_with("verified 32 pending contributions"), "{verified:?}");
        assert_eq!(failures(&data), Vec::<&str>::new());
    }

    /// The store check places each row by the receive starts the harness saw: a justified
    /// `qdkg_S` row taken as written before its receive started, or after the next one started,
    /// fails it.
    #[test]
    fn justified_share_outside_its_receive_fails() {
        let mut run = Run::play(&seed("row-complain-lie"), true);
        let fails = |run: &Run| {
            let mut out = BTreeSet::new();
            run.inv3(&mut out);
            out.contains(INV3)
        };
        assert!(!fails(&run));
        let (i, s, r, k) = (0..run.n())
            .flat_map(|i| run.steps[i].iter().enumerate().map(move |(s, step)| (i, s, step)))
            .find_map(|(i, s, step)| {
                if step.kind != StepKind::Pop(DkgMsgKind::Justification) {
                    return None;
                }
                let r = step.rows.iter().position(|row| matches!(row.2, StoreRow::Sk { .. }))?;
                let k = step
                    .starts
                    .iter()
                    .rposition(|start| start.rows <= r)
                    .filter(|k| k + 1 < step.starts.len())?;
                Some((i, s, r, k))
            })
            .expect("a justified write with another receive after it in its pop");
        let starts = run.steps[i][s].starts.clone();
        run.steps[i][s].starts[k].rows = r + 1;
        assert!(fails(&run), "written before its receive started");
        run.steps[i][s].starts = starts;
        run.steps[i][s].starts[k + 1].rows = r;
        assert!(fails(&run), "written after the next receive started");
    }

    #[test]
    fn parse_reads_missing_operands_as_zero() {
        let s = Script::parse(&[13, 1, 2, 0, 0, 0, 0, 0, 0, 0, 9, 5]);
        assert_eq!(s.set, 13);
        assert_eq!(s.flags, 1);
        assert_eq!(s.seed, 2);
        assert_eq!(
            s.actions,
            vec![Action::Rate {
                member: 5,
                kind: 0,
                rate: 0
            }]
        );
        assert_eq!(Script::parse(&[]).actions, vec![]);
        let long = Script::parse(&[0; 400]);
        assert_eq!(long.actions.len(), MAX_ACTIONS);
        let long = Script::parse(&[255; 400]);
        assert_eq!(long.actions.len(), MAX_ACTIONS);
        assert_eq!(set_index(255), set_index(248));
    }

    fn signers(run: &Run, i: usize) -> Vec<Vec<bool>> {
        let finals = &run.finals[i].as_ref().expect("a finalizer").finals;
        finals.iter().map(|qc| bits(qc.signers())).collect()
    }

    #[test]
    fn honest_quorums_finalize_alike() {
        for set in [S3, DIP24, S12] {
            let run = Run::play(&script(set, 0, vec![]), true);
            let n = run.n();
            for i in 0..n {
                assert_eq!(signers(&run, i), vec![vec![true; n]], "set {set} node {i}");
            }
        }
    }

    #[test]
    fn split_finalizes_two_finals() {
        let run = Run::play(&seed("final-split-01-12-01"), true);
        assert_eq!(signers(&run, 0), vec![vec![true, true, false]]);
        assert_eq!(signers(&run, 1), vec![vec![false, true, true]]);
        assert_eq!(signers(&run, 2), vec![vec![true, true, false]]);
        let bytes =
            |i: usize| final_commitment_bytes(&run.finals[i].as_ref().expect("final").finals[0]);
        assert_eq!(bytes(0), bytes(2));
    }

    #[test]
    fn missing_row_group_disagrees() {
        let s = seed("group-missing-vvec-row");
        let Some(Action::MissingRow {
            node,
            ..
        }) = s.actions.first().copied()
        else {
            panic!("the seed starts with its missing row");
        };
        let (node, run) = (usize::from(node), Run::play(&s, true));
        let skipped = run.pops[node]
            .iter()
            .flat_map(|p| relays(&p.outcome))
            .filter(|(_, skipped)| *skipped)
            .count();
        assert!(skipped >= 3, "{skipped}");
        let lines: Vec<String> =
            run.net.nodes[node].events.iter().map(ToString::to_string).collect();
        assert!(
            lines
                .iter()
                .any(|l| l.ends_with("quorumPublicKey or quorumVvecHash does not match, skipping")),
            "{lines:?}"
        );
        assert_eq!(run.finals[node].as_ref().expect("final").finals.len(), 1);
    }

    /// A scheme flip alone excepts no key: premature commitments popped under the other scheme
    /// than the one they were made under do not read, so they use none, and the twin is compared
    /// at every node.
    #[test]
    fn twin_exceptions_follow_cache_use() {
        for name in ["commit-truncated-pop-scheme-flipped", "final-scheme-straddle"] {
            let run = Run::play(&seed(name), true);
            let flipped_pop = run.pops[0].iter().any(|p| {
                p.phase == DkgMsgKind::PrematureCommitment
                    && p.bls.scheme() != run.pops[0][0].bls.scheme()
            });
            assert!(flipped_pop, "{name}");
            for i in 0..run.n() {
                assert_eq!(run.cache_exceptions(i).0, BTreeSet::new(), "{name} node {i}");
            }
        }
    }

    /// A node with an excepted key is still compared on all but that key's lines, invs and
    /// finals.
    #[test]
    fn twin_exception_keeps_the_rest_compared() {
        let s = seed("commit-truncated-pop-scheme-flipped");
        let run = Run::play(&s, true);
        let fresh = || Run::play(&s, false);
        let key = bits(run.finals[0].as_ref().expect("a final").finals[0].valid_members());
        let pcommit = *run
            .cache_exceptions(0)
            .1
            .iter()
            .find(|(_, k)| **k == key)
            .expect("a premature commitment of the key")
            .0;
        let except = |i: usize| {
            let (mut ex, keys) = run.cache_exceptions(i);
            if i == 0 {
                ex.insert(key.clone());
            }
            (ex, keys)
        };
        assert!(!run.twin_differs_except(&fresh(), except));
        let mut twin = fresh();
        let rec = twin.finals[0].as_mut().expect("a final");
        rec.finals.push(rec.finals[0].clone());
        twin.net.nodes[0].relayed.push(DkgInv {
            kind: DkgMsgKind::PrematureCommitment,
            hash: pcommit,
        });
        assert!(!run.twin_differs_except(&twin, except));
        assert!(run.twin_differs(&twin));
        let mut twin = fresh();
        twin.net.nodes[0].relayed.push(DkgInv {
            kind: DkgMsgKind::Contribution,
            hash: [7; 32],
        });
        assert!(run.twin_differs_except(&twin, except));
        let mut twin = fresh();
        let rec = twin.finals[1].as_mut().expect("a final");
        rec.finals.push(rec.finals[0].clone());
        assert!(run.twin_differs_except(&twin, except));
    }

    #[test]
    fn misuse_is_phase_order() {
        let run = Run::play(&seed("misuse-out-of-order"), true);
        assert_eq!(run.faults, 0);
        assert_eq!(run.steps.iter().flatten().filter(|s| s.kind == StepKind::Misuse).count(), 3);
    }
}
