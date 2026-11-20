use std::collections::HashSet;
use std::sync::Arc;

use dash_clock::MockClock;
use dash_core_vectors::hex::try_parse_hex;
use dash_evo::bls::BlsCtx;
use dash_evo::locks::{ChainLockSig, ClsigHashSchemes, clsig_hash};
use dash_oracle_sys::{BlockAt, ChainHash, ChainOp, ChainStart, Oracle, Pin};
use dash_p2p_core::ClSig;
use dashd_locks::chainlock::{
    ChainLockHandler, ClChain, ClSigsSnapshot, ClVerdict, ClVerify, Read, Stopped,
};
use dashd_p2p::processing::seams::ChainLocks;
use parking_lot::Mutex;

#[derive(Default)]
struct Model {
    best: ClSigsSnapshot,
    enabled: bool,
    observed: bool,
    recorded: bool,
}

impl Model {
    fn check_active(&mut self) {
        if self.enabled && !self.observed {
            self.best = ClSigsSnapshot::default();
        }
        self.observed = self.enabled;
    }
}

#[derive(Clone)]
struct Chain(Arc<Mutex<Model>>, [u8; 32]);

impl ClChain for Chain {
    fn intake(&self) -> Result<i32, Stopped> {
        let mut m = self.0.lock();
        m.check_active();
        Ok(m.best.best.as_ref().map_or(-1, |s| s.height))
    }

    fn record(&self, sig: Arc<ChainLockSig>, hash: &[u8; 32], _: i64) -> Result<bool, Stopped> {
        let mut m = self.0.lock();
        m.recorded = true;
        if sig.height <= m.best.best.as_ref().map_or(-1, |s| s.height) {
            return Ok(false);
        }
        m.best = ClSigsSnapshot {
            best: Some(sig.clone()),
            best_hash: *hash,
        };
        Ok(sig.block_hash.as_bytes() != &self.1 || sig.height == 0)
    }

    fn abort_node(&self, msg: String) {
        panic!("{msg}");
    }

    fn reads(&self) -> ClSigsSnapshot {
        self.0.lock().best.clone()
    }
}

struct Verify;
impl ClVerify for Verify {
    fn verify(&self, _: &Read<ClSig>) -> ClVerdict {
        ClVerdict::Valid
    }
}

#[test]
fn generated_records_and_enforcement_match_o2b_outcomes_and_relay() {
    let mut genesis: [u8; 32] =
        try_parse_hex("000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e")
            .unwrap()
            .try_into()
            .unwrap();
    genesis.reverse();
    let oracle = Oracle::new(Pin::V24_0);
    let ChainStart::Started(mut session) = oracle
        .chain_session(
            &[
                "-par=1",
                "-sporkaddr=yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW",
                "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK",
            ],
            1_700_000_000,
            None,
        )
        .unwrap()
    else {
        panic!("oracle starts")
    };
    let state = Arc::new(Mutex::new(Model::default()));
    let enabled = state.clone();
    let clock = Arc::new(MockClock::new());
    clock.set_mocktime(1_700_000_000);
    let mut handler =
        ChainLockHandler::new(Chain(state.clone(), genesis), Verify, clock.clone(), move || {
            enabled.lock().enabled
        });
    let mut program = vec![
        ChainOp::Spork {
            id: 10018,
            value: 0,
        },
        ChainOp::ClRecord {
            height: 0,
            hash: genesis,
            sig: [0; 96],
        },
        ChainOp::ClEnforce,
        ChainOp::ClRecord {
            height: 1,
            hash: genesis,
            sig: [0; 96],
        },
        ChainOp::ClRecord {
            height: 2,
            hash: [9; 32],
            sig: [0; 96],
        },
    ];
    let mut seed = 0x1a55_1234;
    for i in 0..160 {
        seed = seed * 1_664_525_u64 + 1_013_904_223;
        seed &= 0xffff_ffff;
        match i % 16 {
            0 => program.push(ChainOp::Spork {
                id: 10018,
                value: 4_070_908_800,
            }),
            2 => program.push(ChainOp::Spork {
                id: 10018,
                value: 0,
            }),
            3 => program.push(ChainOp::ClEnforce),
            _ => program.push(ChainOp::ClRecord {
                height: ((seed >> 8) % 12) as i32 - 1,
                hash: if seed.is_multiple_of(3) {
                    genesis
                } else {
                    [(seed % 16) as u8; 32]
                },
                sig: [0; 96],
            }),
        }
    }
    let mut outcomes = HashSet::new();
    let mut seen = HashSet::new();
    for (i, op) in program.iter().enumerate() {
        let now = 1_700_000_001 + i as i64;
        clock.set_mocktime(now);
        session.step(&ChainOp::SetTime(now)).unwrap();
        let report = session.step(op).unwrap();
        match op {
            ChainOp::Spork {
                value,
                ..
            } => {
                assert_eq!(report.reply.outcome.as_ref().unwrap().as_bool(), Some(true));
                state.lock().enabled = *value == 0;
                continue;
            }
            ChainOp::ClEnforce => state.lock().check_active(),
            ChainOp::ClRecord {
                height,
                hash,
                sig,
            } => {
                state.lock().recorded = false;
                let sig = ClSig::new(*height, (*hash).into(), (*sig).into());
                let sig_hash = clsig_hash(&sig, ClsigHashSchemes::both(BlsCtx::LEGACY)).unwrap();
                let duplicate = !seen.insert(sig_hash);
                let result = handler.process_new_chainlock(
                    -1,
                    Read {
                        msg: sig,
                        read: BlsCtx::LEGACY,
                    },
                    &sig_hash,
                );
                let outcome = report.reply.outcome.as_ref().unwrap().as_str().unwrap();
                let expected = if duplicate {
                    "seen"
                } else if !state.lock().recorded {
                    "stale"
                } else if result.inventory.is_empty() {
                    "not-better"
                } else {
                    "accepted"
                };
                assert_eq!(outcome, expected, "op {i}: {op:?}");
                assert_eq!(!result.inventory.is_empty(), outcome == "accepted");
                outcomes.insert(outcome.to_owned());
            }
            _ => panic!("program contains only lock ops"),
        }
        let expected = handler
            .best_chainlock(BlsCtx::LEGACY)
            .map(|s| BlockAt(s.height(), ChainHash(*s.block_hash().as_bytes())));
        assert_eq!(report.post.best_cl, Some(expected), "op {i}: {op:?}");
    }
    for outcome in ["seen", "stale", "accepted", "not-better"] {
        assert!(outcomes.contains(outcome), "{outcome} was exercised");
    }
    eprintln!(
        "{} O2b operations, {} distinct locks, all four record outcomes",
        program.len(),
        seen.len()
    );
}
