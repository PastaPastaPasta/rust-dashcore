//! The CS3 boundary rows: named inputs (in `checks.rs`'s formats) at the rules' edges and for
//! quirks 2, 3, 9 and 10. node-l1's `l1_check_rows` compares each with Core and records Core's
//! verdicts in `data/checks_recorded.json`; `checks_replay.rs` replays them through the node.
//!
//! The including crate has `checks` at its root (`checks.rs`, or node-l1's `check_inputs`).

#![allow(clippy::expect_used, reason = "the rows are fixed inputs")]

use dash_consensus::decode_tx_exact;
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::primitives::write_compact_size;
use dash_params::BuriedDeployment;

use crate::checks::{
    ContextualInput, HEADER_INPUT_LEN, HEADER_WINDOWS, Network, Position, Window, header_bytes,
    height_push,
};

/// A named input of one CS3 target.
pub struct Row {
    pub name: String,
    pub target: &'static str,
    pub input: Vec<u8>,
}

fn row(target: &'static str, name: impl Into<String>, input: Vec<u8>) -> Row {
    Row {
        name: name.into(),
        target,
        input,
    }
}

/// A transaction's fields in Core's layout; the payload is written exactly when it is `Some`.
pub struct TxFields {
    pub version: i16,
    pub tx_type: u16,
    /// `(prevout hash, index, scriptSig)`.
    pub inputs: Vec<([u8; 32], u32, Vec<u8>)>,
    pub outputs: Vec<(i64, Vec<u8>)>,
    pub lock_time: u32,
    /// Every input's `nSequence`.
    pub sequence: u32,
    pub payload: Option<Vec<u8>>,
}

impl TxFields {
    /// Version 1, one input spending `[n; 32]:0`, one output.
    pub fn plain(n: u8) -> Self {
        Self {
            version: 1,
            tx_type: 0,
            inputs: vec![([n; 32], 0, vec![0x51])],
            outputs: vec![(1, vec![0x51])],
            lock_time: 0,
            sequence: u32::MAX,
            payload: None,
        }
    }

    /// A coinbase with `script_sig`.
    pub fn coinbase(script_sig: Vec<u8>) -> Self {
        Self {
            inputs: vec![([0; 32], u32::MAX, script_sig)],
            ..Self::plain(0)
        }
    }

    /// `version`, `tx_type`, and the payload field when Core reads one.
    pub fn typed(mut self, version: i16, tx_type: u16) -> Self {
        self.version = version;
        self.tx_type = tx_type;
        self.payload = (version >= 3 && tx_type != 0).then(Vec::new);
        self
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut out = self.version.to_le_bytes().to_vec();
        out.extend(self.tx_type.to_le_bytes());
        write_compact_size(self.inputs.len() as u64, &mut out);
        for (hash, index, script) in &self.inputs {
            out.extend(hash);
            out.extend(index.to_le_bytes());
            write_compact_size(script.len() as u64, &mut out);
            out.extend(script);
            out.extend(self.sequence.to_le_bytes());
        }
        write_compact_size(self.outputs.len() as u64, &mut out);
        for (value, script) in &self.outputs {
            out.extend(value.to_le_bytes());
            write_compact_size(script.len() as u64, &mut out);
            out.extend(script);
        }
        out.extend(self.lock_time.to_le_bytes());
        if let Some(payload) = &self.payload {
            write_compact_size(payload.len() as u64, &mut out);
            out.extend(payload);
        }
        out
    }

    /// The bytes with the first output's script padded so the transaction is `size` bytes.
    pub fn sized(mut self, size: usize) -> Vec<u8> {
        self.outputs[0].1.clear();
        let base = self.bytes().len();
        let mut pad = size.saturating_sub(base);
        loop {
            self.outputs[0].1 = vec![0; pad];
            let len = self.bytes().len();
            if len == size {
                return self.bytes();
            }
            // The script length's CompactSize grew; shorten by the difference.
            pad -= len - size;
        }
    }
}

/// The time of every row's block.
const BLOCK_TIME: u32 = 1_700_000_000;

/// A block: a header with `prev`, `bits` and the transactions' merkle root, then the
/// transactions.
fn block(prev: [u8; 32], bits: u32, txs: &[Vec<u8>]) -> Vec<u8> {
    let leaves: Vec<[u8; 32]> = txs
        .iter()
        .map(|tx| {
            let tx = decode_tx_exact(tx).expect("a row's transaction decodes").into_value();
            *tx.txid().as_bytes()
        })
        .collect();
    let (root, _) = compute_merkle_root(leaves);
    let mut out = header_bytes(4, prev, BLOCK_TIME, bits).to_vec();
    out[36..68].copy_from_slice(&root);
    write_compact_size(txs.len() as u64, &mut out);
    for tx in txs {
        out.extend(tx);
    }
    out
}

/// A contextual tx or block input at `height`: a one-header window, or eleven for the median
/// time past.
fn contextual(network: Network, height: u32, long_window: bool, object: &[u8]) -> Vec<u8> {
    let mut out = Position::encode_height(network, height, 0xc53);
    out.push(if long_window {
        0
    } else {
        3
    });
    out.extend(object);
    out
}

/// A header input at `height` on mainnet's DGW-era chain with a 30-header window, whose
/// candidate time, bits and clock `build` sets from the window: `(time delta from the tip,
/// bits, clock offset)`.
fn header_row(height: u32, build: impl FnOnce(&Window) -> (i32, u32, i16)) -> Vec<u8> {
    let mut out = Position::encode_height(Network::Main, height, 0xc59);
    out.push(3); // HEADER_WINDOWS[3] = 30
    let window = Position::parse(&out).expect("a position").window(HEADER_WINDOWS[3], None);
    let (delta, bits, clock) = build(&window);
    out.push(3); // version 4
    out.extend(delta.to_le_bytes());
    out.push(2); // raw bits
    out.extend(bits.to_le_bytes());
    out.extend(clock.to_le_bytes());
    out.push(0); // checkpoints off
    out.extend([0; 4]);
    out
}

/// A header input at `height` on mainnet with a 30-header window: a candidate 600 s after the
/// tip with version `version` and the expected bits, the clock at the future limit, and with
/// checkpoints on when `best` gives the best header's height (at least the tip's).
fn header_version_row(height: u32, version: i32, best: Option<u32>) -> Vec<u8> {
    let mut out = Position::encode_height(Network::Main, height, 0xc59);
    out.extend([3, 7]); // HEADER_WINDOWS[3] = 30, the version from bytes 19..23
    out.extend(600i32.to_le_bytes());
    out.push(0); // the expected bits
    out.extend(version.to_le_bytes());
    out.extend(0i16.to_le_bytes());
    out.push(best.is_some().into());
    out.extend(best.map_or(0, |best| best - (height - 1)).to_le_bytes());
    out
}

/// `bits` with its target scaled by `num / den` (the mantissa, renormalized if it overflows).
fn scale_bits(bits: u32, num: u32, den: u32) -> u32 {
    let (exponent, mantissa) = (bits >> 24, bits & 0x7f_ffff);
    let scaled = u64::from(mantissa) * u64::from(num) / u64::from(den);
    if scaled <= 0x7f_ffff {
        (exponent << 24) | scaled as u32
    } else {
        ((exponent + 1) << 24) | (scaled >> 8) as u32
    }
}

/// Every boundary row. Every transaction's version is between 1 and 3.
pub fn rows() -> Vec<Row> {
    const CHECK_TX: &str = "diff_check_transaction";
    const CHECK_BLOCK: &str = "diff_check_block";
    const CTX_TX: &str = "diff_contextual_check_tx";
    const CTX_BLOCK: &str = "diff_contextual_check_block";
    const CTX_HEADER: &str = "diff_contextual_check_block_header";
    use dash_consensus::MAX_MONEY;
    let mut rows = Vec::new();
    let no_io = |tx: TxFields| TxFields {
        inputs: vec![],
        outputs: vec![],
        ..tx
    };

    // CheckTransaction: the empty-input and empty-output allowances key on the type whatever the
    // version (quirk 2), and the coinbase scriptSig's one-byte minimum on type 5.
    for (version, tx_type) in [(3, 6), (1, 6), (3, 7), (2, 7)] {
        let tx = no_io(TxFields::plain(1).typed(version, tx_type));
        rows.push(row(
            CHECK_TX,
            format!("quirk 2: v{version} type {tx_type} without inputs or outputs"),
            tx.bytes(),
        ));
    }
    for version in [3, 1] {
        let tx = TxFields {
            inputs: vec![],
            ..TxFields::plain(1).typed(version, 9)
        };
        rows.push(row(CHECK_TX, format!("quirk 2: v{version} type 9 without inputs"), tx.bytes()));
    }
    let tx = TxFields {
        outputs: vec![],
        ..TxFields::plain(1).typed(3, 9)
    };
    rows.push(row(CHECK_TX, "type 9 without outputs", tx.bytes()));
    let tx = TxFields {
        inputs: vec![],
        ..TxFields::plain(1).typed(3, 8)
    };
    rows.push(row(CHECK_TX, "type 8 without inputs", tx.bytes()));
    for (version, tx_type) in [(3, 5), (1, 5), (3, 0)] {
        let tx = TxFields::coinbase(vec![0x51]).typed(version, tx_type);
        rows.push(row(
            CHECK_TX,
            format!("quirk 2: v{version} type {tx_type} coinbase, 1-byte scriptSig"),
            tx.bytes(),
        ));
    }
    for len in [2, 100, 101] {
        let tx = TxFields::coinbase(vec![0x51; len]);
        rows.push(row(CHECK_TX, format!("coinbase, {len}-byte scriptSig"), tx.bytes()));
    }
    for (name, outputs) in [
        ("an output of MAX_MONEY", vec![MAX_MONEY]),
        ("an output of MAX_MONEY + 1", vec![MAX_MONEY + 1]),
        ("an output of -1", vec![-1]),
        ("outputs totalling MAX_MONEY", vec![MAX_MONEY - 1, 1]),
        ("outputs totalling MAX_MONEY + 1", vec![MAX_MONEY, 1]),
    ] {
        let tx = TxFields {
            outputs: outputs.into_iter().map(|v| (v, vec![0x51])).collect(),
            ..TxFields::plain(1)
        };
        rows.push(row(CHECK_TX, name, tx.bytes()));
    }
    let mut tx = TxFields::plain(1);
    tx.inputs.push(tx.inputs[0].clone());
    rows.push(row(CHECK_TX, "a duplicate input", tx.bytes()));
    let mut tx = TxFields::plain(1);
    tx.inputs.push(([0; 32], u32::MAX, vec![0x51]));
    rows.push(row(CHECK_TX, "a null prevout after the first input", tx.bytes()));
    for size in [1_000_000, 1_000_001] {
        rows.push(row(CHECK_TX, format!("{size} bytes"), TxFields::plain(1).sized(size)));
    }
    for len in [10_000, 10_001] {
        let tx = TxFields {
            payload: Some(vec![0; len]),
            ..TxFields::plain(1).typed(3, 8)
        };
        rows.push(row(CHECK_TX, format!("a {len}-byte payload"), tx.bytes()));
    }

    // ContextualCheckTransaction on mainnet: the types from DIP0003, the size from DIP0001
    // (quirk 3: consensus rejects above 100,000 bytes, where policy rejects at it).
    let params = Network::Main.params();
    let dip1 = params.consensus.deployment_height(BuriedDeployment::Dip0001).to_u32();
    let dip3 = params.consensus.deployment_height(BuriedDeployment::Dip0003).to_u32();
    for (height, when) in [(dip3 - 1, "before DIP0003"), (dip3, "at DIP0003")] {
        for (version, tx_type) in [(3, 12), (3, 13), (2, 1), (1, 0)] {
            let tx = TxFields::plain(1).typed(version, tx_type).bytes();
            rows.push(row(
                CTX_TX,
                format!("v{version} type {tx_type} {when}"),
                contextual(Network::Main, height, false, &tx),
            ));
        }
        let tx = TxFields::coinbase(vec![0x51, 0x51]).typed(3, 0).bytes();
        rows.push(row(
            CTX_TX,
            format!("v3 type 0 coinbase {when}"),
            contextual(Network::Main, height, false, &tx),
        ));
    }
    for (height, when) in [(dip1 - 1, "before DIP0001"), (dip1, "at DIP0001")] {
        for size in [100_000, 100_001] {
            let tx = TxFields::plain(1).sized(size);
            rows.push(row(
                CTX_TX,
                format!("quirk 3: {size} bytes {when}"),
                contextual(Network::Main, height, false, &tx),
            ));
        }
    }

    // CheckBlock on regtest without the proof of work: the relaxed sigop and size limits, the
    // CbTx asset-unlock root, and on a devnet the devnet-genesis rule.
    let regtest_no_pow = Network::Regtest.byte() | 8;
    let check_block_row = |name: String, selector: u8, block: Vec<u8>| {
        let mut input = vec![selector];
        input.extend(block);
        row(CHECK_BLOCK, name, input)
    };
    for extra in [0, 1] {
        let mut script = vec![0xae; 2000]; // OP_CHECKMULTISIG counts 20
        script.extend(vec![0xac; extra]);
        let coinbase = TxFields {
            outputs: vec![(1, script)],
            ..TxFields::coinbase(vec![0x51; 2])
        };
        rows.push(check_block_row(
            format!("{} sigops", 40_000 + extra),
            regtest_no_pow,
            block([1; 32], 0x207f_ffff, &[coinbase.bytes()]),
        ));
    }
    for size in [2_000_000, 2_000_001] {
        let coinbase = TxFields::coinbase(vec![0x51; 2]).bytes();
        let first = TxFields::plain(1).sized(999_950);
        let rest = size - 80 - 1 - coinbase.len() - first.len();
        let second = TxFields::plain(2).sized(rest);
        let block = block([1; 32], 0x207f_ffff, &[coinbase, first, second]);
        assert_eq!(block.len(), size);
        rows.push(check_block_row(format!("a {size}-byte block"), regtest_no_pow, block));
    }
    let cbtx_v4 = |asset_unlocks_root: [u8; 32]| {
        let mut payload = 4u16.to_le_bytes().to_vec();
        payload.extend(1u32.to_le_bytes());
        payload.extend([0; 64]);
        payload.push(0);
        payload.extend([0; 96]);
        payload.extend(0i64.to_le_bytes());
        payload.extend(asset_unlocks_root);
        TxFields {
            payload: Some(payload),
            ..TxFields::coinbase(vec![0x51]).typed(3, 5)
        }
        .bytes()
    };
    // An asset unlock: a 145-byte payload whose first byte is its version (from 2, the txid
    // excludes the quorum signing info and the root commits to the instance hashes).
    let asset_unlock = |index: u64, version: u8| {
        let mut payload = vec![version];
        payload.extend(index.to_le_bytes());
        payload.extend(1000u32.to_le_bytes()); // fee
        payload.extend(1u32.to_le_bytes()); // requested height
        payload.extend([3; 32]); // quorum hash
        payload.extend([index as u8; 96]); // quorum signature
        TxFields {
            inputs: vec![],
            payload: Some(payload),
            ..TxFields::plain(1).typed(3, 9)
        }
        .bytes()
    };
    let unlocks = vec![asset_unlock(1, 2), asset_unlock(2, 2)];
    let root_of = |hash: fn(&dash_consensus::Tx) -> [u8; 32]| {
        let leaves = unlocks
            .iter()
            .map(|tx| hash(decode_tx_exact(tx).expect("an asset unlock decodes").value()))
            .collect();
        compute_merkle_root(leaves).0
    };
    let instance_root = root_of(|tx| *tx.instance_hash().as_bytes());
    let txid_root = root_of(|tx| *tx.txid().as_bytes());
    for (name, root, unlocks) in [
        ("CbTx v4 with the root of no asset unlocks", [0u8; 32], vec![]),
        ("CbTx v4 with another asset-unlock root", [7; 32], vec![]),
        ("CbTx v4 with its asset unlocks' instance-hash root", instance_root, unlocks.clone()),
        ("CbTx v4 with its asset unlocks' txid root", txid_root, unlocks.clone()),
        (
            "CbTx v4 with the root of none beside a version-1 asset unlock",
            [0; 32],
            vec![asset_unlock(1, 1)],
        ),
    ] {
        let txs = [vec![cbtx_v4(root)], unlocks].concat();
        rows.push(check_block_row(name.into(), regtest_no_pow, block([1; 32], 0x207f_ffff, &txs)));
    }
    let devnet_genesis = *Network::Devnet.params().consensus.hash_genesis_block.as_bytes();
    let coinbase = TxFields::coinbase(vec![0x51; 2]).bytes();
    rows.push(check_block_row(
        "a devnet block on the genesis block".into(),
        Network::Devnet.byte() | 8,
        block(devnet_genesis, 0x207f_ffff, &[coinbase]),
    ));

    // ContextualCheckBlock: the coinbase height prefix from BIP34 until DIP0003 is `CScript() <<
    // height`, so OP_1..OP_16 for the low heights (quirk 10); the sigop limit before DIP0001.
    for height in [1u32, 5, 16, 17] {
        let push = height_push(height);
        let as_number = vec![1, height as u8, 0x51];
        for (form, script) in [
            ("CScript() << height", [push.clone(), vec![0x51]].concat()),
            ("a one-byte push", as_number),
        ] {
            let block = block([1; 32], 0x207f_ffff, &[TxFields::coinbase(script).bytes()]);
            rows.push(row(
                CTX_BLOCK,
                format!("quirk 10: regtest height {height}, {form}"),
                contextual(Network::Regtest, height, true, &block),
            ));
        }
    }
    let regtest_dip3 =
        Network::Regtest.params().consensus.deployment_height(BuriedDeployment::Dip0003).to_u32();
    for (version, tx_type, form) in [(3, 5, "a CbTx"), (1, 0, "not a CbTx")] {
        let coinbase = TxFields::coinbase(vec![0x51]).typed(version, tx_type).bytes();
        let block = block([1; 32], 0x207f_ffff, &[coinbase]);
        rows.push(row(
            CTX_BLOCK,
            format!("regtest DIP0003 coinbase without a height, {form}"),
            contextual(Network::Regtest, regtest_dip3, true, &block),
        ));
    }
    for (height, when) in [(dip1 - 1, "before DIP0001"), (dip1, "at DIP0001")] {
        for sigops in [20_000, 20_001] {
            let mut script = vec![0xae; 1000];
            script.extend(vec![0xac; sigops - 20_000]);
            let coinbase = TxFields {
                outputs: vec![(1, script)],
                ..TxFields::coinbase([height_push(height), vec![0x51]].concat())
            };
            let block = block([1; 32], 0x207f_ffff, &[coinbase.bytes()]);
            rows.push(row(
                CTX_BLOCK,
                format!("{sigops} sigops {when}"),
                contextual(Network::Main, height, true, &block),
            ));
        }
        // The size limit: 1,000,000 bytes before DIP0001 and 2,000,000 from it, in ten
        // transactions under ContextualCheckTransaction's 100,000-byte limit and a filler.
        for size in [1_000_000, 1_000_001] {
            let coinbase = TxFields::coinbase([height_push(height), vec![0x51]].concat()).bytes();
            let mut txs = vec![coinbase];
            txs.extend((1..=10).map(|n| TxFields::plain(n).sized(99_000)));
            let rest = size - 80 - 1 - txs.iter().map(Vec::len).sum::<usize>();
            txs.push(TxFields::plain(11).sized(rest));
            let block = block([1; 32], 0x207f_ffff, &txs);
            assert_eq!(block.len(), size);
            rows.push(row(
                CTX_BLOCK,
                format!("a {size}-byte block {when}"),
                contextual(Network::Main, height, true, &block),
            ));
        }
    }

    // IsFinalTx in ContextualCheckBlock on mainnet: a lock time below the height, or below the
    // cutoff for a time lock, is final, as is any lock when every input's sequence is final. The
    // cutoff is the median time past from CSV and the block's time before it.
    let csv = params.consensus.deployment_height(BuriedDeployment::Csv).to_u32();
    for (height, when) in [(csv - 1, "before CSV"), (csv, "at CSV")] {
        let position = contextual(Network::Main, height, true, &[]);
        let window = ContextualInput::parse(&position).expect("a position").window;
        let cutoff = if height >= csv {
            window.median_time_past(window.tip()) as u32
        } else {
            BLOCK_TIME
        };
        let mut locks = vec![
            ("a height lock at the height - 1", height - 1, 0),
            ("a height lock at the height", height, 0),
            ("a height lock at the height, final sequences", height, u32::MAX),
            ("a time lock at the cutoff - 1", cutoff - 1, 0),
            ("a time lock at the cutoff", cutoff, 0),
        ];
        if height >= csv {
            locks.push(("a lock of 499999999 (a height)", 499_999_999, 0));
            locks.push(("a lock of 500000000 (a time)", 500_000_000, 0));
        }
        for (lock, lock_time, sequence) in locks {
            let coinbase = TxFields::coinbase([height_push(height), vec![0x51]].concat()).bytes();
            let tx = TxFields {
                lock_time,
                sequence,
                ..TxFields::plain(1)
            };
            let block = block([1; 32], 0x207f_ffff, &[coinbase, tx.bytes()]);
            rows.push(row(CTX_BLOCK, format!("{lock} {when}"), [&position[..], &block].concat()));
        }
    }

    // ContextualCheckBlockHeader on mainnet's DGW chain: up to 68589 the bits may be off by
    // half (quirk 9) and that rejection has no debug message; the time limits.
    for height in [68_589u32, 68_590] {
        for (num, den, factor) in [(1, 1, "exact"), (14, 10, "1.4x"), (16, 10, "1.6x")] {
            let input = header_row(height, |w| {
                (150, scale_bits(w.expected_bits(w.tip_time() + 150), num, den), 0)
            });
            rows.push(row(
                CTX_HEADER,
                format!("quirk 9: height {height}, bits {factor} the target"),
                input,
            ));
        }
    }
    for (offset, name) in [(1, "at"), (0, "one past")] {
        let input = header_row(100_000, |w| {
            let delta = (w.median_time_past(w.tip()) - i64::from(w.tip_time())) as i32 + 1 - offset;
            let time = w.tip_time().wrapping_add_signed(delta);
            (delta, w.expected_bits(time), 0)
        });
        rows.push(row(CTX_HEADER, format!("time {name} the median time past"), input));
    }
    for (clock, name) in [(0, "at"), (-1, "one past")] {
        let input = header_row(100_000, |w| (600, w.expected_bits(w.tip_time() + 600), clock));
        rows.push(row(CTX_HEADER, format!("time {name} the future limit"), input));
    }

    // The version, a signed int32 rendered as 8 hex digits, below each buried deployment's
    // minimum from its height.
    for version in [0, -1, i32::MIN, 3, 4, i32::MAX, 0x2000_0000] {
        let input = header_version_row(100_000, version, None);
        rows.push(row(CTX_HEADER, format!("version {version:#010x} at height 100000"), input));
    }
    for (deployment, version) in [
        (BuriedDeployment::HeightInCoinbase, 1),
        (BuriedDeployment::DerSig, 2),
        (BuriedDeployment::Cltv, 3),
    ] {
        let at = params.consensus.deployment_height(deployment).to_u32();
        for height in [at - 1, at] {
            let input = header_version_row(height, version, None);
            let name = format!("version {version} at height {height} ({deployment:?} from {at})");
            rows.push(row(CTX_HEADER, name, input));
        }
    }
    // A fork below the highest checkpoint the index holds: the checkpoints between the tip and
    // the best header.
    let checkpoints = params.checkpoints.iter().map(|(height, _)| height.to_u32());
    for cp in checkpoints.filter(|&h| h > 200_000).take(3) {
        for (height, best) in
            [(cp - 1, cp + 5), (cp, cp + 5), (cp + 1, cp + 5), (cp - 1, cp), (cp - 1, cp - 1)]
        {
            let input = header_version_row(height, 4, Some(best));
            let name = format!("height {height}, checkpoint {cp}, best header {best}");
            rows.push(row(CTX_HEADER, name, input));
        }
    }

    // A 4040-header window past KGW's era keeps its last 577 headers, not a regenerated
    // 577-header window: the candidate 300 s before the tip is after the window's median time
    // past (Core accepts it); a regenerated window's is later.
    let mut input = Position::encode_height(Network::Main, 68_590, 6);
    input.extend([7, 3]); // HEADER_WINDOWS[7] = 4040, version 4
    input.extend((-300i32).to_le_bytes());
    input.resize(HEADER_INPUT_LEN, 0); // the expected bits, the clock at the future limit
    rows.push(row(CTX_HEADER, "a 4040-header window keeps its last 577 headers", input));
    rows
}
