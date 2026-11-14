//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! Per-network values outside [`ChainParams`](crate::ChainParams), at
//! v24.0.0-rc.5. Core's runtime policy is [`ChainPolicy`](crate::ChainPolicy).

use super::{AssumeutxoData, HardCodedPins};
use crate::Network;

use bitcoin_units::BlockHeight;
use dash_num::Hash256;
use dash_types::Numeric;
use hex_conservative::hex;

/// The per-network values a [`ChainSetup`](super::ChainSetup) holds besides
/// the ones of [`ChainParams`](crate::ChainParams).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct Extras {
    /// Whether a masternode's external IP must be routable.
    pub(super) require_routable_external_ip: bool,
    /// Whether several masternode addresses may share a network group.
    pub(super) allow_multiple_addresses_from_group: bool,
    /// Seconds before an LLMQ connection is retried.
    pub(super) llmq_connection_retry_timeout: i32,
    /// Seconds until a fulfilled request expires.
    pub(super) fulfilled_request_expire_time: i32,
    /// Values Core hard-codes outside its chain parameters.
    pub(super) pins: HardCodedPins,
}

/// Core's `TESTNET_LLMQ_25_67_ACTIVATION_HEIGHT` (quirk 34).
const LLMQ_25_67_ACTIVATION_HEIGHT: BlockHeight = BlockHeight::from_u32(847_000);

/// Hard-coded pins of every chain but mainnet.
const TEST_PINS: HardCodedPins = HardCodedPins {
    pre_dgw_tolerance_until: None,
    llmq_25_67_activation_height: LLMQ_25_67_ACTIVATION_HEIGHT,
};

/// The values of `network`.
pub(super) const fn extras(network: Network) -> Extras {
    match network {
        Network::Main => Extras {
            require_routable_external_ip: true,
            allow_multiple_addresses_from_group: false,
            llmq_connection_retry_timeout: 60,
            fulfilled_request_expire_time: 60 * 60,
            pins: HardCodedPins {
                pre_dgw_tolerance_until: Some(BlockHeight::from_u32(68_589)),
                llmq_25_67_activation_height: LLMQ_25_67_ACTIVATION_HEIGHT,
            },
        },
        Network::Testnet3 => Extras {
            require_routable_external_ip: true,
            allow_multiple_addresses_from_group: false,
            llmq_connection_retry_timeout: 60,
            fulfilled_request_expire_time: 5 * 60,
            pins: TEST_PINS,
        },
        Network::Devnet => Extras {
            require_routable_external_ip: true,
            allow_multiple_addresses_from_group: true,
            llmq_connection_retry_timeout: 60,
            fulfilled_request_expire_time: 5 * 60,
            pins: TEST_PINS,
        },
        Network::Regtest => Extras {
            require_routable_external_ip: false,
            allow_multiple_addresses_from_group: true,
            llmq_connection_retry_timeout: 1,
            fulfilled_request_expire_time: 5 * 60,
            pins: TEST_PINS,
        },
    }
}

/// The BIP155-serialized fixed seed addresses of `network`.
pub(super) const fn fixed_seeds(network: Network) -> &'static [u8] {
    match network {
        Network::Main => &MAINNET_FIXED_SEEDS_V24,
        Network::Testnet3 => &TESTNET_FIXED_SEEDS_V24,
        Network::Devnet | Network::Regtest => &[],
    }
}

/// The assumeutxo snapshots `network` trusts.
pub(super) const fn assumeutxo(network: Network) -> &'static [AssumeutxoData] {
    match network {
        Network::Regtest => &ASSUMEUTXO_V24,
        _ => &[],
    }
}

/// Regtest snapshot at `height` with UTXO-set hash `hash` and evo-section
/// hash `evo_hash`.
const fn snapshot(height: u32, hash: [u8; 32], evo_hash: Option<Hash256>) -> AssumeutxoData {
    AssumeutxoData {
        height: BlockHeight::from_u32(height),
        hash_serialized: Hash256::from_bendian(hash),
        evo_hash,
        chain_tx: height,
    }
}

const SNAPSHOT_110: [u8; 32] =
    hex!("9b2a277a3e3b979f1a539d57e949495d7f8247312dbc32bce6619128c192b44b");
const SNAPSHOT_200: [u8; 32] =
    hex!("8a5bdd92252fc6b24663244bbe958c947bb036dc1f94ccd15439f48d8d1cb4e3");
const ZERO: Option<Hash256> = Some(Hash256::ZERO);

static ASSUMEUTXO_V24: [AssumeutxoData; 2] =
    [snapshot(110, SNAPSHOT_110, ZERO), snapshot(200, SNAPSHOT_200, ZERO)];

#[rustfmt::skip]
static MAINNET_FIXED_SEEDS_V24: [u8; 2884] = hex!(concat!(
  "0420cfb5e558be0d672d55acca2a4fb818888fd21548a91f186c7999baa586cecbba270f0420c236fcafff7c46843e75",
  "26acea2ad4f1b121960194a81b0a1751cf5f521bc0db270f0420ba5bab3a877c7498c27006d1350849b96b17d52621cc",
  "a8c173319a4476cbd777270f0420ae08410ddf89564f35fc2a45513e094181c7e3847d257f7122f895e47092a55b270f",
  "04209d329c4d1e352dbd4982b9a5664f455256ac5db3e7e3393eac8122102f23260d270f0420835025a93357385d60d2",
  "9e7b7b2bfbb172951c65b8b59515204ea4627b1637ea270f042080c965bbe7dd120ec185df981c8d0ea4f17c17b1f72c",
  "faff9922f2cc8491fe63270f04207dd03c5edb99248efadc32cd6748d219e858d547b576eeda9cd57cbc6cfe93ac270f",
  "04207e87e89bd53f4bb62621ff57a7bef3085e90979dacc987ec5ca6db72b9b67932270f04206d54e7bea858cd359980",
  "b2326c86a2a911623d3b67345dd1203141923f8e5726270f0420609b96e1cafd869083e3eb908e372c584cb77986b84b",
  "10b2929b7765466ccf84270f04205594a681ae3385fe02ec3dccc21e62230c1668eef868281e5c951fdf9ffe6d3d270f",
  "04205783984d479e719412674376a5625be6a2bc89905465d404123c04b1535b21e9270f0420574c369aa062ed8658fa",
  "037d05832f355675468a049de6d7be1efaf2366f29d0270f042049b9652324d9714256a068a15c734dc728ba96e6d69e",
  "dd6afe00c9a40b325c19270f042045e9fa710ab15a05d18144165d62bfd8d24909b575e7979c3396504d1b440eae270f",
  "042044d4098b4679554a85892aee6881aa9a6327285fb0a8da6bac87df38325f2d89270f04204167eea8aef7d77c7a80",
  "5aaf2dfd9bd193ab8ae992234ccad9035ef9d71aa0aa270f04203f2381376b32e603dea5bcc9e8e698a6dce06cbb075e",
  "2364eaa9fa104088fcb7270f04203e96f49d6bf8aedfcc0feb500899a0c17243c90f508f681f4b5a80110172203d270f",
  "04202e5decf0624cca6e90df00651cca4acb66eea2a694278d6cec154be0a486e321270f04201f3b77a4090552600bdc",
  "d424d04a50fa2d5c9cf1d0f24c2de9190dd2ff4491a3270f042015dd119818614281e2bfb88a4b70a31a93fc60405313",
  "1379e8fca37cdc423b61270f0420060f6b3d30e44db0c9619cff8226183d058042410fe6de742b3f23fba7dab15c270f",
  "0420070f6abe825444394a0063aa7b30fc0461046db1928f2f129a724722164f67e1270f0420dd75b606bc355055df7e",
  "b0f9b26a6d2af16c348ccee8a41ee601f72fa917d14f270f0420d89d506e4d6230388c108cb29eaa2da25f632a843e94",
  "970e8de1ea8bab78eafb270f0104d99ae714270f0104d8ee4b2e270f0104d8e6e87d270f0104d18d24ce270f0104d14a",
  "5f42270f0104d14a42b0270f0104d057665b270f0104d0576659270f0104d0576656270f0104d0576215270f0104cff4",
  "f728270f0104cfb4e906270f0104ca051021270f0104c75508bf270f0104c75508be270f0104c7550808270f0104c362",
  "5fd2270f0104c31afee4270f0104c2ee19c7270f0104c2875464270f0104c2875021270f0104c0af7fc6270f0104c0a9",
  "0657270f0104bc28b243270f0104b9f373db270f0104b9e4539c270f0104b9e45384270f0104b9d8b305270f0104b9c6",
  "ea0c270f0104b9c113d6270f0104b9b928ab270f0104b9b92868270f0104b9a5ab75270f0104b9a4a3da270f0104b9a4",
  "a355270f0104b98ed490270f0104b970f935270f0104b970f931270f0104b91c6591270f0104b86bb6c6270f0104b2fd",
  "fd05270f0104b2d057e2270f0104b2d057d5270f0104b29d5bb8270f0104b29d5bb2270f0104b29d5bb1270f0104b280",
  "e4c3270f0104b2689b05270f0104b23f7981270f0104b23eab10270f0104b15d8d56270f0104b15d8d3c270f0104b066",
  "4191270f0104acee0719270f0104acecf451270f0104ac6891a6270f0104ac5675d2270f0104ac565b9e270f0104ac56",
  "49a0270f0104a93ace3f270f0104a8775004270f0104a758a910270f0104a3ac4229270f0104a2d4236b270f0104a2d4",
  "236a270f0104a2d42368270f0104a2d42364270f0104a161a573270f01049f4b79a3270f01049db45b4e270f01049dad",
  "7a1a270f01049dad7a19270f01049dad7a15270f01049dad7a14270f01049c431d2d270f010498435fb8270f010497f4",
  "55fd270f010497f455db270f010497f455b2270f010497f4557b270f010495686e77270f0104951c944d270f0104932d",
  "29d2270f0104901fbc2a270f01049018eeb2270f01048f14210a270f01048bb48f93270f01048ba2d7a9270f01048ba2",
  "d34c270f01048b54d85b270f01048b1c6102270f010486ffb6b9270f0104851261e2270f010484f3c5f0270f010484f3",
  "c5b3270f010484f3c563270f010484f3c517270f010482ff9ba3270f010482a2e9ba270f010473bef839270f01047284",
  "acd7270f01046deb43f6270f01046dadc643270f01046bbd034a270f01046bb3845d270f01046baecce8270f01046baa",
  "fea0270f01046a370916270f010468cf4b90270f010468cf4b03270f010468cf49af270f01045bc795b1270f01045bc6",
  "6c29270f01045bc66c28270f01045bc66c27270f01045bc66c23270f01045b63a9ba270f010459b34960270f0104597d",
  "ffd3270f0104597dd1e2270f0104597d32ce270f0104597d320e270f0104597503f2270f0104592e2ada270f0104592e",
  "2ad8270f010459280fde270f0104592383db270f0104592383da270f01045923839e270f010459238395270f010457e5",
  "1694270f010457e5161f270f010457e41840270f0104566ba862270f0104566ba828270f010455d1f1f5270f010455d1",
  "f15d270f010455d1f157270f010455d1f156270f010455bef303270f010454f7b4c9270f010454f2b3cc270f01045462",
  "0cc4270f010454093211270f0104539564ba270f0104539564b8270f0104539564b7270f010452df517f270f010452ca",
  "e653270f010452b492c0270f010451e3fa33270f010450f99308270f010450f084e7270f01044e531300270f01044e11",
  "d4f9270f01044e114728270f01044dedecd6270f01044dedec42270f01044dedeb00270f01044ddf6304270f01044ddd",
  "94cc270f01044d693d77270f01044d693d4f270f01044d2a5b48270f01044d2a4a44270f01044d2a102e270f01044a32",
  "5a71270f0104483e3a6c270f0104483c26a0270f0104453d6bd7270f010442f4f346270f010442a37171270f010442a3",
  "715b270f010442a37107270f01043eee7f94270f01043e3cf4ae270f0104362526e2270f01043424665b270f01043421",
  "09ac270f010433a12286270f0104339ebc58270f0104339ea9ed270f0104339ea71f270f0104334f6db6270f01043344",
  "031d270f01042efafe0a270f01042ee17833270f01042ee03297270f01042e94e779270f01042e3efaf7270f01042e3e",
  "8490270f01042e2428f2270f01042e2423f4270f01042e1ebdfb270f01042e04a27f270f01042d99ba64270f01042d97",
  "46cb270f01042d9746ca270f01042d959ac2270f01042d87b44f270f01042d87b444270f01042d809c1e270f01042d4f",
  "126a270f01042d3dba79270f01042d3a384f270f01042cf063d6270f01042ba7f46d270f01042b9b87d0270f01042b86",
  "b471270f01042b85ab65270f010426667d6c270f010426360c6f270f01042561e315270f0104253cf4fd270f010422d2",
  "ee43270f01041f946394270f01041f4d6c88270f01041f3aaab5270f01041f3aaab4270f01041f3aaa8b270f01041f3a",
  "aa82270f01041f3826b9270f01041f382601270f01041f382600270f01041f0a6124270f010405afa2d7270f010405af",
  "a2bc270f010405afa2b9270f010405afa2b8270f010405357c78270f010405236719270f010405236713270f01040502",
  "493a270f0104050243be270f010402e97823270f0104023bdb36270f0104021ceae6270f010402195070270f01040175",
  "4bdc270f",
));

#[rustfmt::skip]
static TESTNET_FIXED_SEEDS_V24: [u8; 200] = hex!(concat!(
  "0104cef5a73f4e1f0104a223f3554e1d010457e5167e4e1f010455d1f3294e1f010455d1f30a4e1f010455d1f3094e1f",
  "010455d1f3084e1f010455d1f3074e1f010455d1f3064e1f010455d1f3054e1f010455d1f3044e1f010455d1f3034e1f",
  "010455d1f3024e1f010444437a514e1f010444437a484e1f010444437a464e1f010444437a424e1f010444437a3e4e1f",
  "010444437a334e1f010444437a2a4e1f010444437a214e1f010444437a1e4e1f010444437a174e1f01043e3cf4ae4e1f",
  "010422b266904e1f",
));
