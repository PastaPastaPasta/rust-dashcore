//! The node half of `setban`, `listbanned` and `clearbanned` (`rpc/net.cpp`), over SB6's ban
//! list and its immediate saves (P4a D6), and Core's warning for a legacy `banlist.dat` (D2),
//! which [`crate::store::FileStore`] logs as it loads the ban list.
//! The RPC rows marshal the arguments and the rows; the errors are Core's texts.

use std::path::Path;

use dash_args::strencodings::quoted;
use dash_p2p::lookup::{lookup_host, lookup_subnet};
use dash_p2p::{NetCore, Network, SubNet};

/// `setban`'s command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetBan {
    /// `add`, with `bantime` (0 or absent: `-bantime`) and `absolute`.
    Add {
        /// The ban time in seconds, or the end of the ban when `absolute`.
        bantime: i64,
        /// `bantime` is a UNIX time.
        absolute: bool,
    },
    /// `remove`.
    Remove,
}

/// A `setban` failure: Core's RPC error code name and text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetBanError {
    /// `RPC_CLIENT_INVALID_IP_OR_SUBNET`.
    InvalidIpOrSubnet(&'static str),
    /// `RPC_CLIENT_NODE_ALREADY_ADDED`.
    AlreadyBanned(&'static str),
    /// `RPC_INVALID_PARAMETER`.
    InvalidParameter(&'static str),
}

/// Core's `setban` after its argument checks: a target with a `/` is a subnet
/// (`LookupSubNet`), any other a single address (`LookupHost(.., false)` and the CJDNS flip).
// Core: rpc/net.cpp:743-826 setban
pub fn set_ban(core: &NetCore, target: &str, command: SetBan) -> Result<(), SetBanError> {
    let cjdns = core.is_reachable(Network::Cjdns);
    let is_subnet = target.contains('/');
    // A single address must be a valid `CNetAddr` (not only a valid one-address subnet).
    let subnet = if is_subnet {
        lookup_subnet(target, cjdns)
    } else {
        lookup_host(target)
            .map(|a| a.maybe_flip_to_cjdns(cjdns))
            .filter(|a| a.is_valid())
            .map(SubNet::single)
    }
    .ok_or(SetBanError::InvalidIpOrSubnet("Error: Invalid IP/Subnet"))?;
    let now = core.clock().node_now().0;
    match command {
        SetBan::Add {
            bantime,
            absolute,
        } => {
            let banned = if is_subnet {
                core.banlist().is_subnet_banned(&subnet, now)
            } else {
                core.banlist().is_banned(subnet.network(), now)
            };
            if banned {
                return Err(SetBanError::AlreadyBanned("Error: IP/Subnet already banned"));
            }
            if absolute && bantime < now {
                return Err(SetBanError::InvalidParameter(
                    "Error: Absolute timestamp is in the past",
                ));
            }
            core.ban(subnet, bantime, absolute);
            Ok(())
        }
        SetBan::Remove => {
            if core.unban(&subnet) {
                Ok(())
            } else {
                Err(SetBanError::InvalidIpOrSubnet(
                    "Error: Unban failed. Requested address/subnet was not previously manually banned.",
                ))
            }
        }
    }
}

/// One `listbanned` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BannedRow {
    /// `address`: `CSubNet::ToString`.
    pub address: String,
    /// `ban_created`.
    pub ban_created: i64,
    /// `banned_until`.
    pub banned_until: i64,
    /// `ban_duration`: `banned_until - ban_created`.
    pub ban_duration: i64,
    /// `time_remaining`: `banned_until - now`.
    pub time_remaining: i64,
}

/// Core's `listbanned` rows, in the ban map's order, after `SweepBanned`.
// Core: rpc/net.cpp:828-871 listbanned
pub fn banned_rows(core: &NetCore) -> Vec<BannedRow> {
    let now = core.clock().node_now().0;
    core.banlist()
        .banned(now)
        .into_iter()
        .map(|(net, e)| BannedRow {
            address: net.to_string(),
            ban_created: e.create_time,
            banned_until: e.ban_until,
            ban_duration: e.ban_until - e.create_time,
            time_remaining: e.ban_until - now,
        })
        .collect()
}

/// Core's warning when the pre-v20 `banlist.dat` sits in `net_dir` (`GetDataDirNet()`); Core
/// never reads that file. `CBanDB::Read` logs it each time it loads the ban list, before
/// `banlist.json` is read.
// Core: addrdb.cpp:153-157 CBanDB::Read
pub fn legacy_banlist_warning(net_dir: &Path) -> Option<String> {
    let dat = net_dir.join("banlist.dat");
    dat.exists().then(|| {
        format!(
            "banlist.dat ignored because it can only be read by Dash Core version 19.x. Remove {} to silence this warning.",
            quoted(&dat.display().to_string())
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Core's text, with the path `fs::quoted`; nothing without the legacy file.
    #[test]
    fn legacy_banlist_warning_is_cores() {
        let dir = std::env::temp_dir().join(format!("p4a-banlist-dat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(legacy_banlist_warning(&dir), None);
        std::fs::write(dir.join("banlist.dat"), b"").unwrap();
        assert_eq!(
            legacy_banlist_warning(&dir).unwrap(),
            format!(
                "banlist.dat ignored because it can only be read by Dash Core version 19.x. Remove \"{}/banlist.dat\" to silence this warning.",
                dir.display()
            )
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
