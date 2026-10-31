//! The live `getcreditpoolinfo` walk (EV3-2 §4.3): chains [`connect_records`] from block to block
//! and compares each pool with the reference node's, after checking each mined block's CbTx
//! balance with [`check_block_diff`].
//!
//! `cargo run -p dash-dml --release --example credit_walk` (run through the script)
//!
//! `dash-oracle-sys/scripts/credit-walk.py` reads the blocks and the RPC's pools (pure reads, one
//! at a time) and streams them on stdin, one line each:
//!
//! - `start <main|test> <height> <checkpoint> [<v24 height>]`: the walk's first compared height,
//!   from the empty pool at V20's height, or from the checkpoint file when it holds a later one.
//!   Answers `ready <height>`: the next height to send (window blocks first when starting fresh).
//! - `prime <height> <block hex>`: a block below the first compared height, for its credit data
//!   (the window start of later blocks).
//! - `step <height> <block hex> <balance> <currentlimit> <window height> <window balance>
//!   <window unlocked>`: amounts in duffs, the window height -1 for none. Answers `match <height>`,
//!   or `mismatch <height> <field> core=<x> node=<y>`, or `reject <height> <reason>` where the
//!   node rejects a mined block.
//!
//! The checkpoint (every 1000 steps and at the end) holds the next height, the parent's `nBits`,
//! the pool after the parent with its used indexes (which the RPC does not print), and the
//! window's credit data.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use dash_consensus::payments::PaymentParams;
use dash_consensus::subsidy::block_subsidy_inner;
use dash_consensus::{Amount, decode_block};
use dash_dml::creditpool::{
    ConnectAt, CreditBlockData, CreditPool, PoolEras, check_block_diff, connect_records,
    credit_block_data, platform_reward,
};
use dash_dml::testkit::replay::{mainnet, testnet};
use dash_evo::payments::PaymentsParams;
use dash_params::ChainSetup;
use dash_primitives::{SpecialPayload, TxType};

const CHECKPOINT_EVERY: u32 = 1000;

struct Walk {
    params: &'static ChainSetup,
    payments: PaymentsParams,
    payment: PaymentParams,
    v24_height: Option<u32>,
    checkpoint: String,
    /// The next height to step.
    next: u32,
    /// The parent's `nBits`.
    prev_bits: u32,
    prev: CreditPool,
    /// The credit data of the last `CreditPoolPeriodBlocks()` blocks.
    window: BTreeMap<u32, CreditBlockData>,
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex")).collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Walk {
    fn period(&self) -> u32 {
        self.params.credit_pool_period_blocks as u32
    }

    fn save(&self) {
        let mut pool = Vec::new();
        self.prev.encode(&mut pool);
        let mut text = format!("{} {} {}\n", self.next, self.prev_bits, to_hex(&pool));
        for (h, d) in &self.window {
            text.push_str(&format!("{h} {} {}\n", d.balance, d.unlocked));
        }
        let tmp = format!("{}.tmp", self.checkpoint);
        std::fs::write(&tmp, text).expect("write the checkpoint");
        std::fs::rename(&tmp, &self.checkpoint).expect("move the checkpoint");
    }

    fn load(&mut self) -> bool {
        let Ok(text) = std::fs::read_to_string(&self.checkpoint) else {
            return false;
        };
        let mut lines = text.lines();
        let head: Vec<&str> = lines.next().expect("checkpoint head").split(' ').collect();
        self.next = head[0].parse().expect("next height");
        self.prev_bits = head[1].parse().expect("nBits");
        self.prev = CreditPool::decode(&mut hex(head[2]).as_slice()).expect("checkpoint pool");
        for line in lines {
            let f: Vec<&str> = line.split(' ').collect();
            self.window.insert(
                f[0].parse().expect("height"),
                CreditBlockData {
                    balance: f[1].parse().expect("balance"),
                    unlocked: f[2].parse().expect("unlocked"),
                },
            );
        }
        true
    }

    /// The block's records; `Err` with the reject reason where the node rejects it.
    fn connect(&mut self, h: u32, bytes: &[u8]) -> Result<(), String> {
        let block = decode_block(&mut &bytes[..]).expect("a block").into_value();
        let c = &self.params.consensus;
        let active = |height: u32| h >= height;
        let dip3 = active(c.dip0003_height.to_u32());
        let v20 = active(c.v20_height.to_u32());
        let at = ConnectAt {
            dip3,
            v20,
            eras: PoolEras {
                v24: self.v24_height.is_some_and(|v24| h >= v24),
                withdrawals: active(c.withdrawals_height.to_u32()),
            },
        };
        // Only the pool's construction, from V20, reads the window start.
        let window_start = h.checked_sub(self.period()).filter(|_| v20).map(|w| {
            self.window.get(&w).copied().unwrap_or_else(|| panic!("no window data at {w}"))
        });
        if v20 {
            // ProcessSpecialTxsInBlock's check, as ConnectBlock runs it on the mined block.
            let coinbase = &block.txs()[0];
            if coinbase.tx_type() == TxType::CoinbaseCommitment
                && let Some(Ok(decoded)) = coinbase.payload()
                && let SpecialPayload::CoinbaseCommitment(cbtx) = &decoded.payload
            {
                let subsidy =
                    block_subsidy_inner(self.prev_bits, h as i32 - 1, v20, self.payments.subsidy())
                        .map_err(|e| format!("subsidy {e:?}"))?;
                let reward = platform_reward(
                    active(c.mn_rr_height.to_u32()),
                    h as i32,
                    subsidy,
                    &self.payment,
                )
                .map_err(|e| format!("platform reward {e:?}"))?;
                let dip8 = active(c.dip0008_height.to_u32());
                let balance = cbtx.credit_pool_balance.unwrap_or(0);
                check_block_diff(&block, balance, &self.prev, reward, dip8, v20)
                    .map_err(|e| e.reject_reason().to_owned())?;
            }
        }
        let records = connect_records(&block, at, &self.prev, window_start.as_ref());
        let data = match (dip3, records.data) {
            (true, Some(data)) => data,
            (true, None) => {
                return Err(format!("{:?}", credit_block_data(&block, true).err()));
            }
            (false, _) => CreditBlockData::default(),
        };
        self.window.insert(h, data);
        if let Some(w) = h.checked_sub(self.period()) {
            self.window.retain(|&k, _| k > w);
        }
        self.prev = match records.pool {
            Some(Ok(pool)) => pool,
            Some(Err(e)) => return Err(format!("pool: {e}")),
            None => CreditPool::default(),
        };
        self.prev_bits = block.header().bits();
        self.next = h + 1;
        Ok(())
    }
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let mut walk: Option<Walk> = None;
    let mut steps = 0u32;
    for line in stdin.lock().lines() {
        let line = line.expect("stdin");
        let f: Vec<&str> = line.split(' ').collect();
        match f[0] {
            "start" => {
                let params = match f[1] {
                    "main" => mainnet(),
                    "test" => testnet(),
                    other => panic!("unknown chain {other}"),
                };
                let first: u32 = f[2].parse().expect("height");
                let mut w = Walk {
                    params,
                    payments: PaymentsParams::from_chain_params(params).expect("payments params"),
                    payment: PaymentParams::from_chain_params(params).expect("payment params"),
                    v24_height: f.get(4).map(|v| v.parse().expect("v24 height")),
                    checkpoint: f[3].to_owned(),
                    next: first,
                    prev_bits: 0,
                    prev: CreditPool::default(),
                    window: BTreeMap::new(),
                };
                let ready = if w.load() {
                    w.next
                } else {
                    first - w.period()
                };
                writeln!(out, "ready {ready}").expect("stdout");
                walk = Some(w);
            }
            "prime" => {
                let w = walk.as_mut().expect("started");
                let h: u32 = f[1].parse().expect("height");
                if let Err(reason) = w.connect(h, &hex(f[2])) {
                    writeln!(out, "reject {h} {reason}").expect("stdout");
                }
            }
            "step" => {
                let w = walk.as_mut().expect("started");
                let h: u32 = f[1].parse().expect("height");
                assert_eq!(h, w.next, "steps in order");
                let core: Vec<Amount> =
                    f[3..8].iter().map(|v| v.parse().expect("amount")).collect();
                // `getcreditpoolinfo`'s window start: `GetBalanceAt` of the ancestor one window
                // below, read before the connect drops it from the window.
                let start = h.checked_sub(w.period());
                let window_height = start.map_or(-1, Amount::from);
                let window_balance = start.map_or(0, |s| {
                    w.window.get(&s).unwrap_or_else(|| panic!("no window data at {s}")).balance
                });
                if let Err(reason) = w.connect(h, &hex(f[2])) {
                    writeln!(out, "reject {h} {reason}").expect("stdout");
                    out.flush().expect("stdout");
                    continue;
                }
                let node = [
                    w.prev.locked,
                    w.prev.current_limit,
                    window_height,
                    window_balance,
                    w.prev.lately_unlocked,
                ];
                let names = [
                    "balance",
                    "currentlimit",
                    "window.height",
                    "window.balance",
                    "window.unlocked",
                ];
                let diff = (0..5).find(|&i| core[i] != node[i]);
                match diff {
                    None => writeln!(out, "match {h}"),
                    Some(i) => {
                        writeln!(out, "mismatch {h} {} core={} node={}", names[i], core[i], node[i])
                    }
                }
                .expect("stdout");
                steps += 1;
                if steps.is_multiple_of(CHECKPOINT_EVERY) {
                    w.save();
                }
            }
            "end" => {
                if let Some(w) = &walk {
                    w.save();
                }
                writeln!(out, "done").expect("stdout");
            }
            other => panic!("unknown line {other}"),
        }
        out.flush().expect("stdout");
    }
}
