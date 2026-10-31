//! Blind recovery at open (SB0 §9.5): sync, read the locators, and resume each subscriber from
//! its own. A crash can lose only a suffix of Buffered commits, so a subscriber may hold data
//! for headers the store lost, or for a branch the restored active chain left.

use dash_kv::{Commit, KvBatch, KvEngine};

use super::hdr::io::HdrIo;
use super::schema::*;
use super::{Error, Result, Store, END};

/// What recovery did to one subscriber.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovered {
    /// No locator, or it names a block of the active chain: nothing to do.
    Kept,
    /// The locator names a block off the active chain, whose branch leaves it at this height,
    /// or an active block below the subscriber's top (rows written above the header tip): the
    /// rows above this height were removed and the locator moved there.
    Rewound(u32),
    /// The locator names a block the store does not know (a lost Buffered suffix), or no common
    /// ancestor was found: the subscriber's rows and locator were removed, back to the anchor.
    Reset,
}

impl<E: KvEngine, F: HdrIo> Store<E, F> {
    /// Recovery at open, after SB14's Open made the winning slot durable (D17), against that
    /// slot as the active tip: each subscriber's locator checked against the active chain, a
    /// rewind committed Durable with the subscriber's rows and locator in one `WriteSet`.
    /// Returns what was done, in [`Subscriber::ALL`] order.
    pub fn recover(&self) -> Result<[Recovered; 3]> {
        let mut out = [Recovered::Kept; 3];
        for s in Subscriber::ALL {
            out[s as usize] = self.recover_sub(s)?;
        }
        Ok(out)
    }

    fn recover_sub(&self, s: Subscriber) -> Result<Recovered> {
        let Some((hash, height)) = self.locator(s)? else {
            return Ok(Recovered::Kept);
        };
        if self.active_height_of(&hash)? == Some(height) {
            // Rows above the locator were written with no header there: nothing checks them.
            if self.view().sub(s).hi.is_some_and(|hi| hi > height) {
                self.rewind_sub(s, height)?;
                return Ok(Recovered::Rewound(height));
            }
            return Ok(Recovered::Kept);
        }
        match self.fork_of(&hash)? {
            Some(fork) => {
                self.rewind_sub(s, fork)?;
                Ok(Recovered::Rewound(fork))
            }
            None => {
                self.clear_sub(s, Commit::Durable)?;
                Ok(Recovered::Reset)
            }
        }
    }

    /// The height where the branch of `hash` leaves the active chain: its highest ancestor
    /// that is active. `None` when the hash or an ancestor is unknown before one is found.
    fn fork_of(&self, hash: &[u8; 32]) -> Result<Option<u32>> {
        let mut at = *hash;
        let mut below: Option<u32> = None;
        loop {
            let Some(id) = self.id_of(&at)? else {
                return Ok(None);
            };
            let row = self
                .row(id)?
                .ok_or_else(|| Error::Corrupt(format!("hdr_hash names id {} with no row", id.0)))?;
            // Each step goes one height down, so a cycle in the records cannot loop forever.
            if below.is_some_and(|b| b.checked_sub(1) != Some(row.height)) {
                return Err(Error::Corrupt(format!("header {} is not one below its child", id.0)));
            }
            below = Some(row.height);
            if self.active_id(row.height) == Some(id) {
                return Ok(Some(row.height));
            }
            if row.kind != 0 {
                return Ok(None);
            }
            at = prev_of(&row);
        }
    }

    /// Removes `s`'s rows above `fork` and points its locator at its new top, in one Durable
    /// `WriteSet`.
    fn rewind_sub(&self, s: Subscriber, fork: u32) -> Result<()> {
        self.commit_naming(
            Commit::Durable,
            |st, db, cap| {
                let bounds = st.sub_bounds(s).truncated(fork);
                let locator = st.locator_at(bounds.hi, cap)?;
                let names = Self::locator_height(locator.as_ref());
                let mut b = db.batch();
                if let Some(above) = fork.checked_add(1) {
                    b.delete_range(s.cf(), &be32(above), &END);
                }
                Self::put_locator(&mut b, s, locator);
                Ok((b, names, bounds))
            },
            |st, bounds| st.rows_write().subs[s as usize] = bounds,
        )
    }
}
