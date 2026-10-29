//! Core's `debugLogs` lines from the block build (EV2 §3.9): `ProcessSpecialTxsInBlock` builds
//! with `debugLogs = true`, so Core logs every registration, update, revocation, removal, PoSe
//! punishment and ban with fixed texts. They never affect results.

use dash_primitives::OutPoint;

use crate::ProTxHash;

/// One `debugLogs` line of `RebuildListFromBlock` or `PoSePunish`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DmlEvent {
    /// A ProRegTx reused a registered collateral (`specialtxman.cpp` v24:395-399).
    Replaced {
        pro_tx: ProTxHash,
        collateral: OutPoint,
        height: i32,
        total: u64,
    },
    /// A ProRegTx added a masternode (v24:450-452).
    Added {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A ProUpServTx revived a banned masternode (v24:500-502).
    Revived {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A ProUpServTx or ProUpRegTx updated a masternode (v24:518-520, 583-585).
    Updated {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A ProUpRevTx revoked the operator key (v24:615-617).
    Revoked {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A ProUpShareTx updated a share (v24:645-647).
    ShareUpdated {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A ProUpSharedRegTx updated a shared registrar (v24:698-700).
    SharedRegistrarUpdated {
        pro_tx: ProTxHash,
        height: i32,
    },
    /// A transaction spent a collateral (v24:749-753).
    CollateralSpent {
        pro_tx: ProTxHash,
        collateral: OutPoint,
        height: i32,
        total: u64,
    },
    /// `PoSePunish` on a masternode that was not banned (`deterministicmns.cpp` v24:298-302).
    Punished {
        pro_tx: ProTxHash,
        height: i32,
        banned: bool,
        from: i32,
        to: i32,
        max: i32,
    },
    /// An Evo payee's streak grew (v24:769-771; category `mnpayments`).
    StreakBumped {
        pro_tx: ProTxHash,
        to: i32,
    },
    /// An unpaid Evo's streak was reset (v24:783-785; category `mnpayments`).
    StreakReset {
        pro_tx: ProTxHash,
        from: i32,
    },
}

impl DmlEvent {
    /// Core's log text, without the `__func__` prefix and, for `Added`, `Updated`, `Revoked`,
    /// `ShareUpdated` and `SharedRegistrarUpdated`, without the `": " + proTx.ToString()` suffix
    /// Core appends (`specialtxman.cpp` v24:451, 519, 584, 616, 646, 699; EV2b D19).
    pub fn core_message(&self) -> String {
        match self {
            DmlEvent::Replaced {
                pro_tx,
                collateral,
                height,
                total,
            } => format!(
                "MN {pro_tx} removed from list because collateral was used for a new ProRegTx. \
                 collateralOutpoint={}, nHeight={height}, mapCurMNs.allMNsCount={total}",
                short(collateral)
            ),
            DmlEvent::Added {
                pro_tx,
                height,
            } => format!("MN {pro_tx} added at height {height}"),
            DmlEvent::Revived {
                pro_tx,
                height,
            } => format!("MN {pro_tx} revived at height {height}"),
            DmlEvent::Updated {
                pro_tx,
                height,
            } => format!("MN {pro_tx} updated at height {height}"),
            DmlEvent::Revoked {
                pro_tx,
                height,
            } => format!("MN {pro_tx} revoked operator key at height {height}"),
            DmlEvent::ShareUpdated {
                pro_tx,
                height,
            } => format!("MN {pro_tx} updated share at height {height}"),
            DmlEvent::SharedRegistrarUpdated {
                pro_tx,
                height,
            } => format!("MN {pro_tx} updated shared registrar at height {height}"),
            DmlEvent::CollateralSpent {
                pro_tx,
                collateral,
                height,
                total,
            } => format!(
                "MN {pro_tx} removed from list because collateral was spent. \
                 collateralOutpoint={}, nHeight={height}, mapCurMNs.allMNsCount={total}",
                short(collateral)
            ),
            DmlEvent::Punished {
                pro_tx,
                height,
                banned,
                from,
                to,
                max,
            } => format!(
                "{} MN {pro_tx} at height {height}, penalty {from}->{to} (max={max})",
                if *banned {
                    "banned"
                } else {
                    "punished"
                }
            ),
            DmlEvent::StreakBumped {
                pro_tx,
                to,
            } => format!("MN {pro_tx} is an EvoNode, bumping nConsecutivePayments to {to}"),
            DmlEvent::StreakReset {
                pro_tx,
                from,
            } => format!("MN {pro_tx}, reset nConsecutivePayments {from}->0"),
        }
    }
}

/// `COutPoint::ToStringShort()`: `txid-n`.
fn short(op: &OutPoint) -> String {
    format!("{}-{}", op.hash, op.index)
}

/// Where the build reports its `debugLogs` lines (EV2 §2.11). Never affects results.
pub trait DmlTrace {
    /// Records one line.
    fn event(&mut self, event: DmlEvent);
}

/// Discards every line.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTrace;

impl DmlTrace for NoTrace {
    fn event(&mut self, _: DmlEvent) {}
}

/// Collects every line, in order.
impl DmlTrace for Vec<DmlEvent> {
    fn event(&mut self, event: DmlEvent) {
        self.push(event);
    }
}
