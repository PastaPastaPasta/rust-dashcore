//! EV2b's validated-block target (EV2 §6.3, EV2b D34/D40): `diff_dml_block`, Core v24's
//! masternode-list part of `ProcessSpecialTxsInBlock` (`oracle_dml_block_effects`: the
//! `CheckSpecialTxInner` loop, `BuildNewListFromBlock`, `ProcessBlock`) against our check loop,
//! `build_new_list_from_block` and `process_block`, on `diff_dml_apply`'s inputs without its
//! reachable-intersection filter: Core's check loop is the filter.
//!
//! The rejecting layer comes first. Our check loop ([`dml_checks`], EV1's layer as scaffolding
//! until EV1-2..5 land) runs on every block, and is compared with Core's at its rejection point
//! (D56): the rejected transaction, the stage, the class and the reason. A rejection of ours
//! where Core's loop accepts, or before its rejection point, is a divergence, and so is our
//! acceptance of the transaction it rejects, unless the step it rejects at is one the node lacks
//! ([`dml_checks::missing_checks`]): that block is out of domain until the step's EV1 slice
//! lands. Past the check loop Core rejects in the build or in `ProcessBlock`, and so must we, in
//! the same layer with the same reason; a refusal of ours that is not a verdict
//! (`DmlException::OutOfDomain`, a store fault) is a divergence there, since every such input
//! is one Core's checks refuse first (D2, D12).
//!
//! Compared on top of the verdict: the tip list's payee; on acceptance the new list's canonical
//! bytes; `ProcessBlock`'s effects: the diff row (Core's `dmn_D4`, read back and compared as
//! canonical diff bytes with our `oldList.BuildDiff(newList)`), whether a snapshot row was
//! written and its list, and the platform-ban resets. Core reports the resets of masternodes in
//! the tip's or the new list (the shim bans those first, oracle.h); ours are the
//! `ResetPlatformBan` effects for the same set, deduplicated and sorted. The reference twin
//! builds the block too (`xcheck`, D24).
//!
//! Signatures are not checked (`check_sigs` 0: ConnectBlock's assumevalid path): they are EV1's
//! and BS6b's. Core's initial-snapshot row (`pindex->pprev == m_initial_snapshot_index`) cannot
//! occur in the shim, whose context holds its list as the tip's snapshot, so our side passes
//! `is_first_dip3_block = false` (D41).

use std::collections::BTreeSet;
use std::sync::Arc;

use dash_consensus::primitives::decode_block;
use dash_consensus::{BlockValidationError, BlockValidationResult};
use dash_dml::codec::core_disk;
use dash_dml::specialtx::SpecialTxRules;
use dash_dml::store::MEMBERS_MISSING;
use dash_dml::testkit::replay::{Carried, Coins, Headers, ReplayContext, Replayed, replay_carried};
use dash_dml::{
    DmlBuildError, DmlException, DmlParams, MnList, NoTrace, ProcessedList, StoreError,
    process_block,
};
use dash_evo::effect::Effect;
use dash_oracle_sys::{
    Chain, Context, DEPLOYMENT_EVO_SHARES, DmlEffects, DmlStage, ErrorKind, Oracle, Pin, Rejection,
    ThresholdState, Verdict,
};
use dash_params::ChainParams;

use crate::checks::result_code;
use crate::dml::{canonical, canonical_diff};
use crate::dml_apply::checked::{FLAG_CHECKED, Walk};
use crate::dml_apply::{compare_generated, replay_context, split_block};
use crate::dml_checks::{self, CheckCtx, CheckOutcome};
use crate::inputs::oracle_failure;
use crate::{Compared, Divergence, Outcome, check, divergence};

/// Our side past the build: `process_block`'s result and its platform-ban resets of the
/// masternodes in the tip's or the new list.
struct Processed {
    result: Result<ProcessedList, BlockValidationError>,
    resets: Vec<[u8; 32]>,
}

/// `process_block` over the built list, as `ProcessSpecialTxsInBlock` runs it after the build
/// (not in just-check mode, DIP3 active at the block), at the height after the context's tip
/// (`pindex->nHeight`; a supplied default list has height -1, Core's shim stores it at the tip).
/// Every reset is at that height (`ResetPlatformBan(proTxHash, nHeight)`, v24
/// `deterministicmns.cpp:745`).
fn process_ours(
    params: &ChainParams,
    ctx: &ReplayContext,
    prev: &Arc<MnList>,
    block: &[u8],
    built: MnList,
) -> Result<Processed, Divergence> {
    // `replay` read it already.
    let block =
        decode_block(&mut &block[..]).map_err(|_| divergence("our block read"))?.into_value();
    let height = block_height(ctx)?;
    let hash = block.header().hash();
    let mut seen: BTreeSet<[u8; 32]> = prev.core_order().iter().map(|p| *p.as_bytes()).collect();
    seen.extend(built.core_order().iter().map(|p| *p.as_bytes()));
    let mut effects = Vec::new();
    let result = process_block(
        &block,
        (hash, height),
        prev,
        built,
        &DmlParams::new(params),
        false,
        &mut effects,
    );
    let mut resets = BTreeSet::new();
    for effect in &effects {
        if let Effect::ResetPlatformBan {
            pro_tx_hash,
            height: at,
        } = effect
        {
            check(*at == height, "reset height")?;
            if seen.contains(pro_tx_hash.as_bytes()) {
                resets.insert(*pro_tx_hash.as_bytes());
            }
        }
    }
    Ok(Processed {
        result,
        resets: resets.into_iter().collect(),
    })
}

/// The height of the block after the context's tip: `pindex->nHeight`.
fn block_height(ctx: &ReplayContext) -> Result<i32, Divergence> {
    i32::try_from(ctx.headers.len())
        .ok()
        .and_then(|n| ctx.first_height.checked_add(n))
        .ok_or(divergence("height"))
}

/// Our rejection's result class (`BlockValidationResult`, Core's numbering); `None` for a store
/// fault. An exception is Core's `BLOCK_CONSENSUS` `failed-procspectxsinblock`
/// (`specialtxman.cpp` v24:1017).
fn build_class(e: &DmlBuildError) -> Option<i32> {
    match e {
        DmlBuildError::Invalid(e) => Some(e.result() as i32),
        DmlBuildError::Exception(_) => Some(BlockValidationResult::Consensus as i32),
        DmlBuildError::Store(_) => None,
    }
}

/// Core's rows against ours for an accepted block.
fn compare_rows(core: &DmlEffects, ours: &ProcessedList) -> Result<(), Divergence> {
    let row = core.diff_row.as_deref().ok_or(divergence("core diff row"))?;
    let core_diff =
        core_disk::decode_diff_d4(&mut &row[..]).map_err(|_| divergence("core diff row read"))?;
    check(canonical_diff(&core_diff) == canonical_diff(&ours.diff), "diff row")?;
    match (&core.snapshot_row, &ours.delta.snapshot) {
        (None, None) => Ok(()),
        (Some(row), Some(list)) => {
            let core_list = core_disk::decode_list_s3(&mut &row[..])
                .map_err(|_| divergence("core snapshot row read"))?;
            check(canonical(&core_list) == canonical(list), "snapshot row")
        }
        _ => Err(divergence("snapshot row")),
    }
}

/// [`compare_dml_block`]'s outcome, with Core's rejection and the check layer's comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detailed {
    pub outcome: Outcome,
    /// The stage Core rejected the block in and its reason, when the comparison got there.
    pub core: Option<(DmlStage, String)>,
    /// The check layer's comparison (D56), when the comparison got there.
    pub check: Option<CheckOutcome>,
    /// DIP3 is not active at the block: Core's check loop is all it runs.
    pub pre_dip3: bool,
}

impl From<Outcome> for Detailed {
    fn from(outcome: Outcome) -> Detailed {
        Detailed {
            outcome,
            core: None,
            check: None,
            pre_dip3: false,
        }
    }
}

impl Detailed {
    fn rejected(outcome: Outcome, stage: DmlStage, reason: &str) -> Detailed {
        Detailed {
            core: Some((stage, reason.to_owned())),
            check: Some(CheckOutcome::Accepted),
            ..outcome.into()
        }
    }
}

/// `diff_dml_block`'s comparison of one block over one context.
pub fn compare_dml_block(
    oracle: &Oracle,
    chain: &Chain,
    params: &ChainParams,
    ctx: &Context,
    block: &[u8],
) -> Compared {
    compare_dml_block_detailed(oracle, chain, params, ctx, block).map(|d| d.outcome)
}

/// [`compare_dml_block`], with Core's rejecting stage and reason and the check layer's
/// comparison.
pub fn compare_dml_block_detailed(
    oracle: &Oracle,
    chain: &Chain,
    params: &ChainParams,
    ctx: &Context,
    block: &[u8],
) -> Result<Detailed, Divergence> {
    compare_dml_block_carried(oracle, chain, params, ctx, block, None).map(|(d, _)| d)
}

/// [`compare_dml_block_detailed`] for a block of a sequence (EV2b D54): our side builds over the
/// lists `carried` from the sequence's earlier blocks, when given, instead of reading the
/// context's (Core's) list, and returns its new lists when both sides accept.
pub fn compare_dml_block_carried(
    oracle: &Oracle,
    chain: &Chain,
    params: &ChainParams,
    ctx: &Context,
    block: &[u8],
    carried: Option<&Carried>,
) -> Result<(Detailed, Option<Carried>), Divergence> {
    let (core, effects) = match oracle.dml_block_effects(chain, block, ctx, false, false) {
        Ok(core) => core,
        Err(e) if e.kind == ErrorKind::Exception => return Err(divergence("oracle exception")),
        Err(e) => return oracle_failure(&e).map(|o| (o.into(), None)),
    };
    let Some(replay_ctx) = replay_context(ctx) else {
        return Ok((Outcome::OutOfDomain.into(), None));
    };
    let height = block_height(&replay_ctx)?;
    let prev = match carried {
        Some(c) => c.list.clone(),
        None => Arc::new(
            core_disk::decode_list_s3(&mut &replay_ctx.list[..])
                .map_err(|_| divergence("our list read"))?,
        ),
    };
    // The check layer first, on every block (D56).
    let ours =
        decode_block(&mut &block[..]).map_err(|_| divergence("our block read"))?.into_value();
    let headers = Headers::new(replay_ctx.first_height, &replay_ctx.headers)
        .ok_or(divergence("our header read"))?;
    let coins = Coins::new(&replay_ctx.coins, None);
    let rules = SpecialTxRules {
        v24: replay_ctx.v24,
        evo_shares: ctx.deployments.contains(&(DEPLOYMENT_EVO_SHARES, ThresholdState::Active)),
    };
    let cx = CheckCtx::new(params, &headers, &coins, rules, &prev);
    if cx.height != height {
        return Err(divergence("check layer: height"));
    }
    // `DeploymentActiveAfter(tip, DEPLOYMENT_DIP0003)` is false.
    let pre_dip3 = !cx.dip3;
    let checks = dml_checks::check_block(&cx, &ours)?;
    let core_check = match (core.stage, &core.verdict) {
        (Some(DmlStage::Check), Verdict::Reject(r)) => Some(r),
        _ => None,
    };
    let check_layer = dml_checks::compare(&checks, &ours, core_check)?;
    let at_check = |r: &Rejection| Detailed {
        outcome: match check_layer {
            CheckOutcome::Exact {
                ..
            } => Outcome::BothReject,
            _ => Outcome::OutOfDomain,
        },
        core: Some((DmlStage::Check, r.reason.clone())),
        check: Some(check_layer),
        pre_dip3,
    };
    // Before DIP3 Core's check loop is all it runs: no build of ours to compare with it.
    if let (Some(r), true) = (core_check, pre_dip3) {
        return Ok((at_check(r), None));
    }
    let Replayed {
        payee,
        result,
        twin,
        twin_list,
    } = replay_carried(params, &replay_ctx, carried, block, &mut NoTrace).map_err(divergence)?;
    if payee != core.payee {
        return Err(divergence("payee"));
    }
    twin.map_err(|e| divergence(e.0))?;
    // Core stopped in its check loop: our build's verdict is not compared, so it cannot mask the
    // check layer's. Where our loop accepts every transaction (a step we lack rejects), the node
    // would build the block: a store fault there is a divergence (D4), but for members the
    // context lacks, which Core's loop never read (Opus M1). The build's out-of-domain refusals
    // are not: they guard states the step we lack excludes (D56).
    if let Some(r) = core_check {
        if checks.iter().all(|t| t.reject.is_none())
            && let Err(DmlBuildError::Store(e)) = &result
            && !matches!(e, StoreError::Inconsistent { what } if what.starts_with(MEMBERS_MISSING))
        {
            return Err(divergence("store fault"));
        }
        return Ok((at_check(r), None));
    }
    let built = match (result, &core.verdict, core.stage) {
        (Err(DmlBuildError::Store(_)), ..) => return Err(divergence("store fault")),
        (Err(DmlBuildError::Exception(DmlException::OutOfDomain(_))), ..) => {
            return Err(divergence("out of domain past Core's checks"));
        }
        (Err(e), Verdict::Reject(r), Some(DmlStage::Build)) => {
            check(build_class(&e) == result_code(&r.result), "result class")?;
            check(e.reject_reason() == Some(r.reason.as_str()), "reason")?;
            return Ok((Detailed::rejected(Outcome::BothReject, DmlStage::Build, &r.reason), None));
        }
        (Ok(built), Verdict::Accept | Verdict::Reject(_), None | Some(DmlStage::Process)) => built,
        _ => return Err(divergence("layer")),
    };
    // Before DIP3 Core accepts without a build or ProcessBlock: no list to compare.
    if core.verdict == Verdict::Accept && core.list.is_none() {
        let detailed = Detailed {
            check: Some(CheckOutcome::Accepted),
            pre_dip3,
            ..Outcome::OutOfDomain.into()
        };
        return Ok((detailed, None));
    }
    let processed = process_ours(params, &replay_ctx, &prev, block, built)?;
    check(processed.resets == effects.resets, "platform-ban resets")?;
    match (processed.result, &core.verdict) {
        (Ok(ours), Verdict::Accept) => {
            let (core_canonical, _) = core.list.as_ref().ok_or(divergence("core list"))?;
            check(canonical(&ours.list) == *core_canonical, "list")?;
            compare_rows(&effects, &ours)?;
            let twin = twin_list.ok_or(divergence("twin list"))?;
            let carried = Carried {
                twin: twin.with_block_hash(*ours.list.block_hash()),
                list: ours.list,
            };
            let detailed = Detailed {
                check: Some(CheckOutcome::Accepted),
                ..Outcome::BothAccept.into()
            };
            Ok((detailed, Some(carried)))
        }
        (Err(e), Verdict::Reject(r)) => {
            check(Some(e.result() as i32) == result_code(&r.result), "result class")?;
            check(e.reject_reason() == r.reason, "reason")?;
            Ok((Detailed::rejected(Outcome::BothReject, DmlStage::Process, &r.reason), None))
        }
        _ => Err(divergence("verdict")),
    }
}

/// One block of a checked sequence and how the comparison ended.
pub struct SequenceBlock {
    pub detailed: Detailed,
    /// The block's transactions after the coinbase.
    pub txs: usize,
    /// Values its payloads took that an earlier block of the sequence freed.
    pub retaken: usize,
}

/// A checked sequence (EV2b D54, [`checked`](crate::dml_apply::checked)): each block through
/// [`compare_dml_block_carried`], our lists carried from block to block, the context moved past
/// each block both sides accept. Stops at the first divergence.
pub fn compare_sequence(oracle: &Oracle, input: &[u8]) -> Result<Vec<SequenceBlock>, Divergence> {
    let Some(mut walk) = Walk::new(input) else {
        return Ok(vec![]);
    };
    let (mut carried, mut blocks) = (None, Vec::new());
    while let Some(block) = walk.next_block() {
        let (detailed, next) = compare_dml_block_carried(
            oracle,
            &walk.chain(),
            walk.params(),
            &walk.ctx,
            &block,
            carried.as_ref(),
        )?;
        if let Some(next) = next {
            // Our list is Core's in every serialized byte (`compare_dml_block_carried`).
            walk.accepted(&canonical(&next.list)).ok_or(divergence("sequence context"))?;
            carried = Some(next);
        }
        let txs = split_block(&block).map_or(0, |(_, txs)| txs.len().saturating_sub(1));
        blocks.push(SequenceBlock {
            detailed,
            txs,
            retaken: walk.retaken,
        });
    }
    Ok(blocks)
}

/// `diff_dml_block`: `diff_dml_apply`'s generated block over a crafted case's list, through
/// [`compare_dml_block`]; with [`FLAG_CHECKED`] a checked sequence ([`compare_sequence`]),
/// whose last block's outcome it reports.
pub fn compare_block(bytes: &[u8], pin: Pin) -> Compared {
    if bytes.get(1).is_some_and(|flags| flags & FLAG_CHECKED != 0) {
        if pin != Pin::V24_0 {
            return Ok(Outcome::OutOfDomain);
        }
        let blocks = compare_sequence(&Oracle::new(pin), bytes)?;
        return Ok(blocks.last().map_or(Outcome::OutOfDomain, |b| b.detailed.outcome));
    }
    compare_generated(bytes, pin, compare_dml_block)
}
