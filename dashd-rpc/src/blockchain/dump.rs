//! `dumptxoutset` (RP1b-1b): the coins at their best block, written to a file in Core's
//! snapshot format, which REL-1 compares with dashd's byte for byte (US-NODE-16). The scan and
//! the file are the node's ([`Chainman::dump_txout_set`]); this module resolves the path, opens
//! the temporary file and renames it, as Core's command does.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dash_args::paths;
use dash_primitives::BlockHash;

use super::{NodeChain, TxOutSetError, ensure_chainman, get_hex};
use crate::UniValue;
use crate::protocol::{RPC_INVALID_PARAMETER, RpcError};
use crate::request::param;
use crate::server::{Command, RpcServer};

/// `CreateUTXOSnapshot`'s facts of the file it wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxOutSetDump {
    /// The snapshot's base: the coins' best block, and its height.
    pub base: BlockHash,
    pub height: u32,
    /// `coins_count`: the coins written.
    pub coins: u64,
    /// `hashSerialized`, in Core's internal byte order.
    pub hash: [u8; 32],
    /// The base's `nChainTx`.
    pub n_chain_tx: u64,
}

/// Registers `dumptxoutset` on `server`; relative paths are under `data_dir_net`, Core's
/// `GetDataDirNet()`.
pub fn register_dump_commands(
    server: &RpcServer,
    chain: Arc<dyn NodeChain>,
    data_dir_net: PathBuf,
) {
    server.append_command(Command::new("dumptxoutset", move |r| {
        dumptxoutset(&*chain, &data_dir_net, &r.params)
    }));
}

/// Core's `dumptxoutset`: the path under the network's data directory, refused if it exists;
/// the file written as `<path>.incomplete`, then renamed. A failed snapshot leaves the
/// temporary file, as Core's does. The file system sees each path up to its first NUL, as
/// libstdc++ passes `c_str()`, and the replies and log print it whole: `a\u0000b` writes `a`
/// (its `.incomplete` cut off too, so the rename is `a` onto itself).
// Core: rpc/blockchain.cpp:3091-3140
fn dumptxoutset(
    chain: &dyn NodeChain,
    data_dir_net: &Path,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let name = param(params, 0).get_str()?;
    let temp_name = format!("{name}.incomplete");
    let [path, temppath, c_path, c_temppath] = [name, &*temp_name, c_str(name), c_str(&temp_name)]
        .map(|n| paths::abs_path_join(data_dir_net, Path::new(n)));
    if paths::exists(&c_path).map_err(fs_error)? {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!(
                "{} already exists. If you are sure this is what you want, move it out of the way \
                 first",
                path.display()
            ),
        ));
    }
    // `fopen(temppath, "wb")`.
    let Ok(file) = File::create(&c_temppath) else {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!("Couldn't open file {} for writing.", temppath.display()),
        ));
    };
    // `CreateUTXOSnapshot`'s errors: `GetUTXOStats` failing, the interruption point, and
    // `AutoFile`'s failed write (an `std::ios_base::failure`, whose `what()` libstdc++ suffixes).
    // Core: rpc/blockchain.cpp:3175-3177; streams.cpp:49; rpc/server.cpp:343-346
    let dump =
        ensure_chainman(chain)?.dump_txout_set(file, &path, &temppath).map_err(|e| match e {
            TxOutSetError::Unwritable => {
                RpcError::Runtime("AutoFile::write: write failed: iostream error".into())
            }
            e => super::txout_set_error(e),
        })?;
    paths::rename(&c_temppath, &c_path).map_err(fs_error)?;
    let mut o = UniValue::new_object();
    o.push_kv_end("coins_written", dump.coins.into());
    o.push_kv_end("base_hash", dump.base.to_string().into());
    o.push_kv_end("base_height", i64::from(dump.height).into());
    o.push_kv_end("path", path.display().to_string().into());
    o.push_kv_end("txoutset_hash", get_hex(&dump.hash).into());
    o.push_kv_end("nchaintx", dump.n_chain_tx.into());
    Ok(o)
}

/// `name` as a C string reads it: up to its first NUL. The data directory, an OS path, has none.
fn c_str(name: &str) -> &str {
    name.split('\0').next().unwrap_or(name)
}

/// A throwing `std::filesystem` call's `filesystem_error`, which `ExecuteCommand` reports as any
/// `std::exception`.
// Core: rpc/server.cpp:540-541
fn fs_error(e: dash_args::RuntimeError) -> RpcError {
    RpcError::Runtime(e.0)
}

#[cfg(test)]
mod tests;
