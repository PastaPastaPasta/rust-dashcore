//! Transaction checks (CS3, CS5b; FR-CON-06 to -09): Core's `CheckTransaction`,
//! `ContextualCheckTransaction` and `IsFinalTx` (CS3), the input checks and sigop counts that read
//! the spent coins and the relative lock times ([`inputs`], [`locks`]; CS5b), with Core's reject
//! reasons and result classes.
//!
//! Every type test compares `tx_type().to_base()` numerically, as Core compares `nType`, and
//! every version test goes through [`Tx`]'s predicates, as Core calls `CTransaction`'s.

use dash_params::BuriedDeployment;
use dash_types::Numeric as _;

use crate::ctx::{ConsensusCtx, Deployment};
use crate::primitives::canonical::{
    TRANSACTION_ASSET_UNLOCK, TRANSACTION_COINBASE, TRANSACTION_NORMAL,
};
use crate::script::interpreter::{LOCKTIME_THRESHOLD, SEQUENCE_FINAL};
use crate::validation::{TxValidationError, TxValidationResult};
use crate::{MAX_MONEY, Tx, money_range};

pub mod inputs;
pub mod locks;

pub use inputs::{
    COINBASE_MATURITY, MissingCoin, check_tx_inputs, p2sh_sigop_count, transaction_sigop_count,
};
pub use locks::{
    LockTimeFlags, SequenceLocks, block_lock_time_flags, calculate_sequence_locks,
    evaluate_sequence_locks, sequence_locks,
};

/// `MAX_LEGACY_BLOCK_SIZE`: the block size limit before DIP0001, and the transaction size limit.
// Core: consensus/consensus.h:10
pub const MAX_LEGACY_BLOCK_SIZE: u32 = 1_000_000;
/// `MAX_DIP0001_BLOCK_SIZE`.
// Core: consensus/consensus.h:11
pub const MAX_DIP0001_BLOCK_SIZE: u32 = 2_000_000;
/// `MAX_TX_EXTRA_PAYLOAD`: the largest `vExtraPayload` `CheckTransaction` accepts.
// Core: consensus/consensus.h:22
pub const MAX_TX_EXTRA_PAYLOAD: usize = 10_000;
/// `MAX_STANDARD_TX_SIZE`: a policy constant that is also consensus from DIP0001
/// (`ContextualCheckTransaction`, with `>`; the mempool's policy check uses `>=`: quirk 3).
// Core: policy/policy.h:27
pub const MAX_STANDARD_TX_SIZE: u32 = 100_000;

// The transaction types this module uses besides `canonical`'s (0, 5 and 9).
// Core: primitives/transaction.h:29 TxType
/// `TRANSACTION_QUORUM_COMMITMENT`.
pub const TRANSACTION_QUORUM_COMMITMENT: u16 = 6;
/// `TRANSACTION_MNHF_SIGNAL`.
pub const TRANSACTION_MNHF_SIGNAL: u16 = 7;
/// `TRANSACTION_PROVIDER_UPDATE_SHARED_REGISTRAR`, v24's highest transaction type. Every type
/// from `TRANSACTION_NORMAL` (0) up to it is one `ContextualCheckTransaction` lists.
pub const TRANSACTION_PROVIDER_UPDATE_SHARED_REGISTRAR: u16 = 12;

/// `MaxBlockSize(fDIP0001Active)`.
// Core: consensus/consensus.h:12 MaxBlockSize
pub const fn max_block_size(dip0001_active: bool) -> u32 {
    if dip0001_active {
        MAX_DIP0001_BLOCK_SIZE
    } else {
        MAX_LEGACY_BLOCK_SIZE
    }
}

/// `MaxBlockSigOps(fDIP0001Active)`: 20,000 before DIP0001, 40,000 from it.
// Core: consensus/consensus.h:17 MaxBlockSigOps
pub const fn max_block_sigops(dip0001_active: bool) -> u32 {
    max_block_size(dip0001_active) / 50
}

fn consensus(reason: &'static str) -> TxValidationError {
    TxValidationError::new(TxValidationResult::Consensus, reason)
}

/// Core's `CheckTransaction`: the checks that need no context. Every rejection is
/// `TX_CONSENSUS` with no debug message.
///
/// The empty-input and empty-output allowances and the one-byte coinbase scriptSig key on the
/// type alone, whatever the version (quirk 2).
// Core: consensus/tx_check.cpp:14 CheckTransaction
pub fn check_transaction(tx: &Tx) -> Result<(), TxValidationError> {
    let tx_type = tx.tx_type().to_base();
    let (mut allow_empty_in, mut allow_empty_out) = (false, false);
    if tx_type == TRANSACTION_QUORUM_COMMITMENT || tx_type == TRANSACTION_MNHF_SIGNAL {
        allow_empty_in = true;
        allow_empty_out = true;
    }
    if tx_type == TRANSACTION_ASSET_UNLOCK {
        allow_empty_in = true;
    }

    if !allow_empty_in && tx.inputs().is_empty() {
        return Err(consensus("bad-txns-vin-empty"));
    }
    if !allow_empty_out && tx.outputs().is_empty() {
        return Err(consensus("bad-txns-vout-empty"));
    }
    // `GetSerializeSize(tx)`: Core's serialization, which is what `Tx::size` measures.
    if tx.size() > MAX_LEGACY_BLOCK_SIZE {
        return Err(consensus("bad-txns-oversize"));
    }
    if tx.extra_payload().len() > MAX_TX_EXTRA_PAYLOAD {
        return Err(consensus("bad-txns-payload-oversize"));
    }

    // Negative or overflowing output values (CVE-2010-5139). Each value is at most MAX_MONEY
    // and the running total is in range before each addition, so the sum cannot overflow.
    let mut value_out: i64 = 0;
    for output in tx.outputs() {
        if output.value < 0 {
            return Err(consensus("bad-txns-vout-negative"));
        }
        if output.value > MAX_MONEY {
            return Err(consensus("bad-txns-vout-toolarge"));
        }
        value_out += output.value;
        if !money_range(value_out) {
            return Err(consensus("bad-txns-txouttotal-toolarge"));
        }
    }

    // Duplicate inputs (CVE-2018-17144). Core inserts into a `std::set`; any duplicate rejects,
    // so sorting and comparing neighbours gives the same verdict.
    let mut prevouts: Vec<_> = tx
        .inputs()
        .iter()
        .map(|input| (*input.prevout.hash.as_bytes(), input.prevout.index))
        .collect();
    prevouts.sort_unstable();
    if prevouts.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(consensus("bad-txns-inputs-duplicate"));
    }

    if tx.is_coinbase() {
        // With CbTx the height moved into the payload, so its scriptSig may be one byte.
        let min_cb_size = if tx_type == TRANSACTION_COINBASE {
            1
        } else {
            2
        };
        let size = tx.inputs()[0].script_sig.as_bytes().len();
        if size < min_cb_size || size > 100 {
            return Err(consensus("bad-cb-length"));
        }
    } else if tx.inputs().iter().any(|input| input.prevout.is_null()) {
        return Err(consensus("bad-txns-prevout-null"));
    }
    Ok(())
}

/// Core's `ContextualCheckTransaction` for a transaction in the block after `ctx.prev`: the type
/// rules from DIP0003 and the 100,000-byte size limit from DIP0001 (quirk 3). Every rejection is
/// `TX_CONSENSUS` with no debug message.
// Core: validation.cpp:351 ContextualCheckTransaction
pub fn contextual_check_transaction(tx: &Tx, ctx: &ConsensusCtx) -> Result<(), TxValidationError> {
    let dip0001 =
        ctx.deployments.active_after(ctx.prev, Deployment::Buried(BuriedDeployment::Dip0001));
    let dip0003 =
        ctx.deployments.active_after(ctx.prev, Deployment::Buried(BuriedDeployment::Dip0003));

    if dip0003 {
        let tx_type = tx.tx_type().to_base();
        if tx.is_special_tx_version() {
            // Core lists every type from TRANSACTION_NORMAL to
            // TRANSACTION_PROVIDER_UPDATE_SHARED_REGISTRAR.
            if tx_type > TRANSACTION_PROVIDER_UPDATE_SHARED_REGISTRAR {
                return Err(consensus("bad-txns-type"));
            }
            if tx.is_coinbase() && tx_type != TRANSACTION_COINBASE {
                return Err(consensus("bad-txns-cb-type"));
            }
        } else if tx_type != TRANSACTION_NORMAL {
            return Err(consensus("bad-txns-type"));
        }
    }

    if dip0001 && tx.size() > MAX_STANDARD_TX_SIZE {
        return Err(consensus("bad-txns-oversize"));
    }
    Ok(())
}

/// Core's `IsFinalTx`: whether the transaction's lock time lets it into a block at
/// `block_height` with lock-time cutoff `block_time`.
// Core: consensus/tx_verify.cpp:19 IsFinalTx
pub fn is_final_tx(tx: &Tx, block_height: i32, block_time: i64) -> bool {
    let lock_time = i64::from(tx.lock_time());
    if lock_time == 0 {
        return true;
    }
    let limit = if lock_time < i64::from(LOCKTIME_THRESHOLD) {
        i64::from(block_height)
    } else {
        block_time
    };
    if lock_time < limit {
        return true;
    }
    // An unsatisfied lock time is ignored when every input's sequence is final.
    tx.inputs().iter().all(|input| input.sequence == SEQUENCE_FINAL)
}
