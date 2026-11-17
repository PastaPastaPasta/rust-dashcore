use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Crit {
    D1,
    D2,
    D3,
    D4,
    D5,
    D6,
    Stall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Warn {
    TipSkew,
    ChainLockSkew,
    Rewind,
    Rejection,
    PeerRpc,
    NodeRpc,
    Lag,
    /// An `--on-crit` hook could not be started; it is retried at every poll.
    Hook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Class {
    Crit(Crit),
    Warn(Warn),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alarm {
    pub class: Class,
    pub h: Option<i64>,
    pub b: Option<String>,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AlarmKey {
    pub class: Class,
    pub h: Option<i64>,
    pub b: Option<String>,
}

impl Alarm {
    pub fn key(&self) -> AlarmKey {
        AlarmKey {
            class: self.class,
            h: self.h,
            b: self.b.clone(),
        }
    }
}
