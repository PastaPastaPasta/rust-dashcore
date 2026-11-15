//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! Errors building chain parameters from Core's command-line options.

use crate::builder::ChainOption;
use crate::prelude::*;

use core::fmt;

/// A version-bits field `-vbparams` failed to parse.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VbField {
    /// `<start>`.
    StartTime,
    /// `<end>`.
    Timeout,
    /// `<min_activation_height>`.
    MinActivationHeight,
    /// `<window>`.
    WindowSize,
    /// `<threshold/thresholdstart>`.
    ThresholdStart,
    /// `<thresholdmin>`.
    ThresholdMin,
    /// `<falloffcoeff>`.
    FalloffCoeff,
    /// `<useehf>`.
    UseEhf,
}

impl VbField {
    /// Core's name for the field in its error text.
    const fn core_name(self) -> &'static str {
        match self {
            Self::StartTime => "nStartTime",
            Self::Timeout => "nTimeout",
            Self::MinActivationHeight => "min_activation_height",
            Self::WindowSize => "nWindowSize",
            Self::ThresholdStart => "nThresholdStart",
            Self::ThresholdMin => "nThresholdMin",
            Self::FalloffCoeff => "nFalloffCoeff",
            Self::UseEhf => "nUseEHF",
        }
    }
}

/// Why chain parameters could not be built.
///
/// The variants before [`NegativeHeight`](Self::NegativeHeight) are Core's
/// own refusals; their `Display` is Core's exception text, byte for byte, at
/// v24.0. The rest are inputs Core accepts or asserts on that
/// this crate refuses.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ParamsError {
    /// A `-testactivationheight` value without `@`.
    ActivationHeightFormat(String),
    /// A `-testactivationheight` height outside `[0, INT_MAX)`.
    ActivationHeightValue(String),
    /// A `-testactivationheight` name Core does not know.
    ActivationHeightName(String),
    /// A `-vbparams` value without 3, 4, 6 or 9 fields.
    VbParamsMalformed,
    /// A `-vbparams` field that is not an integer of its type.
    VbParamsField(VbField, String),
    /// A `-vbparams` deployment the release does not have.
    VbParamsDeployment(String),
    /// A `-dip3params` value without 2 fields.
    Dip3Malformed,
    /// A `-dip3params` activation height that is not an `int`.
    Dip3Activation(String),
    /// A `-dip3params` enforcement height that is not an `int`.
    Dip3Enforcement(String),
    /// A `-budgetparams` value without 3 fields.
    BudgetMalformed,
    /// A `-budgetparams` masternode start that is not an `int`.
    BudgetMasternode(String),
    /// A `-budgetparams` budget start that is not an `int`.
    BudgetBudget(String),
    /// A `-budgetparams` superblock start that is not an `int`.
    BudgetSuperblock(String),
    /// An LLMQ size override (named by Core's quorum name) without 2 fields.
    LlmqParamsMalformed(&'static str),
    /// An LLMQ size override whose size is not an `int`.
    LlmqSize(&'static str, String),
    /// An LLMQ size override whose threshold is not an `int`.
    LlmqThreshold(&'static str, String),
    /// An LLMQ size above [`MAX_LLMQ_SIZE`](crate::MAX_LLMQ_SIZE) (v24).
    LlmqSizeTooLarge(&'static str, i32),
    /// An LLMQ role option naming no configured quorum type.
    LlmqTypeName(ChainOption),
    /// An LLMQ role option naming a type whose rotation does not suit it.
    LlmqRotation(ChainOption),
    /// `-powtargetspacing` is not an `int64_t`.
    PowTargetSpacingParse(String),
    /// `-powtargetspacing` is below 1.
    PowTargetSpacingValue(String),
    /// A `-sporkaddr` that is not a P2PKH address of the chain.
    SporkAddress,
    /// An option the release does not have, as Core's argument parser
    /// reports it (`-minsporkkeys` at v24).
    InvalidParameter(String),
    /// A negation of an option that is not
    /// [`negatable`](ChainOption::negatable), as Core's argument parser
    /// reports it.
    NegationForbidden(ChainOption),
    /// A height Core parses into an `int` without a range check is negative
    /// in the final parameters (a `-vbparams` minimum activation height, a
    /// `-dip3params` or a `-budgetparams` height). Heights here are unsigned,
    /// so this is refused where Core accepts it (backlog BL-31).
    NegativeHeight(ChainOption, i64),
    /// Regtest's V20 height is below DIP0003's, where Core asserts.
    V20BelowDip3,
    /// Regtest's MN_RR height is below V20's, where Core asserts.
    MnRrBelowV20,
    /// No devnet genesis nonce exists for the name, where Core asserts.
    DevnetGenesis(String),
    /// An LLMQ size override (named by Core's quorum name) whose size or
    /// threshold is outside `u16`, which Core's `int` holds (quirk 46). It is
    /// reported only once Core's own checks have passed.
    LlmqOutOfRange(&'static str, i32, i32),
}

impl fmt::Display for ParamsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const HEIGHT_ARG: &str = "for -testactivationheight=name@height.";
        match self {
            Self::ActivationHeightFormat(a) => write!(f, "Invalid format ({a}) {HEIGHT_ARG}"),
            Self::ActivationHeightValue(a) => write!(f, "Invalid height value ({a}) {HEIGHT_ARG}"),
            Self::ActivationHeightName(a) => write!(f, "Invalid name ({a}) {HEIGHT_ARG}"),
            Self::VbParamsMalformed => f.write_str(concat!(
                "Version bits parameters malformed, expecting ",
                "<deployment>:<start>:<end> or ",
                "<deployment>:<start>:<end>:<min_activation_height> or ",
                "<deployment>:<start>:<end>:<min_activation_height>:<window>:<threshold> or ",
                "<deployment>:<start>:<end>:<min_activation_height>:<window>:<thresholdstart>:",
                "<thresholdmin>:<falloffcoeff>:<useehf>"
            )),
            Self::VbParamsField(field, v) => write!(f, "Invalid {} ({v})", field.core_name()),
            Self::VbParamsDeployment(d) => write!(f, "Invalid deployment ({d})"),
            Self::Dip3Malformed => {
                f.write_str("DIP3 parameters malformed, expecting <activation>:<enforcement>")
            }
            Self::Dip3Activation(v) => write!(f, "Invalid activation height ({v})"),
            Self::Dip3Enforcement(v) => write!(f, "Invalid enforcement height ({v})"),
            Self::BudgetMalformed => f.write_str(
                "Budget parameters malformed, expecting <masternode>:<budget>:<superblock>",
            ),
            Self::BudgetMasternode(v) => write!(f, "Invalid masternode start height ({v})"),
            Self::BudgetBudget(v) => write!(f, "Invalid budget start block ({v})"),
            Self::BudgetSuperblock(v) => write!(f, "Invalid superblock start height ({v})"),
            Self::LlmqParamsMalformed(q) => {
                write!(f, "{q} parameters malformed, expecting <size>:<threshold>")
            }
            Self::LlmqSize(q, v) => write!(f, "Invalid {q} size ({v})"),
            Self::LlmqThreshold(q, v) => write!(f, "Invalid {q} threshold ({v})"),
            Self::LlmqSizeTooLarge(q, size) => write!(
                f,
                "Invalid {q} size ({size}), must not exceed the maximum supported quorum size ({})",
                crate::MAX_LLMQ_SIZE
            ),
            Self::LlmqTypeName(o) => write!(f, "Invalid LLMQ type specified for {}.", o.name()),
            Self::LlmqRotation(ChainOption::LlmqChainLocks) => {
                f.write_str("LLMQ type specified for -llmqchainlocks must NOT use rotation")
            }
            Self::LlmqRotation(o) => {
                write!(f, "LLMQ type specified for {} must use rotation", o.name())
            }
            Self::PowTargetSpacingParse(v) => {
                write!(f, "Invalid parsing of powTargetSpacing ({v})")
            }
            Self::PowTargetSpacingValue(v) => write!(f, "Invalid value of powTargetSpacing ({v})"),
            Self::SporkAddress => f.write_str("Invalid spork address specified with -sporkaddr"),
            Self::InvalidParameter(arg) => write!(f, "Invalid parameter {arg}"),
            Self::NegationForbidden(o) => {
                write!(f, "Negating of {} is meaningless and therefore forbidden", o.name())
            }
            Self::NegativeHeight(o, v) => write!(f, "negative height {v} in {}", o.name()),
            Self::V20BelowDip3 => f.write_str("v20 height below dip0003 height"),
            Self::MnRrBelowV20 => f.write_str("mn_rr height below v20 height"),
            Self::DevnetGenesis(name) => write!(f, "no devnet genesis block for {name}"),
            Self::LlmqOutOfRange(q, size, threshold) => {
                write!(f, "{q} size ({size}) or threshold ({threshold}) outside 0 to {}", u16::MAX)
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ParamsError {}
