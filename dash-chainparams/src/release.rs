//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! The Dash Core release whose chain parameters are known.

use crate::{BuriedDeployment, BuriedDeploymentExt, DeploymentPos};

/// A Dash Core release line whose chain parameters are known.
///
/// `V24_0` is v24.0.0, the release the network tables hold. The node targets it alone (DEC-82);
/// the field shapes that vary by release elsewhere (`ChainSetup::require_standard`,
/// `ChainSetup::platform_hrp`, the `evo_shares` deployment) are kept.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Release {
    /// The v24.0 line (v24.0.0).
    V24_0,
}

impl Release {
    /// Every release, oldest first.
    pub const ALL: [Self; 1] = [Self::V24_0];

    /// Core's version-bits deployments, in `DeploymentPos` order.
    ///
    /// Their count is Core's `MAX_VERSION_BITS_DEPLOYMENTS`.
    pub const fn deployments(self) -> &'static [DeploymentPos] {
        match self {
            Self::V24_0 => {
                &[DeploymentPos::TestDummy, DeploymentPos::V24, DeploymentPos::EvoShares]
            }
        }
    }

    /// Core's buried deployments, in `BuriedDeployment` order.
    ///
    /// v24.0.0 ends with `ipv4_optional` (consensus/params.h:39-41). The release takes a fixed
    /// prefix of [`BuriedDeploymentExt::ALL`], so a deployment appended there does not join it.
    pub fn buried_deployments(self) -> &'static [BuriedDeployment] {
        match self {
            Self::V24_0 => &BURIED_DEPLOYMENTS[..16],
        }
    }
}

/// [`BuriedDeploymentExt::ALL`] in a static, for the `'static` slices
/// the `buried_deployments` method of [`Release`] returns.
static BURIED_DEPLOYMENTS: [BuriedDeployment; 16] = <BuriedDeployment as BuriedDeploymentExt>::ALL;
