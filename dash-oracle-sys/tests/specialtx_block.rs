//! `oracle_check_special_tx_block` (ABI 13): Core's block-path special-transaction check,
//! MNHF signals (type 7) and asset unlocks (type 9) included, on a synthetic regtest chain.
//!
//! Provenance: the chain is regtest from height 0 with `-dip3params=2:2` and V20 from height
//! 50; quorums are seeded as mined commitments whose public keys are BLS keys this file derives
//! (dash-pkc signs in the basic scheme, Core verifies), each signed by three masternodes the
//! chain context gives as the quorum's members (`common::quorum`), as Core stores them. Each expected reason is the one Core's own tests assert
//! (`src/test/evo_mnhf_tests.cpp`, `src/test/evo_assetlocks_tests.cpp`,
//! `test/functional/feature_asset_locks.py`) or, for the branches those do not reach, the
//! line of `src/evo/mnhftx.cpp` / `src/evo/assetlocktx.cpp` at the pin that returns it.
//! Nothing is read off the oracle. The last test runs Core's trivially_valid/invalid vectors
//! through both special-transaction entries and requires the same verdicts.

mod common;

use common::quorum::{
    MEMBERS, bls_key, hash256, member_dmns, quorum_commitment, signed_commitment,
};
use common::{Coverage, empty_list, hex, trivial_vectors};
use dash_oracle_sys::{
    BlockContext, Chain, Checkpoints, CoinsView, Context, DEPLOYMENT_V24, ErrorKind,
    MinedCommitment, Oracle, Pin, QuorumMembers, ResultCode, ThresholdState, UnlockIndexes,
    Verdict,
};
use dash_pkc::bls::BlsSigId;

/// `Consensus::LLMQType`: regtest's `llmqTypeMnhf` and `llmqTypePlatform` (chainparams.cpp).
const LLMQ_TEST: u8 = 100;
const LLMQ_TEST_PLATFORM: u8 = 106;
/// `LLMQ_50_60`: not configured on regtest. `LLMQ_TEST_INSTANTSEND`: configured, but not enabled
/// once DIP0024 quorums are (regtest's DIP0024 InstantSend type is LLMQ_TEST_DIP0024).
const LLMQ_50_60: u8 = 1;
const LLMQ_TEST_INSTANTSEND: u8 = 104;
/// The tip of the chain the checks run on; quorum bases are at multiples of the DKG interval
/// (24 for both types), each mined 12 blocks later, inside the mining window.
const TIP: usize = 120;
const PLATFORM_BASES: [usize; 4] = [24, 48, 72, 96];
const MNHF_BASE: usize = 96;
/// `DEPLOYMENT_V24`'s bit on regtest (an EHF deployment), `DEPLOYMENT_TESTDUMMY`'s (not) and
/// one no deployment uses.
const V24_BIT: u8 = 13;
const TESTDUMMY_BIT: u8 = 28;
const UNUSED_BIT: u8 = 5;
/// `-testactivationheight=v20@50`: the asset-unlock indexes reach the check from there.
const V20_HEIGHT: usize = 50;
/// `TxValidationResult` (consensus/validation.h).
const TX_CONSENSUS: ResultCode = ResultCode::Tx(1);
const TX_BAD_SPECIAL: ResultCode = ResultCode::Tx(7);

fn oracle() -> Oracle {
    Oracle::new(Pin::V24_0)
}

fn chain() -> Chain {
    Chain::new("regtest", &["-dip3params=2:2", "-testactivationheight=v20@50"])
}

/// The `TxValidationResult` Core rejects with `reason`: every MNHF reject is TX_CONSENSUS
/// (mnhftx.cpp:97-153); an asset unlock's are TX_BAD_SPECIAL but for the index, quorum hash and
/// signature checks (assetlocktx.cpp:129-152, 185-219); DIP0003 inactive is TX_BAD_SPECIAL
/// (specialtxman.cpp:250).
fn result_of(reason: &str) -> ResultCode {
    match reason {
        "bad-assetunlock-duplicated-index"
        | "bad-assetunlock-quorum-hash"
        | "bad-assetunlock-quorum-internal-error"
        | "bad-assetunlock-not-verified" => TX_CONSENSUS,
        r if r.starts_with("bad-mnhf-") => TX_CONSENSUS,
        r if r.starts_with("bad-assetunlock") || r == "bad-tx-type-dip3-inactive" => TX_BAD_SPECIAL,
        other => panic!("no class recorded for {other}"),
    }
}

/// The time of the regtest header at `height`: ten minutes apart.
fn block_time(height: usize) -> u32 {
    1_417_713_337 + 600 * height as u32
}

/// Regtest headers from height 0 (linkage and times are what the checks read).
fn regtest_headers(oracle: &Oracle, n: usize) -> (Vec<[u8; 80]>, Vec<[u8; 32]>) {
    let (mut out, mut hashes) = (Vec::new(), Vec::new());
    let mut prev = [0u8; 32];
    for i in 0..n {
        let h = header(prev, block_time(i));
        prev = oracle.header_decode(&h).unwrap().hash;
        out.push(h);
        hashes.push(prev);
    }
    (out, hashes)
}

fn header(prev: [u8; 32], time: u32) -> [u8; 80] {
    let mut h = common::header(time, 0x207f_ffff);
    h[0..4].copy_from_slice(&4i32.to_le_bytes());
    h[4..36].copy_from_slice(&prev);
    h
}

/// The commitment of a three-member quorum: [`MEMBERS`] valid and signing, key `seed`.
fn commitment(llmq_type: u8, quorum_hash: [u8; 32], seed: u8) -> Vec<u8> {
    quorum_commitment(llmq_type, quorum_hash, 3, &MEMBERS, seed)
}

/// Byte ranges of [`commitment`]'s fields.
const QC_VERSION: std::ops::Range<usize> = 0..2;
const QC_BITSETS: std::ops::Range<usize> = 35..39;
const QC_PUBLIC_KEY: std::ops::Range<usize> = 39..87;
const QC_VVEC_HASH: std::ops::Range<usize> = 87..119;
const QC_QUORUM_SIG: std::ops::Range<usize> = 119..215;
const QC_MEMBERS_SIG: std::ops::Range<usize> = 215..311;

/// `llmq::SignHash` of a request.
fn sign_hash(
    llmq_type: u8,
    quorum_hash: [u8; 32],
    request_id: [u8; 32],
    msg: [u8; 32],
) -> [u8; 32] {
    hash256(&[&[llmq_type][..], &quorum_hash, &request_id, &msg].concat())
}

/// `SerializeHash(std::make_pair(prefix, n))`.
fn request_id(prefix: &str, n: u64) -> [u8; 32] {
    hash256(&[&[prefix.len() as u8][..], prefix.as_bytes(), &n.to_le_bytes()].concat())
}

fn sign(seed: u8, hash: [u8; 32]) -> [u8; 96] {
    bls_key(seed).sign_with(&hash, BlsSigId::Basic).to_bytes()
}

fn special_tx(tx_type: u16, inputs: usize, outputs: usize, payload: &[u8]) -> Vec<u8> {
    let mut tx = (3u32 | u32::from(tx_type) << 16).to_le_bytes().to_vec();
    tx.push(inputs as u8);
    for i in 0..inputs {
        tx.extend([i as u8 + 1; 32]);
        tx.extend(0u32.to_le_bytes());
        tx.push(0);
        tx.extend(u32::MAX.to_le_bytes());
    }
    tx.push(outputs as u8);
    for i in 0..outputs {
        tx.extend((100_000 + i as i64).to_le_bytes());
        tx.push(25);
        tx.extend([0x76, 0xa9, 0x14]);
        tx.extend([i as u8; 20]);
        tx.extend([0x88, 0xac]);
    }
    tx.extend(0u32.to_le_bytes());
    tx.push(payload.len() as u8);
    tx.extend(payload);
    tx
}

/// An MNHF signal for `bit` against `quorum_hash`, signed by key `seed` as Core verifies it
/// (mnhftx.cpp:143-153: the txid with a null signature, the "mnhf" request id).
fn mnhf(version: u8, bit: u8, quorum_hash: [u8; 32], seed: u8) -> Vec<u8> {
    special_tx(7, 0, 0, &mnhf_payload(version, bit, quorum_hash, seed))
}

fn mnhf_payload(version: u8, bit: u8, quorum_hash: [u8; 32], seed: u8) -> Vec<u8> {
    let payload = |sig: &[u8; 96]| [&[version, bit][..], &quorum_hash, sig].concat();
    let msg = hash256(&special_tx(7, 0, 0, &payload(&[0; 96])));
    let sig = sign(seed, sign_hash(LLMQ_TEST, quorum_hash, request_id("mnhf", bit.into()), msg));
    payload(&sig)
}

#[derive(Clone, Copy)]
struct Unlock {
    version: u8,
    index: u64,
    requested_height: u32,
    quorum_hash: [u8; 32],
    inputs: usize,
    outputs: usize,
    seed: u8,
}

impl Unlock {
    fn new(quorum_hash: [u8; 32], seed: u8) -> Self {
        Self {
            version: 1,
            index: 7,
            requested_height: 110,
            quorum_hash,
            inputs: 0,
            outputs: 2,
            seed,
        }
    }

    /// The transaction, signed as Core verifies it (assetlocktx.cpp:222-229, 145: the whole
    /// serialization with a null signature, the "plwdtx" request id).
    fn tx(&self) -> Vec<u8> {
        special_tx(9, self.inputs, self.outputs, &self.payload())
    }

    fn payload(&self) -> Vec<u8> {
        let payload = |sig: &[u8; 96]| {
            let mut p = vec![self.version];
            p.extend(self.index.to_le_bytes());
            p.extend(1000u32.to_le_bytes());
            p.extend(self.requested_height.to_le_bytes());
            p.extend(self.quorum_hash);
            p.extend(sig);
            p
        };
        let msg = hash256(&special_tx(9, self.inputs, self.outputs, &payload(&[0; 96])));
        let hash =
            sign_hash(LLMQ_TEST_PLATFORM, self.quorum_hash, request_id("plwdtx", self.index), msg);
        payload(&sign(self.seed, hash))
    }
}

/// The chain, its quorums and the contexts the checks run in.
struct Setup {
    oracle: Oracle,
    headers: Vec<[u8; 80]>,
    hashes: Vec<[u8; 32]>,
    /// A header off the chain, on top of height 100.
    fork: [u8; 80],
    fork_hash: [u8; 32],
}

/// The key of the quorum based at `height`.
fn seed_of(height: usize) -> u8 {
    (height / 24) as u8
}

impl Setup {
    fn new() -> Self {
        let oracle = oracle();
        let (headers, hashes) = regtest_headers(&oracle, TIP + 1);
        let fork = header(hashes[100], block_time(101) + 1);
        let fork_hash = oracle.header_decode(&fork).unwrap().hash;
        Self {
            oracle,
            headers,
            hashes,
            fork,
            fork_hash,
        }
    }

    /// The commitment of the quorum based at `base`, mined 12 blocks later.
    fn mined(&self, llmq_type: u8, base: usize) -> MinedCommitment {
        self.mined_at(llmq_type, base, base + 12)
    }

    fn mined_at(&self, llmq_type: u8, base: usize, mined_height: usize) -> MinedCommitment {
        MinedCommitment {
            llmq_type,
            commitment: commitment(llmq_type, self.hashes[base], seed_of(base)),
            mined_height: mined_height as i32,
        }
    }

    /// The node's commitments: every platform quorum and one MNHF quorum.
    fn all_mined(&self) -> Vec<MinedCommitment> {
        let mut mined: Vec<_> =
            PLATFORM_BASES.iter().map(|&b| self.mined(LLMQ_TEST_PLATFORM, b)).collect();
        mined.push(self.mined(LLMQ_TEST, MNHF_BASE));
        mined
    }

    fn block(&self) -> BlockContext {
        BlockContext {
            fork_headers: vec![self.fork],
            mined: self.all_mined(),
            unlock_indexes: UnlockIndexes::from_indexes([1, 2, 3, 9]),
        }
    }

    /// The headers `first..=tip`, V24 in `v24`, members for each of `mined`.
    fn context(
        &self,
        first: usize,
        tip: usize,
        v24: ThresholdState,
        mined: &[MinedCommitment],
    ) -> Context {
        let quorums = mined
            .iter()
            .map(|m| QuorumMembers {
                llmq_type: m.llmq_type,
                quorum_hash: m.commitment[3..35].try_into().unwrap(),
                members: member_dmns(&MEMBERS),
            })
            .collect();
        Context::new(
            first as i32,
            self.headers[first..=tip].to_vec(),
            false,
            vec![(DEPLOYMENT_V24, v24)],
            1_500_000_000,
            Checkpoints::Disabled,
            CoinsView::Unchecked(vec![]),
            empty_list(),
            quorums,
        )
    }

    fn full(&self) -> Context {
        self.context(0, TIP, ThresholdState::Active, &self.all_mined())
    }

    fn check(
        &self,
        tx: &[u8],
        ctx: &Context,
        block: &BlockContext,
        check_sigs: bool,
    ) -> dash_oracle_sys::Result<Verdict> {
        self.check_on(&chain(), tx, ctx, block, check_sigs)
    }

    fn check_on(
        &self,
        chain: &Chain,
        tx: &[u8],
        ctx: &Context,
        block: &BlockContext,
        check_sigs: bool,
    ) -> dash_oracle_sys::Result<Verdict> {
        self.oracle.check_special_tx_block(chain, tx, ctx, block, check_sigs)
    }

    /// Core's reason, the same whatever `check_sigs` says (types 7 and 9 verify their quorum
    /// signature either way); `None` for an accept.
    /// The reason's `TxValidationResult` must be the one Core's line returns ([`result_of`]).
    fn reason(&self, tx: &[u8], ctx: &Context, block: &BlockContext) -> Option<String> {
        self.reason_on(&chain(), tx, ctx, block)
    }

    fn reason_on(
        &self,
        chain: &Chain,
        tx: &[u8],
        ctx: &Context,
        block: &BlockContext,
    ) -> Option<String> {
        let verdicts =
            [true, false].map(|sigs| self.check_on(chain, tx, ctx, block, sigs).unwrap());
        assert_eq!(verdicts[0], verdicts[1], "check_sigs changed Core's verdict");
        match &verdicts[0] {
            Verdict::Accept => None,
            Verdict::Reject(r) => {
                assert_eq!(r.result, result_of(&r.reason), "{}", r.reason);
                Some(r.reason.clone())
            }
        }
    }
}

fn expect(coverage: &mut Coverage, got: Option<String>, expected: Option<&str>, what: &str) {
    assert_eq!(got.as_deref(), expected, "{what}");
    coverage.hit(expected.unwrap_or("accept"));
}

#[test]
fn mnhf_signals() {
    let s = Setup::new();
    let (ctx, block) = (s.full(), s.block());
    let q = s.hashes[MNHF_BASE];
    let seed = seed_of(MNHF_BASE);
    let mut cov = Coverage::default();
    let mut run =
        |tx: Vec<u8>, ctx: &Context, block: &BlockContext, expected: Option<&str>, what: &str| {
            expect(&mut cov, s.reason(&tx, ctx, block), expected, what)
        };

    run(mnhf(1, V24_BIT, q, seed), &ctx, &block, None, "signed by the quorum");
    // The signature is verified whatever check_sigs says (mnhftx.cpp:151-153).
    run(mnhf(1, V24_BIT, q, seed + 1), &ctx, &block, Some("bad-mnhf-invalid"), "another key");
    run(mnhf(1, V24_BIT, q, 0), &ctx, &block, Some("bad-mnhf-invalid"), "another key");

    let payload = mnhf_payload(1, V24_BIT, q, seed);
    let truncated = special_tx(7, 0, 0, &payload[..payload.len() - 1]);
    run(truncated, &ctx, &block, Some("bad-mnhf-payload"), "a truncated payload");
    for version in [0, 2] {
        run(
            mnhf(version, V24_BIT, q, seed),
            &ctx,
            &block,
            Some("bad-mnhf-version"),
            "payload version",
        );
    }
    // VERSIONBITS_NUM_BITS = 29 (versionbits.h:22).
    for bit in [29, 32] {
        run(mnhf(1, bit, q, seed), &ctx, &block, Some("bad-mnhf-nbit-out-of-bounds"), "bit");
    }
    // evo_mnhf_tests.cpp:82-85 (bit 28) and 76-79 (an unknown quorum hash).
    run(mnhf(1, TESTDUMMY_BIT, q, seed), &ctx, &block, Some("bad-mnhf-non-ehf"), "a non-EHF bit");
    run(
        mnhf(1, V24_BIT, [0x42; 32], seed),
        &ctx,
        &block,
        Some("bad-mnhf-quorum-hash"),
        "unknown hash",
    );
    // A known block off the chain (mnhftx.cpp:135-138).
    run(
        mnhf(1, V24_BIT, s.fork_hash, seed),
        &ctx,
        &block,
        Some("bad-mnhf-quorum-hash"),
        "a fork block",
    );
    // A chain block with no commitment of the MNHF type (one of the platform type is there),
    // and the MNHF quorum when the node has not mined its commitment (mnhftx.cpp:147-149).
    let platform = s.hashes[72];
    run(
        mnhf(1, V24_BIT, platform, seed_of(72)),
        &ctx,
        &block,
        Some("bad-mnhf-missing-quorum"),
        "other type",
    );
    let unmined = BlockContext {
        mined: s.all_mined().into_iter().filter(|m| m.llmq_type != LLMQ_TEST).collect(),
        ..s.block()
    };
    run(mnhf(1, V24_BIT, q, seed), &ctx, &unmined, Some("bad-mnhf-missing-quorum"), "not mined");
    // Before DIP0003 (active from height 2) no special transaction is valid.
    let early = s.context(0, 0, ThresholdState::Defined, &[]);
    run(
        mnhf(1, V24_BIT, q, seed),
        &early,
        &BlockContext::default(),
        Some("bad-tx-type-dip3-inactive"),
        "dip3",
    );

    cov.require(&[
        ("accept", 1),
        ("bad-mnhf-invalid", 2),
        ("bad-mnhf-payload", 1),
        ("bad-mnhf-version", 2),
        ("bad-mnhf-nbit-out-of-bounds", 2),
        ("bad-mnhf-non-ehf", 1),
        ("bad-mnhf-quorum-hash", 2),
        ("bad-mnhf-missing-quorum", 2),
        ("bad-tx-type-dip3-inactive", 1),
    ]);
}

/// The MNHF check reads the tip's median time past (IsValidMNActivation, chainparams.cpp:
/// 109-131, after IsTriviallyValid, mnhftx.cpp:120-126): a deployment outside its time window
/// is skipped, and a bit no deployment (left) uses is accepted as unknown, as Core's
/// evo_mnhf_tests.cpp:97-134 does by moving TESTDUMMY's window. The window edges here sit on
/// the median time past itself, so the verdict turns on the shim giving Core exactly that time,
/// from all the headers or from exactly the 11 it is computed from.
#[test]
fn mnhf_median_time_past() {
    let s = Setup::new();
    let (q, seed) = (s.hashes[MNHF_BASE], seed_of(MNHF_BASE));
    // The MNHF quorum mined at the first block of its mining window, which is the tip: the tip's
    // median time past is block 101's time.
    let tip = MNHF_BASE + 10;
    let mtp = i64::from(block_time(tip - 5));
    let mined = vec![s.mined_at(LLMQ_TEST, MNHF_BASE, tip)];
    let block = BlockContext {
        mined: mined.clone(),
        ..BlockContext::default()
    };
    let contexts = [
        s.context(0, tip, ThresholdState::Active, &mined),
        s.context(tip - 10, tip, ThresholdState::Active, &mined),
    ];
    for (start, timeout, bit, expected) in [
        (mtp, i64::MAX, TESTDUMMY_BIT, Some("bad-mnhf-non-ehf")),
        (mtp + 1, i64::MAX, TESTDUMMY_BIT, None),
        (0, mtp, TESTDUMMY_BIT, Some("bad-mnhf-non-ehf")),
        (0, mtp - 1, TESTDUMMY_BIT, None),
        (mtp, i64::MAX, UNUSED_BIT, None),
        (mtp, i64::MAX, V24_BIT, None),
    ] {
        let vbparams = format!("-vbparams=testdummy:{start}:{timeout}");
        let chain =
            Chain::new("regtest", &["-dip3params=2:2", "-testactivationheight=v20@50", &vbparams]);
        for ctx in &contexts {
            let got = s.reason_on(&chain, &mnhf(1, bit, q, seed), ctx, &block);
            assert_eq!(
                got.as_deref(),
                expected,
                "{vbparams}, bit {bit}, first {}",
                ctx.first_height
            );
        }
    }

    // Ten headers do not hold the median time past: a signal for a bit a deployment uses is
    // refused; for one none uses, Core does not read the time and goes on to the quorum, which
    // the node has not mined at this tip.
    let short = s.context(MNHF_BASE, tip - 1, ThresholdState::Active, &[]);
    for bit in [TESTDUMMY_BIT, V24_BIT] {
        let err = s.check(&mnhf(1, bit, q, seed), &short, &BlockContext::default(), true);
        assert_eq!(err.unwrap_err().kind, ErrorKind::Context, "bit {bit}");
    }
    let unknown = s.reason(&mnhf(1, UNUSED_BIT, q, seed), &short, &BlockContext::default());
    assert_eq!(unknown.as_deref(), Some("bad-mnhf-missing-quorum"));
}

#[test]
fn asset_unlocks() {
    let s = Setup::new();
    let (ctx, block) = (s.full(), s.block());
    let q = s.hashes[96];
    let base = Unlock::new(q, seed_of(96));
    let mut cov = Coverage::default();
    let mut run =
        |tx: Vec<u8>, ctx: &Context, block: &BlockContext, expected: Option<&str>, what: &str| {
            expect(&mut cov, s.reason(&tx, ctx, block), expected, what)
        };

    run(base.tx(), &ctx, &block, None, "signed by an active quorum");
    // The three newest platform quorums are active (signingActiveQuorumCount 2, plus one).
    let third = Unlock::new(s.hashes[48], seed_of(48));
    run(third.tx(), &ctx, &block, None, "the third newest quorum");
    run(
        Unlock {
            outputs: 32,
            ..base
        }
        .tx(),
        &ctx,
        &block,
        None,
        "32 withdrawals",
    );

    // evo_assetlocks_tests.cpp:446-458, 600-623, 474-499.
    run(
        Unlock {
            inputs: 1,
            ..base
        }
        .tx(),
        &ctx,
        &block,
        Some("bad-assetunlocktx-have-input"),
        "input",
    );
    run(
        Unlock {
            outputs: 33,
            ..base
        }
        .tx(),
        &ctx,
        &block,
        Some("bad-assetunlocktx-too-many-outs"),
        "33 outs",
    );
    let payload = base.payload();
    let truncated = special_tx(9, 0, base.outputs, &payload[..payload.len() - 1]);
    run(truncated, &ctx, &block, Some("bad-assetunlocktx-payload"), "a truncated payload");
    for version in [0, 3, 255] {
        run(
            Unlock {
                version,
                ..base
            }
            .tx(),
            &ctx,
            &block,
            Some("bad-assetunlocktx-version"),
            "version",
        );
    }
    let before_v24 = s.context(0, TIP, ThresholdState::Started, &s.all_mined());
    let v2 = Unlock {
        version: 2,
        ..base
    };
    run(v2.tx(), &before_v24, &block, Some("bad-assetunlocktx-version-2"), "version 2 before V24");
    run(v2.tx(), &ctx, &block, None, "version 2 after V24");
    run(
        Unlock {
            version: 1,
            ..base
        }
        .tx(),
        &before_v24,
        &block,
        None,
        "version 1 before V24",
    );
    run(
        Unlock {
            index: 9,
            ..base
        }
        .tx(),
        &ctx,
        &block,
        Some("bad-assetunlock-duplicated-index"),
        "index",
    );
    let indexes = BlockContext {
        unlock_indexes: UnlockIndexes::from_indexes([u64::MAX - 1, u64::MAX]),
        ..s.block()
    };
    run(
        Unlock {
            index: u64::MAX,
            ..base
        }
        .tx(),
        &ctx,
        &indexes,
        Some("bad-assetunlock-duplicated-index"),
        "max",
    );
    run(
        Unlock::new([0x42; 32], 1).tx(),
        &ctx,
        &block,
        Some("bad-assetunlock-quorum-hash"),
        "unknown hash",
    );

    // feature_asset_locks.py:765, 806: a quorum outside the scan. Here: the oldest mined
    // quorum, a chain block with no commitment and a fork block.
    for (hash, seed, what) in [
        (s.hashes[24], seed_of(24), "the fourth newest quorum"),
        (s.hashes[60], 1, "no commitment"),
        (s.fork_hash, 1, "a fork block"),
    ] {
        run(
            Unlock::new(hash, seed).tx(),
            &ctx,
            &block,
            Some("bad-assetunlock-too-old-quorum"),
            what,
        );
    }
    let unmined = BlockContext {
        mined: s.all_mined().into_iter().filter(|m| m.commitment[3..35] != q[..]).collect(),
        ..s.block()
    };
    let without = s.context(0, TIP, ThresholdState::Active, &unmined.mined);
    run(
        base.tx(),
        &without,
        &unmined,
        Some("bad-assetunlock-too-old-quorum"),
        "commitment not mined",
    );

    // feature_asset_locks.py:530, 1065: the window is [requestedHeight, requestedHeight + 48),
    // and a requested height whose expiry overflows is too late.
    for (requested_height, expected) in [
        (TIP as u32, None),
        (TIP as u32 + 1, Some("bad-assetunlock-too-late")),
        (TIP as u32 - 47, None),
        (TIP as u32 - 48, Some("bad-assetunlock-too-late")),
        (u32::MAX - 47, Some("bad-assetunlock-too-late")),
    ] {
        let tx = Unlock {
            requested_height,
            ..base
        }
        .tx();
        run(tx, &ctx, &block, expected, "the height window");
    }
    // feature_asset_locks.py:454: a signature the quorum did not make.
    run(
        Unlock {
            seed: 1,
            ..base
        }
        .tx(),
        &ctx,
        &block,
        Some("bad-assetunlock-not-verified"),
        "another key",
    );
    run(
        Unlock {
            seed: seed_of(72),
            ..base
        }
        .tx(),
        &ctx,
        &block,
        Some("bad-assetunlock-not-verified"),
        "quorum 72's key",
    );

    cov.require(&[
        ("accept", 7),
        ("bad-assetunlocktx-have-input", 1),
        ("bad-assetunlocktx-too-many-outs", 1),
        ("bad-assetunlocktx-payload", 1),
        ("bad-assetunlocktx-version", 3),
        ("bad-assetunlocktx-version-2", 1),
        ("bad-assetunlock-duplicated-index", 2),
        ("bad-assetunlock-quorum-hash", 1),
        ("bad-assetunlock-too-old-quorum", 4),
        ("bad-assetunlock-too-late", 3),
        ("bad-assetunlock-not-verified", 2),
    ]);
}

/// Verdicts that turn on blocks below a context that starts above genesis are refused, and
/// the same rejects stay rejects when the context covers what Core read.
#[test]
fn context_refusals() {
    let s = Setup::new();
    let q = s.hashes[MNHF_BASE];
    let seed = seed_of(MNHF_BASE);
    let above = |m: &MinedCommitment, first: usize| {
        s.hashes[first..].iter().any(|h| m.commitment[3..35] == h[..])
    };
    let window = |first: usize| {
        let mined: Vec<_> = s.all_mined().into_iter().filter(|m| above(m, first)).collect();
        // The fork header follows height 100: a window above it cannot hold it.
        let fork_headers = if first <= 100 {
            vec![s.fork]
        } else {
            vec![]
        };
        let block = BlockContext {
            fork_headers,
            mined: mined.clone(),
            ..s.block()
        };
        (s.context(first, TIP, ThresholdState::Active, &mined), block)
    };
    let context_err = |tx: &[u8], ctx: &Context, block: &BlockContext| {
        let err = s.check(tx, ctx, block, true).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Context, "{}", err.message);
    };

    // The headers from 100 hold the tip's median time past; an unknown quorum hash may be a
    // block below them.
    let (ctx, block) = window(100);
    context_err(&mnhf(1, V24_BIT, [0x42; 32], seed), &ctx, &block);
    context_err(&Unlock::new([0x42; 32], 1).tx(), &ctx, &block);
    // A fork block is known: the reject stands.
    assert_eq!(
        s.reason(&mnhf(1, V24_BIT, s.fork_hash, seed), &ctx, &block).as_deref(),
        Some("bad-mnhf-quorum-hash")
    );

    // Fewer headers than the median time past reads (11): refused once the signal passes its
    // trivial checks, a reject before.
    let (short, short_block) = window(115);
    context_err(&mnhf(1, V24_BIT, q, seed), &short, &short_block);
    assert_eq!(
        s.reason(&mnhf(0, V24_BIT, q, seed), &short, &short_block).as_deref(),
        Some("bad-mnhf-version")
    );

    // From 50 the node may have older platform quorums than the two it holds (72 and 96): a
    // quorum the scan does not reach is refused; from 40 the scan is full and it is too old.
    let (ctx, block) = window(50);
    context_err(&Unlock::new(s.hashes[60], 1).tx(), &ctx, &block);
    assert_eq!(s.reason(&Unlock::new(s.hashes[96], seed_of(96)).tx(), &ctx, &block), None);
    let (ctx, block) = window(40);
    let too_old = s.reason(&Unlock::new(s.hashes[60], 1).tx(), &ctx, &block);
    assert_eq!(too_old.as_deref(), Some("bad-assetunlock-too-old-quorum"));

    // A quorum based at the first header of a context above genesis: that header's parent is
    // the placeholder every context continues below it, not missing, so the commitment is
    // checked and seeded as a node with the whole chain stores it (no ORACLE_E_ARG for a
    // parentless base, which only genesis is).
    let (ctx, block) = window(72);
    assert!(block.mined.iter().any(|m| m.commitment[3..35] == s.hashes[72]));
    assert_eq!(s.reason(&Unlock::new(s.hashes[96], seed_of(96)).tx(), &ctx, &block), None);

    // Members of every quorum Core builds come from the context (D4).
    let full = s.full();
    let no_members = Context {
        quorums: full.quorums.iter().filter(|m| m.quorum_hash != s.hashes[72]).cloned().collect(),
        ..full.clone()
    };
    context_err(&Unlock::new(q, seed_of(96)).tx(), &no_members, &s.block());
}

/// Block contexts that would seed a state no node reaches are refused (`oracle.h`): each row
/// names the shim's refusal message.
#[test]
fn block_context_refusals() {
    let s = Setup::new();
    // The MNHF quorum based at 72, which the node has not mined: a commitment it may add. The
    // chain context holds its members.
    let extra = s.mined(LLMQ_TEST, 72);
    let ctx =
        s.context(0, TIP, ThresholdState::Active, &[s.all_mined(), vec![extra.clone()]].concat());
    let tx = Unlock::new(s.hashes[96], seed_of(96)).tx();
    let refused_in = |ctx: &Context, block: BlockContext, message: &str| {
        let err = s.check(&tx, ctx, &block, true).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Argument, "{message}: {}", err.message);
        assert!(err.message.contains(message), "{message}: {}", err.message);
    };
    let refused = |block: BlockContext, message: &str| refused_in(&ctx, block, message);
    let with_mined = |extra: MinedCommitment| BlockContext {
        mined: [s.all_mined(), vec![extra]].concat(),
        ..s.block()
    };
    assert_eq!(s.check(&tx, &ctx, &with_mined(extra.clone()), true), Ok(Verdict::Accept));
    let patched = |range: std::ops::Range<usize>, bytes: &[u8]| {
        let mut qc = extra.clone();
        qc.commitment.splice(range, bytes.iter().copied());
        with_mined(qc)
    };

    refused(
        BlockContext {
            fork_headers: vec![header([0x42; 32], 1)],
            ..s.block()
        },
        "does not follow",
    );
    refused(
        BlockContext {
            fork_headers: vec![s.headers[50]],
            ..s.block()
        },
        "already in the block index",
    );
    let mut garbage = extra.clone();
    garbage.commitment.truncate(40);
    refused(with_mined(garbage), "mined commitment");
    let mut trailing = extra.clone();
    trailing.commitment.push(0);
    refused(with_mined(trailing), "bytes after the commitment");
    refused(
        with_mined(MinedCommitment {
            llmq_type: LLMQ_TEST_PLATFORM,
            ..extra.clone()
        }),
        "not the record's",
    );
    let unconfigured = MinedCommitment {
        llmq_type: LLMQ_50_60,
        commitment: commitment(LLMQ_50_60, s.hashes[72], 1),
        mined_height: 84,
    };
    refused(with_mined(unconfigured), "not configured");
    // ProcessBlock(): the type enabled at the block before (ChainstateManager::IsQuorumTypeEnabled)
    // and DIP0003 active at the mining block (regtest's DIP0003 height without -dip3params).
    let disabled = MinedCommitment {
        llmq_type: LLMQ_TEST_INSTANTSEND,
        commitment: commitment(LLMQ_TEST_INSTANTSEND, s.hashes[72], 1),
        mined_height: 84,
    };
    refused(with_mined(disabled), "bad-qc-not-allowed");
    let no_indexes = BlockContext {
        unlock_indexes: UnlockIndexes::default(),
        ..with_mined(extra.clone())
    };
    let premature = s.check_on(&Chain::regtest(), &tx, &ctx, &no_indexes, true);
    let err = premature.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Argument, "{}", err.message);
    assert!(err.message.contains("bad-qc-premature"), "{}", err.message);
    let null = [&extra.commitment[..35], &[3, 0, 3, 0], &[0; 48], &[0; 32], &[0; 192]].concat();
    refused(
        with_mined(MinedCommitment {
            commitment: null,
            ..extra.clone()
        }),
        "null commitment",
    );
    refused(
        with_mined(MinedCommitment {
            commitment: commitment(LLMQ_TEST, s.fork_hash, 1),
            ..extra.clone()
        }),
        "not a context header on the chain",
    );
    for mined_height in [72, TIP as i32 + 1] {
        refused(
            with_mined(MinedCommitment {
                mined_height,
                ..extra.clone()
            }),
            "mined_height is not above the base",
        );
    }
    // ProcessCommitment() (blockprocessor.cpp): the mining window of LLMQ_TEST is blocks 10-18
    // of the DKG cycle, whose base is its first block.
    for mined_height in [72 + 9, 72 + 19] {
        refused(
            with_mined(MinedCommitment {
                mined_height,
                ..extra.clone()
            }),
            "mining window",
        );
    }
    refused(
        with_mined(MinedCommitment {
            mined_height: 96 + 12,
            ..extra.clone()
        }),
        "base block of mined_height's DKG cycle",
    );
    refused(
        with_mined(MinedCommitment {
            commitment: commitment(LLMQ_TEST, s.hashes[60], 1),
            ..extra.clone()
        }),
        "base block of mined_height's DKG cycle",
    );
    // CFinalCommitment::Verify() without members or signatures (commitment.cpp).
    // Versions that decode as version 3 does (not legacy, not indexed).
    for version in [0u16, 5] {
        refused(patched(QC_VERSION, &version.to_le_bytes()), &format!("nVersion {version}, not 3"));
    }
    refused(patched(QC_BITSETS, &[4, 0b111, 4, 0b111]), "bitset");
    refused(patched(QC_BITSETS, &[3, 0b111, 3, 0b001]), "valid members");
    refused(patched(QC_BITSETS, &[3, 0b001, 3, 0b111]), "signers");
    refused(patched(QC_PUBLIC_KEY, &[0; 48]), "quorumPublicKey");
    refused(patched(QC_VVEC_HASH, &[0; 32]), "quorumVvecHash");
    refused(patched(QC_QUORUM_SIG, &[0; 96]), "quorumSig");
    refused(patched(QC_MEMBERS_SIG, &[0; 96]), "membersSig");
    // ProcessBlock() with BLS checks (blockprocessor.cpp:274-284, commitment.cpp:36-110): the
    // signatures over the commitment hash, by the members and by the quorum key.
    let signatures = "signatures do not verify against its quorum's members (bad-qc-invalid)";
    let other_key = bls_key(0x77).sign_with(&[0x22; 32], BlsSigId::Basic).to_bytes();
    refused(patched(QC_QUORUM_SIG, &other_key), signatures);
    refused(patched(QC_MEMBERS_SIG, &other_key), signatures);
    // Signed by two of the three members, or by masternodes that are not its members.
    let with_qc = |commitment: Vec<u8>| {
        with_mined(MinedCommitment {
            commitment,
            ..extra.clone()
        })
    };
    let signed = |bits, signers: &[u8]| {
        with_qc(signed_commitment(LLMQ_TEST, s.hashes[72], 3, bits, signers, seed_of(72)))
    };
    for signers in [&MEMBERS[..2], &[0x41, 0x42, 0x43][..]] {
        refused(signed(3, signers), signatures);
    }
    // The fixture of ABI 13: three bits set and no members. Core's members are what it signs
    // against, so the members' signature cannot verify.
    let members_of_72 = |members: Vec<Vec<u8>>| {
        let mut ctx = ctx.clone();
        let q = ctx
            .quorums
            .iter_mut()
            .find(|q| q.llmq_type == LLMQ_TEST && q.quorum_hash == s.hashes[72]);
        q.unwrap().members = members;
        ctx
    };
    refused_in(&members_of_72(vec![]), with_mined(extra.clone()), signatures);
    // Its old signatures, over an arbitrary message, with the members.
    refused(
        with_qc([&extra.commitment[..QC_QUORUM_SIG.start], &other_key, &other_key].concat()),
        signatures,
    );
    // CFinalCommitment::Verify() (commitment.cpp:177-188): two members, three bits, signed by
    // the two (Core aggregates the signers below the member count).
    let two_members = members_of_72(member_dmns(&MEMBERS[..2]));
    refused_in(
        &two_members,
        signed(3, &MEMBERS[..2]),
        "a validMembers or signers bit is set at or beyond the quorum's member count (bad-qc-invalid)",
    );
    // Two members and two bits is a commitment Core stores.
    assert_eq!(s.check(&tx, &two_members, &signed(2, &MEMBERS[..2]), true), Ok(Verdict::Accept));
    // Members the context does not give: no verdict.
    let mut no_members = ctx.clone();
    no_members.quorums.retain(|q| q.quorum_hash != s.hashes[72] || q.llmq_type != LLMQ_TEST);
    let err = s.check(&tx, &no_members, &with_mined(extra.clone()), true).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Context, "{}", err.message);
    // A quorum based at genesis is parentless (commitment.cpp:122-128).
    refused(with_mined(s.mined_at(LLMQ_TEST, 0, 12)), "parentless quorum base block");
    // A second commitment of a quorum. (A non-rotated type mines one quorum per DKG cycle, so
    // two of its quorums at one height already fail the base rule.)
    refused(with_mined(s.mined(LLMQ_TEST_PLATFORM, 72)), "another commitment of its quorum");
    let below = s.context(30, TIP, ThresholdState::Active, &[]);
    let err = s.check(&tx, &below, &s.block(), true).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Argument, "a base below the headers: {}", err.message);

    // Asset-unlock indexes once V20 is active after the tip only (specialtxman.cpp:867-872).
    let indexes = BlockContext {
        unlock_indexes: UnlockIndexes::from_indexes([1]),
        ..BlockContext::default()
    };
    let before = s.context(0, V20_HEIGHT - 2, ThresholdState::Defined, &[]);
    let err = s.check(&tx, &before, &indexes, true).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Argument, "{}", err.message);
    assert!(err.message.contains("before V20"), "{}", err.message);
    let from = s.context(0, V20_HEIGHT - 1, ThresholdState::Defined, &[]);
    assert!(s.check(&tx, &from, &indexes, true).is_ok());

    // Core's CRangesSet deserializer: empty, overlapping, adjacent and unordered ranges.
    for ranges in [
        vec![(5, 5)],
        vec![(1, 4), (3, 6)],
        vec![(1, 3), (3, 5)],
        vec![(7, 9), (1, 3)],
        vec![(0, 0)],
    ] {
        let block = BlockContext {
            unlock_indexes: UnlockIndexes {
                ranges: ranges.clone(),
            },
            ..s.block()
        };
        refused(block, "block context");
    }
}

#[test]
fn unlock_indexes_are_canonical() {
    assert_eq!(
        UnlockIndexes::from_indexes([5, 1, 2, 3, 9, 2]).ranges,
        vec![(1, 4), (5, 6), (9, 10)]
    );
    assert_eq!(
        UnlockIndexes::from_indexes([u64::MAX, u64::MAX - 1]).ranges,
        vec![(u64::MAX - 1, 0)]
    );
    assert_eq!(UnlockIndexes::from_indexes([]).ranges, vec![]);
}

/// Core's trivially_valid/invalid vectors (src/test/data, evo_trivialvalidation.cpp's
/// contexts) through the mempool entry and the block path: the same verdicts.
#[test]
fn block_path_agrees_with_check_special_tx() {
    let oracle = oracle();
    let chain = Chain::new("regtest", &["-testactivationheight=v19@100", "-dip3params=2:2"]);
    let (all, _) = regtest_headers(&oracle, 101);
    let mut compared = 0;
    for file in ["trivially_valid.json", "trivially_invalid.json"] {
        for row in trivial_vectors(file) {
            let tip = if row[2] == "basic" {
                100
            } else {
                98
            };
            let tx = hex(&row[3]);
            for bls_legacy in [true, false] {
                let ctx = Context::new(
                    0,
                    all[..=tip].to_vec(),
                    bls_legacy,
                    vec![],
                    1_500_000_000,
                    Checkpoints::Disabled,
                    CoinsView::Unchecked(vec![]),
                    empty_list(),
                    vec![],
                );
                let mempool = oracle.check_special_tx(&chain, &tx, &ctx, true).map_err(|e| e.kind);
                let block = oracle
                    .check_special_tx_block(&chain, &tx, &ctx, &BlockContext::default(), true)
                    .map_err(|e| e.kind);
                assert_eq!(block, mempool, "{file} {}", row[0]);
                compared += 1;
            }
        }
    }
    assert!(compared >= 20, "{compared} vectors");
}
