//! A step's result in a system-neutral form (O2b §4.1's planes): what the comparator, the
//! census and the world model read. Core's comes from its decoded report; O2b-3b adds
//! `NodeSut`'s.

use dash_oracle_sys::{ChainOp, ChainReport, Event, Post, Reply};
use dash_statedigest::Record;
use serde_json::Value;

/// One op's result.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub reply: Reply,
    /// The op's C/R/D/A records (an `H` opens each load's).
    pub records: Vec<Record>,
    pub events: Vec<Event>,
    pub post: Post,
}

impl From<ChainReport> for Step {
    fn from(r: ChainReport) -> Step {
        Step {
            reply: r.reply,
            records: r.records,
            events: r.events,
            post: r.post,
        }
    }
}

/// A reply's class, for the census and the expectations (O2b §6.5's `(op kind × reply class)`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReplyClass {
    /// A null or object result (accepted, done).
    Ok,
    /// `submitblock`'s and proposals' BIP22 strings that are not rejects.
    Duplicate,
    Inconclusive,
    /// A BIP22 reject string or an RPC error's message.
    Reject(String),
    /// The session is down.
    Down,
}

impl Step {
    /// The class of `op`'s reply. Most ops reply as their RPC does (`submitblock`'s BIP22
    /// strings, an RPC error); the session's own ops reply in the shim's shapes (oracle.h):
    /// `CL_RECORD` with `ProcessNewChainLock`'s outcome (only `accepted` takes the lock),
    /// `ACTIVATE` and `FLUSH` with `{ok, state}`, `SPORK` with whether it was signed.
    pub fn class(&self, op: &ChainOp) -> ReplyClass {
        let value = match &self.reply.outcome {
            Err(e) if e.message == "session-down" => return ReplyClass::Down,
            Err(e) => return ReplyClass::Reject(e.message.clone()),
            Ok(v) => v,
        };
        match (op, value) {
            (
                ChainOp::ClRecord {
                    ..
                },
                Value::String(s),
            ) if s == "accepted" => ReplyClass::Ok,
            (
                ChainOp::ClRecord {
                    ..
                },
                Value::String(s),
            ) => ReplyClass::Reject(s.clone()),
            (
                ChainOp::Activate
                | ChainOp::Flush {
                    ..
                },
                Value::Object(o),
            ) => {
                if o.get("ok") == Some(&Value::Bool(true)) {
                    ReplyClass::Ok
                } else {
                    let state = o.get("state").and_then(Value::as_str).unwrap_or_default();
                    ReplyClass::Reject(state.to_owned())
                }
            }
            (
                ChainOp::Spork {
                    ..
                },
                Value::Bool(false),
            ) => ReplyClass::Reject("unsigned".into()),
            (_, Value::String(s)) => match s.as_str() {
                "duplicate" => ReplyClass::Duplicate,
                "inconclusive" => ReplyClass::Inconclusive,
                other => ReplyClass::Reject(other.to_owned()),
            },
            (_, Value::Object(o)) if o.get("status") == Some(&Value::from("failure")) => {
                ReplyClass::Reject(
                    o.get("message").and_then(Value::as_str).unwrap_or_default().to_owned(),
                )
            }
            _ => ReplyClass::Ok,
        }
    }
}
