use std::collections::BTreeMap;

use anyhow::{Result, anyhow, ensure};
use dash_statedigest::compare::{Projector, ProjectorSnapshot, Slot};
use dash_statedigest::{Chain, Kind, Origin, Outcome, Record, parse_line};
use serde::{Deserialize, Serialize};

use super::tail::Tail;

pub struct Side {
    pub tail: Tail,
    pub proj: Projector,
    pub pending: BTreeMap<i64, Outcome>,
    pub run: Vec<u8>,
    pub chain: Option<Chain>,
    pub checkpoint_seen: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SideState {
    pub tail: Tail,
    pub projector: ProjectorSnapshot,
    pub pending: BTreeMap<i64, String>,
    pub run: Vec<u8>,
    pub chain: Option<String>,
    #[serde(default)]
    pub checkpoint_seen: bool,
}

impl Side {
    pub fn new(tail: Tail) -> Self {
        Self {
            tail,
            proj: Projector::new(),
            pending: BTreeMap::new(),
            run: Vec::new(),
            chain: None,
            checkpoint_seen: false,
        }
    }

    pub fn slot(&self, h: i64) -> Option<&Slot> {
        let i = self.proj.chain().binary_search_by_key(&h, |s| s.h).ok()?;
        self.proj.chain().get(i)
    }

    pub fn tip(&self) -> Option<i64> {
        self.proj.chain().last().map(|s| s.h)
    }

    pub fn feed(&mut self, at: usize, record: &Record) -> Result<()> {
        if let Record::Header(h) = record {
            ensure!(self.chain.is_none_or(|chain| chain == h.chain), "digest changed chains");
            self.chain = Some(h.chain);
            self.run = h.run.clone();
            self.checkpoint_seen = false;
        } else {
            ensure!(self.chain.is_some(), "digest does not begin with H");
        }
        self.proj.feed(at, record)?;
        if let Record::Outcome(o) = record {
            if o.kind() == Kind::Connect && o.id.org == Origin::Tip {
                drop(self.pending.split_off(&o.id.h));
                self.pending.insert(o.id.h, o.clone());
            } else if o.kind() == Kind::Disconnect
                && matches!(o.id.org, Origin::Tip | Origin::Replay)
            {
                // Only projection changes decide whether a disconnect removed the record.
                if self.slot(o.id.h).is_none_or(|s| s.b != o.id.b) {
                    self.pending.remove(&o.id.h);
                }
            }
        }
        Ok(())
    }

    pub fn observe_checkpoint(&mut self, height: i64) {
        if self.slot(height).is_some() {
            self.checkpoint_seen = true;
        }
    }

    pub fn snapshot(&self) -> Result<SideState> {
        let pending = self
            .pending
            .iter()
            .map(|(h, o)| {
                let mut bytes = Vec::new();
                Record::Outcome(o.clone()).encode(&mut bytes);
                Ok((*h, String::from_utf8(bytes)?))
            })
            .collect::<Result<_>>()?;
        Ok(SideState {
            tail: self.tail.clone(),
            projector: self.proj.snapshot(),
            pending,
            run: self.run.clone(),
            chain: self.chain.map(|c| c.as_str().to_owned()),
            checkpoint_seen: self.checkpoint_seen,
        })
    }

    pub fn restore(s: SideState) -> Result<Self> {
        let pending = s
            .pending
            .into_iter()
            .map(|(h, text)| {
                let Record::Outcome(o) = parse_line(text.as_bytes())? else {
                    return Err(anyhow!("pending record is not an outcome"));
                };
                ensure!(
                    o.kind() == Kind::Connect && o.id.org == Origin::Tip && o.id.h == h,
                    "invalid pending record"
                );
                Ok((h, o))
            })
            .collect::<Result<_>>()?;
        let chain = s
            .chain
            .map(|c| Chain::from_bytes(c.as_bytes()).ok_or_else(|| anyhow!("invalid saved chain")))
            .transpose()?;
        let side = Self {
            tail: s.tail,
            proj: Projector::restore(s.projector)?,
            pending,
            run: s.run,
            chain,
            checkpoint_seen: s.checkpoint_seen,
        };
        for (h, o) in &side.pending {
            ensure!(side.slot(*h).is_some_and(|s| s.b == o.id.b), "pending record is off chain");
        }
        Ok(side)
    }
}
