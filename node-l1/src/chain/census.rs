//! The census (O2b §6.5): what the scored inputs reached, and what the generator and the tie
//! model did. `fuzzctl` reads it from the stats file (`NODE_L1_CHAIN_STATS`).
//!
//! An input's census merges into the run's only if the input was not discarded (D30); the
//! discards themselves are counted by site and op kind.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::step::ReplyClass;

/// One input's counts.
#[derive(Clone, Debug, Default)]
pub struct Census {
    /// `(op kind, reply class)`.
    pub ops: BTreeMap<(&'static str, &'static str), u64>,
    /// Every reject string reached (BIP22 strings and RPC errors' messages).
    pub rejects: BTreeMap<String, u64>,
    /// The named rows of O2b §6.5 this input reached.
    pub rows: BTreeMap<&'static str, u64>,
    /// Intents the generator dropped (the tie model, an unadmitted spork), by reason.
    pub avoided: BTreeMap<&'static str, u64>,
    /// Restarts deferred behind an extension of the active chain (G-T1).
    pub deferred_restarts: u64,
    /// Replies the generator did not expect, by `op kind: expected`.
    pub unexpected: BTreeMap<String, u64>,
}

fn bump<K: Ord>(map: &mut BTreeMap<K, u64>, key: K) {
    *map.entry(key).or_insert(0) += 1;
}

fn add_counts<K: Ord>(into: &mut BTreeMap<K, u64>, from: BTreeMap<K, u64>) {
    for (key, n) in from {
        *into.entry(key).or_insert(0) += n;
    }
}

pub fn class_name(class: &ReplyClass) -> &'static str {
    match class {
        ReplyClass::Ok => "ok",
        ReplyClass::Duplicate => "duplicate",
        ReplyClass::Inconclusive => "inconclusive",
        ReplyClass::Reject(_) => "reject",
        ReplyClass::Down => "down",
    }
}

impl Census {
    pub fn op(&mut self, kind: &'static str, class: &ReplyClass) {
        bump(&mut self.ops, (kind, class_name(class)));
        if let ReplyClass::Reject(r) = class {
            bump(&mut self.rejects, reject_key(r));
        }
    }

    /// An op the second system did not serve (O2b-3b's Diff mode): its input ends there, out of
    /// domain, counted under `(kind, key)` ([`Unsupported::key`](super::run::Unsupported::key)).
    pub fn unsupported(&mut self, kind: &'static str, key: &'static str) {
        bump(&mut self.ops, (kind, key));
    }

    /// `org=verifydb` outcome records the record plane left out (D17), both sides', under the
    /// row `verifydb`. Diff mode has none until restarts or the restore step are compared.
    pub fn verifydb(&mut self, n: u64) {
        if n > 0 {
            *self.rows.entry("verifydb").or_insert(0) += n;
        }
    }

    pub fn row(&mut self, name: &'static str) {
        bump(&mut self.rows, name);
    }

    pub fn avoided(&mut self, reason: &'static str) {
        bump(&mut self.avoided, reason);
    }

    pub fn deferred_restart(&mut self) {
        self.deferred_restarts += 1;
    }

    pub fn unexpected(&mut self, what: String) {
        bump(&mut self.unexpected, what);
    }

    fn merge(&mut self, other: Census) {
        add_counts(&mut self.ops, other.ops);
        add_counts(&mut self.rejects, other.rejects);
        add_counts(&mut self.rows, other.rows);
        add_counts(&mut self.avoided, other.avoided);
        add_counts(&mut self.unexpected, other.unexpected);
        self.deferred_restarts += other.deferred_restarts;
    }
}

/// The run's totals.
#[derive(Clone, Debug, Default)]
pub struct Totals {
    pub inputs: u64,
    pub discarded: u64,
    /// Discards by the first tie site Core reported (D30; O2b-1 I37's sites 1–14).
    pub discard_sites: BTreeMap<u32, u64>,
    /// Discards by the op kind that fired the taint.
    pub discard_ops: BTreeMap<&'static str, u64>,
    /// CL_ENFORCE ops run, and the discards site 13 caused (O2b-1 I38).
    pub cl_enforce: u64,
    pub site13: u64,
    /// Inputs replayed in the same process, and replays whose reports differed.
    pub replays: u64,
    pub replay_mismatches: u64,
    /// Scored inputs only.
    pub census: Census,
}

impl Totals {
    /// Fold an input in. `discard` is the op kind and the tie sites of the step that fired the
    /// taint, if it fired.
    pub fn input(&mut self, census: Census, discard: Option<(&'static str, &[u32])>) {
        self.inputs += 1;
        self.cl_enforce +=
            census.ops.iter().filter(|((k, _), _)| *k == "cl_enforce").map(|(_, n)| n).sum::<u64>();
        match discard {
            None => self.census.merge(census),
            Some((op, sites)) => {
                self.discarded += 1;
                bump(&mut self.discard_ops, op);
                if let Some(&site) = sites.first() {
                    bump(&mut self.discard_sites, site);
                }
                if op == "cl_enforce" && sites.contains(&13) {
                    self.site13 += 1;
                }
            }
        }
    }

    /// The tie-discard rate (gated at 1%, O2b §6.5).
    pub fn discard_rate(&self) -> f64 {
        if self.inputs == 0 {
            0.0
        } else {
            self.discarded as f64 / self.inputs as f64
        }
    }

    pub fn to_json(&self) -> Value {
        let c = &self.census;
        let ops: BTreeMap<String, u64> =
            c.ops.iter().map(|((k, cl), n)| (format!("{k}:{cl}"), *n)).collect();
        json!({
            "inputs": self.inputs,
            "discarded": self.discarded,
            "discard_rate": self.discard_rate(),
            "discard_sites": self.discard_sites.iter().map(|(s, n)| (s.to_string(), *n)).collect::<BTreeMap<_, _>>(),
            "discard_ops": self.discard_ops,
            "cl_enforce": self.cl_enforce,
            "site13": self.site13,
            "replays": self.replays,
            "replay_mismatches": self.replay_mismatches,
            "ops": ops,
            "rejects": c.rejects,
            "rows": c.rows,
            "avoided": c.avoided,
            "deferred_restarts": c.deferred_restarts,
            "unexpected": c.unexpected,
        })
    }
}

/// A reject's census key: its text with every block hash in it (`Must submit previous header
/// (<hash>) first`) replaced by `<hash>`.
fn reject_key(reject: &str) -> String {
    let mut key = String::with_capacity(reject.len());
    let mut rest = reject;
    while !rest.is_empty() {
        let hex = rest.bytes().take_while(u8::is_ascii_hexdigit).count();
        if hex == 64 {
            key.push_str("<hash>");
            rest = &rest[64..];
        } else {
            let skip = hex.max(1);
            let end = rest.char_indices().nth(skip).map_or(rest.len(), |(i, _)| i);
            key.push_str(&rest[..end]);
            rest = &rest[end..];
        }
    }
    key
}
