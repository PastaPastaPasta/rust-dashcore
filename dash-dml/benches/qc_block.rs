//! The PF0 row of the block processor (PLAN LQ2 BG, "crit commitment verification per block"; LQ2
//! D18): `process_block` over a real rotated mining block, mainnet 2,089,770, which mines all 32
//! LLMQ_60_75 commitments of the cycle at 2,089,728 (`data/mainnet-qc-2089770.bin`, cut from the
//! Core-produced lq1-rot capture by `data/make_qc_block.py`).
//!
//! `qc_block/mainnet_2089770` (`crit:qc_block`) connects the block with `fBLSChecks`: the count
//! checks are skipped (crash replay, LQ2 D5, since the fixture holds no mined index), so each
//! iteration is the v24 duplicate pre-pass, the 32 commitments' queued signature checks (Core's
//! check-queue work) and their `ProcessCommitment` (`Verify(checkSigs = false)` and the rows).
//! The members are Core's (`CalculateQuorum` over the cycle's lists, from the capture), so the
//! precompute is a no-op. The bench checks first that the block connects with 32 rows.
//!
//! `qc_root/*` (`crit:qc_root`; LQ2 D22, DEC-325, DEC-341) is the CbTx v2 `merkleRootQuorums` of the
//! next block, 2,089,771, with no commitment of its own, over mainnet's whole active set: the 32
//! rows written, and for every other configured type its `signingActiveQuorumCount` commitments of
//! its size (`other_types`). `quorum_root` at 2,089,770 through a `QcHashCache`. `unchanged` reads
//! the same active set as the last root, so its hashes are the cache's; `mined` drops the cache
//! first, as connecting a block that mines commitments does (`DropQcHashesCache`), so every
//! commitment is read and hashed.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::primitives::{Block, core_serialize, decode_block};
use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::blockprocessor::pool::null_commitment;
use dash_dml::blockprocessor::{
    BlockLookup, QcBlockAt, QcCtx, QcHashCache, QcMode, process_block, qc_hashes, quorum_root,
};
use dash_dml::store::{EvoDelta, MemEvoStore, MinedCommitmentRecord, StoreError};
use dash_dml::testkit::replay::{Headers, Members, ReplayQuorum, mainnet};
use dash_evo::EffectSink;
use dash_evo::bls_ctx_after;
use dash_evo::llmq::{
    CompleteChain, LlmqCommitment, LlmqCtx, MemberError, PrecomputeMode, QuorumPrecompute,
    SerialBlsChecks, commitment_version,
};
use dash_primitives::{BlockHash, QuorumHash, Transaction, TxType};
use dash_types::Numeric;

const DATA: &[u8] = include_bytes!("data/mainnet-qc-2089770.bin");
const LLMQ_60_75: u8 = 5;

/// The fixture: headers from `first`, the mining block's height, the masternodes, and per quorum
/// its base height, members (indexes into the masternodes) and commitment.
struct Fixture {
    first: u32,
    headers: Vec<[u8; 80]>,
    mined: u32,
    quorums: Vec<(u32, Vec<Vec<u8>>, Vec<u8>)>,
}

fn read(data: &mut &[u8], n: usize) -> Vec<u8> {
    let (head, tail) = data.split_at(n);
    *data = tail;
    head.to_vec()
}

fn u32_le(data: &mut &[u8]) -> u32 {
    u32::from_le_bytes(read(data, 4).try_into().expect("four bytes"))
}

fn decode(mut data: &[u8]) -> Fixture {
    assert_eq!(read(&mut data, 8), b"lq2qcb01");
    let (first, n_headers, mined) = (u32_le(&mut data), u32_le(&mut data), u32_le(&mut data));
    let headers =
        (0..n_headers).map(|_| read(&mut data, 80).try_into().expect("a header")).collect();
    let mns: Vec<Vec<u8>> = (0..u32_le(&mut data))
        .map(|_| {
            let n = u32_le(&mut data) as usize;
            read(&mut data, n)
        })
        .collect();
    let quorums = (0..u32_le(&mut data))
        .map(|_| {
            let base = u32_le(&mut data);
            let members =
                (0..u32_le(&mut data)).map(|_| mns[u32_le(&mut data) as usize].clone()).collect();
            let n = u32_le(&mut data) as usize;
            (base, members, read(&mut data, n))
        })
        .collect();
    assert!(data.is_empty());
    Fixture {
        first,
        headers,
        mined,
        quorums,
    }
}

/// Buried deployments at their mainnet heights, `BlockId` counting from `first`; no versionbits
/// deployment is active.
struct Buried {
    first: u32,
}

impl DeploymentView for Buried {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let next = prev.map_or(0, |p| self.first + p.0 + 1);
        match deployment {
            Deployment::Buried(d) => next >= mainnet().consensus.deployment_height(d).to_u32(),
            Deployment::Versionbits(_) => false,
        }
    }

    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_after(block.0.checked_sub(1).map(BlockId), deployment)
    }
}

struct Lookup(BTreeMap<[u8; 32], (BlockId, u32)>);

impl BlockLookup for Lookup {
    fn lookup(&self, hash: &BlockHash) -> Option<(BlockId, u32)> {
        self.0.get(hash.as_bytes()).copied()
    }
}

/// Core's members are given: nothing to precompute.
struct Given;

impl QuorumPrecompute for Given {
    type Error = StoreError;

    fn precompute(
        &self,
        _: BlockId,
        _: PrecomputeMode,
        _: &mut dyn EffectSink,
    ) -> Result<(), MemberError<StoreError>> {
        Ok(())
    }
}

/// The mining block: a coinbase and one quorum-commitment transaction per commitment.
fn block(height: u32, commitments: &[Vec<u8>]) -> Block {
    let tx = |version, tx_type, extra_payload| {
        core_serialize(&Transaction {
            version,
            tx_type,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            extra_payload,
        })
    };
    let mut raw = vec![0; 80];
    raw.push(u8::try_from(commitments.len() + 1).expect("a one-byte count"));
    raw.extend(tx(1, TxType::Spend, vec![]));
    for qc in commitments {
        let payload = [&1u16.to_le_bytes()[..], &height.to_le_bytes(), qc].concat();
        raw.extend(tx(3, TxType::QuorumCommitment, payload));
    }
    decode_block(&mut &raw[..]).expect("a block").into_value()
}

/// The rest of mainnet's active set at 2,089,770 beside the 32 LLMQ_60_75 rows: for each other
/// configured type, `signingActiveQuorumCount` non-rotated commitments (version 3, as V19 makes
/// them) of the type's size, every member a signer and valid, with `like`'s BLS fields. The
/// fixture holds no older headers or commitments, so the bases are every 8th block from the
/// fixture's first and each is mined `1 + j` blocks later for the `j`th type: a root's cost is
/// the rows' reads, encodings and hashes, wherever the blocks are. The records by mining height.
fn other_types(
    headers: &Headers,
    first: u32,
    like: &LlmqCommitment,
) -> Vec<(u32, MinedCommitmentRecord)> {
    let mut out = Vec::new();
    let others = mainnet().consensus.llmqs.iter().filter(|p| p.llmq_type.to_base() != LLMQ_60_75);
    for (j, params) in (0u32..).zip(others) {
        for k in 0..u32::try_from(params.signing_active_quorum_count).expect("a count") {
            let base = 8 * k;
            let hash = QuorumHash::from(*headers.header(BlockId(base)).hash.as_bytes());
            let mut qc = null_commitment(params, hash, commitment_version(false, true), 0);
            for bits in [&mut qc.signers, &mut qc.valid_members] {
                bits.data.fill(0xff);
                if bits.num_bits % 8 != 0 {
                    *bits.data.last_mut().expect("a byte") >>= 8 - bits.num_bits % 8;
                }
            }
            qc.quorum_public_key = like.quorum_public_key.clone();
            qc.quorum_vvec_hash = like.quorum_vvec_hash;
            qc.quorum_sig = like.quorum_sig.clone();
            qc.members_sig = like.members_sig.clone();
            let record = MinedCommitmentRecord {
                llmq_type: params.llmq_type,
                quorum_hash: hash,
                base_height: first + base,
                rotation_index: None,
                commitment: Arc::new(qc),
            };
            out.push((first + base + 1 + j, record));
        }
    }
    out.sort_by_key(|(mined, _)| *mined);
    out
}

fn qc_block(c: &mut Criterion) {
    let f = decode(DATA);
    let headers = Headers::new(f.first as i32, &f.headers).expect("the headers");
    let lookup = Lookup(
        (0..f.headers.len() as u32)
            .map(|i| (*headers.header(BlockId(i)).hash.as_bytes(), (BlockId(i), f.first + i)))
            .collect(),
    );
    let replay: Vec<ReplayQuorum> = f
        .quorums
        .iter()
        .map(|(base, members, _)| ReplayQuorum {
            llmq_type: LLMQ_60_75,
            quorum_hash: *headers.header(BlockId(base - f.first)).hash.as_bytes(),
            members: members.clone(),
        })
        .collect();
    let members = Members::new(&headers, &replay).expect("the members");
    let commitments: Vec<Vec<u8>> = f.quorums.iter().map(|(_, _, qc)| qc.clone()).collect();
    let block = block(f.mined, &commitments);
    let at = QcBlockAt {
        id: BlockId(f.mined - f.first),
        height: f.mined,
    };
    let deployments = Buried {
        first: f.first,
    };
    let chain = CompleteChain::<StoreError>::new(&headers);
    let bls = bls_ctx_after(&deployments, Some(BlockId(at.id.0 - 1)));
    let llmq = LlmqCtx::new(mainnet(), &chain, &deployments, bls).expect("in domain");
    let store = MemEvoStore::new();
    let mined = store.view(&headers);
    let ctx = QcCtx {
        llmq,
        index: &lookup,
        mined: &mined,
        members: &members,
        precompute: &Given,
        bls_checks: &SerialBlsChecks,
    };
    let mode = QcMode {
        just_check: false,
        bls_checks: true,
    };
    let connect = || process_block(&ctx, &block, at, None, mode, &mut Vec::new());
    let records = connect().expect("the block connects");
    assert_eq!(records.len(), 32, "a row per commitment");

    let mut group = c.benchmark_group("qc_block");
    group.sample_size(10);
    group.bench_function("mainnet_2089770", |b| b.iter(|| black_box(connect())));
    group.finish();

    let others = other_types(&headers, f.first, &records[0].commitment);
    let mut written = MemEvoStore::new();
    let deltas = others.into_iter().map(|(height, record)| (height, vec![record]));
    for (height, mined_commitments) in deltas.chain([(f.mined, records)]) {
        written
            .apply(&EvoDelta {
                block: headers.header(BlockId(height - f.first)).hash,
                height,
                dml: None,
                mined_commitments,
                credit_data: None,
                credit_pool: None,
                mnhf_signals: None,
                erased_mined: Vec::new(),
                evo_tip: None,
            })
            .expect("the rows");
    }
    let rows = written.view(&headers);
    let hashes = qc_hashes(&ctx.llmq, &rows, &mut QcHashCache::new(), at.id).expect("no fault");
    let active = hashes.plain.values().map(Vec::len).sum::<usize>()
        + hashes.indexed.values().map(BTreeMap::len).sum::<usize>();
    let configured: usize = mainnet()
        .consensus
        .llmqs
        .iter()
        .map(|p| usize::try_from(p.signing_active_quorum_count).expect("a count"))
        .sum();
    assert_eq!(active, configured, "mainnet's whole active set");
    let next = self::block(f.mined + 1, &[]);
    let mut cache = QcHashCache::new();
    let root = |cache: &mut QcHashCache| {
        quorum_root(&ctx.llmq, &rows, cache, &next, at.id).expect("no fault").expect("a root")
    };
    assert_eq!(root(&mut cache), root(&mut QcHashCache::new()), "the cache is transparent");
    let mut group = c.benchmark_group("qc_root");
    group.bench_function("mainnet_2089771_unchanged", |b| b.iter(|| black_box(root(&mut cache))));
    group.bench_function("mainnet_2089771_mined", |b| {
        b.iter(|| {
            cache.clear();
            black_box(root(&mut cache))
        })
    });
    group.finish();
}

criterion_group!(benches, qc_block);
criterion_main!(benches);
