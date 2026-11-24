//! The mempool's `CheckSpecialTx` over V's LLMQ reads ([`check_special_tx_at`]): an asset unlock
//! against platform quorums mined on the active chain, with a side branch, per-height
//! deployments and injectable store faults.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use dash_consensus::ctx::{ChainAncestry, Deployment, DeploymentView};
use dash_consensus::hash::sha256d;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_consensus::{BlockId, Tx, TxRef};
use dash_dml::blockprocessor::BlockLookup;
use dash_dml::netinfo::NetInfoRules;
use dash_dml::specialtx::reasons::{
    BAD_ASSETUNLOCK_NOT_VERIFIED, BAD_ASSETUNLOCK_QUORUM_HASH, BAD_ASSETUNLOCK_TOO_OLD_QUORUM,
    BAD_TX_TYPE_DIP3_INACTIVE, FAILED_CHECK_SPECIAL_TX,
};
use dash_dml::testkit::replay::{CORE_NET_INFO, mainnet};
use dash_dml::testkit::{MemChain, operator_key, operator_sig};
use dash_dml::{MnList, StoreError};
use dash_evo::llmq::{
    BlsSigValue, CompleteChain, LlmqCommitment, LlmqCtx, MinedCommitment, MinedCommitmentReader,
    MinedFamily, MinedIndexEntry,
};
use dash_evo::locks::sign_hash;
use dash_params::{BuriedDeployment, ChainSetup};
use dash_pkc::bls::{BlsCtx, BlsEagerPk, BlsEagerSig, BlsGlobalSig, BlsSchemeKind};
use dash_primitives::{
    AssetUnlock, BlockHash, DynBitset, QuorumHash, QuorumVvecHash, Transaction, TxOut, TxType,
};
use dash_types::codec::BaseCodec;
use dashd_chainstate::chain::Fatal;
use dashd_chainstate::connect::SpecialTxRules;

use super::{ChainInputs, MempoolQuorums, check_special_tx_at};
use crate::accept::view::InputView;

fn params() -> &'static ChainSetup {
    mainnet()
}

/// The main chain's length, its first quorum base and the quorum count: mainnet's platform type
/// (`LLMQ_100_67`, `dkgInterval` 24) has a quorum based at every multiple of 24 from 720, each
/// mined 12 blocks later under key `base / 24`.
const MAIN: u32 = 1_300;
const FIRST_BASE: u32 = 720;
const QUORUMS: u32 = 24;
const MINED_AFTER: u32 = 12;

/// The active chain up to `tip` and a side branch forked at 1000, 60 blocks long.
struct Fixture {
    chain: MemChain,
    main: BlockId,
    side: BlockId,
    tip: BlockId,
}

impl Fixture {
    fn new(tip: u32) -> Fixture {
        let mut chain = MemChain::new();
        let main = chain.extend(chain.genesis(), MAIN);
        let fork = chain.ancestor(main, 1_000).expect("main chain");
        let side = chain.extend(fork, 60);
        let tip = chain.ancestor(main, tip).expect("main chain");
        Fixture {
            chain,
            main,
            side,
            tip,
        }
    }

    fn at(&self, height: u32) -> BlockId {
        self.chain.ancestor(self.tip, height).expect("below the tip")
    }

    fn quorum_hash(&self, block: BlockId) -> QuorumHash {
        QuorumHash::from(*self.chain.hash(block).as_bytes())
    }

    /// The quorum based at `base` on the main chain, and its key.
    fn main_quorum(&self, base: u32) -> (QuorumHash, u8) {
        let block = self.chain.ancestor(self.main, base).expect("main chain");
        (self.quorum_hash(block), (base / 24) as u8)
    }

    /// A version-2 unlock of index 7 requested at `requested`, naming `quorum` and signed by
    /// `key` (basic scheme) over Core's `SignHash` (`evo/assetlocktx.cpp:144-147`).
    fn unlock(&self, requested: u32, quorum: QuorumHash, key: u8) -> TxRef {
        let p = unlock_payload(requested, quorum, [0; 96]);
        let msg = sha256d(&p.sig_hash_bytes(unlock_tx(&p).raw()));
        let hash = sign_hash(
            params().consensus.llmq_type_platform,
            &quorum,
            p.request_id().as_bytes(),
            &msg,
        );
        let sig = operator_sig(key, &hash).to_bytes(BlsSchemeKind::Basic);
        unlock_tx(&unlock_payload(requested, quorum, sig))
    }
}

fn unlock_payload(requested_height: u32, quorum: QuorumHash, sig: [u8; 96]) -> AssetUnlock {
    AssetUnlock::new(
        2,
        7,
        1_000,
        requested_height,
        quorum,
        BlsGlobalSig::decode(&sig).expect("decodes"),
    )
}

fn unlock_tx(p: &AssetUnlock) -> TxRef {
    let mut payload = Vec::new();
    p.encode(&mut payload);
    let script = [&[0x76, 0xa9, 0x14][..], &[9; 20], &[0x88, 0xac]].concat();
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::AssetUnlock,
        vec![],
        vec![TxOut::from_duffs(1_000, script.into())],
        0,
        payload,
    )))
}

/// A provider revocation whose payload is never reached: only its type matters here.
fn pro_up_rev() -> TxRef {
    Arc::new(Tx::new(Transaction::new(
        3,
        TxType::ProviderUpdateRevoke,
        vec![],
        vec![TxOut::from_duffs(1_000, vec![0x51].into())],
        0,
        vec![0; 4],
    )))
}

/// Buried deployments by activation height (active for blocks at or above it), read on `chain`.
struct Heights<'a> {
    chain: &'a MemChain,
    dip3: u32,
    dip20: u32,
    v19: u32,
}

impl Heights<'_> {
    fn gate(&self, d: Deployment) -> u32 {
        match d {
            Deployment::Buried(BuriedDeployment::Dip0003) => self.dip3,
            Deployment::Buried(BuriedDeployment::Dip0020) => self.dip20,
            Deployment::Buried(BuriedDeployment::V19) => self.v19,
            _ => 0,
        }
    }
}

impl DeploymentView for Heights<'_> {
    fn active_after(&self, prev: Option<BlockId>, d: Deployment) -> bool {
        prev.is_some_and(|p| self.chain.header(p).height + 1 >= self.gate(d))
    }
    fn active_at(&self, block: BlockId, d: Deployment) -> bool {
        self.chain.header(block).height >= self.gate(d)
    }
}

/// `LookupBlockIndex`: every header the fixture made, main chain or side branch.
struct AnyHeader<'a>(&'a Fixture);

impl BlockLookup for AnyHeader<'_> {
    fn lookup(&self, hash: &BlockHash) -> Option<(BlockId, u32)> {
        let f = self.0;
        let on = |end: BlockId| {
            (0..=f.chain.header(end).height)
                .filter_map(move |h| f.chain.ancestor(end, h))
                .find(|&id| f.chain.hash(id) == *hash)
        };
        on(f.main).or_else(|| on(f.side)).map(|id| (id, f.chain.header(id).height))
    }
}

/// The platform quorums of [`Fixture`], mined on the main chain only; `fault` fails every read.
struct Mined<'a> {
    fixture: &'a Fixture,
    fault: Option<StoreError>,
    reads: AtomicU32,
}

impl<'a> Mined<'a> {
    fn new(fixture: &'a Fixture) -> Mined<'a> {
        Mined {
            fixture,
            fault: None,
            reads: AtomicU32::new(0),
        }
    }

    fn bases(&self) -> impl Iterator<Item = u32> {
        (0..QUORUMS).map(|i| FIRST_BASE + 24 * i)
    }

    fn check(&self) -> Result<(), StoreError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.fault.clone().map_or(Ok(()), Err)
    }

    fn commitment(&self, base: u32) -> LlmqCommitment {
        let none = DynBitset::new(0, Vec::new()).expect("the empty bitset");
        let (hash, key) = self.fixture.main_quorum(base);
        let pk = operator_key(key, BlsSchemeKind::Basic).get();
        let null_sig = || BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic);
        LlmqCommitment::new(
            3,
            params().consensus.llmq_type_platform,
            hash,
            None,
            none.clone(),
            none,
            BlsEagerPk::new(pk, BlsSchemeKind::Basic),
            QuorumVvecHash::from([0x11; 32]),
            null_sig(),
            null_sig(),
        )
    }

    /// Whether `at` is on the main chain at or above `height`.
    fn on_main_through(&self, at: BlockId, height: u32) -> bool {
        let f = self.fixture;
        let h = f.chain.header(at).height;
        h >= height && f.chain.ancestor(f.main, h) == Some(at)
    }
}

impl MinedCommitmentReader for Mined<'_> {
    type Error = StoreError;

    fn chain(&self) -> &dyn ChainAncestry {
        &self.fixture.chain
    }

    fn mined_commitment(
        &self,
        llmq_type: dash_params::LlmqType,
        quorum_hash: &QuorumHash,
        at: BlockId,
    ) -> Result<Option<MinedCommitment>, StoreError> {
        self.check()?;
        assert_eq!(llmq_type, params().consensus.llmq_type_platform);
        let found = self.bases().find(|&base| {
            self.fixture.main_quorum(base).0 == *quorum_hash
                && self.on_main_through(at, base + MINED_AFTER)
        });
        Ok(found.map(|base| {
            let mined = self.fixture.chain.ancestor(self.fixture.main, base + MINED_AFTER);
            (Arc::new(self.commitment(base)), self.fixture.chain.hash(mined.expect("main chain")))
        }))
    }

    fn mined_index(
        &self,
        llmq_type: dash_params::LlmqType,
        family: MinedFamily,
        chain_end: BlockId,
        max_mined_height: u32,
        limit: usize,
    ) -> Result<Vec<MinedIndexEntry>, StoreError> {
        self.check()?;
        assert_eq!(llmq_type, params().consensus.llmq_type_platform);
        assert_eq!(family, MinedFamily::NonRotated);
        let cap = max_mined_height.min(self.fixture.chain.header(chain_end).height);
        let mut rows: Vec<_> = self
            .bases()
            .filter(|&base| {
                (1..=cap).contains(&(base + MINED_AFTER))
                    && self.on_main_through(chain_end, base + MINED_AFTER)
            })
            .map(|base| MinedIndexEntry {
                mined_height: base + MINED_AFTER,
                base_height: base,
            })
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.mined_height));
        rows.truncate(limit);
        Ok(rows)
    }
}

/// One check of `tx` at `pindex`, with the tip's scheme `tip_bls`.
struct Check<'a> {
    fixture: &'a Fixture,
    deployments: Heights<'a>,
    mined: Mined<'a>,
    tip_bls: BlsCtx,
    list: Result<Arc<MnList>, StoreError>,
    lists_read: AtomicU32,
}

impl<'a> Check<'a> {
    /// Everything active from height 1, the tip's scheme basic.
    fn new(fixture: &'a Fixture) -> Check<'a> {
        Check {
            fixture,
            deployments: Heights {
                chain: &fixture.chain,
                dip3: 1,
                dip20: 1,
                v19: 1,
            },
            mined: Mined::new(fixture),
            tip_bls: BlsCtx::BASIC,
            list: Ok(Arc::new(MnList::default_pre_dip3())),
            lists_read: AtomicU32::new(0),
        }
    }

    fn run(&self, tx: &Tx, pindex: BlockId) -> Result<Result<(), TxValidationError>, Fatal> {
        let p = params();
        let complete = CompleteChain::<StoreError>::new(&self.fixture.chain);
        let llmq = LlmqCtx::new(p, &complete, &self.deployments, self.tip_bls).expect("in domain");
        let index = AnyHeader(self.fixture);
        let inputs = ChainInputs {
            params: p,
            deployments: &self.deployments,
            ancestry: &self.fixture.chain,
            tip_legacy: self.tip_bls.is_legacy(),
            net_info: NetInfoRules::new(p, mainnet(), CORE_NET_INFO),
        };
        let list = || {
            self.lists_read.fetch_add(1, Ordering::Relaxed);
            self.list.clone()
        };
        let quorums = MempoolQuorums {
            ctx: &llmq,
            mined: &self.mined,
            index: &index,
            tip: self.fixture.tip,
        };
        let coins = InputView::default();
        let coins_fault = |e: Infallible| match e {};
        check_special_tx_at(
            &inputs,
            tx,
            pindex,
            SpecialTxRules {
                v24: true,
                ..Default::default()
            },
            &list,
            &coins,
            quorums,
            &coins_fault,
        )
    }

    fn verdict(&self, tx: &Tx, pindex: BlockId) -> Result<(), (TxValidationResult, String)> {
        self.run(tx, pindex)
            .expect("no fault")
            .map_err(|e| (e.result(), e.reject_reason().to_owned()))
    }
}

fn rejected(result: TxValidationResult, reason: &str) -> Result<(), (TxValidationResult, String)> {
    Err((result, reason.to_owned()))
}

/// Condition (a): Core verifies an unlock's quorum signature under the process-global
/// `bls_legacy_scheme`, which follows the tip (`evo/specialtxman.cpp:1043-1047`, `:1068-1072`),
/// whichever window end `CheckSpecialTxForMempool` checks at (`validation.cpp:827-836`); the
/// check's own `pindexPrev` only places the quorum window (`evo/assetlocktx.cpp:115-152`). A
/// basic-scheme signature checked at a block before V19 is admitted under a basic tip, and the
/// same check under a legacy tip fails.
#[test]
fn tip_scheme_holds_across_a_scheme_change_in_the_window() {
    let fixture = Fixture::new(1_200);
    let mut check = Check::new(&fixture);
    check.deployments.v19 = 1_100;
    let (quorum, key) = fixture.main_quorum(1_032);
    let tx = fixture.unlock(1_050, quorum, key);
    let pindex = fixture.at(1_050);
    assert!(
        !check.deployments.active_after(Some(pindex), Deployment::Buried(BuriedDeployment::V19))
    );
    assert!(
        check
            .deployments
            .active_after(Some(fixture.tip), Deployment::Buried(BuriedDeployment::V19))
    );

    assert_eq!(check.verdict(&tx, pindex), Ok(()));

    check.tip_bls = BlsCtx::LEGACY;
    assert_eq!(
        check.verdict(&tx, pindex),
        rejected(TxValidationResult::Consensus, BAD_ASSETUNLOCK_NOT_VERIFIED)
    );
}

/// Condition (b): `LookupBlockIndex` finds any known header (`evo/assetlocktx.cpp:218-220`), so a
/// quorum hash on a side branch passes it and fails the active-chain scan as
/// `bad-assetunlock-too-old-quorum`, `TX_BAD_SPECIAL` (`:125-130`); only a hash no header has is
/// `bad-assetunlock-quorum-hash`, `TX_CONSENSUS`.
#[test]
fn side_chain_quorum_hash_is_bad_special() {
    let fixture = Fixture::new(1_200);
    let check = Check::new(&fixture);
    let side_base = fixture.chain.ancestor(fixture.side, 1_032).expect("side branch");
    let side_quorum = fixture.quorum_hash(side_base);
    let (_, key) = fixture.main_quorum(1_032);
    let pindex = fixture.at(1_100);

    let tx = fixture.unlock(1_090, side_quorum, key);
    assert_eq!(
        check.verdict(&tx, pindex),
        rejected(TxValidationResult::BadSpecial, BAD_ASSETUNLOCK_TOO_OLD_QUORUM)
    );

    let tx = fixture.unlock(1_090, QuorumHash::from([0xee; 32]), key);
    assert_eq!(
        check.verdict(&tx, pindex),
        rejected(TxValidationResult::Consensus, BAD_ASSETUNLOCK_QUORUM_HASH)
    );

    let (quorum, key) = fixture.main_quorum(1_032);
    let tx = fixture.unlock(1_090, quorum, key);
    assert_eq!(check.verdict(&tx, pindex), Ok(()), "the main-chain quorum of the same height");
}

/// Condition (c): the DIP0003 gate (`evo/specialtxman.cpp:227`) and `ScanQuorums`'
/// `IsQuorumTypeEnabled(type, pindexStart)` (`llmq/quorumsman.cpp:183`; `LLMQ_100_67` follows
/// DIP0020, `validation.cpp:6680-6681`) read the block the check is at, not the tip.
#[test]
fn deployment_checks_read_the_checked_block() {
    let fixture = Fixture::new(1_200);
    let (quorum, key) = fixture.main_quorum(1_032);
    let tx = fixture.unlock(1_050, quorum, key);
    let pindex = fixture.at(1_050);

    let mut check = Check::new(&fixture);
    check.deployments.dip3 = 1_060;
    assert_eq!(
        check.verdict(&tx, pindex),
        rejected(TxValidationResult::BadSpecial, BAD_TX_TYPE_DIP3_INACTIVE)
    );
    assert_eq!(check.verdict(&tx, fixture.at(1_060)), Ok(()), "past DIP0003 at the checked block");

    let mut check = Check::new(&fixture);
    check.deployments.dip20 = 1_100;
    assert_eq!(
        check.verdict(&tx, pindex),
        rejected(TxValidationResult::BadSpecial, BAD_ASSETUNLOCK_TOO_OLD_QUORUM),
        "the platform type is not enabled at the checked block, so the scan is empty"
    );
    check.deployments.dip20 = 1_051;
    assert_eq!(check.verdict(&tx, pindex), Ok(()));
}

/// Condition (d): an evo read that throws inside `CheckSpecialTxInner` is Core's catch,
/// `failed-check-special-tx`, `TX_CONSENSUS` (`evo/specialtxman.cpp:267-271`): a mined-commitment
/// read in the quorum scan, or `GetListForBlock` for a provider check. A readiness refusal has no
/// Core counterpart and is a fault.
#[test]
fn store_faults_are_failed_check_special_tx() {
    let fixture = Fixture::new(1_200);
    let (quorum, key) = fixture.main_quorum(1_032);
    let unlock = fixture.unlock(1_050, quorum, key);
    let pindex = fixture.at(1_050);
    let failed = rejected(TxValidationResult::Consensus, FAILED_CHECK_SPECIAL_TX);

    let mut check = Check::new(&fixture);
    check.mined.fault = Some(StoreError::read("a mined commitment row"));
    assert_eq!(check.verdict(&unlock, pindex), failed);
    assert_eq!(check.lists_read.load(Ordering::Relaxed), 0, "an unlock reads no list");
    check.mined.fault = Some(StoreError::NotBuilt("the quorum block processor"));
    assert!(check.run(&unlock, pindex).is_err(), "a readiness refusal is not a verdict");

    let revoke = pro_up_rev();
    let mut check = Check::new(&fixture);
    assert_ne!(check.verdict(&revoke, pindex), failed, "the list read answers");
    check.list = Err(StoreError::inconsistent("the list at the checked block"));
    assert_eq!(check.verdict(&revoke, pindex), failed);
    check.list = Err(StoreError::NotBuilt("the masternode list"));
    assert!(check.run(&revoke, pindex).is_err());
    assert_eq!(check.mined.reads.load(Ordering::Relaxed), 0, "a revocation reads no quorum");
}

/// PreChecks refuses a quorum commitment (`qc-not-allowed`, `validation.cpp:866-869`) before
/// `CheckSpecialTx`, so one reaching it is an assert, never a verdict.
#[test]
fn quorum_commitment_is_an_assert() {
    let fixture = Fixture::new(1_200);
    let qc = Arc::new(Tx::new(Transaction::new(
        3,
        TxType::QuorumCommitment,
        vec![],
        vec![],
        0,
        vec![0; 4],
    )));
    assert!(Check::new(&fixture).run(&qc, fixture.tip).is_err());
}
