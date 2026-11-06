//! Dash Core's BLS microbenchmarks (`src/bench/bls.cpp`, `src/bench/bls_dkg.cpp`, v24.0.0-rc.3)
//! ported onto dash-pkc's `bls::core` (PLAN BS11b D30).
//!
//! Each row keeps Core's name, sizes and per-iteration work: one closure call here is one call of
//! Core's `bench.run` closure. All of it runs under the basic scheme, which is what Core's bench
//! measures (`bls::bls_legacy_scheme` is false there) and mainnet's scheme since v19. Core's inputs
//! come from `GetRandHash` and `MakeNewKey`; here they come from a fixed LCG, so the instruction
//! counts PF0 compares are deterministic. The `bls_verify_*_legacy*` rows are not Core's: they
//! are BS11a-4 D9's legacy-flag verify rows (design D64), on legacy keys left as decoded or
//! resolved (`BlsValue::resolve`). A fixture is built on first use, so a filtered-out row
//! builds nothing, and all setup stays outside the measured closure, as it does outside Core's
//! `bench.run`.
//!
//! `bls::core` runs on the calling thread alone. Core's `CBLSWorker` pool (`DEFAULT_WORKER_COUNT`
//! threads) is not reproduced: the DKG rows measure Core's `parallel = false` compositions.
//! `BLS_Verify_Batched` takes 16 closure calls per batch, as in Core, so a run whose iteration
//! count is not a multiple of 16 ends part-way through a batch.

use std::convert::Infallible;
use std::hint::black_box;
use std::sync::LazyLock;

use criterion::{Criterion, criterion_group, criterion_main};
use dash_pkc::bls::core::{
    BlsPkValue, BlsSigValue, BlsSkValue, aggregate_public_keys_insecure_pair,
    aggregate_signatures_insecure, build_quorum_verification_vector, generate_contributions,
    make_new_key, verify_contribution_shares, verify_insecure, verify_insecure_aggregated,
    verify_secure_aggregated,
};
use dash_pkc::bls::{
    BlsPoint, BlsPublicKey, BlsScChia, BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSignature,
};
use rand_core::{TryCryptoRng, TryRng};

const FLAG: BlsSchemeKind = BlsSchemeKind::Basic;

/// `BuildTestVectors(1000, 10, ..)` of `BLS_Verify_Normal` and `BLS_Verify_Batched`.
const VERIFY_COUNT: usize = 1_000;
const VERIFY_INVALID: usize = 10;
/// `BLS_Verify_Batched`'s `batchSize`.
const BATCH_SIZE: usize = 16;
/// The DKG rows' quorum size (`_simple_100`, `_aggregated_100`) and the invalid shares per
/// iteration of the verify rows (`invalidCount` 5).
const QUORUM_SIZE: usize = 100;
const INVALID_SHARES: usize = 5;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }

    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        self.fill(&mut out);
        out
    }

    fn fill(&mut self, dst: &mut [u8]) {
        for chunk in dst.chunks_mut(8) {
            let word = self.next().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }

    /// An index below `n`, from the high bits.
    fn below(&mut self, n: usize) -> usize {
        ((self.next() >> 33) % n as u64) as usize
    }
}

/// The deterministic stand-in for the system RNG `generate_contributions` and `make_new_key`
/// take. It is not cryptographically secure and exists only for this bench.
struct DetRng(Lcg);

impl TryRng for DetRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        Ok((self.0.next() >> 32) as u32)
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        Ok(self.0.next())
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        self.0.fill(dst);
        Ok(())
    }
}

impl TryCryptoRng for DetRng {}

/// Core's `BuildTestVectors` outputs (without the secret keys, which only signing used).
struct Vectors {
    pks: Vec<BlsPkValue>,
    sigs: Vec<BlsSigValue>,
    hashes: Vec<[u8; 32]>,
    invalid: Vec<bool>,
}

/// `BuildTestVectors(count, invalid_count, ..)`: `invalid_count` flags set and shuffled, then for
/// each index a key, a message hash and the key's basic signature of it. A flagged index gets the
/// signature of another key instead. Signing is not in `bls::core`, so the typed API signs.
fn build_vectors(count: usize, invalid_count: usize, seed: u64) -> Vectors {
    let mut rng = Lcg(seed);
    let mut invalid = vec![false; count];
    invalid[..invalid_count].fill(true);
    for i in (1..count).rev() {
        invalid.swap(i, rng.below(i + 1));
    }

    let key = |rng: &mut Lcg| {
        BlsSecretKey::<BlsScIetf>::from_ikm(&rng.bytes::<32>()).expect("a 32-byte IKM")
    };
    let mut out = Vectors {
        pks: vec![],
        sigs: vec![],
        hashes: vec![],
        invalid,
    };
    for i in 0..count {
        let sk = key(&mut rng);
        let hash = rng.bytes::<32>();
        let signer = if out.invalid[i] {
            key(&mut rng)
        } else {
            sk.clone()
        };
        let sig = signer.sign_digest(&hash).to_bytes();
        let pk = sk.public_key().to_bytes();
        out.pks.push(BlsPkValue::from_canonical(&pk, FLAG).expect("a canonical key"));
        out.sigs.push(BlsSigValue::from_canonical(&sig, FLAG).expect("a canonical signature"));
        out.hashes.push(hash);
    }
    out
}

/// `BuildTestVectors(1000, 10, ..)`.
static MIXED: LazyLock<Vectors> =
    LazyLock::new(|| build_vectors(VERIFY_COUNT, VERIFY_INVALID, 0xb15));

/// `BuildTestVectors(1000, 0, ..)`.
static VALID: LazyLock<Vectors> = LazyLock::new(|| build_vectors(VERIFY_COUNT, 0, 0xb16));

/// A member's id: `WriteLE64(id.begin(), i + 1)` over a zeroed `uint256`.
fn member_id(i: usize) -> [u8; 32] {
    let mut id = [0; 32];
    id[..8].copy_from_slice(&(i as u64 + 1).to_le_bytes());
    id
}

/// One DKG member's contribution: its verification vector and the share it sends to each member.
struct Member {
    vvec: Vec<BlsPkValue>,
    sk_shares: Vec<BlsSkValue>,
}

/// Core's `DKG(quorumSize)`: every member's `GenerateContributions(quorumSize / 2 + 1, ids, ..)`.
struct Dkg {
    ids: Vec<[u8; 32]>,
    members: Vec<Member>,
}

static DKG: LazyLock<Dkg> = LazyLock::new(|| {
    let ids: Vec<[u8; 32]> = (0..QUORUM_SIZE).map(member_id).collect();
    let mut rng = DetRng(Lcg(0xd16));
    let members = ids
        .iter()
        .map(|_| {
            let (vvec, sk_shares) = generate_contributions(QUORUM_SIZE / 2 + 1, &ids, &mut rng)
                .expect("a threshold of at least two");
            Member {
                vvec,
                sk_shares,
            }
        })
        .collect();
    Dkg {
        ids,
        members,
    }
});

/// `ReceiveVvecs`: each member's verification vector.
fn received_vvecs(dkg: &Dkg) -> Vec<Option<&[BlsPkValue]>> {
    dkg.members.iter().map(|m| Some(m.vvec.as_slice())).collect()
}

/// `Bench_VerifyContributionShares`: one iteration receives member `who`'s shares, replaces
/// `INVALID_SHARES` random ones (possibly repeating an index) with new keys, verifies and checks
/// every answer; the next iteration is the next member.
macro_rules! verify_contribution_shares_row {
    ($group:expr, $name:literal, $aggregated:expr) => {
        $group.bench_function($name, |b| {
            let dkg = &*DKG;
            let vvecs = received_vvecs(dkg);
            let mut rng = DetRng(Lcg(0x5e1));
            let mut who = 0;
            b.iter(|| {
                let mut shares: Vec<BlsSkValue> =
                    dkg.members.iter().map(|m| m.sk_shares[who].clone()).collect();
                let mut invalid = Vec::with_capacity(INVALID_SHARES);
                for _ in 0..INVALID_SHARES {
                    let idx = rng.0.below(shares.len());
                    shares[idx] = make_new_key(&mut rng);
                    invalid.push(idx);
                }

                let refs: Vec<&BlsSkValue> = shares.iter().collect();
                let result =
                    verify_contribution_shares(&dkg.ids[who], &vvecs, &refs, $aggregated, FLAG);
                for (i, ok) in result.iter().enumerate() {
                    assert_eq!(*ok, !invalid.contains(&i));
                }
                who = (who + 1) % dkg.members.len();
            });
        });
    };
}

/// A legacy-flag verify's inputs: `count` legacy keys and their signatures of `hash`. One key's
/// signature is kept as made, for `verify_insecure`; more keys' are secure-aggregated, for
/// `verify_secure_aggregated`, which checks a commitment's members signature so
/// (`llmq/commitment.cpp:90`).
struct LegacyVectors {
    sig: BlsSigValue,
    pks: Vec<BlsPkValue>,
    hash: [u8; 32],
}

fn legacy_vectors(count: usize, seed: u64) -> LegacyVectors {
    let mut rng = Lcg(seed);
    let hash = rng.bytes::<32>();
    let sks: Vec<BlsSecretKey<BlsScChia>> = (0..count)
        .map(|_| BlsSecretKey::from_ikm(&rng.bytes::<32>()).expect("a 32-byte IKM"))
        .collect();
    let pks: Vec<BlsPublicKey<BlsScChia>> = sks.iter().map(BlsSecretKey::public_key).collect();
    let sigs: Vec<BlsSignature<BlsScChia>> = sks.iter().map(|sk| sk.sign_digest(&hash)).collect();
    let sig = match sigs.as_slice() {
        [sig] => *sig,
        _ => BlsSignature::secure_aggregate(
            &sigs.iter().collect::<Vec<_>>(),
            &pks.iter().collect::<Vec<_>>(),
        )
        .expect("a secure aggregate"),
    };
    let pks: Vec<BlsPkValue> = pks
        .iter()
        .map(|pk| BlsPkValue::from_canonical(&pk.to_bytes(), LEGACY).expect("a canonical key"))
        .collect();
    assert!(pks.iter().all(|pk| matches!(pk.point(), Some(BlsPoint::OnCurve(_)))));
    LegacyVectors {
        sig: BlsSigValue::from_canonical(&sig.to_bytes(), LEGACY).expect("a canonical signature"),
        pks,
        hash,
    }
}

const LEGACY: BlsSchemeKind = BlsSchemeKind::Legacy;

/// The legacy rows' fixtures: one key (the insecure verify) and the 50 and 400 of the secure one.
static LEGACY_1: LazyLock<LegacyVectors> = LazyLock::new(|| legacy_vectors(1, 0xb17));
static LEGACY_50: LazyLock<LegacyVectors> = LazyLock::new(|| legacy_vectors(50, 0xb18));
static LEGACY_400: LazyLock<LegacyVectors> = LazyLock::new(|| legacy_vectors(400, 0xb19));

/// `v`'s keys, resolved or as decoded.
fn legacy_keys(v: &LegacyVectors, resolved: bool) -> Vec<BlsPkValue> {
    let mut pks = v.pks.clone();
    if resolved {
        pks.iter_mut().for_each(BlsPkValue::resolve);
        assert!(pks.iter().all(|pk| matches!(pk.point(), Some(BlsPoint::Usable(_)))));
    }
    pks
}

/// A legacy-flag verify row, keys resolved or not: `verify_insecure` for one key, else
/// `verify_secure_aggregated`, checked valid on every call.
macro_rules! legacy_verify_row {
    ($group:expr, $name:literal, $fixture:expr, $resolved:expr) => {
        $group.bench_function($name, |b| {
            let v = &*$fixture;
            let pks = legacy_keys(v, $resolved);
            let refs: Vec<&BlsPkValue> = pks.iter().collect();
            b.iter(|| {
                let verdict = match refs.as_slice() {
                    [pk] => verify_insecure(&v.sig, pk, &v.hash, LEGACY),
                    _ => verify_secure_aggregated(&v.sig, &refs, &v.hash, LEGACY),
                };
                assert!(verdict.is_valid());
            });
        });
    };
}

fn bls_core(c: &mut Criterion) {
    let mut group = c.benchmark_group("bls_core");
    // The 1000-signature rows take seconds per iteration.
    group.sample_size(10);

    // bls.cpp:46-58. `pubKey1.AggregateInsecure(pubKey2)` adds in place, so the sum accumulates.
    group.bench_function("BLS_PubKeyAggregate_Normal", |b| {
        let vectors = &*VALID;
        let mut pk1 = vectors.pks[0].clone();
        let pk2 = &vectors.pks[1];
        b.iter(|| {
            pk1 = aggregate_public_keys_insecure_pair(&pk1, pk2);
        });
    });

    // bls.cpp:100-122: one verify per iteration, cycling over the 1000 and checking the verdict
    // against the invalid flags.
    group.bench_function("BLS_Verify_Normal", |b| {
        let v = &*MIXED;
        let mut i = 0;
        b.iter(|| {
            let valid = verify_insecure(&v.sigs[i], &v.pks[i], &v.hashes[i], FLAG).is_valid();
            assert_eq!(valid, !v.invalid[i]);
            i = (i + 1) % v.pks.len();
        });
    });

    // bls.cpp:125-151: every signature of 1000 valid ones verified alone, per iteration.
    group.bench_function("BLS_Verify_LargeBlock1000", |b| {
        let v = &*VALID;
        b.iter(|| {
            for i in 0..v.pks.len() {
                assert!(verify_insecure(&v.sigs[i], &v.pks[i], &v.hashes[i], FLAG).is_valid());
            }
        });
    });

    // bls.cpp:180-206: the aggregate of 1000 valid signatures over 1000 distinct messages, made
    // in setup, verified per iteration.
    group.bench_function("BLS_Verify_LargeAggregatedBlock1000", |b| {
        let v = &*VALID;
        let sigs: Vec<&BlsSigValue> = v.sigs.iter().collect();
        let agg = aggregate_signatures_insecure(&sigs);
        let pks: Vec<&BlsPkValue> = v.pks.iter().collect();
        b.iter(|| {
            assert!(verify_insecure_aggregated(&agg, &pks, &v.hashes, FLAG).is_valid());
        });
    });

    // bls.cpp:254-308: Core's loop, not `CBLSBatchVerifier`. Only every 16th call works: the next
    // 16 inputs are aggregated and verified at once, and if the aggregate fails each is verified
    // alone. The batch then checks every answer against the invalid flags.
    group.bench_function("BLS_Verify_Batched", |b| {
        let v = &*MIXED;
        let (mut i, mut j) = (0, 0);
        b.iter(|| {
            j += 1;
            if j % BATCH_SIZE != 0 {
                return;
            }

            let start = i;
            let mut pks = Vec::with_capacity(BATCH_SIZE);
            let mut sigs = Vec::with_capacity(BATCH_SIZE);
            let mut hashes = Vec::with_capacity(BATCH_SIZE);
            for _ in 0..BATCH_SIZE {
                pks.push(&v.pks[i]);
                sigs.push(&v.sigs[i]);
                hashes.push(v.hashes[i]);
                i = (i + 1) % v.pks.len();
            }

            let batch_sig = aggregate_signatures_insecure(&sigs);
            let batch_valid =
                verify_insecure_aggregated(&batch_sig, &pks, &hashes, FLAG).is_valid();
            let valid: Vec<bool> = if batch_valid {
                vec![true; BATCH_SIZE]
            } else {
                (0..BATCH_SIZE)
                    .map(|k| verify_insecure(sigs[k], pks[k], &hashes[k], FLAG).is_valid())
                    .collect()
            };
            for (k, ok) in valid.iter().enumerate() {
                assert_eq!(*ok, !v.invalid[(start + k) % v.pks.len()]);
            }
        });
    });

    // bls_dkg.cpp:83-90, BENCH_BuildQuorumVerificationVectors(simple, 100, 10): the members'
    // vectors received in setup, summed per iteration.
    group.bench_function("BLSDKG_BuildQuorumVerificationVectors_simple_100", |b| {
        let vvecs = received_vvecs(&DKG);
        b.iter(|| {
            black_box(build_quorum_verification_vector(black_box(&vvecs), FLAG))
                .expect("a quorum vector")
        });
    });

    // bls_dkg.cpp:113-135, BENCH_GenerateContributions(simple, 100, 5): every one of 100 members
    // generates its contribution to the 100 ids, with threshold 100 / 2 + 1.
    group.bench_function("BLSDKG_GenerateContributions_simple_100", |b| {
        let ids: Vec<[u8; 32]> = (0..QUORUM_SIZE).map(member_id).collect();
        let mut rng = DetRng(Lcg(0x6e4));
        b.iter(|| {
            for _ in 0..QUORUM_SIZE {
                black_box(
                    generate_contributions(QUORUM_SIZE / 2 + 1, &ids, &mut rng)
                        .expect("two or more"),
                );
            }
        });
    });

    // bls_dkg.cpp:92-110, BENCH_VerifyContributionShares(simple, 100, 5, false, 10) and
    // (aggregated, 100, 5, true, 10).
    verify_contribution_shares_row!(group, "BLSDKG_VerifyContributionShares_simple_100", false);
    verify_contribution_shares_row!(group, "BLSDKG_VerifyContributionShares_aggregated_100", true);

    // BS11a-4 D9: verifies under the legacy flag, whose keys Core never tests for the subgroup.
    legacy_verify_row!(group, "bls_verify_insecure_legacy", LEGACY_1, false);
    legacy_verify_row!(group, "bls_verify_insecure_legacy_resolved", LEGACY_1, true);
    legacy_verify_row!(group, "bls_verify_secure_legacy_50", LEGACY_50, false);
    legacy_verify_row!(group, "bls_verify_secure_legacy_50_resolved", LEGACY_50, true);
    legacy_verify_row!(group, "bls_verify_secure_legacy_400", LEGACY_400, false);
    legacy_verify_row!(group, "bls_verify_secure_legacy_400_resolved", LEGACY_400, true);
    group.finish();
}

criterion_group!(benches, bls_core);
criterion_main!(benches);
