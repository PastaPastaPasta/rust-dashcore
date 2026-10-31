//! Generators of the CS5b inputs (`diff_check_tx_inputs`, `diff_sequence_locks`), shared by the
//! census (`l1_inputs.rs`) and the twins. Each builds a structurally valid transaction around
//! the boundaries its target compares: coinbase maturity, the money range, value in against
//! value out, asset-unlock fees, P2SH redeem scripts, and relative locks of either kind.

#![allow(dead_code, reason = "each includer uses a subset")]

use dash_consensus::MAX_MONEY;
use dash_consensus::script::interpreter::{
    SEQUENCE_LOCKTIME_DISABLE_FLAG, SEQUENCE_LOCKTIME_TYPE_FLAG,
};
use node_l1::check_inputs::{Network, Position};
use node_l1::inputs::{CoinRecord, LocksInput, TxInputsInput};

use crate::generate::Rng;
use crate::spec::{In, TxSpec};

const COIN: i64 = dash_consensus::subsidy::COIN;
const SEQUENCE_DISABLE: u32 = SEQUENCE_LOCKTIME_DISABLE_FLAG;
const SEQUENCE_TYPE: u32 = SEQUENCE_LOCKTIME_TYPE_FLAG;

fn prevout_hash(rng: &mut Rng) -> [u8; 32] {
    let mut hash = [0; 32];
    hash.copy_from_slice(&rng.bytes(32));
    hash
}

/// One push of `data` (at most 255 bytes).
fn push(data: &[u8]) -> Vec<u8> {
    let mut out = match data.len() {
        0..=75 => vec![data.len() as u8],
        _ => vec![0x4c, data.len() as u8],
    };
    out.extend(data);
    out
}

/// `OP_m <keys> OP_n OP_CHECKMULTISIG` with `n` 33-byte keys.
fn multisig(rng: &mut Rng) -> Vec<u8> {
    let n = 1 + rng.below(3) as u8;
    let mut script = vec![0x50 + 1 + rng.below(u64::from(n)) as u8];
    for _ in 0..n {
        script.push(33);
        script.extend(rng.bytes(33));
    }
    script.extend([0x50 + n, 0xae]);
    script
}

/// A coin's script and the scriptSig that spends it: P2SH with a redeem script pushed (or
/// something else in the scriptSig), P2PKH, bare multisig, `OP_TRUE`, empty or `OP_RETURN`.
fn scripts(rng: &mut Rng) -> (Vec<u8>, Vec<u8>) {
    match rng.below(7) {
        0..=2 => {
            let mut script = vec![0xa9, 0x14];
            script.extend(rng.bytes(20));
            script.push(0x87);
            let redeem = match rng.below(3) {
                0 => multisig(rng),
                1 => vec![0xac, 0xad, 0xac],
                _ => {
                    let len = rng.below(30) as usize;
                    rng.bytes(len)
                }
            };
            let script_sig = match rng.below(4) {
                0 => [vec![0x00], push(&redeem)].concat(),
                1 => vec![0x51, 0xac],
                _ => push(&redeem),
            };
            (script, script_sig)
        }
        3 => {
            let mut script = vec![0x76, 0xa9, 0x14];
            script.extend(rng.bytes(20));
            script.extend([0x88, 0xac]);
            (script, push(&rng.bytes(71)))
        }
        4 => (multisig(rng), vec![0x00]),
        5 => (
            if rng.chance(2) {
                vec![0x51]
            } else {
                vec![]
            },
            vec![],
        ),
        _ => (vec![0x6a, 0x01, 0x00], vec![]),
    }
}

fn spend_height(rng: &mut Rng) -> u32 {
    match rng.below(4) {
        0 => rng.below(200) as u32,
        1 => rng.field_u32() & 0x7fff_ffff,
        _ => 100 + rng.below(3_000_000) as u32,
    }
}

/// Outputs worth `total` (clamped to the money range) in one to three pieces.
fn outputs(rng: &mut Rng, total: i64) -> Vec<(i64, Vec<u8>)> {
    let mut left = total.clamp(0, MAX_MONEY);
    let n = 1 + rng.below(3);
    (0..n)
        .map(|i| {
            let value = if i + 1 == n {
                left
            } else {
                rng.below(left as u64 + 1) as i64
            };
            left -= value;
            (value, vec![0x51])
        })
        .collect()
}

/// A `diff_check_tx_inputs` input.
pub fn check_tx_inputs_bytes(rng: &mut Rng) -> Vec<u8> {
    let height = spend_height(rng);
    if rng.chance(8) {
        return TxInputsInput::encode(height, &[], &asset_unlock(rng));
    }
    let n = rng.below(5) as usize;
    let mut inputs: Vec<In> = Vec::new();
    let mut coins = Vec::new();
    let mut total: i64 = 0;
    for i in 0..n {
        let (hash, index) = if i > 0 && rng.chance(10) {
            let prev = &inputs[rng.below(i as u64) as usize];
            (prev.hash, prev.index)
        } else {
            (prevout_hash(rng), rng.below(3) as u32)
        };
        let (script, script_sig) = scripts(rng);
        let value = match rng.below(8) {
            0 => rng.field_amount(),
            1 => rng.pick(&[MAX_MONEY, MAX_MONEY + 1, -1, -2, 0]),
            _ => rng.below(50 * COIN as u64) as i64,
        };
        if (0..=MAX_MONEY).contains(&value) {
            total = total.saturating_add(value);
        }
        let mut flags = 0;
        if rng.chance(3) {
            flags |= CoinRecord::COINBASE;
        }
        if rng.chance(12) {
            flags |= CoinRecord::ABSENT;
        }
        let depth = match rng.below(4) {
            0 => rng.pick(&[99, 100, 101, 0, -1, 1]),
            1 => rng.field_i16(),
            _ => rng.below(1000) as i16,
        };
        inputs.push(In {
            hash,
            index,
            script: script_sig,
            sequence: u32::MAX,
        });
        coins.push(CoinRecord {
            flags,
            depth,
            value,
            script,
        });
    }
    if rng.chance(10) {
        coins.truncate(rng.below(n as u64 + 1) as usize);
    }
    let out_total = match rng.below(4) {
        0 => total + 1 + rng.below(1000) as i64,
        1 => total,
        _ => total - rng.below(total as u64 / 10 + 1) as i64,
    };
    let tx = TxSpec {
        version: rng.pick(&[1, 2, 3]),
        tx_type: 0,
        inputs,
        outputs: outputs(rng, out_total),
        lock_time: 0,
        payload: None,
    };
    TxInputsInput::encode(height, &coins, &tx.bytes())
}

/// An asset unlock: version, index, fee, requested height, quorum hash and signature, sometimes
/// cut short or extended.
fn asset_unlock(rng: &mut Rng) -> Vec<u8> {
    let mut payload = vec![if rng.chance(8) {
        rng.next_u64() as u8
    } else {
        1
    }];
    payload.extend(rng.next_u64().to_le_bytes());
    let any = rng.next_u64() as u32;
    let fee = rng.pick(&[0, 1, 1_000, any, u32::MAX]);
    payload.extend(fee.to_le_bytes());
    payload.extend((rng.next_u64() as u32).to_le_bytes());
    payload.extend(rng.bytes(32));
    payload.extend(vec![0; 96]);
    match rng.below(6) {
        0 => payload.truncate(rng.below(payload.len() as u64) as usize),
        1 => {
            let len = 1 + rng.below(4) as usize;
            payload.extend(rng.bytes(len));
        }
        _ => {}
    }
    let inputs = (0..rng.below(2))
        .map(|_| In {
            hash: prevout_hash(rng),
            index: 0,
            script: vec![],
            sequence: u32::MAX,
        })
        .collect();
    let value = rng.below(10 * COIN as u64) as i64;
    TxSpec {
        version: 3,
        tx_type: 9,
        inputs,
        outputs: outputs(rng, value),
        lock_time: 0,
        payload: Some(payload),
    }
    .bytes()
}

/// A sequence number: disabled, time- or height-based near small values, the final values, or
/// any.
fn sequence(rng: &mut Rng) -> u32 {
    match rng.below(6) {
        0 => SEQUENCE_DISABLE | rng.next_u64() as u32,
        1 => SEQUENCE_TYPE | rng.below(16) as u32,
        2 => rng.below(40) as u32,
        3 => rng.pick(&[u32::MAX, u32::MAX - 1, 0]),
        4 => SEQUENCE_TYPE | rng.below(0x1_0000) as u32,
        _ => rng.next_u64() as u32,
    }
}

/// A `diff_sequence_locks` input: a transaction of version 0 to 3 in a block on any network.
pub fn sequence_locks_bytes(rng: &mut Rng) -> Vec<u8> {
    let network = rng.pick(&Network::ALL);
    let anchor = rng.below(network.anchors().len() as u64) as u8;
    let position = Position::encode(network, anchor, rng.below(300) as i16 - 150, rng.next_u64());
    let window = rng.below(4) as u8;
    let flags = rng.pick(&[1, 1, 1, 0, 3, u32::MAX, u32::MAX - 1]);
    let n = 1 + rng.below(4) as usize;
    let inputs: Vec<_> = (0..n)
        .map(|_| In {
            hash: prevout_hash(rng),
            index: 0,
            script: vec![],
            sequence: sequence(rng),
        })
        .collect();
    let depths: Vec<u16> = (0..n - usize::from(rng.chance(8)))
        .map(|_| {
            if rng.chance(4) {
                rng.below(3) as u16
            } else {
                rng.below(80) as u16
            }
        })
        .collect();
    let tx = TxSpec {
        version: rng.pick(&[0, 1, 2, 3]),
        tx_type: 0,
        inputs,
        outputs: vec![(COIN, vec![0x51])],
        lock_time: 0,
        payload: None,
    };
    LocksInput::encode(&position, window, flags, &depths, &tx.bytes())
}
