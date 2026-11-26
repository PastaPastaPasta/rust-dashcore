//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! Core's names and order of the buried deployments, which base-sdk's [`BuriedDeployment`] lacks.

use crate::BuriedDeployment;

/// Core's values derived from a [`BuriedDeployment`].
pub trait BuriedDeploymentExt: Sized {
    /// Every buried deployment, in Core's `BuriedDeployment` order.
    const ALL: [Self; 16];

    /// Core's `DeploymentName`, as `getdeploymentinfo` reports it.
    fn name(self) -> &'static str;

    /// The name regtest's `-testactivationheight` accepts, if it moves this deployment.
    fn test_activation_name(self) -> Option<&'static str>;
}

impl BuriedDeploymentExt for BuriedDeployment {
    const ALL: [Self; 16] = [
        Self::HeightInCoinbase,
        Self::Cltv,
        Self::DerSig,
        Self::Bip147,
        Self::Csv,
        Self::Dip0001,
        Self::Dip0003,
        Self::Dip0008,
        Self::Dip0020,
        Self::Dip0024,
        Self::Brr,
        Self::V19,
        Self::V20,
        Self::MnRr,
        Self::Withdrawals,
        Self::Ipv4Optional,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::HeightInCoinbase => "bip34",
            Self::Cltv => "bip65",
            Self::DerSig => "bip66",
            Self::Bip147 => "bip147",
            Self::Csv => "csv",
            Self::Dip0001 => "dip0001",
            Self::Dip0003 => "dip0003",
            Self::Dip0008 => "dip0008",
            Self::Dip0020 => "dip0020",
            Self::Dip0024 => "dip0024",
            Self::Brr => "realloc",
            Self::V19 => "v19",
            Self::V20 => "v20",
            Self::MnRr => "mn_rr",
            Self::Withdrawals => "withdrawals",
            Self::Ipv4Optional => "ipv4_optional",
        }
    }

    fn test_activation_name(self) -> Option<&'static str> {
        match self {
            Self::Bip147 => Some("bip147"),
            Self::HeightInCoinbase => Some("bip34"),
            Self::DerSig => Some("dersig"),
            Self::Cltv => Some("cltv"),
            Self::Csv => Some("csv"),
            Self::Brr => Some("brr"),
            Self::Dip0001 => Some("dip0001"),
            Self::Dip0008 => Some("dip0008"),
            Self::Dip0024 => Some("dip0024"),
            Self::V19 => Some("v19"),
            Self::V20 => Some("v20"),
            Self::MnRr => Some("mn_rr"),
            Self::Ipv4Optional => Some("ipv4_optional"),
            Self::Dip0003 | Self::Dip0020 | Self::Withdrawals => None,
        }
    }
}
