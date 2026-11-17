//! Builders of transaction, header and block wire bytes for node-l1's tests: the fields are
//! written in Core's layout exactly as given, so the rows can build inputs Core and the node may
//! disagree on.

#![allow(dead_code)]

#[path = "../../../dash-consensus/tests/support/gen.rs"]
pub mod generate;

#[allow(unused_imports)]
pub use generate::MAX_MONEY;
use generate::compact_size;

/// One input of a transaction under construction.
#[derive(Clone)]
pub struct In {
    pub hash: [u8; 32],
    pub index: u32,
    pub script: Vec<u8>,
    pub sequence: u32,
}

impl In {
    pub fn spend(n: u8) -> In {
        In {
            hash: [n; 32],
            index: 0,
            script: vec![0x51],
            sequence: u32::MAX,
        }
    }
}

/// A transaction's fields, written in Core's wire layout by [`TxSpec::bytes`]. The payload is
/// written exactly when it is `Some`, whatever the version and type say.
#[derive(Clone)]
pub struct TxSpec {
    pub version: i16,
    pub tx_type: u16,
    pub inputs: Vec<In>,
    pub outputs: Vec<(i64, Vec<u8>)>,
    pub lock_time: u32,
    pub payload: Option<Vec<u8>>,
}

impl TxSpec {
    pub fn plain() -> TxSpec {
        TxSpec {
            version: 1,
            tx_type: 0,
            inputs: vec![In::spend(7)],
            outputs: vec![(1, vec![0x51])],
            lock_time: 0,
            payload: None,
        }
    }

    /// A version-3 transaction of `tx_type` with `payload`.
    pub fn special(tx_type: u16, payload: Vec<u8>) -> TxSpec {
        TxSpec {
            version: 3,
            tx_type,
            payload: Some(payload),
            ..TxSpec::plain()
        }
    }

    pub fn coinbase() -> TxSpec {
        TxSpec {
            inputs: vec![In {
                hash: [0; 32],
                index: u32::MAX,
                script: vec![0x01, 0x02],
                sequence: u32::MAX,
            }],
            ..TxSpec::plain()
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.tx_type.to_le_bytes());
        compact_size(self.inputs.len() as u64, &mut out);
        for input in &self.inputs {
            out.extend_from_slice(&input.hash);
            out.extend_from_slice(&input.index.to_le_bytes());
            compact_size(input.script.len() as u64, &mut out);
            out.extend_from_slice(&input.script);
            out.extend_from_slice(&input.sequence.to_le_bytes());
        }
        compact_size(self.outputs.len() as u64, &mut out);
        for (value, script) in &self.outputs {
            out.extend_from_slice(&value.to_le_bytes());
            compact_size(script.len() as u64, &mut out);
            out.extend_from_slice(script);
        }
        out.extend_from_slice(&self.lock_time.to_le_bytes());
        if let Some(payload) = &self.payload {
            compact_size(payload.len() as u64, &mut out);
            out.extend_from_slice(payload);
        }
        out
    }
}

/// An 80-byte header: version 4, everything else zero.
pub fn header() -> Vec<u8> {
    let mut out = vec![0u8; 80];
    out[0] = 4;
    out
}

/// [`header`] ‖ CompactSize(n) ‖ the transactions.
pub fn block_of(txs: &[Vec<u8>]) -> Vec<u8> {
    let mut out = header();
    compact_size(txs.len() as u64, &mut out);
    for tx in txs {
        out.extend_from_slice(tx);
    }
    out
}

/// An asset-unlock payload: `len` bytes `0, 1, 2, …` with the first set to `version`.
pub fn unlock_payload(version: u8, len: usize) -> Vec<u8> {
    let mut payload: Vec<u8> = (0..len).map(|i| i as u8).collect();
    if let Some(first) = payload.first_mut() {
        *first = version;
    }
    payload
}

/// A version-2 asset unlock (type 9, no inputs, a 145-byte payload with a non-zero
/// quorum-signing tail): its v24 stable txid differs from its instance hash.
pub fn asset_unlock_v2() -> TxSpec {
    TxSpec {
        inputs: vec![],
        ..TxSpec::special(9, unlock_payload(2, 145))
    }
}
