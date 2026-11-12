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
/// Outputs per image fan-out transaction (from height 101, one per block).
const IMAGE_FAN_OUT: u32 = 4;

/// The image tip's time; inputs run from here plus an offset.
pub const IMAGE_TIP_TIME: i64 = GENESIS_TIME as i64 + IMAGE_HEIGHT as i64 * SPACING as i64;
/// The input clock: the tip an hour old (out of IBD), or seven hours old (in IBD, 6 h age).
pub const T_RECENT: i64 = IMAGE_TIP_TIME + 3600;
pub const T_OLD: i64 = IMAGE_TIP_TIME + 7 * 3600;

/// An M3 schedule (O2b §2.5). S0p is O2b-4's.
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
}

impl Schedule {
    /// The chain-parameter options (`SetupChainParamsOptions`'s), which D8's parity check
    /// gives `oracle_chainparams_json`.
    pub fn chain_args(self) -> Vec<&'static str> {
        vec![
            "-dip3params=2000:2000",
            "-testactivationheight=v20@2000",
            "-testactivationheight=mn_rr@2000",
            "-vbparams=testdummy:0:999999999999:0:12:9",
        ]
    }

    /// Its script-check threads, as the operators' expectations read them (`-par` in
    /// [`Schedule::args`]).
    pub fn par(self) -> super::mutate::Par {
        match self {
            Schedule::S0 | Schedule::S0p => super::mutate::Par::One,
            Schedule::S1 | Schedule::S2 => super::mutate::Par::Four,
        }
    }

    /// Both sides' options (beyond the implied `-regtest`): the chain's, then the node's.
    pub fn args(self) -> Vec<&'static str> {
        let par = match self {
            Schedule::S0 | Schedule::S0p => "-par=1",
            Schedule::S1 | Schedule::S2 => "-par=4",
        };
        let dbcache = match self {
            Schedule::S0p => "-dbcache=4",
            Schedule::S0 | Schedule::S1 | Schedule::S2 => "-dbcache=300",
        };
        let mut args = self.chain_args();
        args.extend([
            "-fastprune",
            "-sporkaddr=yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW",
            "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK",
            dbcache,
            par,
        ]);
        if self == Schedule::S0p {
            args.push("-maxmempool=5");
        }
        if self == Schedule::S2 {
            args.push(av::av_arg());
        }
        args
    }

    pub fn name(self) -> &'static str {
        match self {
            Schedule::S0 => "S0",
            Schedule::S1 => "S1",
            Schedule::S2 => "S2",
            Schedule::S0p => "S0p",
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
            Schedule::S0 | Schedule::S0p | Schedule::S1 => image_world(),
            Schedule::S2 => av::world(),
        };
        w.fan_out = super::generate::fan_out(self);
        w
    }
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
}
