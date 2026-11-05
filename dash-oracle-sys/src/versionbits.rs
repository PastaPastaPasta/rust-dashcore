//! Version-bits queries on a chain context (ABI 12; `src/test/oracle/oracle.h`, "version bits").
//!
//! [`Oracle::versionbits_state`] runs a list of `VersionBitsCache` queries, in order, against
//! one cache that starts empty, as a node's does. The EHF signal heights Core reads come from
//! Core's own `CMNHFManager` over the signals each header's block holds ([`SignalChange`]).

use super::addrman::{take_le, take_list, take_u8};
use super::*;

/// From `height` on, until the next change, every header at which V20 is active holds
/// `signals`: `(bit, height of the block that mined the signal)`, what
/// `CMNHFManager::GetForBlock` reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignalChange {
    pub height: i32,
    pub signals: Vec<(u8, i32)>,
}

/// Appends a vector of `(uint8_t bit, int32_t height)`.
pub(crate) fn write_signals(out: &mut Vec<u8>, signals: &[(u8, i32)]) {
    write_compact_size(out, signals.len() as u64);
    for &(bit, height) in signals {
        out.push(bit);
        out.extend(height.to_le_bytes());
    }
}

/// Appends `changes` in the shim's format (oracle.h "version bits": a vector of `int32_t height`
/// then a vector of `(uint8_t bit, int32_t height)`).
pub(crate) fn write_changes(out: &mut Vec<u8>, changes: &[SignalChange]) {
    write_compact_size(out, changes.len() as u64);
    for change in changes {
        out.extend(change.height.to_le_bytes());
        write_signals(out, &change.signals);
    }
}

/// A query. `None` heights stand for `nullptr` (genesis's parent); others name the context's
/// block at that height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VbQuery {
    /// `VersionBitsCache::State(index, params, pos)`.
    State {
        pos: u8,
        height: Option<i32>,
    },
    /// `VersionBitsCache::StateSinceHeight(index, params, pos)`.
    StateSinceHeight {
        pos: u8,
        height: Option<i32>,
    },
    /// `VersionBitsCache::Statistics(index, params, pos, &blocks)`.
    Statistics {
        pos: u8,
        height: Option<i32>,
    },
    /// `VersionBitsCache::ComputeBlockVersion(index, params)`.
    ComputeBlockVersion {
        height: Option<i32>,
    },
    /// `CMNHFManager::GetSignalsStage(index)`.
    SignalsStage {
        height: Option<i32>,
    },
}

impl VbQuery {
    fn encode(self, out: &mut Vec<u8>) {
        let (op, pos, height) = match self {
            Self::State {
                pos,
                height,
            } => (0u8, pos, height),
            Self::StateSinceHeight {
                pos,
                height,
            } => (1, pos, height),
            Self::Statistics {
                pos,
                height,
            } => (2, pos, height),
            Self::ComputeBlockVersion {
                height,
            } => (3, 0, height),
            Self::SignalsStage {
                height,
            } => (4, 0, height),
        };
        out.extend([op, pos]);
        out.extend(height.unwrap_or(-1).to_le_bytes());
    }
}

/// `BIP9Stats` and the signalling blocks `Statistics` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VbStats {
    pub period: i32,
    pub threshold: i32,
    pub elapsed: i32,
    pub count: i32,
    pub possible: bool,
    pub blocks: Vec<bool>,
}

/// The answer to a [`VbQuery`] of the same kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VbAnswer {
    State(ThresholdState),
    StateSinceHeight(i32),
    Statistics(VbStats),
    ComputeBlockVersion(i32),
    SignalsStage(Vec<(u8, i32)>),
}

impl VbAnswer {
    fn decode(query: VbQuery, r: &mut &[u8]) -> Option<Self> {
        Some(match query {
            VbQuery::State {
                ..
            } => Self::State(ThresholdState::try_from(take_u8(r)?).ok()?),
            VbQuery::StateSinceHeight {
                ..
            } => Self::StateSinceHeight(take_le(r)?),
            VbQuery::Statistics {
                ..
            } => Self::Statistics(VbStats {
                period: take_le(r)?,
                threshold: take_le(r)?,
                elapsed: take_le(r)?,
                count: take_le(r)?,
                possible: take_u8(r)? != 0,
                blocks: take_list(r, |r| Some(take_u8(r)? != 0))?,
            }),
            VbQuery::ComputeBlockVersion {
                ..
            } => Self::ComputeBlockVersion(take_le(r)?),
            VbQuery::SignalsStage {
                ..
            } => Self::SignalsStage(take_list(r, |r| Some((take_u8(r)?, take_le(r)?)))?),
        })
    }
}

impl Oracle {
    /// `oracle_versionbits_state`: `queries`, in order, against one empty `VersionBitsCache` on
    /// the context's chain (whose `deployments` should list none), with the EHF signals
    /// `signals` describes. [`ErrorKind::Context`] when Core reads the signals of a placeholder
    /// block below the headers at which V20 is active; [`ErrorKind::Argument`] when a
    /// deployment's period is below 1, or for a malformed change or query.
    pub fn versionbits_state(
        &self,
        chain: &Chain,
        ctx: &Context,
        signals: &[SignalChange],
        queries: &[VbQuery],
    ) -> Result<Vec<VbAnswer>> {
        let ctx = ctx.serialize();
        let mut changes = Vec::new();
        write_changes(&mut changes, signals);
        let mut calls = Vec::new();
        write_compact_size(&mut calls, queries.len() as u64);
        for query in queries {
            query.encode(&mut calls);
        }
        self.on_chain(chain, || {
            let bytes = self.with_buffer(queries.len() * 32 + 4096, |out, len| unsafe {
                (self.api.versionbits_state)(
                    ctx.as_ptr(),
                    ctx.len(),
                    changes.as_ptr(),
                    changes.len(),
                    calls.as_ptr(),
                    calls.len(),
                    out,
                    len,
                )
            })?;
            let mut r = &bytes[..];
            let answers = queries
                .iter()
                .map(|&query| VbAnswer::decode(query, &mut r))
                .collect::<Option<Vec<_>>>()
                .filter(|_| r.is_empty());
            Ok(answers.expect("the shim's version-bits answer format"))
        })
    }
}
