//! Cases and trials (plan §3.2): their ids, each trial's 8-byte id, and the run-wide ledger that
//! keeps every trial's transactions its own.
//!
//! `case_id = op_id.variant@site/par`; `trial_id = case_id + {cold, prop, seq}`. Every byte a trial
//! adds is a function of its id and the setup chain, never of run order, so a filtered run builds
//! the same bytes as a full one.

use std::collections::BTreeMap;
use std::fmt;

use bitcoin_hashes::sha256;

use crate::chain::mutate::{Mode, Par, Site};

/// One trial of a case (plan §3.2's order: cold, then prop, then seq for positive cases).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A submit on a cold cache.
    Cold,
    /// A proposal.
    Prop,
    /// A proposal, then a submit of its own transactions: warm on purpose, credited as the
    /// sequence case only.
    Seq,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Cold => "cold",
            Kind::Prop => "prop",
            Kind::Seq => "seq",
        }
    }

    /// The mode the trial's credited step runs in.
    pub fn mode(self) -> Mode {
        match self {
            Kind::Cold | Kind::Seq => Mode::Submit,
            Kind::Prop => Mode::Proposal,
        }
    }
}

/// A case: an operator's variant at a site on a leg.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaseId {
    pub op: &'static str,
    pub variant: &'static str,
    pub site: Site,
    pub par: Par,
}

impl fmt::Display for CaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}@{}/{}", self.op, self.variant, self.site.name(), self.par.name())
    }
}

/// A trial of a case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrialId {
    pub case: CaseId,
    pub kind: Kind,
}

impl fmt::Display for TrialId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}+{}", self.case, self.kind.name())
    }
}

impl TrialId {
    /// The trial's 8 bytes: the first eight of SHA-256 over its id's text. Distinct ids give
    /// distinct bytes but for a 2^-64 collision, which [`TxidLedger`] would catch.
    pub fn bytes(&self) -> [u8; 8] {
        let digest = sha256::Hash::hash(self.to_string().as_bytes()).to_byte_array();
        let mut out = [0; 8];
        out.copy_from_slice(&digest[..8]);
        out
    }
}

/// Whose a recorded txid is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// A setup block's transaction.
    Setup(u32),
    Trial(String),
}

/// The run-wide invariant (plan §3.2): each trial's non-coinbase txids are disjoint from every
/// other trial's and from every setup transaction, except the rows a scenario declares reused.
/// A trial's set is recorded when it is built, before anything is submitted.
#[derive(Debug, Default)]
pub struct TxidLedger {
    seen: BTreeMap<[u8; 32], Owner>,
}

impl TxidLedger {
    /// Record `txids` for `owner`; `Err` names the first one another owner already has.
    /// `reused` lists the txids the scenario declares shared (none in a10-4's tier 1).
    pub fn record(
        &mut self,
        owner: Owner,
        txids: &[[u8; 32]],
        reused: &[[u8; 32]],
    ) -> Result<(), String> {
        for txid in txids {
            if reused.contains(txid) {
                continue;
            }
            if let Some(other) = self.seen.get(txid).filter(|o| **o != owner) {
                return Err(format!(
                    "txid {} of {owner:?} is {other:?}'s too (plan §3.2's disjointness)",
                    hex(txid)
                ));
            }
        }
        for txid in txids {
            self.seen.entry(*txid).or_insert_with(|| owner.clone());
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Hex of a hash in RPC order (reversed).
pub fn hex(hash: &[u8; 32]) -> String {
    let mut reversed = *hash;
    reversed.reverse();
    to_hex(&reversed)
}

/// Hex of bytes in their order.
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err(format!("odd hex length {}", text.len()));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|e| format!("hex: {e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(op: &'static str, par: Par) -> CaseId {
        CaseId {
            op,
            variant: "reg",
            site: Site::Reg,
            par,
        }
    }

    #[test]
    fn ids_read_as_the_plan_writes_them() {
        let trial = TrialId {
            case: case("cb_amount", Par::Four),
            kind: Kind::Seq,
        };
        assert_eq!(trial.to_string(), "cb_amount.reg@Reg/par4+seq");
        assert_eq!(trial.kind.mode(), Mode::Submit);
        assert_eq!(Kind::Prop.mode(), Mode::Proposal);
    }

    /// Each trial's bytes are its own, and the same in every run.
    #[test]
    fn trial_bytes_are_a_function_of_the_id() {
        let mut seen = std::collections::BTreeSet::new();
        for op in ["cb_amount", "missing_input", "vout_negative"] {
            for par in [Par::One, Par::Four] {
                for kind in [Kind::Cold, Kind::Prop, Kind::Seq] {
                    let trial = TrialId {
                        case: case(op, par),
                        kind,
                    };
                    assert_eq!(trial.bytes(), trial.bytes());
                    assert!(seen.insert(trial.bytes()), "{trial}");
                }
            }
        }
    }

    #[test]
    fn the_ledger_refuses_a_shared_txid_unless_declared() {
        let mut ledger = TxidLedger::default();
        let a = Owner::Trial("a".into());
        ledger.record(a.clone(), &[[1; 32], [2; 32]], &[]).expect("new");
        ledger.record(a, &[[1; 32]], &[]).expect("the same owner again");
        let b = Owner::Trial("b".into());
        let e = ledger.record(b.clone(), &[[3; 32], [2; 32]], &[]).expect_err("shared");
        assert!(e.contains("Trial(\"a\")"), "{e}");
        assert_eq!(ledger.len(), 2, "nothing of a refused set is recorded");
        ledger.record(b, &[[3; 32], [2; 32]], &[[2; 32]]).expect("declared reuse");
        let e = ledger.record(Owner::Setup(7), &[[3; 32]], &[]).expect_err("a setup tx");
        assert!(e.contains("Setup(7)"), "{e}");
    }
}
