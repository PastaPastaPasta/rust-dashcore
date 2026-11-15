//! The op map (O2b-4 PLAN §5.3): each chain op as the dashd RPC that runs it, or excluded at M3.

use dash_evo::spork::SPORK_DEFS;
use dash_oracle_sys::{ChainOp, FlushMode, RestartKind};
use serde_json::{Value, json};

/// How dashd runs one op.
#[derive(Clone, Debug, PartialEq)]
pub enum Mapped {
    /// One RPC, whose reply is the session's.
    Rpc {
        method: &'static str,
        params: Value,
    },
    /// `gettxoutsetinfo "muhash"`: `FLUSH ALWAYS`, which it forces before it reads the set.
    // Core: src/rpc/blockchain.cpp:1238 at v24.0.0
    Flush,
    /// `stop` (clean) or SIGKILL, then a start.
    Restart(RestartKind),
    /// `sporkupdate <name> <value>`: `"success"` where the session's update was signed, else
    /// null.
    // Core: src/rpc/node.cpp:207-245 at v24.0.0
    Spork {
        name: &'static str,
        value: i64,
    },
    /// No RPC runs it at M3: the reason.
    Excluded(&'static str),
}

/// `op` on dashd. `sporks_admitted`: whether the spork experiment has passed, without which a
/// `SPORK` op is excluded.
pub fn map(op: &ChainOp, sporks_admitted: bool) -> Mapped {
    let rpc = |method, params| Mapped::Rpc {
        method,
        params,
    };
    match op {
        ChainOp::Block(bytes) => rpc("submitblock", json!([hex(bytes)])),
        ChainOp::Header(header) => rpc("submitheader", json!([hex(header)])),
        ChainOp::Proposal(bytes) => {
            rpc("getblocktemplate", json!([{"mode": "proposal", "data": hex(bytes)}]))
        }
        ChainOp::Invalidate(hash) => rpc("invalidateblock", json!([display(hash)])),
        ChainOp::Reconsider {
            hash,
            ignore_chainlocks,
        } => rpc("reconsiderblock", json!([display(hash), ignore_chainlocks])),
        ChainOp::Precious(hash) => rpc("preciousblock", json!([display(hash)])),
        ChainOp::SetTime(time) => rpc("setmocktime", json!([time])),
        ChainOp::Flush {
            mode: FlushMode::Always,
            ..
        } => Mapped::Flush,
        ChainOp::Flush {
            ..
        } => Mapped::Excluded("flush_not_always"),
        ChainOp::Restart(kind) => Mapped::Restart(*kind),
        ChainOp::Spork {
            ..
        } if !sporks_admitted => Mapped::Excluded("spork_not_admitted"),
        ChainOp::Spork {
            id,
            value,
        } => match SPORK_DEFS.iter().find(|d| d.id.0 == *id) {
            Some(def) => Mapped::Spork {
                name: def.name,
                value: *value,
            },
            None => Mapped::Excluded("spork_unknown"),
        },
        ChainOp::Activate => Mapped::Excluded("activate"),
        ChainOp::ClRecord {
            ..
        } => Mapped::Excluded("cl_record"),
        ChainOp::ClEnforce => Mapped::Excluded("cl_enforce"),
    }
}

/// Lowercase hex of `bytes`, in order.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A hash held in Core's internal byte order, as RPCs print it (reversed).
pub fn display(hash: &[u8; 32]) -> String {
    let mut reversed = *hash;
    reversed.reverse();
    hex(&reversed)
}

#[cfg(test)]
mod tests {
    use dash_oracle_sys::{ChainOp, FlushMode, RestartKind};

    use super::{Mapped, display, map};
    use crate::chain::core::{SPORK_19, SPORK_OFF};

    /// 4f test 1: every op kind is mapped or excluded, never unhandled; `SPORK` only once
    /// admitted.
    #[test]
    fn the_op_map_covers_every_op_kind() {
        let hash = [7; 32];
        let flush = |mode| ChainOp::Flush {
            mode,
            label: String::new(),
        };
        let spork = ChainOp::Spork {
            id: SPORK_19,
            value: SPORK_OFF,
        };
        let rpc = |op: &ChainOp| match map(op, false) {
            Mapped::Rpc {
                method,
                params,
            } => (method, params),
            other => panic!("{op:?}: {other:?}"),
        };
        assert_eq!(
            rpc(&ChainOp::Block(vec![1, 0xab])),
            ("submitblock", serde_json::json!(["01ab"]))
        );
        assert_eq!(rpc(&ChainOp::Header([0; 80])).0, "submitheader");
        assert_eq!(
            rpc(&ChainOp::Proposal(vec![2])),
            ("getblocktemplate", serde_json::json!([{"mode": "proposal", "data": "02"}]))
        );
        assert_eq!(
            rpc(&ChainOp::Invalidate(hash)),
            ("invalidateblock", serde_json::json!([display(&hash)]))
        );
        assert_eq!(
            rpc(&ChainOp::Reconsider {
                hash,
                ignore_chainlocks: true
            }),
            ("reconsiderblock", serde_json::json!([display(&hash), true]))
        );
        assert_eq!(rpc(&ChainOp::Precious(hash)).0, "preciousblock");
        assert_eq!(rpc(&ChainOp::SetTime(5)), ("setmocktime", serde_json::json!([5])));
        assert_eq!(map(&flush(FlushMode::Always), false), Mapped::Flush);
        for mode in [FlushMode::IfNeeded, FlushMode::Periodic, FlushMode::IndexOnly] {
            assert_eq!(map(&flush(mode), true), Mapped::Excluded("flush_not_always"));
        }
        for kind in [RestartKind::Clean, RestartKind::Kill] {
            assert_eq!(map(&ChainOp::Restart(kind), false), Mapped::Restart(kind));
        }
        assert_eq!(map(&spork, false), Mapped::Excluded("spork_not_admitted"));
        assert_eq!(
            map(&spork, true),
            Mapped::Spork {
                name: "SPORK_19_CHAINLOCKS_ENABLED",
                value: SPORK_OFF
            }
        );
        let unknown = ChainOp::Spork {
            id: 1,
            value: 0,
        };
        assert_eq!(map(&unknown, true), Mapped::Excluded("spork_unknown"));
        assert_eq!(map(&ChainOp::Activate, true), Mapped::Excluded("activate"));
        let cl = ChainOp::ClRecord {
            height: 1,
            hash,
            sig: [0; 96],
        };
        assert_eq!(map(&cl, true), Mapped::Excluded("cl_record"));
        assert_eq!(map(&ChainOp::ClEnforce, true), Mapped::Excluded("cl_enforce"));
        // The display order is the reverse of the internal one.
        let mut h = [0; 32];
        h[0] = 1;
        assert!(display(&h).ends_with("01") && display(&h).starts_with("00"));
    }
}
