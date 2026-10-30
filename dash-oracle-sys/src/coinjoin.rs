//! The CoinJoin rules (ABI 26, v24 only; PLAN O2a-CJ1).
//!
//! src/coinjoin's checks that read no session state: the denomination rules, entry and
//! final-transaction validation over a coins view ([`CjView`]), collateral validation with the
//! caller's mempool answer, the queue and DSTX message hashes and signatures, the queue time
//! bound and the DSTX structure check. `oracle.h` ("CoinJoin rules") gives the formats and the
//! refused domain; dash-coinjoin is the Rust side (node-l1's `diff_coinjoin`).

use super::*;
use crate::context::{Context, write_coins};

/// A rule of `oracle_cj_rule` (`ORACLE_CJ_*`); `oracle.h` gives each one's argument count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CjRule {
    /// `AmountToDenomination(amount)`
    AmountToDenomination = 0,
    /// `DenominationToAmount(denom)`
    DenominationToAmount = 1,
    /// `IsCollateralAmount(amount)`
    IsCollateralAmount = 2,
    /// `CalculateAmountPriority(amount)`
    AmountPriority = 3,
    /// `GetLargerAdjacentDenom(denom)`
    LargerAdjacentDenom = 4,
    /// `GetSmallerAdjacentDenom(denom)`
    SmallerAdjacentDenom = 5,
    /// `AreAdjacentDenominations(a, b)`
    AdjacentDenominations = 6,
    /// `GetGapThreshold(goal)`
    GapThreshold = 7,
    /// `ShouldPromoteDenoms(smaller_count, larger_count, smaller_fully_mixed, goal)`
    ShouldPromote = 8,
    /// `ShouldDemoteDenoms(larger_count, smaller_count, larger_fully_mixed, goal)`
    ShouldDemote = 9,
    /// `ValidateFinalTxComposition(total_inputs, total_outputs, larger_inputs, larger_outputs)`
    FinalTxComposition = 10,
}

/// Which entry check `oracle_cj_entry` runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CjEntryKind {
    /// `ValidatePromotionEntry`
    Promotion = 0,
    /// `ValidateDemotionEntry`
    Demotion = 1,
}

/// A CoinJoin message: a queue (`dsq`, `CCoinJoinQueue`) or a mixing transaction broadcast
/// (`dstx`, `CCoinJoinBroadcastTx`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CjMessage {
    Dsq = 0,
    Dstx = 1,
}

/// The coins, mempool and InstantSend locks a CoinJoin rule reads (`oracle.h`, "view").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CjView {
    /// The tip's unspent coins.
    pub coins: Vec<ContextCoin>,
    /// Serialized transactions in the mempool (normal, non-coinbase, no shared txid or spent
    /// outpoint; anything else is refused).
    pub mempool: Vec<Vec<u8>>,
    /// The txids `IsLocked()` answers true for.
    pub locked: Vec<[u8; 32]>,
}

impl CjView {
    fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_coins(&mut out, &self.coins);
        write_compact_size(&mut out, self.mempool.len() as u64);
        for tx in &self.mempool {
            out.extend(tx);
        }
        write_compact_size(&mut out, self.locked.len() as u64);
        for txid in &self.locked {
            out.extend(txid);
        }
        out
    }
}

/// Core's verdict on an entry or the inputs and outputs of a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CjInOuts {
    pub accepted: bool,
    /// `PoolMessage` as an integer (`MSG_NOERR`, 19, on acceptance).
    pub message: i32,
    /// `fConsumeCollateralRet` (false for the entry checks, which have none).
    pub consume_collateral: bool,
    /// `SessionDenomCounts` (inputs, outputs) on acceptance; zero otherwise.
    pub counts: [u64; 2],
}

/// `IsCollateralValid()` and how often it asked the mempool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CjCollateral {
    pub valid: bool,
    /// `ProcessTransaction()` calls with `test_accept` true.
    pub test_accept_calls: u32,
    /// `ProcessTransaction()` calls with `test_accept` false (submissions).
    pub submit_calls: u32,
}

/// A CoinJoin message's hashes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CjHashes {
    /// `GetSignatureHash()`
    pub sig_hash: [u8; 32],
    /// The dsq's `GetHash()` (its inventory hash); `None` for a dstx.
    pub hash: Option<[u8; 32]>,
}

/// `CCoinJoinBroadcastTx::IsValidStructure()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CjDstxStructure {
    pub valid: bool,
    /// `fPossiblyValidPostV24Ret`
    pub possibly_valid_post_v24: bool,
}

impl Oracle {
    /// `rule(args)`: a bool result is 0 or 1. See `oracle.h` for each rule's argument domain.
    pub fn cj_rule(&self, rule: CjRule, args: &[i64]) -> Result<i64> {
        self.shared(|| {
            self.with_out(|out| unsafe {
                (self.api.cj_rule)(rule as c_int, args.as_ptr(), args.len(), out)
            })
        })
    }

    /// `ValidatePromotionEntry` or `ValidateDemotionEntry` of the serialized
    /// `std::vector<CTxIn>` and `std::vector<CTxOut>` at `session_denom`.
    pub fn cj_entry(
        &self,
        kind: CjEntryKind,
        vin: &[u8],
        vout: &[u8],
        session_denom: i32,
    ) -> Result<CjInOuts> {
        self.shared(|| {
            let mut message = 0i32;
            let accepted = self.status(unsafe {
                (self.api.cj_entry)(
                    kind as c_int,
                    vin.as_ptr(),
                    vin.len(),
                    vout.as_ptr(),
                    vout.len(),
                    session_denom,
                    &mut message,
                )
            })?;
            Ok(CjInOuts {
                accepted,
                message,
                consume_collateral: false,
                counts: [0; 2],
            })
        })
    }

    /// `CCoinJoinBaseSession::IsValidInOuts()` of the serialized inputs and outputs over `view`.
    #[allow(clippy::too_many_arguments)]
    pub fn cj_in_outs(
        &self,
        chain: &Chain,
        view: &CjView,
        vin: &[u8],
        vout: &[u8],
        session_denom: i32,
        allow_rebalance_shapes: bool,
        final_tx: bool,
    ) -> Result<CjInOuts> {
        let view = view.serialize();
        self.on_chain(chain, || {
            let (mut message, mut consume, mut counts) = (0i32, 0 as c_int, [0u64; 2]);
            let accepted = self.status(unsafe {
                (self.api.cj_in_outs)(
                    view.as_ptr(),
                    view.len(),
                    vin.as_ptr(),
                    vin.len(),
                    vout.as_ptr(),
                    vout.len(),
                    session_denom,
                    allow_rebalance_shapes.into(),
                    final_tx.into(),
                    &mut message,
                    &mut consume,
                    counts.as_mut_ptr(),
                )
            })?;
            Ok(CjInOuts {
                accepted,
                message,
                consume_collateral: consume != 0,
                counts,
            })
        })
    }

    /// `CoinJoin::IsCollateralValid()` of the serialized `tx` over `view`, with
    /// `ProcessTransaction()` answering `mempool`: `Some(base_fees)` for valid, `None` for
    /// invalid.
    pub fn cj_collateral(
        &self,
        chain: &Chain,
        view: &CjView,
        tx: &[u8],
        mempool: Option<i64>,
    ) -> Result<CjCollateral> {
        let view = view.serialize();
        self.on_chain(chain, || {
            let mut calls = [0u32; 2];
            let valid = self.status(unsafe {
                (self.api.cj_collateral)(
                    view.as_ptr(),
                    view.len(),
                    tx.as_ptr(),
                    tx.len(),
                    mempool.is_some().into(),
                    mempool.unwrap_or(0),
                    calls.as_mut_ptr(),
                )
            })?;
            Ok(CjCollateral {
                valid,
                test_accept_calls: calls[0],
                submit_calls: calls[1],
            })
        })
    }

    /// The hashes of a serialized dsq or dstx.
    pub fn cj_message_hash(&self, kind: CjMessage, msg: &[u8]) -> Result<CjHashes> {
        self.shared(|| {
            let (mut sig_hash, mut hash) = ([0u8; 32], [0u8; 32]);
            let hash_ptr = match kind {
                CjMessage::Dsq => hash.as_mut_ptr(),
                CjMessage::Dstx => std::ptr::null_mut(),
            };
            self.ok(unsafe {
                (self.api.cj_message_hash)(
                    kind as c_int,
                    msg.as_ptr(),
                    msg.len(),
                    sig_hash.as_mut_ptr(),
                    hash_ptr,
                )
            })?;
            Ok(CjHashes {
                sig_hash,
                hash: (kind == CjMessage::Dsq).then_some(hash),
            })
        })
    }

    /// `CheckSignature(pubkey)` of a serialized dsq or dstx, the operator key decoded in the
    /// legacy scheme when `pubkey_legacy`.
    pub fn cj_check_signature(
        &self,
        kind: CjMessage,
        msg: &[u8],
        pubkey: &[u8; 48],
        pubkey_legacy: bool,
    ) -> Result<bool> {
        self.shared(|| {
            self.status(unsafe {
                (self.api.cj_check_signature)(
                    kind as c_int,
                    msg.as_ptr(),
                    msg.len(),
                    pubkey.as_ptr(),
                    pubkey_legacy.into(),
                )
            })
        })
    }

    /// `CCoinJoinQueue::IsTimeOutOfBounds(current_time)` of a serialized dsq.
    pub fn cj_queue_time_out_of_bounds(&self, dsq: &[u8], current_time: i64) -> Result<bool> {
        self.shared(|| {
            let out: c_int = self.with_out(|out| unsafe {
                (self.api.cj_queue_time_out_of_bounds)(dsq.as_ptr(), dsq.len(), current_time, out)
            })?;
            Ok(out != 0)
        })
    }

    /// `CCoinJoinBroadcastTx::IsValidStructure()` of a serialized dstx at the context's tip.
    pub fn cj_dstx_structure(
        &self,
        chain: &Chain,
        dstx: &[u8],
        ctx: &Context,
    ) -> Result<CjDstxStructure> {
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            let mut possibly = 0 as c_int;
            let valid = self.status(unsafe {
                (self.api.cj_dstx_structure)(
                    dstx.as_ptr(),
                    dstx.len(),
                    ctx.as_ptr(),
                    ctx.len(),
                    &mut possibly,
                )
            })?;
            Ok(CjDstxStructure {
                valid,
                possibly_valid_post_v24: possibly != 0,
            })
        })
    }
}
