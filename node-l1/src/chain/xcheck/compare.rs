//! The X-check's planes (O2b-4 PLAN §5.1): what the session and a dashd show after one op, read
//! the same way from each, and compared plane by plane.

use dash_evo::spork::SPORK_DEFS;
use dash_oracle_sys::{Post, SporkState};
use serde_json::Value;

use super::opmap::{Mapped, display};
use crate::l3::dashd::{Refusal, RpcError};

/// An RPC's answer, as the session's reply and dashd's are both read: a result, or an error's
/// code (none for the session's `session-down`) and message.
pub type Answer = Result<Value, (Option<i64>, String)>;

/// The session's reply as an [`Answer`].
pub fn session_answer(outcome: &Result<Value, dash_oracle_sys::RpcError>) -> Answer {
    outcome.clone().map_err(|e| (e.code, e.message))
}

/// dashd's reply as an [`Answer`].
pub fn dashd_answer(reply: Result<Value, RpcError>) -> Answer {
    reply.map_err(|e| (Some(e.code), e.message))
}

/// Whether dashd's reply to `mapped` is the session's. RPCs reply alike. A spork update is
/// `"success"` where the session signed it and null where it did not; a flush is any result
/// where the session's flush was one (its utxo planes are compared on their own); a restart is
/// `{"status": "success"}` up, else the same refusal ([`refusal_kind`]) with the same message.
pub fn replies_equal(mapped: &Mapped, session: &Answer, dashd: &Answer) -> bool {
    match mapped {
        Mapped::Rpc {
            ..
        } => session == dashd,
        Mapped::Spork {
            ..
        } => match (session, dashd) {
            (Ok(Value::Bool(true)), Ok(Value::String(s))) => s == "success",
            (Ok(Value::Bool(false)), Ok(Value::Null)) => true,
            _ => false,
        },
        Mapped::Flush => session.is_ok() && dashd.is_ok(),
        Mapped::Restart(_) => {
            let status = |a: &Answer| match a {
                Ok(v) => v.get("status").and_then(Value::as_str).map(str::to_owned),
                Err(_) => None,
            };
            let message = |a: &Answer| match a {
                Ok(v) => v.get("message").and_then(Value::as_str).map(str::to_owned),
                Err(_) => None,
            };
            match (status(session).as_deref(), status(dashd).as_deref()) {
                (Some("success"), Some("success")) => true,
                (Some(s), Some(d)) if s != "success" && d != "success" => {
                    refusal_kind(s) == refusal_kind(d) && message(session) == message(dashd)
                }
                _ => false,
            }
        }
        Mapped::Excluded(_) => false,
    }
}

/// A restart's answer on dashd: up; `failure` for the reindex question a failed load asks; or
/// `init_error` for an `InitError`.
pub fn restart_answer(refusal: Option<&Refusal>) -> Answer {
    Ok(match refusal {
        None => serde_json::json!({"status": "success"}),
        Some(Refusal::Question(text)) => serde_json::json!({"status": "failure", "message": text}),
        Some(Refusal::InitError(text)) => {
            serde_json::json!({"status": "init_error", "message": text})
        }
    })
}

/// How a refused load ends dashd's init, by the session's load status (`ChainstateLoadStatus`'s
/// name, or `init_error` before the load): a `FAILURE` asks the reindex question; a fatal,
/// incompatible or too-small-cache status, and a refusal before the load, are an `InitError`.
// Core: src/init.cpp:2104-2125 at v24.0.0
pub fn refusal_kind(status: &str) -> &str {
    match status {
        "failure" | "interrupted" => status,
        _ => "init_error",
    }
}

/// Whether a restart's answer is a refused load: the program ends there.
pub fn refused(answer: &Answer) -> bool {
    matches!(answer, Ok(v) if v.get("status").and_then(Value::as_str).is_some_and(|s| s != "success"))
}

/// The state planes after an op: the tip, the best header's height (`getblockchaininfo` gives
/// no more of it), IBD, `getchaintips` sorted by height (descending), then hash (the session's
/// order), and every spork's value and whether it is active, in `sporkDefs`' order.
#[derive(Clone, Debug, PartialEq)]
pub struct Planes {
    pub tip: Option<(i64, String)>,
    pub headers: i64,
    pub ibd: Option<bool>,
    pub tips: Vec<Value>,
    pub sporks: Vec<SporkState>,
}

impl Planes {
    /// The session's, from a live post-state. `None` when the session is down.
    pub fn session(post: &Post) -> Option<Planes> {
        if post.down {
            return None;
        }
        let at = |b: &dash_oracle_sys::BlockAt| (i64::from(b.0), format!("{:?}", b.1));
        Some(Planes {
            tip: post.tip.as_ref().map(at),
            headers: match &post.best_header {
                Some(Some(b)) => i64::from(b.0),
                _ => -1,
            },
            ibd: post.ibd,
            tips: post.tips.clone().unwrap_or_default(),
            sporks: post.sporks.clone().unwrap_or_default(),
        })
    }

    /// dashd's, from `getblockchaininfo`, `getchaintips`, `spork show` and `spork active`.
    pub fn dashd(
        info: &Value,
        tips: &Value,
        show: &Value,
        active: &Value,
    ) -> Result<Planes, String> {
        let int = |v: &Value, key: &str| {
            v.get(key).and_then(Value::as_i64).ok_or_else(|| format!("{key} in {v}"))
        };
        let hash = info
            .get("bestblockhash")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("bestblockhash in {info}"))?;
        let mut tips = tips.as_array().ok_or_else(|| format!("getchaintips: {tips}"))?.clone();
        sort_tips(&mut tips);
        let sporks = SPORK_DEFS
            .iter()
            .map(|d| {
                let value = show.get(d.name).and_then(Value::as_i64);
                let on = active.get(d.name).and_then(Value::as_bool);
                match (value, on) {
                    (Some(value), Some(on)) => Ok(SporkState(d.id.0, value, on)),
                    _ => Err(format!("spork {} in {show} / {active}", d.name)),
                }
            })
            .collect::<Result<_, String>>()?;
        Ok(Planes {
            tip: Some((int(info, "blocks")?, hash.to_owned())),
            headers: int(info, "headers")?,
            ibd: info.get("initialblockdownload").and_then(Value::as_bool),
            tips,
            sporks,
        })
    }

    /// The planes in which `self` and `other` differ, in order.
    pub fn differences(&self, other: &Planes) -> Vec<&'static str> {
        let mut differ = Vec::new();
        for (equal, plane) in [
            (self.tip == other.tip, "tip"),
            (self.headers == other.headers, "best_header"),
            (self.ibd == other.ibd, "ibd"),
            (self.tips == other.tips, "tips"),
            (self.sporks == other.sporks, "sporks"),
        ] {
            if !equal {
                differ.push(plane);
            }
        }
        differ
    }
}

/// `getchaintips` rows in the session's order: height descending, then hash.
pub fn sort_tips(tips: &mut [Value]) {
    let key = |t: &Value| {
        (
            std::cmp::Reverse(t.get("height").and_then(Value::as_i64).unwrap_or(i64::MIN)),
            t.get("hash").and_then(Value::as_str).unwrap_or_default().to_owned(),
        )
    };
    tips.sort_by_key(key);
}

/// The one `gettxoutsetinfo` field no X-check comparison reads (OD-L3-124): `disk_size` is
/// Core's `CCoinsViewDB::EstimateSize()` (txdb.cpp:175), LevelDB's `GetApproximateSizes` over the
/// table files written so far, so two dashds given the same writes report different sizes when
/// their background compactions ran at different times. Every other field is compared.
pub const TXOUTSET_UNCOMPARED: &str = "disk_size";

/// Whether two dashds' answers to one op (its `op_kind`) are the same: whole, except that a
/// flush's answer, its `gettxoutsetinfo "muhash"` reply, leaves out [`TXOUTSET_UNCOMPARED`].
/// The only comparison of that reply's JSON: the session pairs check a flush's reply as
/// succeeded on both sides ([`replies_equal`]), and the UTXO planes read named fields
/// ([`UtxoPlanes::dashd`]).
pub fn dashd_answers_equal(op: &str, left: Option<&Answer>, right: Option<&Answer>) -> bool {
    if op != "flush" {
        return left == right;
    }
    let compared = |a: Option<&Answer>| {
        a.cloned().map(|a| {
            a.map(|mut v| {
                if let Some(fields) = v.as_object_mut() {
                    fields.remove(TXOUTSET_UNCOMPARED);
                }
                v
            })
        })
    };
    compared(left) == compared(right)
}

/// The UTXO planes at a `FLUSH ALWAYS` and at a program's end: `gettxoutsetinfo "muhash"`'s
/// `txouts`, `total_amount` (in duffs), `bestblock` and `muhash`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UtxoPlanes {
    pub txouts: u64,
    pub amount: i64,
    pub bestblock: String,
    /// Absent on the session's side where the node's coins were not judged the same.
    pub muhash: Option<String>,
}

impl UtxoPlanes {
    /// The session's: its post's coins and tip, and the node's MuHash3072 (internal order).
    pub fn session(post: &Post, muhash: Option<[u8; 32]>) -> Option<UtxoPlanes> {
        let utxo = post.utxo.as_ref()?;
        Some(UtxoPlanes {
            txouts: utxo.txouts,
            amount: utxo.amount,
            bestblock: format!("{:?}", post.tip.as_ref()?.1),
            muhash: muhash.as_ref().map(display),
        })
    }

    pub fn dashd(info: &Value) -> Result<UtxoPlanes, String> {
        let field = |key: &str| info.get(key).ok_or_else(|| format!("{key} in {info}"));
        let amount = field("total_amount")?.as_f64().ok_or("total_amount")?;
        Ok(UtxoPlanes {
            txouts: field("txouts")?.as_u64().ok_or("txouts")?,
            amount: (amount * 1e8).round() as i64,
            bestblock: field("bestblock")?.as_str().ok_or("bestblock")?.to_owned(),
            muhash: Some(field("muhash")?.as_str().ok_or("muhash")?.to_owned()),
        })
    }

    /// The planes in which dashd's (`self`) and the session's differ; MuHash only where the
    /// session has one.
    pub fn differences(&self, session: &UtxoPlanes) -> Vec<&'static str> {
        let mut differ = Vec::new();
        for (equal, plane) in [
            (self.txouts == session.txouts, "utxo.txouts"),
            (self.amount == session.amount, "utxo.total_amount"),
            (self.bestblock == session.bestblock, "utxo.bestblock"),
            (session.muhash.is_none() || self.muhash == session.muhash, "utxo.muhash"),
        ] {
            if !equal {
                differ.push(plane);
            }
        }
        differ
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::{
        Planes, TXOUTSET_UNCOMPARED, dashd_answers_equal, refused, replies_equal, restart_answer,
        sort_tips,
    };
    use crate::chain::xcheck::opmap::Mapped;
    use crate::l3::dashd::Refusal;

    /// 4f test 6: `getchaintips` rows sort by height, highest first, then by hash, as the
    /// session's post orders them.
    #[test]
    fn chaintips_are_sorted_as_the_design_says() {
        let tip = |height: i64, hash: &str| json!({"height": height, "hash": hash, "status": "x"});
        let mut tips = vec![tip(3, "bb"), tip(5, "cc"), tip(3, "aa"), tip(4, "dd"), tip(5, "0a")];
        sort_tips(&mut tips);
        let order: Vec<(i64, &str)> = tips
            .iter()
            .map(|t| (t["height"].as_i64().unwrap(), t["hash"].as_str().unwrap()))
            .collect();
        assert_eq!(order, [(5, "0a"), (5, "cc"), (4, "dd"), (3, "aa"), (3, "bb")]);
        let info = json!({"blocks": 5, "headers": 6, "bestblockhash": "cc",
                          "initialblockdownload": false});
        let names = dash_evo::spork::SPORK_DEFS.map(|d| d.name);
        let show: serde_json::Map<String, serde_json::Value> =
            names.iter().map(|n| ((*n).to_owned(), json!(4_070_908_800i64))).collect();
        let active: serde_json::Map<String, serde_json::Value> =
            names.iter().map(|n| ((*n).to_owned(), json!(false))).collect();
        let unsorted = json!([tip(3, "bb"), tip(5, "cc")]);
        let planes = Planes::dashd(&info, &unsorted, &show.into(), &active.into()).unwrap();
        assert_eq!(planes.tips[0]["hash"], "cc");
        assert_eq!((planes.tip, planes.headers), (Some((5, "cc".to_owned())), 6));
    }

    /// A restart's reply: up against up, and a refusal against the same kind of refusal with the
    /// same message, the session's load status read as init.cpp ends it (`failure`: the reindex
    /// question; a fatal, incompatible or small-cache status, or `init_error`: an `InitError`).
    #[test]
    fn a_refused_restart_matches_by_kind_and_message() {
        let restart = Mapped::Restart(dash_oracle_sys::RestartKind::Clean);
        let session = |status: &str| Ok(json!({"status": status, "message": "m"}));
        let up = restart_answer(None);
        let question = restart_answer(Some(&Refusal::Question("m".into())));
        let init_error = restart_answer(Some(&Refusal::InitError("m".into())));
        assert!(replies_equal(&restart, &Ok(json!({"status": "success", "message": ""})), &up));
        assert!(replies_equal(&restart, &session("failure"), &question));
        for status in [
            "init_error",
            "failure_fatal",
            "failure_incompatible_db",
            "failure_insufficient_dbcache",
        ] {
            assert!(replies_equal(&restart, &session(status), &init_error), "{status}");
            assert!(!replies_equal(&restart, &session(status), &question), "{status}");
        }
        assert!(!replies_equal(&restart, &session("failure"), &init_error));
        assert!(!replies_equal(&restart, &session("failure"), &up));
        let other = restart_answer(Some(&Refusal::Question("n".into())));
        assert!(!replies_equal(&restart, &session("failure"), &other), "another message");
        assert!(!refused(&up) && refused(&question) && refused(&init_error));
        assert!(!refused(&Ok(serde_json::Value::Null)) && !refused(&Err((Some(-1), "x".into()))));
    }

    /// OD-L3-124: two flush replies that differ only in `disk_size` are the same; a difference in
    /// any other field, or in `disk_size` outside a flush, is not.
    #[test]
    fn a_flush_reply_is_compared_without_disk_size_alone() {
        let reply = json!({
            "height": 203, "bestblock": "5e", "txouts": 528, "bogosize": 26928, "muhash": "f6",
            "total_amount": 99607.14285737, "transactions": 213, "disk_size": 18898,
        });
        let fields: Vec<String> = reply.as_object().unwrap().keys().cloned().collect();
        assert_eq!(fields.len(), 8);
        for field in fields {
            let mut other = reply.clone();
            other[&field] = json!("changed");
            let (left, right) = (Ok(reply.clone()), Ok(other));
            let equal = dashd_answers_equal("flush", Some(&left), Some(&right));
            assert_eq!(equal, field == TXOUTSET_UNCOMPARED, "flush, {field} differs");
            assert!(!dashd_answers_equal("block", Some(&left), Some(&right)), "block, {field}");
        }
        let (left, right) = (Ok(reply.clone()), Err((Some(-1), "x".to_owned())));
        assert!(!dashd_answers_equal("flush", Some(&left), Some(&right)));
        assert!(!dashd_answers_equal("flush", Some(&left), None));
        assert!(dashd_answers_equal("flush", None, None));
    }
}
