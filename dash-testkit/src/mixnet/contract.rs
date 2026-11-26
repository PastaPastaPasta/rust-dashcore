//! The mixed-network contract (PLAN-O12b §4 O12b-1): the observation-log records every run
//! writes, and the [`RolePlan`] that says which slot runs which implementation.
//!
//! # Records
//!
//! Each log line is one [`Envelope`] in JSON:
//! `{"v":1,"run":…,"seq":…,"mono_ns":…,"src":…,"kind":"<Record>",…the record's fields}`.
//! An unknown `kind`, a `v` other than 1, an unknown field and a missing nullable field are all
//! refused. [`check_log`] checks that a log is one run in `seq` order, opened by a `Run` its plan
//! admits.
//!
//! Where the records differ from the plan's sketch:
//! - The envelope is flat, so a [`Run`]'s own kind is `run_kind` (the envelope's `kind` names the
//!   record), and a [`Handoff`]'s position is the envelope's `seq`.
//! - A [`Block`]'s miner is one nullable `miner {slot, runs}`, so a half-known miner is not
//!   representable.
//! - [`MnDiff`] carries `protx listdiff`'s changed PoSe fields; `protx diff` has none.
//!
//! # Presets
//!
//! Slots are `node0` (the controller), `reg<i>` (regular masternodes) and `evo<i>` (evonodes),
//! indexes from 0. Nodes outside the plan are other non-masternode nodes, `node<i>`, and run
//! dashd in every preset (mode A covers rdashd non-masternodes).
//!
//! | Preset | Context | Regular | Evo | Rust-labelled slots |
//! |---|---|---|---|---|
//! | `modeb5` | regtest | 5 | 0 | reg0..reg4 |
//! | `alt5` | regtest | 5 | 0 | reg0, reg2, reg4 |
//! | `alt8` (rotation) | regtest | 8 | 0 | reg0, reg2, reg4, reg6 |
//! | `allrust:<R>:<E>` | regtest | R | E | every masternode slot, and node0 |
//! | `devnet` | devnet | 20 | 12 (DEC-1640) | reg0..reg9, evo0..evo5 |
//! | `ratio1:3` | devnet | 20 | 12 | reg0..reg4, evo0..evo2 (the first ¼ of each kind) |
//! | `ratio3:1` | devnet | 20 | 12 | reg0..reg14, evo0..evo8 (the first ¾ of each kind) |
//!
//! # Roles
//!
//! For a Rust-labelled masternode slot, `runs` is Rust iff the caps hold `mn` and either `dkg`
//! or the context is regtest; otherwise the slot runs dashd, and the plan admits only the
//! rehearsal run kind of its context. A slot is a twin (`CoreTwin`: a dashd twin does its DKG)
//! iff it runs Rust and the caps lack `dkg`. node0 and every other non-masternode node run
//! dashd, except node0 under `allrust`, which runs rdashd as mode A does and is never a twin.
//! Regtest admits `rehearsal` and `stage-a`; devnet admits `devnet-rehearsal` and `soak`. A plan
//! whose node0 runs rdashd (`allrust`) admits only the rehearsal kind: every signature check and
//! every chain fact comes from a dashd node0 (PLAN-O12b R-d, §2).

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The record and plan format version this module reads and writes.
pub const VERSION: u64 = 1;

/// `v`: always [`VERSION`]; anything else fails to deserialize.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct V1;

impl TryFrom<u64> for V1 {
    type Error = String;

    fn try_from(v: u64) -> Result<Self, String> {
        if v == VERSION {
            Ok(V1)
        } else {
            Err(format!("unsupported version {v}; this reader knows {VERSION}"))
        }
    }
}

impl From<V1> for u64 {
    fn from(_: V1) -> u64 {
        VERSION
    }
}

/// A value that did not parse, and what it was meant to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub what: &'static str,
    pub input: String,
}

impl ParseError {
    fn new(what: &'static str, input: &str) -> Self {
        Self {
            what,
            input: input.to_owned(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a valid {}: {:?}", self.what, self.input)
    }
}

impl std::error::Error for ParseError {}

/// Parses `s` as a decimal index with no sign and no leading zero.
fn decimal(s: &str) -> Option<u16> {
    let canonical =
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
    canonical.then(|| s.parse().ok()).flatten()
}

/// A 32-byte value in Core's RPC hex (64 lowercase hex digits): block hashes, txids, proTx
/// hashes, quorum hashes and executable sha256s.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Hex32(String);

impl Hex32 {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Hex32 {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        if s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(Self(s.to_owned()))
        } else {
            Err(ParseError::new("64-digit lowercase hex value", s))
        }
    }
}

impl TryFrom<String> for Hex32 {
    type Error = ParseError;

    fn try_from(s: String) -> Result<Self, ParseError> {
        s.parse()
    }
}

impl From<Hex32> for String {
    fn from(h: Hex32) -> String {
        h.0
    }
}

impl fmt::Display for Hex32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a run happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Context {
    Regtest,
    Devnet,
}

impl Context {
    pub fn name(self) -> &'static str {
        match self {
            Context::Regtest => "regtest",
            Context::Devnet => "devnet",
        }
    }
}

impl FromStr for Context {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        [Context::Regtest, Context::Devnet]
            .into_iter()
            .find(|c| c.name() == s)
            .ok_or_else(|| ParseError::new("context (regtest, devnet)", s))
    }
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A node implementation: a slot's label, or what it actually runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Impl {
    Dashd,
    Rust,
}

impl Impl {
    pub fn rust_if(rust: bool) -> Self {
        if rust {
            Impl::Rust
        } else {
            Impl::Dashd
        }
    }
}

/// What a run is for. Only `stage-a` and `soak` runs with Rust provenance are evidence; a
/// rehearsal checks the harness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunKind {
    Rehearsal,
    StageA,
    DevnetRehearsal,
    Soak,
}

impl RunKind {
    pub const ALL: [RunKind; 4] =
        [RunKind::Rehearsal, RunKind::StageA, RunKind::DevnetRehearsal, RunKind::Soak];

    pub fn context(self) -> Context {
        match self {
            RunKind::Rehearsal | RunKind::StageA => Context::Regtest,
            RunKind::DevnetRehearsal | RunKind::Soak => Context::Devnet,
        }
    }

    pub fn is_rehearsal(self) -> bool {
        matches!(self, RunKind::Rehearsal | RunKind::DevnetRehearsal)
    }
}

/// One Rust-masternode capability switch (`RDASHD_MN_CAPS`, PLAN-O12b §4 O12b-9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cap {
    Mn,
    Sig,
    Lock,
    Qdata,
    Gbt,
    Dkg,
    Gov,
    Cj,
}

impl Cap {
    pub const ALL: [Cap; 8] =
        [Cap::Mn, Cap::Sig, Cap::Lock, Cap::Qdata, Cap::Gbt, Cap::Dkg, Cap::Gov, Cap::Cj];

    pub fn name(self) -> &'static str {
        match self {
            Cap::Mn => "mn",
            Cap::Sig => "sig",
            Cap::Lock => "lock",
            Cap::Qdata => "qdata",
            Cap::Gbt => "gbt",
            Cap::Dkg => "dkg",
            Cap::Gov => "gov",
            Cap::Cj => "cj",
        }
    }
}

/// A set of [`Cap`]s. JSON is a list with no repeats, written in [`Cap::ALL`] order and read in any
/// order; text (the CLI and `RDASHD_MN_CAPS`) is a comma list, or `none`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "Vec<Cap>", into = "Vec<Cap>")]
pub struct Caps(BTreeSet<Cap>);

impl Caps {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn has(&self, cap: Cap) -> bool {
        self.0.contains(&cap)
    }

    pub fn iter(&self) -> impl Iterator<Item = Cap> + '_ {
        self.0.iter().copied()
    }
}

impl FromIterator<Cap> for Caps {
    fn from_iter<I: IntoIterator<Item = Cap>>(caps: I) -> Self {
        Self(caps.into_iter().collect())
    }
}

impl TryFrom<Vec<Cap>> for Caps {
    type Error = String;

    fn try_from(list: Vec<Cap>) -> Result<Self, String> {
        let mut caps = BTreeSet::new();
        for cap in list {
            if !caps.insert(cap) {
                return Err(format!("cap {} is listed twice", cap.name()));
            }
        }
        Ok(Self(caps))
    }
}

impl From<Caps> for Vec<Cap> {
    fn from(caps: Caps) -> Vec<Cap> {
        caps.0.into_iter().collect()
    }
}

impl FromStr for Caps {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        if s == "none" {
            return Ok(Self::none());
        }
        let bad = || {
            ParseError::new(
                "cap list (none, or a comma list of mn sig lock qdata gbt dkg gov cj)",
                s,
            )
        };
        let list = s
            .split(',')
            .map(|name| Cap::ALL.into_iter().find(|c| c.name() == name).ok_or_else(bad))
            .collect::<Result<Vec<_>, _>>()?;
        Self::try_from(list).map_err(|_| bad())
    }
}

impl fmt::Display for Caps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("none");
        }
        let names: Vec<_> = self.iter().map(Cap::name).collect();
        f.write_str(&names.join(","))
    }
}

/// A node of the run: `node<i>` (node0 is the controller), `reg<i>` or `evo<i>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum SlotId {
    Node(u16),
    Regular(u16),
    Evo(u16),
}

impl SlotId {
    pub const CONTROLLER: SlotId = SlotId::Node(0);

    pub fn is_masternode(self) -> bool {
        !matches!(self, SlotId::Node(_))
    }
}

impl FromStr for SlotId {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let parsed = if let Some(i) = s.strip_prefix("node") {
            decimal(i).map(SlotId::Node)
        } else if let Some(i) = s.strip_prefix("reg") {
            decimal(i).map(SlotId::Regular)
        } else if let Some(i) = s.strip_prefix("evo") {
            decimal(i).map(SlotId::Evo)
        } else {
            None
        };
        parsed.ok_or_else(|| ParseError::new("slot (node<i>, reg<i>, evo<i>)", s))
    }
}

impl TryFrom<String> for SlotId {
    type Error = ParseError;

    fn try_from(s: String) -> Result<Self, ParseError> {
        s.parse()
    }
}

impl From<SlotId> for String {
    fn from(slot: SlotId) -> String {
        slot.to_string()
    }
}

impl fmt::Display for SlotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SlotId::Node(i) => write!(f, "node{i}"),
            SlotId::Regular(i) => write!(f, "reg{i}"),
            SlotId::Evo(i) => write!(f, "evo{i}"),
        }
    }
}

/// A topology and its Rust labels; see the module's preset table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Preset {
    ModeB5,
    Alt5,
    Alt8,
    AllRust {
        regular: u16,
        evo: u16,
    },
    Devnet,
    Ratio1To3,
    Ratio3To1,
}

impl Preset {
    pub fn context(self) -> Context {
        match self {
            Preset::ModeB5
            | Preset::Alt5
            | Preset::Alt8
            | Preset::AllRust {
                ..
            } => Context::Regtest,
            Preset::Devnet | Preset::Ratio1To3 | Preset::Ratio3To1 => Context::Devnet,
        }
    }

    /// The masternode counts: (regular, evo).
    pub fn counts(self) -> (u16, u16) {
        match self {
            Preset::ModeB5 | Preset::Alt5 => (5, 0),
            Preset::Alt8 => (8, 0),
            Preset::AllRust {
                regular,
                evo,
            } => (regular, evo),
            Preset::Devnet | Preset::Ratio1To3 | Preset::Ratio3To1 => (20, 12),
        }
    }

    /// Whether the preset labels `slot` Rust.
    pub fn labels_rust(self, slot: SlotId) -> bool {
        let (regular, evo) = self.counts();
        let (i, n) = match slot {
            SlotId::Node(i) => return i == 0 && matches!(self, Preset::AllRust { .. }),
            SlotId::Regular(i) => (i, regular),
            SlotId::Evo(i) => (i, evo),
        };
        i < n
            && match self {
                Preset::ModeB5
                | Preset::AllRust {
                    ..
                } => true,
                Preset::Alt5 | Preset::Alt8 => i % 2 == 0,
                Preset::Devnet => i < n / 2,
                Preset::Ratio1To3 => i < n / 4,
                Preset::Ratio3To1 => i < n * 3 / 4,
            }
    }
}

impl FromStr for Preset {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let preset = match s {
            "modeb5" => Some(Preset::ModeB5),
            "alt5" => Some(Preset::Alt5),
            "alt8" => Some(Preset::Alt8),
            "devnet" => Some(Preset::Devnet),
            "ratio1:3" => Some(Preset::Ratio1To3),
            "ratio3:1" => Some(Preset::Ratio3To1),
            _ => s.strip_prefix("allrust:").and_then(|counts| {
                let (regular, evo) = counts.split_once(':')?;
                let (regular, evo) = (decimal(regular)?, decimal(evo)?);
                (regular > 0 || evo > 0).then_some(Preset::AllRust {
                    regular,
                    evo,
                })
            }),
        };
        preset.ok_or_else(|| {
            ParseError::new(
                "preset (modeb5, alt5, alt8, allrust:<R>:<E>, devnet, ratio1:3, ratio3:1)",
                s,
            )
        })
    }
}

impl TryFrom<String> for Preset {
    type Error = ParseError;

    fn try_from(s: String) -> Result<Self, ParseError> {
        s.parse()
    }
}

impl From<Preset> for String {
    fn from(preset: Preset) -> String {
        preset.to_string()
    }
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Preset::ModeB5 => f.write_str("modeb5"),
            Preset::Alt5 => f.write_str("alt5"),
            Preset::Alt8 => f.write_str("alt8"),
            Preset::AllRust {
                regular,
                evo,
            } => write!(f, "allrust:{regular}:{evo}"),
            Preset::Devnet => f.write_str("devnet"),
            Preset::Ratio1To3 => f.write_str("ratio1:3"),
            Preset::Ratio3To1 => f.write_str("ratio3:1"),
        }
    }
}

/// One slot's role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotPlan {
    pub slot: SlotId,
    pub label: Impl,
    pub runs: Impl,
    pub twin: bool,
}

/// Which slot runs which implementation, for one preset, caps set and context. JSON input is
/// accepted only if it is exactly what [`RolePlan::new`] gives for its preset, caps and context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PlanFields")]
pub struct RolePlan {
    pub v: V1,
    pub preset: Preset,
    pub context: Context,
    pub caps: Caps,
    /// The run kinds this plan may run as, in [`RunKind`] order.
    pub run_kinds: Vec<RunKind>,
    /// node0, then reg0.., then evo0..
    pub slots: Vec<SlotPlan>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanFields {
    #[serde(rename = "v")]
    _v: V1,
    preset: Preset,
    context: Context,
    caps: Caps,
    run_kinds: Vec<RunKind>,
    slots: Vec<SlotPlan>,
}

/// Why a plan cannot be made or read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    /// The preset belongs to the other context.
    Context {
        preset: Preset,
        context: Context,
    },
    /// A plan file's `run_kinds` or `slots` differ from the rules for its preset, caps and
    /// context.
    NotTheRules,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::Context {
                preset,
                context,
            } => write!(f, "preset {preset} is a {} preset, not {context}", preset.context()),
            PlanError::NotTheRules => f.write_str(
                "the plan's run_kinds or slots are not the rules' for its preset, caps and context",
            ),
        }
    }
}

impl std::error::Error for PlanError {}

impl RolePlan {
    pub fn new(preset: Preset, caps: Caps, context: Context) -> Result<Self, PlanError> {
        if preset.context() != context {
            return Err(PlanError::Context {
                preset,
                context,
            });
        }
        let dkg = caps.has(Cap::Dkg);
        let rust_mn = caps.has(Cap::Mn) && (dkg || context == Context::Regtest);
        let (regular, evo) = preset.counts();
        let slots: Vec<SlotPlan> = std::iter::once(SlotId::CONTROLLER)
            .chain((0..regular).map(SlotId::Regular))
            .chain((0..evo).map(SlotId::Evo))
            .map(|slot| {
                let rust = preset.labels_rust(slot);
                let runs_rust = rust && (rust_mn || !slot.is_masternode());
                SlotPlan {
                    slot,
                    label: Impl::rust_if(rust),
                    runs: Impl::rust_if(runs_rust),
                    twin: runs_rust && slot.is_masternode() && !dkg,
                }
            })
            .collect();
        let rehearsal_only = slots
            .iter()
            .any(|s| s.label != s.runs || (s.slot == SlotId::CONTROLLER && s.runs == Impl::Rust));
        let run_kinds = RunKind::ALL
            .into_iter()
            .filter(|k| k.context() == context && (k.is_rehearsal() || !rehearsal_only))
            .collect();
        Ok(Self {
            v: V1,
            preset,
            context,
            caps,
            run_kinds,
            slots,
        })
    }

    pub fn admits(&self, kind: RunKind) -> bool {
        self.run_kinds.contains(&kind)
    }

    pub fn slot(&self, slot: SlotId) -> Option<&SlotPlan> {
        self.slots.iter().find(|s| s.slot == slot)
    }
}

impl TryFrom<PlanFields> for RolePlan {
    type Error = PlanError;

    fn try_from(f: PlanFields) -> Result<Self, PlanError> {
        let plan = RolePlan::new(f.preset, f.caps, f.context)?;
        if plan.run_kinds == f.run_kinds && plan.slots == f.slots {
            Ok(plan)
        } else {
            Err(PlanError::NotTheRules)
        }
    }
}

/// One observation-log line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: V1,
    /// The run's id, the same on every line of a log.
    pub run: String,
    /// Strictly increasing within a run.
    pub seq: u64,
    /// The writer's monotonic clock, in nanoseconds.
    pub mono_ns: u64,
    /// Who wrote the line: the watcher, the driver, or a node's digest tail.
    pub src: String,
    #[serde(flatten)]
    pub record: Record,
}

/// A record; its variant name is the envelope's `kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Record {
    Run(Run),
    Binding(Binding),
    Block(Block),
    Reorg(Reorg),
    Watermark(Watermark),
    Tip(Tip),
    ChainTips(ChainTips),
    Commitment(Commitment),
    MnDiff(MnDiff),
    Handoff(Handoff),
    ClSeen(ClSeen),
    IsReq(IsReq),
    IsSeen(IsSeen),
    CjSession(CjSession),
    Superblock(Superblock),
    Chaos(Chaos),
    LlmqParams(LlmqParams),
    DigestCompare(DigestCompare),
}

/// The first record of every log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub run_kind: RunKind,
    pub context: Context,
    pub preset: Preset,
    pub caps: Caps,
    pub host: Host,
    pub clock: Clock,
    pub binaries: Binaries,
}

/// The allocation the run had: CPUs, and memory and disk in bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub nproc: u32,
    pub mem: u64,
    pub disk: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clock {
    pub source: String,
    pub offset_ms: i64,
}

/// The executables the run used. `rdashd` is null in a Core-only run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binaries {
    pub dashd: Binary,
    #[serde(deserialize_with = "present")]
    pub rdashd: Option<Binary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binary {
    pub sha256: Hex32,
    pub version: String,
}

/// From `from_height` and `from_seq` on, `slot` is the masternode `pro_tx`, run by the
/// executable `exe_sha256`. ProTx churn adds rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub slot: SlotId,
    pub pro_tx: Hex32,
    pub label: Impl,
    pub runs: Impl,
    pub exe_sha256: Hex32,
    pub from_height: u32,
    pub from_seq: u64,
}

/// A reference-chain block. `miner` is null for a block no slot of this run mined (a cached
/// chain's).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub height: u32,
    pub hash: Hex32,
    pub prev: Hex32,
    #[serde(deserialize_with = "present")]
    pub miner: Option<Miner>,
}

/// The slot that mined a block, and the implementation it ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Miner {
    pub slot: SlotId,
    pub runs: Impl,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reorg {
    pub fork_height: u32,
    pub old_tip: Hex32,
    pub new_tip: Hex32,
}

/// Every reference height up to `height` has been recorded: its block, the commitments it mined
/// and its PoSe changes. [`MnDiff`] rows exist only for changes, so a height at or below the
/// watermark with no row for a masternode had none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Watermark {
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tip {
    pub node: SlotId,
    pub height: u32,
    pub hash: Hex32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainTips {
    pub node: SlotId,
    pub tips: Vec<ChainTip>,
}

/// One `getchaintips` entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainTip {
    pub height: u32,
    pub hash: Hex32,
    pub branchlen: u32,
    pub status: TipStatus,
}

/// `getchaintips` statuses (Core v24.0.2 src/rpc/blockchain.cpp:1913-1935).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TipStatus {
    Active,
    Invalid,
    Conflicting,
    HeadersOnly,
    ValidFork,
    ValidHeaders,
    Unknown,
}

/// A mined final commitment, with `quorum info`'s members.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commitment {
    #[serde(rename = "type")]
    pub llmq_type: u8,
    pub quorum_hash: Hex32,
    pub index: u16,
    pub base_height: u32,
    pub mined_height: u32,
    pub null: bool,
    pub members: Vec<Member>,
    pub vvec_hash: Hex32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub pro_tx: Hex32,
    pub valid: bool,
}

/// The PoSe fields of a masternode that changed at `height`: an `updatedMNs` entry of
/// `protx listdiff h-1 h` (Core v24.0.2 src/rpc/evo.cpp:1920-2003), which lists only changed
/// fields (src/evo/dmnstate.cpp:70-78). A field is null if it did not change; heights are Core's
/// values, -1 for none. A row needs at least one field.
///
/// Core lowers a positive penalty by one every block (src/evo/deterministicmns.cpp:317-332), so a
/// value alone does not say whether a penalty rose. The collector therefore opens each
/// masternode's history with a baseline row holding all three fields: at the first `Watermark`'s
/// height, or at the masternode's registration height for one registered later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MnDiffFields")]
pub struct MnDiff {
    pub height: u32,
    pub pro_tx: Hex32,
    pub pose_penalty: Option<u32>,
    pub pose_ban_height: Option<i32>,
    pub pose_revived_height: Option<i32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MnDiffFields {
    height: u32,
    pro_tx: Hex32,
    #[serde(deserialize_with = "present")]
    pose_penalty: Option<u32>,
    #[serde(deserialize_with = "present")]
    pose_ban_height: Option<i32>,
    #[serde(deserialize_with = "present")]
    pose_revived_height: Option<i32>,
}

impl TryFrom<MnDiffFields> for MnDiff {
    type Error = &'static str;

    fn try_from(f: MnDiffFields) -> Result<Self, &'static str> {
        if f.pose_penalty.is_none()
            && f.pose_ban_height.is_none()
            && f.pose_revived_height.is_none()
        {
            return Err("an MnDiff row needs at least one PoSe field");
        }
        Ok(Self {
            height: f.height,
            pro_tx: f.pro_tx,
            pose_penalty: f.pose_penalty,
            pose_ban_height: f.pose_ban_height,
            pose_revived_height: f.pose_revived_height,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuorumRef {
    #[serde(rename = "type")]
    pub llmq_type: u8,
    pub quorum_hash: Hex32,
}

/// How a twin slot's quorum material reached rdashd.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HandoffMethod {
    Seam,
    Qdata,
    TwinRetained,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub slot: SlotId,
    pub quorum: QuorumRef,
    pub method: HandoffMethod,
    pub source_slots: Vec<SlotId>,
    pub ok: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClSeen {
    pub node: SlotId,
    pub height: u32,
    pub hash: Hex32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsReq {
    pub txid: Hex32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsSeen {
    pub node: SlotId,
    pub txid: Hex32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CjSession {
    pub wallet: String,
    pub coordinator_pro_tx: Hex32,
    pub outcome: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Superblock {
    pub height: u32,
    pub trigger_hash: Hex32,
    pub creator_pro_tx: Hex32,
    pub paid: bool,
}

/// A fault the driver injected, between two log positions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chaos {
    pub id: String,
    pub action: String,
    pub targets: Vec<SlotId>,
    pub start_seq: u64,
    pub end_seq: u64,
}

/// An LLMQ type the run enabled, with the parameters it ran with: Core v24.0.2
/// src/llmq/params.h after any `-llmqtest*` settings. The driver writes one per enabled type,
/// after the `Run` and before any `Watermark`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmqParams {
    #[serde(rename = "type")]
    pub llmq_type: u8,
    pub name: String,
    pub size: u16,
    /// The valid members a final commitment needs (src/llmq/commitment.cpp:153-160).
    pub min_size: u16,
    pub dkg_interval: u32,
    pub mining_window_start: u32,
    pub mining_window_end: u32,
    pub rotation: bool,
    pub signing_active_quorum_count: u16,
    pub evo_only: bool,
}

/// The result of comparing `node`'s state digests with node0's over
/// `[from_height, to_height]` (O8's `dash-conformance digest compare`). The runner writes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DigestCompare {
    pub node: SlotId,
    pub from_height: u32,
    pub to_height: u32,
    pub result: DigestResult,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum DigestResult {
    Equal,
    /// The first height whose digests differ.
    Mismatch {
        height: u32,
    },
    Incomplete {
        reason: String,
    },
    NotRun {
        reason: String,
    },
}

/// Why a log is not one well-formed run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogError {
    Empty,
    /// The record at this line index should be the log's only `Run`, or is a second one.
    Run(usize),
    /// The `Run`'s preset is not for its context.
    Plan(PlanError),
    /// The `Run`'s plan does not admit its run kind.
    RunKind(RunKind),
    /// The plan runs rdashd somewhere, but the `Run` names no rdashd binary.
    NoRdashd,
    /// The record at this line index has another run id.
    RunId(usize),
    /// The record at this line index does not have a larger `seq` than the one before it.
    Seq(usize),
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogError::Empty => f.write_str("the log has no records"),
            LogError::Run(0) => f.write_str("record 0 is not a Run"),
            LogError::Run(i) => write!(f, "record {i} is a second Run"),
            LogError::Plan(e) => write!(f, "the Run's {e}"),
            LogError::RunKind(kind) => write!(f, "the Run's plan does not admit run kind {kind:?}"),
            LogError::NoRdashd => {
                f.write_str("the Run's plan runs rdashd, but it names no rdashd binary")
            }
            LogError::RunId(i) => write!(f, "record {i} belongs to another run"),
            LogError::Seq(i) => write!(f, "record {i}'s seq does not increase"),
        }
    }
}

impl std::error::Error for LogError {}

/// Checks that a log is one run: a `Run` first and nowhere else, whose plan admits its run kind
/// and which names an rdashd binary if the plan runs one, one run id, and `seq` strictly
/// increasing.
pub fn check_log(log: &[Envelope]) -> Result<(), LogError> {
    let first = log.first().ok_or(LogError::Empty)?;
    let Record::Run(run) = &first.record else {
        return Err(LogError::Run(0));
    };
    let plan = RolePlan::new(run.preset, run.caps.clone(), run.context).map_err(LogError::Plan)?;
    if !plan.admits(run.run_kind) {
        return Err(LogError::RunKind(run.run_kind));
    }
    if run.binaries.rdashd.is_none() && plan.slots.iter().any(|s| s.runs == Impl::Rust) {
        return Err(LogError::NoRdashd);
    }
    for (i, e) in log.iter().enumerate().skip(1) {
        if matches!(e.record, Record::Run(_)) {
            return Err(LogError::Run(i));
        }
        if e.run != first.run {
            return Err(LogError::RunId(i));
        }
        if e.seq <= log[i - 1].seq {
            return Err(LogError::Seq(i));
        }
    }
    Ok(())
}

/// Deserializes an `Option` that must be present, as null or a value.
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}
