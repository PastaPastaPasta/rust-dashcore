//! Per-client LLMQ parameters, independent of dashcore's process-wide overrides.

use std::sync::Arc;

use dash_mnlist::LlmqConfig;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, LlmqType, Network, Release};
use dash_types::Numeric;
use dashcore::Network as SpvNetwork;

use super::ClientConfig;
use crate::error::SpvError;

pub(super) fn llmq_config(cfg: &ClientConfig) -> Result<Arc<LlmqConfig>, SpvError> {
    let network = match cfg.network {
        SpvNetwork::Mainnet => Network::Main,
        SpvNetwork::Testnet => Network::Testnet3,
        SpvNetwork::Regtest => Network::Regtest,
        SpvNetwork::Devnet => Network::Devnet,
    };
    let mut builder = ChainParamsBuilder::new(network, Release::V24_0);
    if let Some(devnet) = &cfg.devnet {
        builder = builder.arg(ChainOption::Devnet, devnet.name.clone());
        if let Some(params) = devnet.llmq_params {
            builder = builder.arg(
                ChainOption::LlmqDevnetParams,
                format!("{}:{}", params.size, params.threshold),
            );
        }
        for (option, llmq_type) in [
            (ChainOption::LlmqChainLocks, devnet.llmq_chainlocks_type),
            (ChainOption::LlmqInstantSendDip0024, devnet.llmq_instantsend_dip0024_type),
            (ChainOption::LlmqPlatform, devnet.llmq_platform_type),
        ] {
            if let Some(llmq_type) = llmq_type {
                builder = builder.arg(option, LlmqType::from_base(llmq_type.index()).to_string());
            }
        }
    }
    let params = builder.build().map_err(|e| SpvError::Config(e.to_string()))?;
    checked_config(params)
}

fn checked_config(params: ChainSetup) -> Result<Arc<LlmqConfig>, SpvError> {
    LlmqConfig::new(Arc::new(params)).map(Arc::new).map_err(|e| SpvError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use key_wallet::wallet::managed_wallet_info::ManagedWalletInfo;
    use key_wallet_manager::WalletManager;
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    use dashcore::sml::llmq_type::{LLMQType, LlmqDevnetParams};

    use super::*;
    use crate::client::{DashSpvClient, DevnetConfig};
    use crate::test_utils::MockNetworkManager;

    #[test]
    fn stock_networks_build_their_own_llmq_config() {
        for (network, history, keep, roles) in [
            (
                SpvNetwork::Mainnet,
                1826,
                320,
                [LlmqType::Llmq400_60, LlmqType::Llmq60_75, LlmqType::Llmq100_67],
            ),
            (
                SpvNetwork::Testnet,
                1826,
                320,
                [LlmqType::Llmq50_60, LlmqType::Llmq60_75, LlmqType::Llmq25_67],
            ),
            (
                SpvNetwork::Regtest,
                212,
                56,
                [LlmqType::LlmqTest, LlmqType::LlmqTestDip0024, LlmqType::LlmqTestPlatform],
            ),
            (
                SpvNetwork::Devnet,
                356,
                80,
                [LlmqType::LlmqDevnet, LlmqType::LlmqDevnetDip0024, LlmqType::LlmqDevnetPlatform],
            ),
        ] {
            let mut cfg = ClientConfig::new(network);
            if network == SpvNetwork::Devnet {
                cfg.devnet = Some(DevnetConfig::new("sb15"));
            }
            let llmq = llmq_config(&cfg).unwrap();
            assert_eq!((llmq.history_blocks(), llmq.h_keep()), (history, keep));
            let c = &llmq.params().consensus;
            assert_eq!(
                [c.llmq_type_chain_locks, c.llmq_type_dip0024_instant_send, c.llmq_type_platform],
                roles,
                "{network:?}"
            );
        }
    }

    #[test]
    fn devnet_overrides_are_local_to_each_config() {
        let make = |size, threshold| {
            ClientConfig::new(SpvNetwork::Devnet).with_devnet(
                DevnetConfig::new("sb15")
                    .with_llmq_params(LlmqDevnetParams {
                        size,
                        threshold,
                    })
                    .with_chainlocks_type(LLMQType::Llmqtype50_60)
                    .with_instantsend_dip0024_type(LLMQType::Llmqtype60_75)
                    .with_platform_type(LLMQType::LlmqtypeDevnet),
            )
        };
        let a = llmq_config(&make(12, 8)).unwrap();
        let b = llmq_config(&make(9, 6)).unwrap();
        let ty = LlmqType::LlmqDevnet;
        assert_eq!(a.params().consensus.llmq(ty).unwrap().size, 12);
        assert_eq!(a.params().consensus.llmq(ty).unwrap().threshold, 8);
        assert_eq!(b.params().consensus.llmq(ty).unwrap().size, 9);
        assert_eq!(b.params().consensus.llmq(ty).unwrap().threshold, 6);
        assert!(a.roles(LlmqType::Llmq50_60).chain_locks);
        assert!(a.roles(LlmqType::Llmq60_75).instant_send);
        assert!(a.roles(ty).platform);
        assert_eq!((a.history_blocks(), a.h_keep()), (1826, 320));
    }

    #[test]
    fn invalid_devnet_types_and_parameters_are_refused() {
        let base = || ClientConfig::new(SpvNetwork::Devnet);
        for devnet in [
            DevnetConfig::new("sb15").with_platform_type(LLMQType::LlmqtypeUnknown(250)),
            DevnetConfig::new("sb15").with_chainlocks_type(LLMQType::LlmqtypeTest),
            DevnetConfig::new("sb15").with_chainlocks_type(LLMQType::LlmqtypeDevnetDIP0024),
            DevnetConfig::new("sb15").with_instantsend_dip0024_type(LLMQType::LlmqtypeDevnet),
            DevnetConfig::new("sb15").with_llmq_params(LlmqDevnetParams {
                size: u16::MAX,
                threshold: 1,
            }),
        ] {
            let result = llmq_config(&base().with_devnet(devnet.clone()));
            assert!(matches!(result, Err(SpvError::Config(_))), "{devnet:?}: {result:?}");
        }
        let fast = ChainParamsBuilder::new(Network::Devnet, Release::V24_0)
            .arg(ChainOption::PowTargetSpacing, "30")
            .build()
            .unwrap();
        assert!(
            matches!(checked_config(fast), Err(SpvError::Config(message)) if message.contains("minimum lifetime"))
        );
    }

    #[tokio::test]
    async fn invalid_parameters_fail_client_construction() {
        let dir = TempDir::new().unwrap();
        let cfg = ClientConfig::new(SpvNetwork::Devnet).with_storage_path(dir.path()).with_devnet(
            DevnetConfig::new("sb15-invalid").with_llmq_params(LlmqDevnetParams {
                size: u16::MAX,
                threshold: 1,
            }),
        );
        let wallet = Arc::new(RwLock::new(WalletManager::<ManagedWalletInfo>::new(cfg.network)));
        let result =
            DashSpvClient::new(cfg, MockNetworkManager::new(), wallet, vec![Arc::new(())]).await;
        assert!(matches!(result, Err(SpvError::Config(_))));
        assert_ne!(dashcore::sml::llmq_type::llmq_devnet_params().size, u16::MAX);
    }
}
