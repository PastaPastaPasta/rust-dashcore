//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! Devnet chain parameters, for the devnet `-devnet=` names with an empty name.

use crate::{
    AddrParams, Bip9Deployment, Bip9Deployments, ChainParams, ChainTxData, Checkpoint,
    ConsensusParams, LlmqType,
};

use bitcoin_units::BlockHeight;
use dash_num::{Arith256, Hash256};
use dash_types::Numeric;
use hex_conservative::hex;

/// Regtest's table, whose genesis block the devnet shares.
const REGTEST: &ChainParams = bsdk_params::Network::Regtest.chain();

/// Hash of the genesis block, which is regtest's.
const GENESIS_HASH: Hash256 = REGTEST.consensus.hash_genesis_block;

/// Hash of the devnet genesis block (height 1) of the devnet named `devnet`.
pub(crate) const DEVNET_GENESIS_HASH: Hash256 =
    Hash256::from_bendian(hex!("65aeaff7fc5bae2bdf1604f5498108317a79ac56624cffe78e77adc206fe1c8b"));

/// The devnet's table. Its devnet-only consensus values, [`DEVNET_GENESIS_HASH`] among them,
/// are [`ConsensusSetup`](crate::ConsensusSetup) fields.
pub(crate) static PARAMS: ChainParams = ChainParams {
    consensus: ConsensusParams {
        subsidy_halving_interval: 210_240,
        masternode_payments_start_block: BlockHeight::from_u32(4010),
        masternode_payments_increase_block: BlockHeight::from_u32(4030),
        masternode_payments_increase_period: 10,
        instant_send_confirmations_required: 2,
        instant_send_keep_lock: 6,
        budget_payments_start_block: BlockHeight::from_u32(4100),
        budget_payments_cycle_blocks: 50,
        budget_payments_window_blocks: 10,
        superblock_start: (BlockHeight::from_u32(4200), Hash256::ZERO),
        superblock_cycle: 24,
        superblock_maturity_window: 8,
        governance_min_quorum: 1,
        governance_filter_elements: 500,
        masternode_minimum_confirmations: 1,
        bip34: (BlockHeight::from_u32(1), Hash256::ZERO),
        bip65_height: BlockHeight::from_u32(1),
        bip66_height: BlockHeight::from_u32(1),
        bip147_height: BlockHeight::from_u32(1),
        csv_height: BlockHeight::from_u32(1),
        dip0001_height: BlockHeight::from_u32(2),
        dip0003_height: BlockHeight::from_u32(2),
        dip0003_enforcement: (BlockHeight::from_u32(2), Hash256::ZERO),
        dip0008_height: BlockHeight::from_u32(2),
        brr_height: BlockHeight::from_u32(2),
        dip0020_height: BlockHeight::from_u32(2),
        dip0024_height: BlockHeight::from_u32(2),
        dip0024_quorums_height: BlockHeight::from_u32(2),
        v19_height: BlockHeight::from_u32(2),
        v20_height: BlockHeight::from_u32(2),
        mn_rr_height: BlockHeight::from_u32(2),
        withdrawals_height: BlockHeight::from_u32(2),
        ipv4_optional_height: BlockHeight::from_u32(0),
        min_bip9_warning_height: BlockHeight::from_u32(2 + 2016), // withdrawals_height + miner_confirmation_window
        rule_change_activation_threshold: 1512,                   // 75% of 2016
        miner_confirmation_window: 2016,
        deployments: Bip9Deployments {
            test_dummy: Bip9Deployment {
                bit: 28,
                start_time: Bip9Deployment::NEVER_ACTIVE,
                timeout: Bip9Deployment::NO_TIMEOUT,
                min_activation_height: BlockHeight::from_u32(0),
                window_size: 0,
                threshold_start: 0,
                threshold_min: 0,
                falloff_coeff: 0,
                use_ehf: false,
            },
            v24: Bip9Deployment {
                bit: 13,
                start_time: 1_751_328_000, // July 1, 2025
                timeout: Bip9Deployment::NO_TIMEOUT,
                min_activation_height: BlockHeight::from_u32(0),
                window_size: 120,
                threshold_start: 96, // 80% of 120
                threshold_min: 72,   // 60% of 120
                falloff_coeff: 5,
                use_ehf: true,
            },
            evo_shares: Bip9Deployment {
                bit: 14,
                start_time: 1_751_328_000, // July 1, 2025
                timeout: Bip9Deployment::NO_TIMEOUT,
                min_activation_height: BlockHeight::from_u32(0),
                window_size: 120,
                threshold_start: 96, // 80% of 120
                threshold_min: 72,   // 60% of 120
                falloff_coeff: 5,
                use_ehf: true,
            },
        },
        pow_limit: Arith256::from_bendian(hex!(
            "7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        )),
        pow_allow_min_difficulty_blocks: true,
        pow_no_retargeting: false,
        pow_target_spacing: 150,     // 2.5 minutes
        pow_target_timespan: 86_400, // 1 day
        pow_kgw_height: BlockHeight::from_u32(4001),
        pow_dgw_height: BlockHeight::from_u32(4001),
        minimum_chain_work: Arith256::ZERO,
        default_assume_valid: Hash256::ZERO,
        llmqs: &[
            LlmqType::Llmq50_60,
            LlmqType::Llmq60_75,
            LlmqType::Llmq400_60,
            LlmqType::Llmq400_85,
            LlmqType::Llmq100_67,
            LlmqType::LlmqDevnet,
            LlmqType::LlmqDevnetDip0024,
            LlmqType::LlmqDevnetPlatform,
        ],
        llmq_type_chain_locks: LlmqType::LlmqDevnet,
        llmq_type_dip0024_instant_send: LlmqType::LlmqDevnetDip0024,
        llmq_type_platform: LlmqType::LlmqDevnetPlatform,
        llmq_type_mnhf: LlmqType::LlmqDevnet,
        // `hash_genesis_block` and the other fields, regtest's.
        ..REGTEST.consensus
    },
    genesis: REGTEST.genesis,
    message_start: [0xe2, 0xca, 0xff, 0xce],
    default_port: 19799,
    default_platform_p2p_port: 22100,
    default_platform_http_port: 22101,
    rpc_port: 19798,
    onion_service_target_port: 19796,
    dns_seeds: &[],
    addr_params: AddrParams {
        pubkey_addr: 140,                     // addresses start with 'y'
        script_addr: 19,                      // addresses start with '8' or '9'
        secret_key: 239,                      // keys start with '9' or 'c'
        ext_pubkey: [0x04, 0x35, 0x87, 0xCF], // tpub
        ext_secret: [0x04, 0x35, 0x83, 0x94], // tprv
        bip44_idx: 1,
        bech32_hrp: "tdash",
    },
    network_id: "devnet",
    is_test_chain: true,
    pool_min_participants: 2,
    pool_max_participants: 20,
    spork_address: "yjPtiKh2uwk3bDutTEA2q9mCtXyiZRWn55",
    credit_pool_period_blocks: 576,
    checkpoints: &CHECKPOINTS,
    chain_tx_data: ChainTxData {
        timestamp: 1_417_713_338, // the devnet genesis block's time
        tx_count: 2,
        tx_rate: 0.01,
    },
};

const CHECKPOINTS: [Checkpoint; 2] =
    [(BlockHeight::from_u32(0), GENESIS_HASH), (BlockHeight::from_u32(1), DEVNET_GENESIS_HASH)];
