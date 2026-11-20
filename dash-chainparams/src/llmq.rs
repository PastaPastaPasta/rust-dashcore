//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! Core's values derived from base-sdk's LLMQ types and parameters.

use crate::{LlmqParams, LlmqType};

/// Core's static LLMQ table over [`LlmqType`].
pub trait LlmqTypeExt: Sized {
    /// The types of Core's static LLMQ table, in its order.
    const AVAILABLE: [Self; 14];
}

impl LlmqTypeExt for LlmqType {
    const AVAILABLE: [Self; 14] = [
        Self::LlmqTest,
        Self::LlmqTestInstantsend,
        Self::LlmqTestV17,
        Self::LlmqTestDip0024,
        Self::LlmqTestPlatform,
        Self::LlmqDevnet,
        Self::LlmqDevnetDip0024,
        Self::LlmqDevnetPlatform,
        Self::Llmq50_60,
        Self::Llmq60_75,
        Self::Llmq400_60,
        Self::Llmq400_85,
        Self::Llmq100_67,
        Self::Llmq25_67,
    ];
}

/// Largest quorum size the v24 signing protocol supports. v24 refuses larger `-llmqtestparams`
/// and `-llmqdevnetparams` sizes.
pub const MAX_LLMQ_SIZE: u16 = 400;

/// The parameters of `llmq_type` in Core's static LLMQ table, which no override changes (Core's
/// `Consensus::available_llmqs` lookup), or `None` for [`LlmqType::LlmqNone`] and unknown types.
///
/// A chain's own parameters, with its overrides, are
/// [`ConsensusSetup::llmq`](crate::ConsensusSetup::llmq).
pub const fn available_params(llmq_type: LlmqType) -> Option<LlmqParams> {
    llmq_type.params()
}

/// Core's values derived from an [`LlmqParams`].
pub trait LlmqParamsExt {
    /// Core's `max_cycles`: DKG cycles covering `quorums_count` quorums.
    ///
    /// Zero for a rotated type with no active quorums, where Core divides by zero; no table
    /// entry has that.
    fn max_cycles(&self, quorums_count: i32) -> i32;

    /// Core's `max_store_depth`: blocks of recent DKG data to keep.
    fn max_store_depth(&self) -> i32;

    /// Core's `is_single_member`.
    fn is_single_member(&self) -> bool;
}

impl LlmqParamsExt for LlmqParams {
    fn max_cycles(&self, quorums_count: i32) -> i32 {
        if self.use_rotation {
            quorums_count.checked_div(i32::from(self.signing_active_quorum_count)).unwrap_or(0)
        } else {
            quorums_count
        }
    }

    fn max_store_depth(&self) -> i32 {
        <Self as LlmqParamsExt>::max_cycles(self, i32::from(self.keep_old_keys))
            .wrapping_mul(i32::from(self.dkg_interval))
    }

    fn is_single_member(&self) -> bool {
        self.size == 1
    }
}
