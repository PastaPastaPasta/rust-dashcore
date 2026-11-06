//! The Core side: the schedules (O2b §2.5), the per-schedule prefix image (D9) and the
//! process's one chain session.

use dash_oracle_sys::{
    ChainOp, ChainReport, ChainSession, ChainStart, CoreImage, Oracle, Pin, RawReport, SaveOutcome,
    SporkState,
};

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

/// An M3 schedule (O2b §2.5). S2 and S0p are O2b-4's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Schedule {
    S0,
    S1,
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

    /// Both sides' options (beyond the implied `-regtest`): the chain's, then the node's.
    pub fn args(self) -> Vec<&'static str> {
        let par = match self {
            Schedule::S0 => "-par=1",
            Schedule::S1 => "-par=4",
        };
        let mut args = self.chain_args();
        args.extend([
            "-fastprune",
            "-sporkaddr=yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW",
            "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK",
            "-dbcache=300",
            par,
        ]);
        args
    }

    pub fn name(self) -> &'static str {
        match self {
            Schedule::S0 => "S0",
            Schedule::S1 => "S1",
        }
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
    /// Start a session, build the image's chain into it, turn spork 19 on and save (O2b §2.3).
    /// The world it returns is the image's.
    pub fn start(pin: Pin, schedule: Schedule) -> Result<(CoreSut, World), String> {
        let world = image_world();
        let oracle = Oracle::new(pin);
        let args = schedule.args();
        let mut session = match oracle
            .chain_session(&args, T_RECENT, None)
            .map_err(|e| format!("chain_session: {e}"))?
        {
            ChainStart::Started(s) => s,
            ChainStart::Refused(m) => return Err(format!("genesis load refused: {m}")),
        };
        for b in &world.blocks[1..] {
            let r = session
                .step(&ChainOp::Block(b.built_bytes()))
                .map_err(|e| format!("image block: {e}"))?;
            if !matches!(r.reply.outcome, Ok(serde_json::Value::Null)) {
                return Err(format!("image block {} not connected", b.height));
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
