//! CS3's targets: `diff_check_transaction`, `diff_check_block`, `diff_contextual_check_tx`,
//! `diff_contextual_check_block_header` and `diff_contextual_check_block`, the node's checks
//! against Core's `CheckTransaction`, `CheckBlock`, `ContextualCheckTransaction`,
//! `ContextualCheckBlockHeader` and `ContextualCheckBlock` through the oracle (O2a).
//!
//! The input formats and the node's side are in `check_inputs` (shared with dash-consensus's
//! recorded-verdict tests). Compared: Core's verdict, result class, reject reason and debug
//! message; for `diff_check_transaction` also the legacy sigop count.
//!
//! The checks are v24's (DEC-82): at any other pin there is nothing to compare. Inputs the oracle
//! refuses as outside its domain (a window too short for what Core reads, a clock it cannot
//! represent, an empty coinbase under the height rule) are not compared either.

use dash_oracle_sys::{
    Checkpoints, CoinsView, Context, ErrorKind, MnList, Oracle, Pin, ResultCode, Verdict,
};

use crate::check_inputs::{Input, Network, Window};
use crate::{Compared, Divergence, Outcome, check, divergence};

/// Core's side of a check.
pub enum CoreCheck {
    Verdict(Verdict),
    /// Core's deserializer rejects the bytes (or leaves some unread).
    DecodeReject,
    /// The oracle refuses the call (`ORACLE_E_CONTEXT`, `ORACLE_E_ARG`).
    OutOfDomain,
}

impl CoreCheck {
    pub fn from_result(result: dash_oracle_sys::Result<Verdict>) -> Result<Self, Divergence> {
        match result {
            Ok(verdict) => Ok(CoreCheck::Verdict(verdict)),
            Err(e) => match e.kind {
                ErrorKind::Decode | ErrorKind::Trailing => Ok(CoreCheck::DecodeReject),
                ErrorKind::Context | ErrorKind::Argument => Ok(CoreCheck::OutOfDomain),
                _ => Err(divergence("oracle error")),
            },
        }
    }
}

/// The node's verdict in Core's terms (`check_inputs::Verdict`).
type Ours = crate::check_inputs::Verdict;

/// A check's result class (`TxValidationResult` or `BlockValidationResult`); `None` for a
/// boolean result, which no check returns.
pub fn result_code(code: &ResultCode) -> Option<i32> {
    match *code {
        ResultCode::Tx(r) | ResultCode::Block(r) => Some(r),
        ResultCode::Bool => None,
    }
}

/// Compares two verdicts: acceptance, then the result class, reject reason and debug message.
pub fn compare_verdicts(core: &Verdict, ours: &Ours) -> Compared {
    match (core, ours) {
        (Verdict::Accept, Ok(())) => Ok(Outcome::BothAccept),
        (Verdict::Reject(core), Err((result, reason, debug))) => {
            check(result_code(&core.result) == Some(*result), "result class")?;
            check(matches(&core.reason, core.reason_truncated, reason), "reject reason")?;
            check(matches(&core.debug_message, core.debug_truncated, debug), "debug message")?;
            Ok(Outcome::BothReject)
        }
        _ => Err(divergence("verdict")),
    }
}

/// Core's string against ours: equal, or a prefix of ours where the shim truncated it.
fn matches(core: &str, truncated: bool, ours: &str) -> bool {
    if truncated {
        ours.starts_with(core)
    } else {
        core == ours
    }
}

/// Compares the node's side of `input` (`None`: the object does not decode) with Core's; the
/// node's check does not run when Core's falls outside the oracle's domain.
fn compare(core: CoreCheck, input: &Input) -> Compared {
    match (core, input.ours()) {
        (CoreCheck::OutOfDomain, _) => Ok(Outcome::OutOfDomain),
        (CoreCheck::DecodeReject, None) => Ok(Outcome::BothReject),
        (CoreCheck::Verdict(core), Some(ours)) => compare_verdicts(&core, &ours),
        (CoreCheck::DecodeReject, Some(_)) | (CoreCheck::Verdict(_), None) => {
            Err(divergence("decode verdict"))
        }
    }
}

/// The oracle's chain selection for a network.
pub fn chain(network: Network) -> dash_oracle_sys::Chain {
    let (name, args) = network.core_chain();
    dash_oracle_sys::Chain::new(name, args)
}

/// The oracle's context for a block or transaction on top of `window`, with the node's clock at
/// `now` (the oracle's adjusted time; 0 is unset, which only the header check reads).
pub fn context(window: &Window, now: i64) -> Context {
    let params = window.params();
    let height = window.tip() + 1;
    Context::new(
        window.first_height as i32,
        window.headers.clone(),
        height < params.consensus.v19_height.to_u32(),
        vec![],
        now,
        match window.best_header_height {
            None => Checkpoints::Disabled,
            Some(best) => Checkpoints::Enabled {
                best_header_height: best as i32,
            },
        },
        CoinsView::Unchecked(vec![]),
        MnList::Absent,
        vec![],
    )
}

/// Core's side of a CS3 input (v24's oracle).
pub fn core_check(input: &Input) -> Result<CoreCheck, Divergence> {
    let oracle = Oracle::new(Pin::V24_0);
    CoreCheck::from_result(match input {
        Input::CheckTransaction(bytes) => oracle.check_transaction(bytes),
        Input::CheckBlock(i) => {
            oracle.check_block(&chain(i.network), i.block, i.check_pow, i.check_merkle)
        }
        Input::ContextualTx(i) => {
            oracle.contextual_check_tx(&chain(i.window.network), i.object, &context(&i.window, 0))
        }
        Input::ContextualBlock(i) => oracle.contextual_check_block(
            &chain(i.window.network),
            i.object,
            &context(&i.window, 0),
        ),
        Input::ContextualHeader(i) => oracle.contextual_check_header(
            &chain(i.window.network),
            &i.header,
            &context(&i.window, i.now),
        ),
    })
}

/// Compares the CS3 target `target` on `bytes`: nothing at pins other than v24, or when the
/// input is too short for the target's format.
fn compare_target(target: &str, bytes: &[u8], pin: Pin) -> Compared {
    if pin != Pin::V24_0 {
        return Ok(Outcome::OutOfDomain);
    }
    let Some(input) = Input::parse(target, bytes) else {
        return Ok(Outcome::OutOfDomain);
    };
    compare(core_check(&input)?, &input)
}

/// `diff_check_transaction`: `CheckTransaction` and `GetLegacySigOpCount` of one transaction
/// (the bytes exactly).
pub fn compare_check_transaction(bytes: &[u8], pin: Pin) -> Compared {
    let outcome = compare_target("diff_check_transaction", bytes, pin)?;
    if let (Ok(core), Ok(tx)) =
        (Oracle::new(pin).legacy_sigops(bytes), dash_consensus::decode_tx_exact(bytes))
    {
        check(core == tx.value().legacy_sigop_count(), "legacy sigops")?;
    }
    Ok(outcome)
}

/// `diff_check_block`: `CheckBlock` on one of the networks, with or without the proof of work
/// and merkle checks.
pub fn compare_check_block(bytes: &[u8], pin: Pin) -> Compared {
    compare_target("diff_check_block", bytes, pin)
}

/// `diff_contextual_check_tx`: `ContextualCheckTransaction` for the block after a window's tip.
pub fn compare_contextual_check_tx(bytes: &[u8], pin: Pin) -> Compared {
    compare_target("diff_contextual_check_tx", bytes, pin)
}

/// `diff_contextual_check_block`: `ContextualCheckBlock` for a block after a window's tip.
pub fn compare_contextual_check_block(bytes: &[u8], pin: Pin) -> Compared {
    compare_target("diff_contextual_check_block", bytes, pin)
}

/// `diff_contextual_check_block_header`: `ContextualCheckBlockHeader` of a candidate on top of a
/// window, with the node's clock and checkpoint setting.
pub fn compare_contextual_check_block_header(bytes: &[u8], pin: Pin) -> Compared {
    compare_target("diff_contextual_check_block_header", bytes, pin)
}
