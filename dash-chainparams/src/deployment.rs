//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! Version-bits deployment positions.

/// Position of a version-bits deployment, Core's `DeploymentPos`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DeploymentPos {
    /// `testdummy`, used only for testing.
    TestDummy = 0,
    /// `v24`, the v24 upgrade.
    V24 = 1,
    /// `evo_shares`, EvoNode shared collateral (v24 release only).
    EvoShares = 2,
}

impl DeploymentPos {
    /// Core's `DeploymentPos` value.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Core's `VersionBitsDeploymentInfo` name, used by `-vbparams` and RPC.
    pub const fn name(self) -> &'static str {
        match self {
            Self::TestDummy => "testdummy",
            Self::V24 => "v24",
            Self::EvoShares => "evo_shares",
        }
    }

    /// Core's `VersionBitsDeploymentInfo` `gbt_force`.
    pub const fn gbt_force(self) -> bool {
        true
    }
}
