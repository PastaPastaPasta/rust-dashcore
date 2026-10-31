//! EV2b's block build (`dash-dml`'s `build_new_list_from_block`) against Core v24's
//! `RebuildListFromBlock` (`oracle_dml_apply`, O2a-EV): one block over one context's list.
//!
//! Our side reads everything from the oracle's [`Context`]: the tip list (evodb bytes), the
//! headers (the ancestry a QcTx reads), the coins and whether their best block is the tip, the
//! versionbits deployments, and the quorum members Core's `GetAllQuorumMembers` returns. V19 and
//! MN_RR are buried: active from their heights in the chain's parameters.
//!
//! Compared: the payee of the tip list (`GetMNPayee`), the verdict (Core's reject reason; an
//! exception is Core's `failed-procspectxsinblock`), and on acceptance the new list's canonical
//! bytes, which hold every state, the counter, the internal ids and the collision order. Before
//! that, `dash-dml`'s reference twin builds the same block and `xcheck` compares it with ours
//! (EV2b D24): the same error, or lists equal in every structure.
//!
//! Not compared ([`Outcome::OutOfDomain`]): a context without a list, a block either side
//! refuses to decode, a QcTx whose members the context does not supply, and a state Core's
//! checked path never produces (EV2b D2, D12). For the QcTx, the shim's `GetAllQuorumMembers`
//! seam throws and the call fails with `ORACLE_E_CONTEXT` before our result is read; our member
//! source reports `MemberError::Missing`, a store fault, so if the shim ever computed those
//! members itself the input would fail as a "store fault" divergence, not pass silently.

use dash_consensus::primitives::{Tx, decode_tx};
use dash_dml::specialtx::reasons::{
    BAD_PROTX_DUP_KEY, BAD_PROUPSHARE_PAYEE_REUSE, BAD_PROUPSHAREDREG_PAYEE_REUSE,
    BAD_SHARED_COLLATERAL_SPEND,
};
use dash_dml::testkit::replay::{
    CoreVerdict, ReplayCoin, ReplayContext, ReplayQuorum, Replayed, Vector, replay,
};
use dash_dml::{DmlBuildError, DmlException, MnList, NoTrace, UniqueKey};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView as CoreCoins, Context, ContextCoin, ErrorKind,
    MnList as CoreList, Oracle, Pin, QuorumMembers, ThresholdState, Verdict,
};
use dash_params::ChainParams;
use dash_primitives::{OutPoint, SpecialPayload};

use crate::dml::{canonical, class, core_text};
use crate::inputs::oracle_failure;
use crate::{Compared, Outcome, check, divergence};

pub mod checked;
mod input;
pub mod window;

pub use input::{
    Case, FLAG_FORCE_V24, FLAG_UNCHECKED_COINS, Input, MAX_TXS, Prior, assemble, cases, chain_for,
    generate, split_block, testnet_case, with_one_tx,
};

/// `DEPLOYMENT_V24`'s position.
pub(crate) const DEPLOYMENT_V24: u8 = 1;

/// Why our side has nothing to compare.
enum Skip {
    /// The context supplies no list: nothing to build from.
    NoList,
    /// One of our readers refuses an input (the block, the list, a header, a member): a
    /// divergence unless Core refuses the input too.
    Refused(&'static str),
}

/// The oracle's context as `dash-dml`'s replay context (EV2b D25).
pub fn replay_context(ctx: &Context) -> Option<ReplayContext> {
    let CoreList::Supplied(list) = &ctx.mn_list else {
        return None;
    };
    let (coins, coins_at_tip) = match &ctx.coins {
        CoreCoins::Unchecked(coins) => (coins, false),
        CoreCoins::AtTip(coins) => (coins, true),
    };
    Some(ReplayContext {
        first_height: ctx.first_height,
        headers: ctx.headers.clone(),
        coins: coins
            .iter()
            .map(|c| ReplayCoin {
                txid: c.txid,
                vout: c.vout,
                height: c.height,
                coinbase: c.coinbase,
                amount: c.amount,
                script_pubkey: c.script_pubkey.clone(),
            })
            .collect(),
        coins_at_tip,
        quorums: ctx
            .quorums
            .iter()
            .map(|q| ReplayQuorum {
                llmq_type: q.llmq_type,
                quorum_hash: q.quorum_hash,
                members: q.members.clone(),
            })
            .collect(),
        list: list.clone(),
        v24: ctx.deployments.contains(&(DEPLOYMENT_V24, ThresholdState::Active)),
    })
}

/// What a recorded vector keeps for the oracle alone (`Vector::oracle`, EV2b D25): the chain
/// (`v24`: V24 always active), the BLS scheme, the adjusted time and the deployments. `None`
/// for a context with checkpoints on, which a vector does not record.
pub fn oracle_fields(ctx: &Context, chain_v24: bool) -> Option<Vec<(String, String)>> {
    if ctx.checkpoints != Checkpoints::Disabled {
        return None;
    }
    let deployments: Vec<String> =
        ctx.deployments.iter().map(|&(d, s)| format!("{d}:{}", s as u8)).collect();
    Some(vec![
        ("chain_v24".into(), u8::from(chain_v24).to_string()),
        ("bls_legacy".into(), u8::from(ctx.bls_legacy).to_string()),
        ("adjusted_time".into(), ctx.adjusted_time.to_string()),
        ("deployments".into(), deployments.join(",")),
    ])
}

/// The oracle's chain and context of a recorded vector; `None` when a field is malformed or
/// the deployments disagree with the vector's `v24`.
pub fn oracle_context(v: &Vector) -> Option<(Chain, Context)> {
    let get = |key: &str| v.oracle.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let deployments = get("deployments")?
        .split(',')
        .filter(|d| !d.is_empty())
        .map(|d| {
            let (pos, state) = d.split_once(':')?;
            Some((pos.parse().ok()?, ThresholdState::try_from(state.parse::<u8>().ok()?).ok()?))
        })
        .collect::<Option<Vec<_>>>()?;
    let c = &v.ctx;
    if deployments.contains(&(DEPLOYMENT_V24, ThresholdState::Active)) != c.v24 {
        return None;
    }
    let coins = c
        .coins
        .iter()
        .map(|c| ContextCoin {
            txid: c.txid,
            vout: c.vout,
            height: c.height,
            coinbase: c.coinbase,
            amount: c.amount,
            script_pubkey: c.script_pubkey.clone(),
        })
        .collect();
    let ctx = Context::new(
        c.first_height,
        c.headers.clone(),
        get("bls_legacy")? == "1",
        deployments,
        get("adjusted_time")?.parse().ok()?,
        Checkpoints::Disabled,
        if c.coins_at_tip {
            CoreCoins::AtTip(coins)
        } else {
            CoreCoins::Unchecked(coins)
        },
        CoreList::Supplied(c.list.clone()),
        c.quorums
            .iter()
            .map(|q| QuorumMembers {
                llmq_type: q.llmq_type,
                quorum_hash: q.quorum_hash,
                members: q.members.clone(),
            })
            .collect(),
    );
    Some((chain_for(get("chain_v24")? == "1"), ctx))
}

/// Core's payee and result for `block` over `ctx`, as a quirk vector records them (EV2b D25);
/// `None` when the oracle refuses the call.
pub fn core_verdict(
    oracle: &Oracle,
    chain: &Chain,
    ctx: &Context,
    block: &[u8],
) -> Option<(Option<[u8; 32]>, CoreVerdict)> {
    match oracle.dml_apply(chain, block, ctx) {
        Ok(core) => Some((
            core.payee,
            match core.verdict {
                Verdict::Accept => CoreVerdict::Accept(core.canonical?),
                Verdict::Reject(r) => CoreVerdict::Reject(r.reason),
            },
        )),
        Err(e) if e.kind == ErrorKind::Exception => Some((None, CoreVerdict::Exception(e.message))),
        Err(_) => None,
    }
}

/// Splitmix64 bytes from `seed`: the examples' inputs.
pub fn splitmix_bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut state = seed;
    (0..n.div_ceil(8))
        .flat_map(|_| {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            (z ^ (z >> 31)).to_le_bytes()
        })
        .take(n)
        .collect()
}

/// Our side of `oracle_dml_apply`, with the reference twin's (EV2b D24).
fn replay_ours(params: &ChainParams, ctx: &Context, block: &[u8]) -> Result<Replayed, Skip> {
    let ctx = replay_context(ctx).ok_or(Skip::NoList)?;
    replay(params, &ctx, block, &mut NoTrace).map_err(Skip::Refused)
}

/// The text of D12's out-of-domain ProUpServ (`dash-dml` `build/mod.rs`): one Core accepts.
const D12_NETINFO_KIND: &str = "ProUpServTx of the other NetInfo kind before V24";

/// Our refusals that are not verdicts, against Core's result (`core_accepted`): this harness's
/// coins never fail, so a store fault is a divergence; Core accepts D12's ProUpServ; the other
/// out-of-domain inputs are ones Core's own checks refuse first or leave undefined. `None` for
/// any other result of ours.
fn ours_out_of_domain(
    ours: &Result<MnList, DmlBuildError>,
    core_accepted: bool,
) -> Option<Compared> {
    match ours {
        Err(DmlBuildError::Store(_)) => Some(Err(divergence("store fault"))),
        Err(DmlBuildError::Exception(DmlException::OutOfDomain(what))) => {
            Some(if *what == D12_NETINFO_KIND && !core_accepted {
                Err(divergence("D12 verdict"))
            } else {
                Ok(Outcome::OutOfDomain)
            })
        }
        _ => None,
    }
}

/// `diff_dml_apply`'s comparison of one block over one context.
pub fn compare_dml_apply(
    oracle: &Oracle,
    chain: &Chain,
    params: &ChainParams,
    ctx: &Context,
    block: &[u8],
) -> Compared {
    let core = oracle.dml_apply(chain, block, ctx);
    let ours = match replay_ours(params, ctx, block) {
        Ok(ours) => ours,
        Err(Skip::NoList) => return Ok(Outcome::OutOfDomain),
        Err(Skip::Refused(what)) => {
            return match core {
                Ok(_) => Err(divergence(what)),
                Err(_) => Ok(Outcome::OutOfDomain),
            };
        }
    };
    ours.twin.map_err(|e| divergence(e.0))?;
    let core = match core {
        Ok(core) => core,
        // An exception escaping `RebuildListFromBlock`, which `ProcessSpecialTxsInBlock` turns
        // into `failed-procspectxsinblock` (`specialtxman.cpp` v24:993-995). The shim loses the
        // payee with its outputs.
        Err(e) if e.kind == ErrorKind::Exception => {
            if let Some(compared) = ours_out_of_domain(&ours.result, false) {
                return compared;
            }
            return match ours.result {
                Err(DmlBuildError::Exception(DmlException::List(err))) => {
                    let core = class(core_text(&e.message));
                    check(class(&err.core_message()) == core, "exception")?;
                    Ok(Outcome::BothReject)
                }
                Err(DmlBuildError::Exception(DmlException::Members(_))) => Ok(Outcome::BothReject),
                _ => Err(divergence("verdict")),
            };
        }
        Err(e) => return oracle_failure(&e),
    };
    if ours.payee != core.payee {
        return Err(divergence("payee"));
    }
    if let Some(compared) = ours_out_of_domain(&ours.result, core.verdict == Verdict::Accept) {
        return compared;
    }
    match (ours.result, &core.verdict) {
        (Ok(list), Verdict::Accept) => {
            let core_canonical = core.canonical.ok_or(divergence("core list"))?;
            check(canonical(&list) == core_canonical, "list")?;
            Ok(Outcome::BothAccept)
        }
        (Err(e), Verdict::Reject(core)) => {
            check(e.reject_reason() == Some(core.reason.as_str()), "reason")?;
            Ok(Outcome::BothReject)
        }
        _ => Err(divergence("verdict")),
    }
}

/// `diff_dml_apply`: a block of special transactions generated against a crafted case's list
/// ([`input`]), through [`compare_dml_apply`].
pub fn compare_apply(bytes: &[u8], pin: Pin) -> Compared {
    compare_generated(bytes, pin, compare_dml_apply)
}

/// A comparator of one block over one context, as [`compare_dml_apply`].
pub type BlockComparator = fn(&Oracle, &Chain, &ChainParams, &Context, &[u8]) -> Compared;

/// `compare` on the block and context `bytes` generate ([`generate`]); v24.0 only.
pub fn compare_generated(bytes: &[u8], pin: Pin, compare: BlockComparator) -> Compared {
    if pin != Pin::V24_0 {
        return Ok(Outcome::OutOfDomain);
    }
    let Some(input) = generate(bytes) else {
        return Ok(Outcome::OutOfDomain);
    };
    compare(&Oracle::new(pin), &input.chain(), &input.case.params, &input.ctx, &input.block)
}

/// A shared registration accepted (`specialtxman.cpp` v24:358-453).
pub const SHARED_PROREG: &str = "shared ProReg accepted";
/// A ProUpShareTx accepted (v24:619-648).
pub const PROUPSHARE: &str = "ProUpShare accepted";
/// A ProUpSharedRegTx accepted (v24:649-701).
pub const PROUPSHAREDREG: &str = "ProUpSharedReg accepted";
/// A shared registration whose share owner key is already in the list (v24:417-419).
pub const SHARE_OWNER_DUP_KEY: &str = "shared ProReg bad-protx-dup-key";
/// A ProUpSharedRegTx taking an operator key the list holds, the ungated probe (v24:688-690).
pub const SHAREDREG_DUP_KEY: &str = "ProUpSharedReg bad-protx-dup-key";

/// The shared-masternode paths of `RebuildListFromBlock` that L1's public seeds must reach
/// (EV2b D22): a shared registration, a share update and a shared registrar update accepted, and
/// every share-specific reject. The three rejects with their own reason are named by it.
pub const SHARE_PATHS: [&str; 8] = [
    SHARED_PROREG,
    PROUPSHARE,
    PROUPSHAREDREG,
    BAD_PROUPSHARE_PAYEE_REUSE,
    BAD_PROUPSHAREDREG_PAYEE_REUSE,
    BAD_SHARED_COLLATERAL_SPEND,
    SHARE_OWNER_DUP_KEY,
    SHAREDREG_DUP_KEY,
];

/// A transaction's special payload, if it has one that decodes.
pub fn payload(tx: &Tx) -> Option<&SpecialPayload> {
    tx.payload()?.ok().map(|d| &d.payload)
}

/// The [`SHARE_PATHS`] our build reaches on a generated input (no oracle call): the accepted
/// share transactions of an accepted block, or the share-specific check that rejected it.
pub fn share_paths(bytes: &[u8]) -> Vec<&'static str> {
    let Some(input) = generate(bytes) else {
        return vec![];
    };
    let Ok(built) = replay_ours(&input.case.params, &input.ctx, &input.block) else {
        return vec![];
    };
    match built.result {
        Ok(_) => {
            let mut paths: Vec<_> = input
                .txs()
                .iter()
                .filter_map(|tx| match payload(tx)? {
                    SpecialPayload::ProviderRegister(p) if !p.shares.is_empty() => {
                        Some(SHARED_PROREG)
                    }
                    SpecialPayload::ProviderUpdateShare(_) => Some(PROUPSHARE),
                    SpecialPayload::ProviderUpdateSharedRegistrar(_) => Some(PROUPSHAREDREG),
                    _ => None,
                })
                .collect();
            paths.sort_unstable();
            paths.dedup();
            paths
        }
        Err(e) => match e.reject_reason() {
            Some(BAD_PROUPSHARE_PAYEE_REUSE) => vec![BAD_PROUPSHARE_PAYEE_REUSE],
            Some(BAD_PROUPSHAREDREG_PAYEE_REUSE) => vec![BAD_PROUPSHAREDREG_PAYEE_REUSE],
            Some(BAD_SHARED_COLLATERAL_SPEND) => vec![BAD_SHARED_COLLATERAL_SPEND],
            Some(BAD_PROTX_DUP_KEY) => dup_key_path(&input).into_iter().collect(),
            _ => vec![],
        },
    }
}

/// The share-specific check that rejected `input`'s block with `bad-protx-dup-key`, if one did
/// (Opus reviews r2 C1, r3). Core returns that reason from seven checks, all in its first loop
/// over the block, which stops at the first failing transaction (`specialtxman.cpp` v24:349-728):
/// the last transaction of the shortest prefix our build rejects with it. A ProUpSharedRegTx has
/// one such check (v24:688-690). A shared registration failed the share owner check
/// (v24:417-419) if a share owner key is held, in the list the transactions before it build, by
/// a masternode other than the one it replaces by collateral (removed first, v24:389-401). That
/// list can only lack keys the loop still held (the collateral spends after it remove
/// masternodes), so a key found there was held.
fn dup_key_path(input: &Input) -> Option<&'static str> {
    let (header, txs) = split_block(&input.block)?;
    let build = |n: usize| {
        replay_ours(&input.case.params, &input.ctx, &assemble(header, &txs[..n]))
            .ok()
            .map(|o| o.result)
    };
    let culprit = (2..=txs.len()).find(
        |&n| matches!(build(n), Some(Err(e)) if e.reject_reason() == Some(BAD_PROTX_DUP_KEY)),
    )? - 1;
    let tx = decode_tx(&mut &txs[culprit][..]).ok()?.into_value();
    match payload(&tx)? {
        SpecialPayload::ProviderUpdateSharedRegistrar(_) => Some(SHAREDREG_DUP_KEY),
        SpecialPayload::ProviderRegister(p) if !p.shares.is_empty() => {
            let before = build(culprit)?.ok()?;
            let collateral = OutPoint {
                hash: if p.collateral_hash.is_null() {
                    tx.txid()
                } else {
                    p.collateral_hash
                },
                index: p.collateral_index,
            };
            let replaced = before.get_by_collateral(&collateral).map(|d| d.pro_tx_hash);
            p.shares
                .iter()
                .filter_map(|s| before.unique_owner(&UniqueKey::key_id(&s.key_id_owner)))
                .any(|holder| Some(holder.pro_tx_hash) != replaced)
                .then_some(SHARE_OWNER_DUP_KEY)
        }
        _ => None,
    }
}

/// Both sides of a `diff_dml_apply` input, for triage (`examples/dml_explain.rs`): the
/// case, the block's transactions, Core's result and ours.
pub fn explain(bytes: &[u8]) -> String {
    let Some(input) = generate(bytes) else {
        return String::from("no input");
    };
    let short = |p: [u8; 32]| p[..4].to_vec();
    let core = Oracle::new(Pin::V24_0).dml_apply(&input.chain(), &input.block, &input.ctx);
    let core = match core {
        Ok(core) => format!("{:?} payee {:02x?}", core.verdict, core.payee.map(short)),
        Err(e) => format!("{:?}: {}", e.kind, e.message),
    };
    let ours = match replay_ours(&input.case.params, &input.ctx, &input.block) {
        Err(Skip::NoList) => String::from("no list"),
        Err(Skip::Refused(what)) => format!("refused: {what}"),
        Ok(o) => format!(
            "{:?} payee {:02x?}",
            o.result.as_ref().map(|_| "accept").map_err(ToString::to_string),
            o.payee.map(short)
        ),
    };
    let txs: Vec<_> = input.txs().iter().map(|tx| format!("{:?}", tx.payload())).collect();
    format!(
        "case {} (v24 {})\n  {}\ncore: {core}\nours: {ours}",
        input.case.name,
        input.v24,
        txs.join("\n  ")
    )
}
