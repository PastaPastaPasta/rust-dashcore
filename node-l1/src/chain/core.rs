//! The Core side: the schedules (O2b §2.5), the per-schedule prefix image (D9) and the
//! process's one chain session.

use dash_oracle_sys::{
    ChainOp, ChainReport, ChainSession, ChainStart, CoreImage, Oracle, Pin, RawReport, SaveOutcome,
    SporkState,
};

use super::av;
use super::build::{
    self, BlockPlan, GENESIS_TIME, Input, Lock, REGTEST_BITS, SPACING, TxPlan, VERSION_BITS_TOP,
};
use super::step::Step;
use super::world::{Block, Coin, Held, Seq, World};

/// Spork 19 (ChainLocks) and its "off" value.
pub const SPORK_19: i32 = 10018;
pub const SPORK_OFF: i64 = 4_070_908_800;
/// The image's tip height (O2b §2.5: ~200; an input adds at most 64, all below the raised
/// DIP3 at 2000, O2b-3 P5).
pub const IMAGE_HEIGHT: u32 = 200;
/// The raised DIP3 height of every M3 schedule.
pub const DIP3_HEIGHT: u32 = 2000;
/// S3 / S3is `testdummy` start time (`Consensus::BIP9Deployment::NEVER_ACTIVE` in Core
/// `consensus/params.h`): keeps `testdummy` FAILED from genesis so `LLMQ_TEST_V17` never enables.
pub const TESTDUMMY_NEVER_ACTIVE: i64 = -2;
/// S3 / S3is `testdummy` timeout (`Consensus::BIP9Deployment::NO_TIMEOUT` in Core
/// `consensus/params.h`, `i64::MAX`).
pub const TESTDUMMY_NO_TIMEOUT: i64 = i64::MAX;
/// The sole chain-params override for S3 and S3is schedules (O2b-5s-a).
pub const S3_TESTDUMMY_VBPARAMS: &str = "-vbparams=testdummy:-2:9223372036854775807";
/// Error returned by any session-opening path invoked on `Schedule::S3` or `Schedule::S3is`
/// before recorded S3 images exist (5s-a).
pub const S3_UNAVAILABLE: &str = "S3 images are not available yet";
/// M3 schedules' fixed spork address option.
const DEFAULT_SPORK_ADDR_ARG: &str = "-sporkaddr=yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW";
/// Test spork private key option shared by M3 and S3 node options.
const DEFAULT_SPORK_KEY_ARG: &str =
    "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";
/// Outputs per image fan-out transaction (from height 101, one per block).
const IMAGE_FAN_OUT: u32 = 4;

/// The image tip's time; inputs run from here plus an offset.
pub const IMAGE_TIP_TIME: i64 = GENESIS_TIME as i64 + IMAGE_HEIGHT as i64 * SPACING as i64;
/// The input clock: the tip an hour old (out of IBD), or seven hours old (in IBD, 6 h age).
pub const T_RECENT: i64 = IMAGE_TIP_TIME + 3600;
pub const T_OLD: i64 = IMAGE_TIP_TIME + 7 * 3600;

/// Why an [`EraTable`] or a generated block height violates a schedule's era contract (O2b-5s-a).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EraError(pub String);

impl std::fmt::Display for EraError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EraError {}

/// Activation heights and `testdummy` parameters for an S3 / S3is recorded image (O2b-5s-a).
/// 5s-b fills this from the image manifest; 5s-a's tests construct it directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EraTable {
    dip3_enforcement: u32,
    v20: u32,
    mn_rr: u32,
    v24_activation: u32,
    testdummy_start: i64,
    testdummy_timeout: i64,
}

impl EraTable {
    /// Construct an [`EraTable`], returning `Err` unless `testdummy_start == -2`
    /// ([`TESTDUMMY_NEVER_ACTIVE`]) and `testdummy_timeout == i64::MAX` ([`TESTDUMMY_NO_TIMEOUT`]).
    pub fn new(
        dip3_enforcement: u32,
        v20: u32,
        mn_rr: u32,
        v24_activation: u32,
        testdummy_start: i64,
        testdummy_timeout: i64,
    ) -> Result<EraTable, EraError> {
        if testdummy_start != TESTDUMMY_NEVER_ACTIVE {
            return Err(EraError(format!(
                "testdummy start must be {TESTDUMMY_NEVER_ACTIVE} (NEVER_ACTIVE), got \
                 {testdummy_start}"
            )));
        }
        if testdummy_timeout != TESTDUMMY_NO_TIMEOUT {
            return Err(EraError(format!(
                "testdummy nTimeout must be {TESTDUMMY_NO_TIMEOUT} (NO_TIMEOUT), got \
                 {testdummy_timeout}"
            )));
        }
        Ok(EraTable {
            dip3_enforcement,
            v20,
            mn_rr,
            v24_activation,
            testdummy_start,
            testdummy_timeout,
        })
    }

    pub fn dip3_enforcement(&self) -> u32 {
        self.dip3_enforcement
    }

    pub fn v20(&self) -> u32 {
        self.v20
    }

    pub fn mn_rr(&self) -> u32 {
        self.mn_rr
    }

    pub fn v24_activation(&self) -> u32 {
        self.v24_activation
    }

    pub fn testdummy_start(&self) -> i64 {
        self.testdummy_start
    }

    pub fn testdummy_timeout(&self) -> i64 {
        self.testdummy_timeout
    }

    /// The first height inside the S3 / S3is evo era:
    /// `max(dip3_enforcement, v20, mn_rr, v24_activation)`.
    pub fn floor(&self) -> u32 {
        self.dip3_enforcement.max(self.v20).max(self.mn_rr).max(self.v24_activation)
    }
}

/// A schedule (O2b §2.5). S0–S2 are M3's, S0p is O2b-4's, and S3 / S3is are O2b-5's evo
/// schedules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Schedule {
    S0,
    S1,
    /// S1 with S2's line of headers above the image tip and `-assumevalid` on it ([`av`]).
    S2,
    /// Leg B (O2b-4 §2, D45 as OD-L3-112 amends it): S0 with a 4 MiB coins cache and a 5 MB
    /// mempool maximum (the pin's floor) on both sides, so that the coins budget `B(t)` (the
    /// share plus the empty pool's headroom) is small enough for size flushes; the node flushes
    /// in `Production` and Core has its own size flushes off (`-oracle-no-size-flush`), so that
    /// each of the node's flushes is mirrored to Core by label.
    S0p,
    /// O2b-5 evo schedule: stock regtest params with `testdummy` disabled and a recorded image's
    /// spork address.
    S3,
    /// O2b-5 evo + InstantSend schedule: same chain/session args as [`Schedule::S3`].
    S3is,
}

impl Schedule {
    /// Whether `self` is one of O2b-5's evo schedules (`S3` or `S3is`).
    pub fn is_s3(self) -> bool {
        matches!(self, Schedule::S3 | Schedule::S3is)
    }

    /// The chain-parameter options (`SetupChainParamsOptions`'s), which D8's parity check
    /// gives `oracle_chainparams_json`.
    pub fn chain_args(self) -> Vec<&'static str> {
        match self {
            Schedule::S0 | Schedule::S1 | Schedule::S2 | Schedule::S0p => vec![
                "-dip3params=2000:2000",
                "-testactivationheight=v20@2000",
                "-testactivationheight=mn_rr@2000",
                "-vbparams=testdummy:0:999999999999:0:12:9",
            ],
            Schedule::S3 | Schedule::S3is => vec![S3_TESTDUMMY_VBPARAMS],
        }
    }

    /// Its script-check threads, as the operators' expectations read them: [`Schedule::args`]
    /// passes this one's `-par`.
    pub fn par(self) -> super::mutate::Par {
        match self {
            Schedule::S0 | Schedule::S0p | Schedule::S3 | Schedule::S3is => super::mutate::Par::One,
            Schedule::S1 | Schedule::S2 => super::mutate::Par::Four,
        }
    }

    /// Both sides' options (beyond the implied `-regtest`): the chain's, then the node's.
    ///
    /// On `S3` and `S3is`, the spork address comes from the caller (5s-b reads it from the
    /// image manifest): use [`node_args_with_spork`](Self::node_args_with_spork) and
    /// [`core_args_with_spork`](Self::core_args_with_spork).
    pub fn args(self) -> Vec<&'static str> {
        let par = self.par().arg();
        let dbcache = match self {
            Schedule::S0p => "-dbcache=4",
            Schedule::S0 | Schedule::S1 | Schedule::S2 | Schedule::S3 | Schedule::S3is => {
                "-dbcache=300"
            }
        };
        let mut args = self.chain_args();
        args.push("-fastprune");
        if !self.is_s3() {
            args.push(DEFAULT_SPORK_ADDR_ARG);
        }
        args.extend([DEFAULT_SPORK_KEY_ARG, dbcache, par]);
        if self == Schedule::S0p {
            args.push("-maxmempool=5");
        }
        if self == Schedule::S2 {
            args.push(av::av_arg());
        }
        args
    }

    /// Rust `NodeSut` session options (beyond the implied `-regtest`) with caller-supplied
    /// `spork_addr`. On `S3` and `S3is`, builds [`chain_args`](Self::chain_args) followed by S0's
    /// node options (`-fastprune`, `-sporkaddr=<spork_addr>`, `-sporkkey`, `-dbcache=300`,
    /// `-par=1`). On `S0`–`S2` and `S0p`, returns [`args`](Self::args) unchanged.
    pub fn node_args_with_spork(self, spork_addr: &str) -> Vec<String> {
        if !self.is_s3() {
            return self.args().into_iter().map(str::to_owned).collect();
        }
        self.s3_args(spork_addr)
    }

    /// Core shim session options (beyond the implied `-regtest`) with caller-supplied
    /// `spork_addr`. On `S3` and `S3is`, identical to
    /// [`node_args_with_spork`](Self::node_args_with_spork) (D8 params parity). On `S0`–`S2` and
    /// `S0p`, returns [`core_args`](Self::core_args) unchanged.
    pub fn core_args_with_spork(self, spork_addr: &str) -> Vec<String> {
        if !self.is_s3() {
            return self.core_args().into_iter().map(str::to_owned).collect();
        }
        self.s3_args(spork_addr)
    }

    fn s3_args(self, spork_addr: &str) -> Vec<String> {
        let mut args: Vec<String> = self.chain_args().into_iter().map(str::to_owned).collect();
        args.push("-fastprune".to_owned());
        args.push(format!("-sporkaddr={spork_addr}"));
        args.push(DEFAULT_SPORK_KEY_ARG.to_owned());
        args.push("-dbcache=300".to_owned());
        args.push(self.par().arg().to_owned());
        args
    }

    pub fn name(self) -> &'static str {
        match self {
            Schedule::S0 => "S0",
            Schedule::S1 => "S1",
            Schedule::S2 => "S2",
            Schedule::S0p => "S0p",
            Schedule::S3 => "S3",
            Schedule::S3is => "S3is",
        }
    }

    /// Core's options: [`args`](Self::args), and on S0p `-oracle-no-size-flush`, which only the
    /// shim takes (`oracle_chain.cpp:106` at the pin). Its restore and `RESTART` reuse the
    /// session's options, so [`CoreSut::start`] is the one place it is passed.
    pub fn core_args(self) -> Vec<&'static str> {
        let mut args = self.args();
        if self == Schedule::S0p {
            args.push("-oracle-no-size-flush");
        }
        args
    }

    /// The node's flush regime: `Production` on leg B's S0p, else `CoreCausesOnly` (leg A).
    #[cfg(feature = "sut")]
    pub fn regime(self) -> dashd_chainstate::flush::FlushRegime {
        use dashd_chainstate::flush::FlushRegime;
        if self.leg_b() {
            FlushRegime::Production
        } else {
            FlushRegime::CoreCausesOnly
        }
    }

    /// Whether the schedule runs leg B: its node flushes on its own causes, mirrored to Core.
    pub fn leg_b(self) -> bool {
        self == Schedule::S0p
    }

    /// The schedule's image world: [`image_world`], and on S2 its line ([`av::world`]), with
    /// the schedule's fan-out ([`generate::fan_out`](super::generate::fan_out)).
    pub fn world(self) -> World {
        let mut w = match self {
            Schedule::S0 | Schedule::S0p | Schedule::S1 | Schedule::S3 | Schedule::S3is => {
                image_world()
            }
            Schedule::S2 => av::world(),
        };
        w.fan_out = super::generate::fan_out(self);
        w
    }
}

/// Option name without leading dashes or `=<value>`.
fn opt_name(arg: &str) -> &str {
    let stripped = arg.trim_start_matches('-');
    stripped.split('=').next().unwrap_or(stripped)
}

/// Whether `arg` carries an option excluded on S3 / S3is sessions: `-dip3params`,
/// `-testactivationheight`, `-prune` (`-fastprune` is allowed), any `-llmq*params`
/// (`-llmqtestparams` and the rest), or any `-vbparams` other than [`S3_TESTDUMMY_VBPARAMS`].
fn is_excluded_s3_arg(arg: &str) -> bool {
    let opt = opt_name(arg);
    let base = opt.strip_prefix("no").unwrap_or(opt);
    if matches!(base, "dip3params" | "testactivationheight" | "prune") {
        return true;
    }
    if base.starts_with("llmq") && base.ends_with("params") {
        return true;
    }
    if opt == "novbparams" || (opt == "vbparams" && arg != S3_TESTDUMMY_VBPARAMS) {
        return true;
    }
    false
}

/// Extract the S3 / S3is validation and params subset (`-regtest`, `-vbparams`, `-sporkaddr`),
/// normalised by sorting so order differences between the session's args and the manifest's
/// recorded list are ignored.
fn s3_compared_subset<S: AsRef<str>>(args: &[S]) -> Vec<String> {
    let mut subset: Vec<String> = args
        .iter()
        .map(AsRef::as_ref)
        .filter(|a| matches!(opt_name(a), "regtest" | "vbparams" | "sporkaddr"))
        .map(str::to_owned)
        .collect();
    subset.sort();
    subset
}

/// Verify that an S3 / S3is session's `session_args` match the manifest's `recorded` list
/// (O2b-5s-a; 5s-b calls this at every open and reopen on both sides).
///
/// Normalisation: extracts the validation and params subset (`-regtest`, `-vbparams`,
/// `-sporkaddr`) from both lists and sorts both subsets before comparing. Fails if:
/// - either full list holds an excluded option (`-dip3params`, `-testactivationheight`, `-prune`,
///   any `-llmq*params`, or any `-vbparams` other than [`S3_TESTDUMMY_VBPARAMS`]);
/// - the two sorted subsets differ;
/// - the subset lacks [`S3_TESTDUMMY_VBPARAMS`] or a non-empty `-sporkaddr=<addr>`.
pub fn assert_s3_session_args<A: AsRef<str>, B: AsRef<str>>(
    session_args: &[A],
    recorded: &[B],
) -> Result<(), String> {
    for arg in session_args {
        let arg = arg.as_ref();
        if is_excluded_s3_arg(arg) {
            return Err(format!("session_args contains excluded S3 option: {arg}"));
        }
    }
    for arg in recorded {
        let arg = arg.as_ref();
        if is_excluded_s3_arg(arg) {
            return Err(format!("recorded session_args contains excluded S3 option: {arg}"));
        }
    }
    let actual = s3_compared_subset(session_args);
    let want = s3_compared_subset(recorded);
    if actual != want {
        return Err(format!("S3 session args mismatch: got {actual:?}, recorded {want:?}"));
    }
    if !actual.iter().any(|a| a == S3_TESTDUMMY_VBPARAMS) {
        return Err(format!("S3 session args missing {S3_TESTDUMMY_VBPARAMS}: {actual:?}"));
    }
    if !actual.iter().any(|a| a.strip_prefix("-sporkaddr=").is_some_and(|v| !v.is_empty())) {
        return Err(format!("S3 session args missing -sporkaddr=<addr>: {actual:?}"));
    }
    Ok(())
}

/// The op that puts an image block into a side as its world holds it: its header alone for a
/// header-only block (S2's line), else the block.
pub fn image_op(b: &Block) -> ChainOp {
    match b.held {
        Held::Header => ChainOp::Header(b.built.as_ref().expect("a built block").header),
        _ => ChainOp::Block(b.built_bytes()),
    }
}

/// The image's blocks: genesis, then [`IMAGE_HEIGHT`] blocks of OP_TRUE coinbases, each from
/// height 101 on with a fan-out of the coinbase 100 below it, so that the tip has coins to
/// spend (O2b-3 §2.3).
pub fn image_world() -> World {
    let genesis = Block {
        hash: build::genesis_hash(),
        parent: None,
        height: 0,
        time: GENESIS_TIME,
        built: None,
        creates: Vec::new(),
        spends: Vec::new(),
        held: Held::Data,
        invalid: false,
        mutation: None,
        seq: Seq::Loaded,
        durable: Some((Held::Data, false)),
    };
    let mut blocks = vec![genesis];
    for height in 1..=IMAGE_HEIGHT {
        let parent = height as usize - 1;
        let mut txs = Vec::new();
        let mut spends = Vec::new();
        let mut creates = Vec::new();
        let mut fee = 0;
        if height > build::MATURITY {
            let coin = blocks[(height - build::MATURITY) as usize].creates[0];
            let each = (coin.value - 1_000) / i64::from(IMAGE_FAN_OUT);
            fee = coin.value - each * i64::from(IMAGE_FAN_OUT);
            spends.push(coin.outpoint);
            txs.push(TxPlan::simple(
                vec![Input::new(coin.outpoint, Vec::new())],
                (0..IMAGE_FAN_OUT).map(|_| (each, Lock::True.script_pubkey())).collect(),
            ));
        }
        txs.insert(0, build::coinbase(height, build::subsidy(height) + fee, u64::from(height)));
        let plan = BlockPlan {
            prev: blocks[parent].hash,
            version: VERSION_BITS_TOP,
            time: GENESIS_TIME + height * SPACING,
            bits: REGTEST_BITS,
            txs,
            merkle_override: None,
        };
        let built = plan.build();
        for (t, tx) in plan.txs.iter().enumerate() {
            for (o, (value, _)) in tx.outputs.iter().enumerate() {
                creates.push(Coin {
                    outpoint: (built.txids[t], o as u32),
                    value: *value,
                    lock: Lock::True,
                    height,
                    coinbase: t == 0,
                });
            }
        }
        blocks.push(Block {
            hash: built.hash,
            parent: Some(parent),
            height,
            time: plan.time,
            built: Some(built),
            creates,
            spends,
            held: Held::Data,
            invalid: false,
            mutation: None,
            seq: Seq::Loaded,
            durable: Some((Held::Data, false)),
        });
    }
    World::from_chain(blocks, T_RECENT)
}

/// The process's chain session and its schedule's image.
pub struct CoreSut {
    pub session: ChainSession,
    pub image: CoreImage,
}

/// What a Core step gave: the neutral step, the version-normalized raw report (the replay
/// check compares those bytes), and whether the session is tainted after it.
pub struct CoreStep {
    pub step: Step,
    pub raw: RawReport,
    /// The session's latch ([`ChainSession::tainted_since`]): a tie decided this step's state or
    /// an earlier one's, although only the step that fired it lists the sites (O2b-2 F1). Nothing
    /// from here is scored until a clean restore clears it.
    pub tainted: bool,
}

impl CoreSut {
    /// Start a session, build the image's chain into it ([`image_op`]), turn spork 19 on and
    /// save (O2b §2.3). The world it returns is the image's.
    pub fn start(pin: Pin, schedule: Schedule) -> Result<(CoreSut, World), String> {
        if schedule.is_s3() {
            return Err(S3_UNAVAILABLE.into());
        }
        CoreSut::start_with_args(pin, schedule, &schedule.core_args())
    }

    /// [`start`](Self::start) with Core's options `args` in place of the schedule's
    /// [`core_args`](Schedule::core_args): leg B's witness of Core's own size flushes starts it
    /// without `-oracle-no-size-flush`.
    #[doc(hidden)]
    pub fn start_with_args(
        pin: Pin,
        schedule: Schedule,
        args: &[&str],
    ) -> Result<(CoreSut, World), String> {
        if schedule.is_s3() {
            return Err(S3_UNAVAILABLE.into());
        }
        let world = schedule.world();
        let oracle = Oracle::new(pin);
        let mut session = match oracle
            .chain_session(args, T_RECENT, None)
            .map_err(|e| format!("chain_session: {e}"))?
        {
            ChainStart::Started(s) => s,
            ChainStart::Refused(m) => return Err(format!("genesis load refused: {m}")),
        };
        for b in &world.blocks[1..] {
            let r = session.step(&image_op(b)).map_err(|e| format!("image block: {e}"))?;
            if !matches!(r.reply.outcome, Ok(serde_json::Value::Null)) {
                return Err(format!("image block {} not accepted: {:?}", b.height, r.reply));
            }
        }
        let spork = session
            .step(&ChainOp::Spork {
                id: SPORK_19,
                value: 0,
            })
            .map_err(|e| format!("spork: {e}"))?;
        if !spork.post.sporks.is_some_and(|s| s.contains(&SporkState(SPORK_19, 0, true))) {
            return Err("spork 19 is not active in the image".into());
        }
        let image = match session.save().map_err(|e| format!("save: {e}"))? {
            SaveOutcome::Saved(image) => image,
            SaveOutcome::ReloadRefused(_, m) => return Err(format!("image reload refused: {m}")),
            SaveOutcome::Down => return Err("image save: the session was down".into()),
        };
        // A one-chain image has no tie (D9): a tainted one would taint every input silently.
        if image.tainted() {
            return Err("the image's build tainted".into());
        }
        Ok((
            CoreSut {
                session,
                image,
            },
            world,
        ))
    }

    /// One op.
    pub fn step(&mut self, op: &ChainOp) -> Result<CoreStep, String> {
        let r = self.session.step(op).map_err(|e| format!("step: {e}"))?;
        Ok(self.done(r))
    }

    fn done(&self, r: ChainReport) -> CoreStep {
        CoreStep {
            raw: r.raw.normalized(),
            step: r.into(),
            tainted: self.session.tainted_since().is_some(),
        }
    }

    /// The input's start: the clock, then the image restored (step 0, O2b-1 I24).
    pub fn restore(&mut self, mocktime: i64) -> Result<CoreStep, String> {
        let clock =
            self.session.step(&ChainOp::SetTime(mocktime)).map_err(|e| format!("step: {e}"))?;
        if clock.reply.outcome.is_err() {
            return Err("restore: the clock was refused".into());
        }
        let r = self.session.restore(&self.image).map_err(|e| format!("restore: {e}"))?;
        Ok(self.done(r))
    }
}

#[cfg(test)]
mod tests {
    use super::Schedule;

    /// S0p is S0 but for its 4 MiB coins cache and 5 MB mempool maximum on both sides (D8's
    /// check reads the chain's options alone, which are S0's), and Core alone also takes
    /// `-oracle-no-size-flush`.
    #[test]
    fn s0p_is_s0_but_its_cache_its_mempool_and_core_s_size_flushes() {
        assert_eq!(Schedule::S0p.chain_args(), Schedule::S0.chain_args());
        let swap = |a: &'static str| {
            if a == "-dbcache=300" {
                "-dbcache=4"
            } else {
                a
            }
        };
        let mut want: Vec<&str> = Schedule::S0.args().into_iter().map(swap).collect();
        want.push("-maxmempool=5");
        assert_eq!(Schedule::S0p.args(), want);
        assert!(!Schedule::S0.args().iter().any(|a| a.starts_with("-maxmempool")));
        assert_ne!(Schedule::S0p.args(), Schedule::S0.args(), "the cache differs");
        let mut core = Schedule::S0p.args();
        core.push("-oracle-no-size-flush");
        assert_eq!(Schedule::S0p.core_args(), core);
        for leg_a in [Schedule::S0, Schedule::S1, Schedule::S2] {
            assert_eq!(leg_a.core_args(), leg_a.args(), "{}", leg_a.name());
            assert!(!leg_a.leg_b());
        }
        assert!(Schedule::S0p.leg_b());
        assert_eq!(Schedule::S0p.name(), "S0p");
    }

    /// The node flushes in `Production` on S0p alone.
    #[cfg(feature = "sut")]
    #[test]
    fn only_s0p_runs_the_production_regime() {
        use dashd_chainstate::flush::FlushRegime;
        assert_eq!(Schedule::S0p.regime(), FlushRegime::Production);
        for leg_a in [Schedule::S0, Schedule::S1, Schedule::S2] {
            assert_eq!(leg_a.regime(), FlushRegime::CoreCausesOnly, "{}", leg_a.name());
        }
    }

    /// C-4e1-r1-7: each schedule passes `-par` once, as its [`Schedule::par`]'s argument, so the
    /// script-check threads the operators' expectations read are the ones both sides run: one on
    /// S0 and S0p, four on S1 and S2.
    #[test]
    fn each_schedule_s_par_argument_is_its_par() {
        use super::super::mutate::Par;
        for (schedule, par) in [
            (Schedule::S0, Par::One),
            (Schedule::S0p, Par::One),
            (Schedule::S1, Par::Four),
            (Schedule::S2, Par::Four),
            (Schedule::S3, Par::One),
            (Schedule::S3is, Par::One),
        ] {
            assert_eq!(schedule.par(), par, "{}", schedule.name());
            let args: Vec<&str> =
                schedule.args().into_iter().filter(|a| a.starts_with("-par=")).collect();
            assert_eq!(args, [par.arg()], "{}", schedule.name());
        }
    }

    /// O2b-5s-a test 1: S3 and S3is args equal the expected list for a caller-supplied spork
    /// address on both the Core side and the Rust side, and session-opening paths refuse S3 /
    /// S3is with an error ("S3 images are not available yet"), never panicking.
    #[test]
    fn s3_and_s3is_args_equal_expected_on_core_and_rust_sides() {
        use super::{
            CoreSut, DEFAULT_SPORK_KEY_ARG, Pin, S3_TESTDUMMY_VBPARAMS, S3_UNAVAILABLE,
            assert_s3_session_args,
        };
        use crate::chain::run::{Runner, ShimFault};

        assert_eq!(Schedule::S3.name(), "S3");
        assert_eq!(Schedule::S3is.name(), "S3is");
        for schedule in [Schedule::S3, Schedule::S3is] {
            assert!(schedule.is_s3());
            assert!(!schedule.leg_b());
            assert_eq!(schedule.chain_args(), vec![S3_TESTDUMMY_VBPARAMS]);
            for addr in ["yS3TestSporkAddress111111111111111", "yS3OtherSporkAddress22222222222222"]
            {
                let spork_arg = format!("-sporkaddr={addr}");
                let want = vec![
                    S3_TESTDUMMY_VBPARAMS.to_owned(),
                    "-fastprune".to_owned(),
                    spork_arg.clone(),
                    DEFAULT_SPORK_KEY_ARG.to_owned(),
                    "-dbcache=300".to_owned(),
                    "-par=1".to_owned(),
                ];
                let core = schedule.core_args_with_spork(addr);
                let node = schedule.node_args_with_spork(addr);
                assert_eq!(core, want, "{} core args ({addr})", schedule.name());
                assert_eq!(node, want, "{} node args ({addr})", schedule.name());
                assert_eq!(core, node, "{} core vs node parity", schedule.name());

                let recorded = ["-regtest", S3_TESTDUMMY_VBPARAMS, &spork_arg];
                let mut core_with_net = vec!["-regtest".to_owned()];
                core_with_net.extend(core);
                let mut node_with_net = vec!["-regtest".to_owned()];
                node_with_net.extend(node);
                assert_s3_session_args(&core_with_net, &recorded)
                    .unwrap_or_else(|e| panic!("core assert failed: {e}"));
                assert_s3_session_args(&node_with_net, &recorded)
                    .unwrap_or_else(|e| panic!("node assert failed: {e}"));
            }

            #[cfg(feature = "sut")]
            {
                let setup = crate::chain::params::node_params(&schedule.chain_args())
                    .expect("S3 chain_args parse cleanly on Rust side");
                let td = setup
                    .consensus
                    .deployments
                    .iter()
                    .find(|(pos, _)| pos.name() == "testdummy")
                    .expect("testdummy deployment")
                    .1;
                assert_eq!(td.start_time, super::TESTDUMMY_NEVER_ACTIVE);
                assert_eq!(td.timeout, super::TESTDUMMY_NO_TIMEOUT);

                assert_eq!(
                    crate::chain::node::NodeSide::start(
                        Pin::V24_0,
                        schedule,
                        std::path::Path::new("/nonexistent")
                    )
                    .err()
                    .as_deref(),
                    Some(S3_UNAVAILABLE)
                );
                assert_eq!(
                    crate::chain::diff::Diff::start(
                        Pin::V24_0,
                        schedule,
                        crate::chain::run::ChainPlant::None
                    )
                    .err()
                    .as_deref(),
                    Some(S3_UNAVAILABLE)
                );
                assert_eq!(
                    Runner::with_mode(Pin::V24_0, schedule, crate::chain::run::Mode::Diff).err(),
                    Some(ShimFault(S3_UNAVAILABLE.into()))
                );
            }

            assert_eq!(CoreSut::start(Pin::V24_0, schedule).err().as_deref(), Some(S3_UNAVAILABLE));
            assert_eq!(
                CoreSut::start_with_args(Pin::V24_0, schedule, &[]).err().as_deref(),
                Some(S3_UNAVAILABLE)
            );
            assert_eq!(
                Runner::new(Pin::V24_0, schedule).err(),
                Some(ShimFault(S3_UNAVAILABLE.into()))
            );
        }
    }

    /// O2b-5s-a test 2: `assert_s3_session_args` compares the sorted validation/params subset
    /// (`-regtest`, `-vbparams`, `-sporkaddr`) and refuses any excluded option on either side.
    #[test]
    fn s3_session_args_assert_checks_subset_and_exclusions() {
        use super::{S3_TESTDUMMY_VBPARAMS, assert_s3_session_args};

        let recorded = ["-regtest", S3_TESTDUMMY_VBPARAMS, "-sporkaddr=yS3TestAddr"];
        // Equal pair passes.
        assert_s3_session_args(&recorded, &recorded).expect("equal pair");
        // Reordered pair passes (normalisation sorts the validation/params subset).
        let reordered = ["-sporkaddr=yS3TestAddr", "-regtest", S3_TESTDUMMY_VBPARAMS];
        assert_s3_session_args(&reordered, &recorded).expect("reordered pair");
        // Node-only options (-fastprune, -dbcache, -par, -sporkkey) outside the subset pass.
        let mut with_node_opts = vec!["-regtest".to_owned()];
        with_node_opts.extend(Schedule::S3.node_args_with_spork("yS3TestAddr"));
        assert_s3_session_args(&with_node_opts, &recorded).expect("with S0 node options");

        // Extra -prune=1 fails (both added and equal-length).
        let extra_prune = ["-regtest", S3_TESTDUMMY_VBPARAMS, "-sporkaddr=yS3TestAddr", "-prune=1"];
        assert!(assert_s3_session_args(&extra_prune, &recorded).is_err(), "extra -prune=1");
        assert!(assert_s3_session_args(&recorded, &extra_prune).is_err(), "recorded -prune=1");
        assert!(assert_s3_session_args(&extra_prune, &extra_prune).is_err(), "both -prune=1");

        // Missing -vbparams fails (one side or both).
        let missing_vb = ["-regtest", "-sporkaddr=yS3TestAddr"];
        assert!(assert_s3_session_args(&missing_vb, &recorded).is_err(), "missing -vbparams");
        assert!(
            assert_s3_session_args(&missing_vb, &missing_vb).is_err(),
            "both missing -vbparams"
        );

        // -llmq*params items fail (added or equal-length).
        for llmq in [
            "-llmqtestparams=3:2",
            "-llmqtestinstantsendparams=3:2",
            "-llmqtestplatformparams=3:2",
            "-llmqdevnetparams=3:2",
        ] {
            let with_llmq = ["-regtest", S3_TESTDUMMY_VBPARAMS, "-sporkaddr=yS3TestAddr", llmq];
            assert!(assert_s3_session_args(&with_llmq, &recorded).is_err(), "{llmq}");
            assert!(assert_s3_session_args(&with_llmq, &with_llmq).is_err(), "both {llmq}");
        }

        // Other excluded prefixes fail: -dip3params, -testactivationheight, another -vbparams.
        for bad in [
            "-dip3params=2000:2000",
            "-testactivationheight=v20@2000",
            "-vbparams=v24:0:1",
            "-vbparams=testdummy:0:999999999999:0:12:9",
        ] {
            let with_bad = ["-regtest", S3_TESTDUMMY_VBPARAMS, "-sporkaddr=yS3TestAddr", bad];
            assert!(assert_s3_session_args(&with_bad, &recorded).is_err(), "{bad}");
            assert!(assert_s3_session_args(&with_bad, &with_bad).is_err(), "both {bad}");
        }

        // Equal-length mismatching sporkaddr fails (kills length-only mutant).
        let wrong_spork = ["-regtest", S3_TESTDUMMY_VBPARAMS, "-sporkaddr=yOtherAddr"];
        assert!(assert_s3_session_args(&wrong_spork, &recorded).is_err(), "wrong sporkaddr");

        // Missing -regtest on one side fails.
        let no_regtest = [S3_TESTDUMMY_VBPARAMS, "-sporkaddr=yS3TestAddr"];
        assert!(assert_s3_session_args(&no_regtest, &recorded).is_err(), "missing -regtest");

        // Empty sporkaddr fails.
        let empty_spork = ["-regtest", S3_TESTDUMMY_VBPARAMS, "-sporkaddr="];
        assert!(assert_s3_session_args(&empty_spork, &empty_spork).is_err(), "empty sporkaddr");
    }
}
