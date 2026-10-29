//! Chain names and the per-chain base parameters ArgsManager needs.
//!
//! Core: src/chainparamsbase.{h,cpp} at v24.0.0-rc.3. The full chain parameters live in
//! base-sdk's `dash-params`; this is only the part `ArgsManager` and `dash-cli` read: the
//! datadir subdirectory and the default RPC and onion-target ports.

/// Core: `CBaseChainParams::MAIN`.
pub const MAIN: &str = "main";
/// Core: `CBaseChainParams::TESTNET`.
pub const TESTNET: &str = "test";
/// Core: `CBaseChainParams::DEVNET`.
pub const DEVNET: &str = "devnet";
/// Core: `CBaseChainParams::REGTEST`.
pub const REGTEST: &str = "regtest";

/// Core: `CBaseChainParams`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseChainParams {
    data_dir: String,
    rpc_port: u16,
    onion_service_target_port: u16,
}

impl BaseChainParams {
    /// The datadir subdirectory (empty for mainnet; `devnet-<name>` for a named devnet).
    pub fn data_dir(&self) -> &str {
        &self.data_dir
    }
    pub fn rpc_port(&self) -> u16 {
        self.rpc_port
    }
    pub fn onion_service_target_port(&self) -> u16 {
        self.onion_service_target_port
    }
}

/// Core: `CreateBaseChainParams(chain)`. `devnet_name` is `ArgsManager::GetDevNetName()`, which
/// only the devnet uses. The error is the `std::runtime_error` Core throws.
pub fn create_base_chain_params(
    chain: &str,
    devnet_name: impl FnOnce() -> String,
) -> Result<BaseChainParams, String> {
    // Port numbers for incoming Tor connections (9996, 19996, 19796, 19896) have been chosen
    // arbitrarily to keep ranges of used ports tight.
    let (data_dir, rpc_port, onion_service_target_port) = match chain {
        MAIN => (String::new(), 9998, 9996),
        TESTNET => ("testnet3".to_string(), 19998, 19996),
        DEVNET => (devnet_name(), 19798, 19796),
        REGTEST => ("regtest".to_string(), 19898, 19896),
        _ => return Err(format!("CreateBaseChainParams: Unknown chain {chain}.")),
    };
    Ok(BaseChainParams {
        data_dir,
        rpc_port,
        onion_service_target_port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_params_match_core() {
        let main = create_base_chain_params(MAIN, || unreachable!()).unwrap();
        assert_eq!(
            (main.data_dir(), main.rpc_port(), main.onion_service_target_port()),
            ("", 9998, 9996)
        );
        let devnet = create_base_chain_params(DEVNET, || "devnet-x".into()).unwrap();
        assert_eq!(devnet.data_dir(), "devnet-x");
        assert_eq!(
            create_base_chain_params("foo", || unreachable!()).unwrap_err(),
            "CreateBaseChainParams: Unknown chain foo."
        );
    }
}
