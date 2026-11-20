//! `CMNListBuilder` (`evo/specialtxman.h` v24.0.0:137-167): `RebuildListFromBlock` split into
//! its three phases, so block assembly can apply transactions as it selects them, try a package
//! on a clone and keep the clone only if every transaction in it applies.
//!
//! [`build`](super::build) is these phases in sequence, so the whole-block build and the
//! builder are one port. The tests check both against `pre_split`, the one-piece build frozen
//! from before the split.

use std::sync::Arc;

use dash_consensus::coins::CoinsView;
use dash_consensus::primitives::{Block, Tx};
use dash_evo::effect::EffectSink;
use dash_num::Hash256;
use dash_primitives::TxType;

use super::{
    BAD_SHARED_COLLATERAL_SPEND, BuildList, BuiltList, DML_PREV_LIST_MISMATCH, DmlBuildError,
    DmlRules, PrevBlock, apply_special_tx,
};
use crate::list::MnList;
use crate::store::{DmlMemberSource, StoreError};
use crate::trace::{DmlEvent, DmlTrace};
use crate::types::Dmn;

/// The builder over either list (EV2b D24): [`MnListBuilder`]'s, and the reference twin's for
/// its whole-block build.
#[derive(Clone)]
pub(crate) struct ListBuilder<'a, 'b, L> {
    prev: PrevBlock<'a>,
    /// `m_list`: the list after the transactions applied so far.
    list: L,
    /// `FinishListForBlock`'s `prevList.GetMNPayee(pindexPrev)` (v24.0.0:738), read here: the
    /// predecessor list does not change.
    payee: Option<Arc<Dmn>>,
    rules: DmlRules<'b>,
}

impl<'a, 'b, L: BuildList> ListBuilder<'a, 'b, L> {
    /// `BeginListForBlock(pindexPrev, prevList)`.
    // Core: v24.0.0 evo/specialtxman.cpp:309-344 CSpecialTxProcessor::BeginListForBlock
    pub(crate) fn begin(
        prev: PrevBlock<'a>,
        prev_list: &L,
        rules: DmlRules<'b>,
    ) -> Result<Self, DmlBuildError> {
        // Core asserts this (v24.0.0:315); the chainstate never passes anything else.
        if !(prev_list.is_default() || *prev_list.block_hash() == prev.hash) {
            return Err(DmlBuildError::Store(StoreError::inconsistent(DML_PREV_LIST_MISMATCH)));
        }
        let mut list = prev_list.start_build(prev.height.wrapping_add(1));

        // Step 0: the payee, from the predecessor before any mutation (v24.0.0:738).
        let payee = prev_list.payee(rules.payee());

        // Step 1: confirmations over prevList, banned included (v24.0.0:326-341).
        let confirmed = Hash256::from(*prev.hash.as_bytes());
        list.confirm(prev_list, prev.height, rules.params.min_confirmations, &confirmed)?;

        // Step 2: DecreaseScores (v24.0.0:343).
        list.decrease_scores()?;

        Ok(Self {
            prev,
            list,
            payee,
            rules,
        })
    }

    /// `ApplyTxToList`: step 3 for one transaction (v24.0.0:346-730). On an error the list may be
    /// partly updated, as Core's is; a caller trying a transaction applies it to a clone.
    // Core: v24.0.0 evo/specialtxman.cpp:346-730 CSpecialTxProcessor::ApplyTxToList
    pub(crate) fn apply_tx<C: CoinsView + ?Sized>(
        &mut self,
        tx: &Tx,
        coins: &C,
        members: &DmlMemberSource<'_>,
        effects: &mut dyn EffectSink,
        trace: &mut dyn DmlTrace,
    ) -> Result<(), DmlBuildError> {
        if !tx.is_special_tx_version() {
            return Ok(());
        }
        apply_special_tx(tx, self.prev, &mut self.list, coins, members, &self.rules, effects, trace)
    }

    /// `FinishListForBlock`: steps 4-6 over `block`, on a copy of the list.
    // Core: v24.0.0 evo/specialtxman.cpp:732-805 CSpecialTxProcessor::FinishListForBlock
    pub(crate) fn finish(
        &self,
        block: &Block,
        trace: &mut dyn DmlTrace,
    ) -> Result<BuiltList<L>, DmlBuildError> {
        let height = self.prev.height.wrapping_add(1);
        let mut new = self.list.clone();

        // Step 4: collateral spends by every transaction, coinbase skipped (v24.0.0:742-768).
        for tx in block.txs().iter().skip(1) {
            for input in tx.inputs() {
                let prevout = &input.prevout();
                let Some(dmn) = new.get_by_collateral(prevout) else {
                    continue;
                };
                if dmn.collateral != *prevout {
                    continue;
                }
                // The raw `nType`, without an `IsSpecialTxVersion()` guard (EV2 §3.5).
                if dmn.cold.is_shared() && tx.tx_type() != TxType::ProviderDissolve {
                    return Err(DmlBuildError::invalid(BAD_SHARED_COLLATERAL_SPEND));
                }
                let (pro_tx, collateral) = (dmn.pro_tx_hash, dmn.collateral);
                new.remove(&pro_tx)?;
                trace.event(DmlEvent::CollateralSpent {
                    pro_tx,
                    collateral,
                    height,
                    total: new.counts().total(),
                });
            }
        }

        // Step 5: payee bookkeeping, even if the payee was banned or revoked in this block
        // (v24.0.0:772-787).
        if let Some(paid) = self.payee.as_ref().and_then(|p| new.get(&p.pro_tx_hash)) {
            let pro_tx = paid.pro_tx_hash;
            let mut hot = paid.hot;
            hot.last_paid_height = height;
            if paid.is_evo() && !self.rules.mn_rr {
                hot.consecutive_payments = hot.consecutive_payments.wrapping_add(1);
                trace.event(DmlEvent::StreakBumped {
                    pro_tx,
                    to: hot.consecutive_payments,
                });
            }
            new.update_hot(&pro_tx, hot)?;
        }

        // Step 6: reset the streaks of unpaid evonodes over the snapshot `newList2`
        // (v24.0.0:789-802).
        new.reset_streaks(self.payee.as_ref().map(|p| p.pro_tx_hash), self.rules.mn_rr, trace)?;

        Ok(BuiltList {
            list: new,
            payee: self.payee.clone(),
        })
    }
}

/// `CMNListBuilder`: [`build_new_list_from_block`](crate::build_new_list_from_block) one
/// transaction at a time. `begin`, then [`apply_tx`](Self::apply_tx) for each non-coinbase
/// transaction in block order, then [`finish`](Self::finish) over the block gives the
/// whole-block build's result and error. Cloning is cheap (the list is persistent maps), and a
/// clone is independent of the builder it came from.
///
/// The coins view and member source are passed to each `apply_tx` (Core keeps the view), so the
/// builder borrows only the ancestry and the rules.
// Core: v24.0.0 evo/specialtxman.h:137-167, evo/specialtxman.cpp:824-853 CMNListBuilder
#[derive(Clone)]
pub struct MnListBuilder<'a, 'b>(ListBuilder<'a, 'b, MnList>);

impl<'a, 'b> MnListBuilder<'a, 'b> {
    /// `CMNListBuilder(processor, prevList, pindexPrev, ...)`: the confirmations and the PoSe
    /// decay of the block at `prev.height + 1`.
    ///
    /// # Errors
    ///
    /// A store fault if `prev_list` is neither the default list nor `prev`'s (Core asserts), or
    /// Core's exception from a confirmation's `UpdateMN` or the PoSe decay
    /// (`specialtxman.cpp` v24.0.0:337-343).
    pub fn begin(
        prev: PrevBlock<'a>,
        prev_list: &MnList,
        rules: DmlRules<'b>,
    ) -> Result<Self, DmlBuildError> {
        ListBuilder::begin(prev, prev_list, rules).map(Self)
    }

    /// `ApplyTx(tx, state)`: the same result, and the same exception, as the whole-block build
    /// processing `tx` next. A non-special transaction changes nothing; collateral spends are
    /// left to [`finish`](Self::finish). After an error the builder is partly updated: try a
    /// transaction on a clone to keep the builder as it was. Discarding the clone does not take
    /// back what the trial already sent to `effects` and `trace` (a member computation's snapshot
    /// rows, its events), so give each trial its own sinks and keep them only with the clone.
    ///
    /// # Errors
    ///
    /// The whole-block build's error for `tx`.
    pub fn apply_tx<C: CoinsView + ?Sized>(
        &mut self,
        tx: &Tx,
        coins: &C,
        members: &DmlMemberSource<'_>,
        effects: &mut dyn EffectSink,
        trace: &mut dyn DmlTrace,
    ) -> Result<(), DmlBuildError> {
        self.0.apply_tx(tx, coins, members, effects, trace)
    }

    /// `Finish(block, state, mnListRet)`: collateral spends by every non-coinbase transaction of
    /// `block`, then the payee and streak bookkeeping. It does not modify the builder.
    ///
    /// # Errors
    ///
    /// The whole-block build's error from those steps.
    pub fn finish(
        &self,
        block: &Block,
        trace: &mut dyn DmlTrace,
    ) -> Result<BuiltList, DmlBuildError> {
        self.0.finish(block, trace)
    }
}

#[cfg(test)]
mod tests {
    //! The builder, and the whole-block build made of its phases, against the frozen pre-split
    //! oracle ([`rebuild_list_from_block`], Core's `RebuildListFromBlock` as one body) over
    //! Core-produced vectors (EV2b D25's `ev2b-quirks.txt`: accepts, rejects and an exception)
    //! and over generated and seeded streams of their transactions, with malformed provider and
    //! commitment transactions mixed in.

    use dash_consensus::ChainAncestry;
    use dash_consensus::primitives::{Transaction, TxRef, core_serialize, decode_block};
    use dash_evo::Effect;
    use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
    use dash_primitives::BlockHash;
    use proptest::collection::vec;
    use proptest::prelude::*;
    use proptest::sample::Index;

    use super::*;
    use crate::build::pre_split::rebuild_list_from_block;
    use crate::codec::{core_disk, encode_canonical_list_unvalidated};
    use crate::netinfo::NetInfoRules;
    use crate::testkit::replay::{
        CORE_NET_INFO, Coins, CoreVerdict, Headers, Members, Vector, agrees, mainnet, payee_rules,
        read_vectors,
    };
    use crate::{DmlEvent, DmlParams, NoTrace, build_new_list_from_block};

    const VECTORS: &str = include_str!("../../tests/data/ev2b-quirks.txt");

    fn vectors() -> Vec<Vector> {
        let vectors = read_vectors(VECTORS).expect("the vector file");
        assert!(vectors.len() >= 9, "the quirk vectors");
        vectors
    }

    fn regtest() -> ChainSetup {
        ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest")
    }

    /// A build's observable result: the list's canonical bytes and the payee, or the error's
    /// class, reject reason and text.
    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Built {
            list: Vec<u8>,
            payee: Option<[u8; 32]>,
        },
        Failed {
            class: &'static str,
            reason: Option<String>,
            text: String,
        },
    }

    fn outcome(result: &Result<BuiltList, DmlBuildError>) -> Outcome {
        match result {
            Ok(built) => {
                let mut list = Vec::new();
                encode_canonical_list_unvalidated(&built.list, &mut list);
                Outcome::Built {
                    list,
                    payee: built.payee.as_ref().map(|d| *d.pro_tx_hash.as_bytes()),
                }
            }
            Err(error) => Outcome::Failed {
                class: match error {
                    DmlBuildError::Invalid(_) => "invalid",
                    DmlBuildError::Exception(_) => "exception",
                    DmlBuildError::Store(_) => "store",
                },
                reason: error.reject_reason().map(str::to_owned),
                text: error.to_string(),
            },
        }
    }

    /// One build, with what it emitted.
    #[derive(Debug, PartialEq, Eq)]
    struct Run {
        outcome: Outcome,
        trace: Vec<DmlEvent>,
        effects: Vec<Effect>,
    }

    /// How a run drives the build.
    #[derive(Clone, Copy, Debug)]
    enum Mode {
        /// The frozen pre-split oracle, independent of the builder's phases.
        Reference,
        /// `build_new_list_from_block`, now the builder's phases in sequence.
        Whole,
        /// `begin; apply_tx*; finish` on one builder.
        Builder,
        /// Each transaction applied to a clone first, as block assembly tries a package; the
        /// first error is the run's.
        Trial,
    }

    /// A vector's context, read once.
    struct Ctx {
        params: ChainSetup,
        prev_list: MnList,
        headers: Headers,
        coins: Coins,
        v: Vector,
    }

    impl Ctx {
        fn new(v: Vector) -> Ctx {
            let headers = Headers::new(v.ctx.first_height, &v.ctx.headers).expect("headers");
            let tip_hash = headers.header(headers.tip()).hash;
            let coins = Coins::new(&v.ctx.coins, v.ctx.coins_at_tip.then_some(tip_hash));
            let prev_list = core_disk::decode_list_s3(&mut &v.ctx.list[..]).expect("Core's list");
            Ctx {
                params: regtest(),
                prev_list,
                headers,
                coins,
                v,
            }
        }

        fn block(&self) -> Block {
            decode_block(&mut &self.v.block[..]).expect("the block").into_value()
        }

        /// The vector's block with `txs` after its coinbase.
        fn with_txs(&self, txs: &[TxRef]) -> Block {
            let block = self.block();
            let mut raw = self.v.block[..80].to_vec();
            let count = u8::try_from(txs.len() + 1).expect("a small block");
            assert!(count < 0xfd);
            raw.push(count);
            raw.extend(core_serialize(&*block.txs()[0]));
            for tx in txs {
                raw.extend(core_serialize(&**tx));
            }
            decode_block(&mut &raw[..]).expect("the stream's block").into_value()
        }

        /// `f` with the build's arguments, as `testkit::replay` sets them up.
        fn with_args<R>(
            &self,
            f: impl FnOnce(PrevBlock<'_>, &DmlRules<'_>, &Members<'_>) -> R,
        ) -> R {
            let tip = self.headers.tip();
            let header = self.headers.header(tip);
            let height = i32::try_from(header.height).expect("height");
            let dml = DmlParams::new(&self.params);
            let net_info = NetInfoRules::new(&self.params, mainnet(), CORE_NET_INFO);
            let buried = payee_rules(&self.params, height);
            let rules = DmlRules {
                params: &dml,
                net_info: &net_info,
                v19: buried.v19,
                mn_rr: buried.mn_rr,
                v24: self.v.ctx.v24,
            };
            let members = Members::new(&self.headers, &self.v.ctx.quorums).expect("members");
            let prev = PrevBlock {
                id: tip,
                hash: header.hash,
                height,
                chain: &self.headers,
            };
            f(prev, &rules, &members)
        }

        fn run(&self, block: &Block, mode: Mode) -> Run {
            let (mut trace, mut effects) = (Vec::new(), Vec::new());
            let result = self.with_args(|prev, rules, members| match mode {
                Mode::Reference => rebuild_list_from_block(
                    block,
                    prev,
                    &self.prev_list,
                    &self.coins,
                    members,
                    rules,
                    &mut effects,
                    &mut trace,
                ),
                Mode::Whole => build_new_list_from_block(
                    block,
                    prev,
                    &self.prev_list,
                    &self.coins,
                    members,
                    rules,
                    &mut effects,
                    &mut trace,
                ),
                Mode::Builder | Mode::Trial => {
                    let mut builder = MnListBuilder::begin(prev, &self.prev_list, *rules)?;
                    for tx in block.txs().iter().skip(1) {
                        if let Mode::Trial = mode {
                            let mut trial = builder.clone();
                            trial.apply_tx(tx, &self.coins, members, &mut effects, &mut trace)?;
                            builder = trial;
                        } else {
                            builder.apply_tx(tx, &self.coins, members, &mut effects, &mut trace)?;
                        }
                    }
                    builder.finish(block, &mut trace)
                }
            });
            Run {
                outcome: outcome(&result),
                trace,
                effects,
            }
        }

        /// Block assembly's use: each transaction tried on a clone and kept only if it applies;
        /// the result, and the transactions kept.
        fn skip_failing(&self, txs: &[TxRef]) -> (Outcome, Vec<TxRef>) {
            let mut kept = Vec::new();
            let result = self.with_args(|prev, rules, members| {
                let mut builder = MnListBuilder::begin(prev, &self.prev_list, *rules)?;
                for tx in txs {
                    let mut trial = builder.clone();
                    let applied =
                        trial.apply_tx(tx, &self.coins, members, &mut Vec::new(), &mut NoTrace);
                    if applied.is_ok() {
                        builder = trial;
                        kept.push(Arc::clone(tx));
                    }
                }
                builder.finish(&self.with_txs(&kept), &mut NoTrace)
            });
            (outcome(&result), kept)
        }
    }

    /// Malformed transactions a stream mixes in: a commitment and a ProRegTx whose payloads do
    /// not decode, and a normal transaction (which the list build ignores).
    fn extras() -> Vec<TxRef> {
        let raw = |version, tx_type| {
            let tx = Transaction::new(version, tx_type, vec![], vec![], 0, vec![]);
            let mut block = vec![0; 80];
            block.push(1);
            block.extend(core_serialize(&tx));
            decode_block(&mut &block[..]).expect("one transaction").into_value().txs()[0].clone()
        };
        vec![
            raw(3, TxType::QuorumCommitment),
            raw(3, TxType::ProviderRegister),
            raw(2, TxType::Spend),
        ]
    }

    /// Every way of driving the build gives the pre-split oracle's run: list bytes, payee, error
    /// class, reason and text, trace and effects. Returns the oracle's run.
    fn assert_equivalent(ctx: &Ctx, block: &Block, what: &str) -> Run {
        let reference = ctx.run(block, Mode::Reference);
        assert_eq!(ctx.run(block, Mode::Whole), reference, "{what}: whole-block build");
        assert_eq!(ctx.run(block, Mode::Builder), reference, "{what}: builder");
        assert_eq!(ctx.run(block, Mode::Trial), reference, "{what}: trial on clones");
        reference
    }

    /// Each vector's own block: the builder's run is the pre-split oracle's (list bytes, payee,
    /// error class, reason and text, trace and effects), and its result is Core's verdict.
    #[test]
    fn the_builder_builds_cores_vectors_as_the_pre_split_build() {
        let mut verdicts = [0usize; 3];
        for v in vectors() {
            let ctx = Ctx::new(v);
            let block = ctx.block();
            let name = ctx.v.name.clone();
            assert_equivalent(&ctx, &block, &name);
            let result = ctx.with_args(|prev, rules, members| {
                let mut builder = MnListBuilder::begin(prev, &ctx.prev_list, *rules)?;
                for tx in block.txs().iter().skip(1) {
                    builder.apply_tx(tx, &ctx.coins, members, &mut Vec::new(), &mut NoTrace)?;
                }
                builder.finish(&block, &mut NoTrace)
            });
            assert!(agrees(&result.map(|b| b.list), &ctx.v.core), "{name}: Core's verdict");
            verdicts[match ctx.v.core {
                CoreVerdict::Accept(_) => 0,
                CoreVerdict::Reject(_) => 1,
                CoreVerdict::Exception(_) => 2,
            }] += 1;
        }
        assert!(verdicts.iter().all(|n| *n > 0), "accepts, rejects and exceptions: {verdicts:?}");
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 256,
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        /// `begin; apply_tx*; finish`, on one builder and trial by trial on clones, and the
        /// whole-block build give the pre-split oracle's run (list bytes, payee, error class,
        /// reason and text, trace and effects) over any stream drawn from a vector's
        /// transactions, repeats included, with malformed commitments, ProRegTxs and normal
        /// transactions mixed in.
        #[test]
        fn begin_apply_finish_is_the_pre_split_build(
            vector in any::<Index>(),
            picks in vec((any::<bool>(), any::<Index>()), 0..12),
        ) {
            let vectors = vectors();
            let ctx = Ctx::new(vectors[vector.index(vectors.len())].clone());
            let own: Vec<TxRef> = ctx.block().txs()[1..].to_vec();
            let extras = extras();
            let txs: Vec<TxRef> = picks
                .iter()
                .map(|(take_own, i)| {
                    let from = if *take_own && !own.is_empty() { &own } else { &extras };
                    Arc::clone(&from[i.index(from.len())])
                })
                .collect();
            let block = ctx.with_txs(&txs);
            let reference = ctx.run(&block, Mode::Reference);
            prop_assert_eq!(&ctx.run(&block, Mode::Whole), &reference);
            prop_assert_eq!(&ctx.run(&block, Mode::Builder), &reference);
            prop_assert_eq!(&ctx.run(&block, Mode::Trial), &reference);
        }
    }

    /// Fixed cases beside the property: seeded streams over each vector's transactions
    /// (subsets in any order, malformed commitments and provider transactions inserted), each
    /// compared with the pre-split oracle on list, error, trace and effects. A stream with a
    /// non-applying commitment fails at it with the oracle's error, which the template throws.
    #[test]
    fn seeded_streams_match_the_pre_split_build() {
        let extras = extras();
        let mut state = 0x9e37_79b9_u32;
        let mut next = move |bound: usize| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 8) as usize % bound
        };
        let (mut built, mut failed, mut qc_failed) = (0, 0, 0);
        for v in vectors() {
            let ctx = Ctx::new(v);
            let own: Vec<TxRef> = ctx.block().txs()[1..].to_vec();
            for round in 0..48 {
                let mut txs = own.clone();
                for i in (1..txs.len()).rev() {
                    txs.swap(i, next(i + 1));
                }
                txs.truncate(next(txs.len() + 1));
                for _ in 0..next(3) {
                    let extra = extras[next(extras.len())].clone();
                    txs.insert(next(txs.len() + 1), extra);
                }
                let block = ctx.with_txs(&txs);
                let reference =
                    assert_equivalent(&ctx, &block, &format!("{} round {round}", ctx.v.name));
                match reference.outcome {
                    Outcome::Built {
                        ..
                    } => built += 1,
                    Outcome::Failed {
                        ref reason,
                        ..
                    } => {
                        failed += 1;
                        qc_failed += usize::from(reason.as_deref() == Some("bad-qc-payload"));
                    }
                }
            }
        }
        assert!(built > 0 && failed > 0 && qc_failed > 0, "{built} {failed} {qc_failed}");
    }

    /// Block assembly keeps a transaction only if it applies on a clone: the builder then holds
    /// the pre-split oracle's build of the kept transactions, so a rejected trial leaves no
    /// mark on the list.
    #[test]
    fn skipping_failed_trials_builds_the_kept_transactions() {
        let extras = extras();
        let mut skipped = 0;
        for v in vectors() {
            let ctx = Ctx::new(v);
            let own: Vec<TxRef> = ctx.block().txs()[1..].to_vec();
            for reversed in [false, true] {
                let mut txs = Vec::new();
                for (i, tx) in own.iter().enumerate() {
                    txs.push(extras[i % extras.len()].clone());
                    txs.push(tx.clone());
                }
                if reversed {
                    txs.reverse();
                }
                let (trial, kept) = ctx.skip_failing(&txs);
                skipped += txs.len() - kept.len();
                let reference = ctx.run(&ctx.with_txs(&kept), Mode::Reference);
                assert_eq!(trial, reference.outcome, "{}", ctx.v.name);
            }
        }
        assert!(skipped > 0);
    }

    /// A clone and its original diverge independently in both directions, and `finish` leaves
    /// its builder as it was.
    #[test]
    fn clones_diverge_independently() {
        let ctx = vectors()
            .into_iter()
            .map(Ctx::new)
            .find(|ctx| {
                matches!(ctx.v.core, CoreVerdict::Accept(_))
                    && ctx.run(&ctx.block(), Mode::Reference).outcome
                        != ctx.run(&ctx.with_txs(&[]), Mode::Reference).outcome
            })
            .expect("an accepted vector whose transactions change the list");
        let block = ctx.block();
        let empty = ctx.with_txs(&[]);
        ctx.with_args(|prev, rules, members| {
            let finish =
                |b: &MnListBuilder<'_, '_>, block: &Block| outcome(&b.finish(block, &mut NoTrace));
            let apply_all = |b: &mut MnListBuilder<'_, '_>| {
                for tx in block.txs().iter().skip(1) {
                    b.apply_tx(tx, &ctx.coins, members, &mut Vec::new(), &mut NoTrace)
                        .expect("Core accepts the block");
                }
            };
            let whole = ctx.run(&block, Mode::Reference).outcome;
            let untouched = ctx.run(&empty, Mode::Reference).outcome;
            assert_ne!(whole, untouched);

            // The clone moves; the original does not.
            let original = MnListBuilder::begin(prev, &ctx.prev_list, *rules).expect("begin");
            let mut clone = original.clone();
            apply_all(&mut clone);
            assert_eq!(finish(&original, &empty), untouched);
            assert_eq!(finish(&clone, &block), whole);
            // `finish` does not modify the builder.
            assert_eq!(finish(&clone, &block), whole);

            // The original moves; an earlier clone does not.
            let mut original = MnListBuilder::begin(prev, &ctx.prev_list, *rules).expect("begin");
            let clone = original.clone();
            apply_all(&mut original);
            assert_eq!(finish(&clone, &empty), untouched);
            assert_eq!(finish(&original, &block), whole);
        });
    }

    /// The precondition `begin` checks is the pre-split oracle's: a list that is neither
    /// default nor the predecessor's is a store fault.
    #[test]
    fn begin_refuses_a_list_not_at_prev() {
        let ctx = Ctx::new(vectors().remove(0));
        let elsewhere = MnList::new(BlockHash::from([7; 32]), 1, 0);
        let block = ctx.block();
        ctx.with_args(|prev, rules, members| {
            let ours = MnListBuilder::begin(prev, &elsewhere, *rules).err();
            let reference = rebuild_list_from_block(
                &block,
                prev,
                &elsewhere,
                &ctx.coins,
                members,
                rules,
                &mut Vec::new(),
                &mut NoTrace,
            )
            .err();
            let whole = build_new_list_from_block(
                &block,
                prev,
                &elsewhere,
                &ctx.coins,
                members,
                rules,
                &mut Vec::new(),
                &mut NoTrace,
            )
            .err();
            assert!(matches!(ours, Some(DmlBuildError::Store(_))), "{ours:?}");
            assert_eq!(ours, reference);
            assert_eq!(whole, reference);
        });
    }
}
