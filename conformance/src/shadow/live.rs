//! JSON-RPC adapters used by the live shadow comparator.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::digest::extract::NodeRpc;
use crate::rpc::{Auth, Rpc};

use super::{ChainLock, MissingCoinstatsIndex, NodeReply, PeerRpc, PeerStatus, RdashdRpc};

/// The fixed warning prefix emitted by rdashd when its active chain disagrees with a ChainLock.
pub const CHAINLOCK_WARNING_PREFIX: &str =
    "Warning: This node disagrees with the ChainLocked chain";

/// A live peer (dashd) JSON-RPC adapter.
pub struct LivePeer {
    rpc: Rpc,
}

impl LivePeer {
    pub fn new(url: String, auth: Auth, timeout: Duration) -> Self {
        Self {
            rpc: Rpc::with_timeout(url, auth, timeout),
        }
    }
}

/// `getbestchainlock`, `None` when the node has no ChainLock (`RPC_INTERNAL_ERROR`, "Unable to
/// find any ChainLock") or no such method (`RPC_METHOD_NOT_FOUND`); any other answer is an error.
fn best_chainlock(rpc: &Rpc) -> Result<Option<ChainLock>> {
    const RPC_INTERNAL_ERROR: i64 = -32603;
    const RPC_METHOD_NOT_FOUND: i64 = -32601;
    match rpc.try_call("getbestchainlock", json!([])).context("getbestchainlock")? {
        Ok(value) => parse_chainlock(&value).map(Some),
        Err(RPC_INTERNAL_ERROR | RPC_METHOD_NOT_FOUND) => Ok(None),
        Err(code) => Err(anyhow!("getbestchainlock: RPC error {code}")),
    }
}

impl PeerRpc for LivePeer {
    /// Fails with [`MissingCoinstatsIndex`] when `getindexinfo` answers without
    /// `coinstatsindex`; any other failure (a refused connection, -28 while the peer loads,
    /// another RPC error) is an ordinary RPC failure.
    fn status(&self) -> Result<PeerStatus> {
        let index = self.rpc.call("getindexinfo", json!([]))?;
        let index = index.get("coinstatsindex").ok_or(MissingCoinstatsIndex)?;
        let index_height = number(index, "best_block_height")?;
        let chain = self.rpc.call("getblockchaininfo", json!([]))?;
        let headers = number(&chain, "headers")?;
        Ok(PeerStatus {
            index_height,
            headers,
            chainlock: best_chainlock(&self.rpc)?,
        })
    }

    fn node(&self) -> Result<&dyn NodeRpc> {
        Ok(&self.rpc)
    }
}

/// A live rdashd JSON-RPC adapter.
pub struct LiveRdashd {
    rpc: Rpc,
}

impl LiveRdashd {
    pub fn new(url: String, auth: Auth, timeout: Duration) -> Self {
        Self {
            rpc: Rpc::with_timeout(url, auth, timeout),
        }
    }
}

impl RdashdRpc for LiveRdashd {
    fn status(&self) -> Result<NodeReply> {
        let info = self.rpc.call("getblockchaininfo", json!([]))?;
        let warnings = match info.get("warnings") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(items)) => {
                items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ")
            }
            Some(other) => other.to_string(),
            None => String::new(),
        };
        Ok(NodeReply {
            warnings,
            chainlock: best_chainlock(&self.rpc),
        })
    }
}

fn number(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .with_context(|| format!("RPC result missing integer {key}"))
}

/// `getbestchainlock`'s reply, as Core shapes it: `{"blockhash", "height", "signature", …}`.
fn parse_chainlock(value: &Value) -> Result<ChainLock> {
    let h = number(value, "height").context("getbestchainlock")?;
    let hash = value
        .get("blockhash")
        .and_then(Value::as_str)
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| anyhow!("getbestchainlock: missing blockhash"))?;
    Ok(ChainLock {
        h,
        hash: hash.to_owned(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::{LivePeer, LiveRdashd, parse_chainlock};
    use crate::rpc::Auth;
    use crate::shadow::{MissingCoinstatsIndex, PeerRpc, RdashdRpc};

    /// A JSON-RPC server on a free port that answers each request with `answer(method)`'s
    /// `(result, error)` pair.
    pub(crate) fn fake_rpc(answer: impl Fn(&str) -> (Value, Value) + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    return;
                };
                let mut reader = BufReader::new(stream);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                let (result, error) = answer(request["method"].as_str().unwrap());
                let reply =
                    json!({"result": result, "error": error, "id": request["id"]}).to_string();
                let status = if error.is_null() {
                    "200 OK"
                } else {
                    "500 Internal Server Error"
                };
                let mut stream = reader.into_inner();
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
        });
        url
    }

    pub(crate) fn auth() -> Auth {
        Auth::UserPass("u".into(), "p".into())
    }

    pub(crate) fn error(code: i64) -> (Value, Value) {
        (Value::Null, json!({"code": code, "message": "no"}))
    }

    pub(crate) fn no_coinstatsindex(_: &str) -> (Value, Value) {
        (json!({"txindex": {"synced": true, "best_block_height": 3}}), Value::Null)
    }

    #[test]
    fn peer_status_is_fatal_only_without_coinstatsindex() {
        let url = fake_rpc(no_coinstatsindex);
        let peer = LivePeer::new(url, auth(), Duration::from_secs(5));
        assert!(peer.status().unwrap_err().is::<MissingCoinstatsIndex>());

        for code in [-28, -1] {
            let url = fake_rpc(move |_| error(code));
            let peer = LivePeer::new(url, auth(), Duration::from_secs(5));
            assert!(!peer.status().unwrap_err().is::<MissingCoinstatsIndex>());
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let peer = LivePeer::new(url, auth(), Duration::from_secs(5));
        assert!(!peer.status().unwrap_err().is::<MissingCoinstatsIndex>());
    }

    #[test]
    fn rdashd_chainlock_failures_other_than_none_are_errors() {
        let url = fake_rpc(|method| match method {
            "getblockchaininfo" => (json!({"warnings": ""}), Value::Null),
            _ => error(-32603),
        });
        let node = LiveRdashd::new(url, auth(), Duration::from_secs(5));
        assert_eq!(node.status().unwrap().chainlock.unwrap(), None);

        let url = fake_rpc(|method| match method {
            "getblockchaininfo" => (json!({"warnings": ""}), Value::Null),
            _ => error(-32601),
        });
        let node = LiveRdashd::new(url, auth(), Duration::from_secs(5));
        assert_eq!(node.status().unwrap().chainlock.unwrap(), None);

        let url = fake_rpc(|method| match method {
            "getblockchaininfo" => (json!({"warnings": ""}), Value::Null),
            _ => error(-1),
        });
        let node = LiveRdashd::new(url, auth(), Duration::from_secs(5));
        assert!(node.status().unwrap().chainlock.is_err());

        let url = fake_rpc(|method| match method {
            "getblockchaininfo" => (json!({"warnings": ""}), Value::Null),
            _ => (json!({"height": 3}), Value::Null),
        });
        let node = LiveRdashd::new(url, auth(), Duration::from_secs(5));
        assert!(node.status().unwrap().chainlock.is_err());
    }

    #[test]
    fn chainlock_reply_parses_core_keys() {
        let cl = parse_chainlock(
            &json!({"blockhash": "ab", "height": 7, "signature": "00", "known_block": true}),
        )
        .unwrap();
        assert_eq!((cl.h, cl.hash.as_str()), (7, "ab"));
        assert!(parse_chainlock(&json!({"blockhash": "", "height": 7})).is_err());
        assert!(parse_chainlock(&json!({"blockhash": "ab"})).is_err());
    }
}
