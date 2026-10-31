//! Unit tests of CS5b's transaction functions: `check_tx_inputs` (every reject reason, its
//! class and debug message, and the order Core checks in), the sigop counts that read spent
//! coins, and the relative lock times. Core's verdicts on the same rules are compared live in
//! node-l1 (`diff_check_tx_inputs`, `diff_sequence_locks`) and replayed from a Core-built chain
//! in `locktime_vectors.rs`.

#[path = "support/checks.rs"]
mod checks;

use std::collections::BTreeMap;

use dash_consensus::coins::{CoinRef, CoinsView, ViewOr};
use dash_consensus::primitives::write_compact_size;
use dash_consensus::script::ScriptFlags;
use dash_consensus::script::interpreter::{
    SEQUENCE_LOCKTIME_DISABLE_FLAG, SEQUENCE_LOCKTIME_TYPE_FLAG,
};
use dash_consensus::subsidy::COIN;
use dash_consensus::tx::{
    LockTimeFlags, MissingCoin, SequenceLocks, block_lock_time_flags, calculate_sequence_locks,
    check_tx_inputs, evaluate_sequence_locks, p2sh_sigop_count, sequence_locks,
    transaction_sigop_count,
};
use dash_consensus::{MAX_MONEY, Tx, TxValidationResult, decode_tx_exact, format_money};
use dash_primitives::{BlockHash, OutPoint, TxHash, TxOut};
use dash_types::Numeric as _;

use checks::{Network, Window, header_bytes, header_hash};

/// A transaction's fields in Core's layout, one sequence per input.
struct Fields {
    version: i16,
    tx_type: u16,
    /// `(prevout hash byte, index, scriptSig, nSequence)`.
    inputs: Vec<(u8, u32, Vec<u8>, u32)>,
    outputs: Vec<(i64, Vec<u8>)>,
    payload: Option<Vec<u8>>,
}

impl Fields {
    fn spending(inputs: &[(u8, u32)], outputs: &[i64]) -> Self {
        Self {
            version: 2,
            tx_type: 0,
            inputs: inputs.iter().map(|&(h, i)| (h, i, vec![], u32::MAX)).collect(),
            outputs: outputs.iter().map(|&v| (v, vec![0x51])).collect(),
            payload: None,
        }
    }

    fn tx(&self) -> Tx {
        let mut out = self.version.to_le_bytes().to_vec();
        out.extend(self.tx_type.to_le_bytes());
        write_compact_size(self.inputs.len() as u64, &mut out);
        for (hash, index, script, sequence) in &self.inputs {
            out.extend([*hash; 32]);
            out.extend(index.to_le_bytes());
            write_compact_size(script.len() as u64, &mut out);
            out.extend(script);
            out.extend(sequence.to_le_bytes());
        }
        write_compact_size(self.outputs.len() as u64, &mut out);
        for (value, script) in &self.outputs {
            out.extend(value.to_le_bytes());
            write_compact_size(script.len() as u64, &mut out);
            out.extend(script);
        }
        out.extend(0u32.to_le_bytes());
        if let Some(payload) = &self.payload {
            write_compact_size(payload.len() as u64, &mut out);
            out.extend(payload);
        }
        decode_tx_exact(&out).expect("the test transaction decodes").into_value()
    }
}

fn outpoint(hash: u8, index: u32) -> OutPoint {
    OutPoint {
        hash: TxHash::from_lendian([hash; 32]),
        index,
    }
}

/// A view of coins held as outputs, as Core's in-memory cache holds them (no compression).
#[derive(Default)]
struct Coins(BTreeMap<OutPoint, (TxOut, u32, bool)>);

impl Coins {
    fn with(mut self, hash: u8, index: u32, value: i64, height: u32, coinbase: bool) -> Self {
        self.put(hash, index, value, vec![0x51], height, coinbase);
        self
    }

    fn put(&mut self, hash: u8, index: u32, value: i64, script: Vec<u8>, height: u32, cb: bool) {
        let out = TxOut {
            value,
            script_pubkey: script.into(),
        };
        self.0.insert(outpoint(hash, index), (out, height, cb));
    }
}

impl CoinsView for Coins {
    type Error = std::convert::Infallible;

    fn coin(&self, op: &OutPoint) -> Result<Option<CoinRef<'_>>, Self::Error> {
        Ok(self.0.get(op).map(|(out, height, cb)| CoinRef::from_output(out, *height, *cb)))
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

/// The rejection's class, reason and debug message.
fn rejection(
    result: Result<i64, ViewOr<std::convert::Infallible, dash_consensus::TxValidationError>>,
) -> (TxValidationResult, String, String) {
    match result {
        Err(ViewOr::Reject(e)) => {
            (e.result(), e.reject_reason().to_owned(), e.debug_message().to_owned())
        }
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[test]
fn accepts_and_returns_the_fee() {
    let coins = Coins::default().with(1, 0, 5 * COIN, 10, false).with(2, 3, COIN, 20, true);
    let tx = Fields::spending(&[(1, 0), (2, 3)], &[4 * COIN, COIN]).tx();
    assert_eq!(check_tx_inputs(&tx, &coins, 120), Ok(COIN));
    // The whole value may go out, leaving no fee.
    let tx = Fields::spending(&[(1, 0)], &[5 * COIN]).tx();
    assert_eq!(check_tx_inputs(&tx, &coins, 11), Ok(0));
}

#[test]
fn missing_inputs_are_reported_before_anything_else() {
    // Input 0 is an immature coinbase, input 1 is missing: HaveInputs runs first.
    let coins = Coins::default().with(1, 0, COIN, 100, true);
    let tx = Fields::spending(&[(1, 0), (9, 0)], &[1]).tx();
    assert_eq!(
        rejection(check_tx_inputs(&tx, &coins, 101)),
        (
            TxValidationResult::MissingInputs,
            "bad-txns-inputs-missingorspent".into(),
            "CheckTxInputs: inputs missing/spent".into()
        )
    );
}

#[test]
fn the_first_reject_of_core_s_loop_wins_once_every_input_is_present() {
    // Core's loop returns at its first reject, so a later input's reject never shows.
    let coins = Coins::default()
        .with(1, 0, COIN, 100, true)
        .with(2, 0, MAX_MONEY + 1, 1, false)
        .with(3, 0, COIN, 1, false)
        .with(4, 0, COIN, 60, true);
    let rejected = |inputs: &[(u8, u32)]| {
        rejection(check_tx_inputs(&Fields::spending(inputs, &[1]).tx(), &coins, 101))
    };
    let premature = |depth: i64| {
        (
            TxValidationResult::PrematureSpend,
            "bad-txns-premature-spend-of-coinbase".to_owned(),
            format!("tried to spend coinbase at depth {depth}"),
        )
    };
    let out_of_range =
        (TxValidationResult::Consensus, "bad-txns-inputvalues-outofrange".to_owned(), "".into());
    assert_eq!(rejected(&[(1, 0), (2, 0)]), premature(1));
    assert_eq!(rejected(&[(2, 0), (1, 0)]), out_of_range);
    assert_eq!(rejected(&[(3, 0), (2, 0), (1, 0)]), out_of_range);
    // Of two immature coinbases, the first one's depth is printed.
    assert_eq!(rejected(&[(3, 0), (4, 0), (1, 0)]), premature(41));
    assert_eq!(rejected(&[(1, 0), (4, 0)]), premature(1));
    // HaveInputs outranks both, wherever the missing input is.
    for inputs in [&[(2, 0), (1, 0), (9, 0)][..], &[(1, 0), (9, 0), (2, 0)]] {
        assert_eq!(rejected(inputs).1, "bad-txns-inputs-missingorspent");
    }
}

#[test]
fn a_premature_spend_outranks_the_sum_it_would_overflow() {
    // Core checks maturity before `nValueIn +=`: the immature coinbase's value is never added,
    // so the sum it would take past MAX_MONEY is not the reject.
    let coins = Coins::default().with(5, 0, MAX_MONEY, 1, false).with(1, 0, COIN, 100, true);
    let tx = Fields::spending(&[(5, 0), (1, 0)], &[1]).tx();
    assert_eq!(
        rejection(check_tx_inputs(&tx, &coins, 101)),
        (
            TxValidationResult::PrematureSpend,
            "bad-txns-premature-spend-of-coinbase".into(),
            "tried to spend coinbase at depth 1".into()
        )
    );
}

#[test]
fn coinbase_maturity_is_100_blocks() {
    let coins = Coins::default().with(1, 0, COIN, 1_000, true).with(2, 0, COIN, 1_000, false);
    let tx = Fields::spending(&[(1, 0)], &[1]).tx();
    assert_eq!(
        rejection(check_tx_inputs(&tx, &coins, 1_099)),
        (
            TxValidationResult::PrematureSpend,
            "bad-txns-premature-spend-of-coinbase".into(),
            "tried to spend coinbase at depth 99".into()
        )
    );
    assert!(check_tx_inputs(&tx, &coins, 1_100).is_ok());
    // A spend height below the coin's: a negative depth, printed as Core prints its int.
    assert_eq!(
        rejection(check_tx_inputs(&tx, &coins, 990)).2,
        "tried to spend coinbase at depth -10"
    );
    // Not a coinbase: no maturity.
    let tx = Fields::spending(&[(2, 0)], &[1]).tx();
    assert!(check_tx_inputs(&tx, &coins, 1_000).is_ok());
}

#[test]
fn input_values_must_stay_in_the_money_range() {
    let consensus = |coins: &Coins, inputs: &[(u8, u32)]| {
        rejection(check_tx_inputs(&Fields::spending(inputs, &[1]).tx(), coins, 500))
    };
    let expected =
        (TxValidationResult::Consensus, "bad-txns-inputvalues-outofrange".into(), "".into());
    // One value above MAX_MONEY; a negative one; a sum above MAX_MONEY.
    let coins = Coins::default()
        .with(1, 0, MAX_MONEY + 1, 1, false)
        .with(2, 0, -1, 1, false)
        .with(3, 0, MAX_MONEY, 1, false)
        .with(4, 0, 1, 1, false)
        .with(5, 0, i64::MAX, 1, false);
    assert_eq!(consensus(&coins, &[(1, 0)]), expected);
    assert_eq!(consensus(&coins, &[(2, 0)]), expected);
    assert_eq!(consensus(&coins, &[(3, 0), (4, 0)]), expected);
    // MAX_MONEY plus i64::MAX would overflow; Core's verdict is the same reason.
    assert_eq!(consensus(&coins, &[(3, 0), (5, 0)]), expected);
    assert!(check_tx_inputs(&Fields::spending(&[(3, 0)], &[1]).tx(), &coins, 500).is_ok());
}

#[test]
fn value_in_below_value_out_prints_both_amounts() {
    let coins = Coins::default().with(1, 0, COIN + 50, 1, false);
    let tx = Fields::spending(&[(1, 0)], &[COIN, 51]).tx();
    assert_eq!(
        rejection(check_tx_inputs(&tx, &coins, 500)),
        (
            TxValidationResult::Consensus,
            "bad-txns-in-belowout".into(),
            "value in (1.0000005) < value out (1.00000051)".into()
        )
    );
}

/// An asset unlock payload: version, index, fee, requested height, quorum hash and a zero
/// signature (which reads under either scheme).
fn unlock_payload(fee: u32, trailing: usize) -> Vec<u8> {
    let mut payload = vec![2];
    payload.extend(7u64.to_le_bytes());
    payload.extend(fee.to_le_bytes());
    payload.extend(100u32.to_le_bytes());
    payload.extend([0xab; 32]);
    payload.extend([0; 96]);
    payload.extend(vec![0; trailing]);
    payload
}

/// An asset unlock without inputs, paying one output.
fn asset_unlock(fee: u32, trailing: usize) -> Tx {
    Fields {
        version: 3,
        tx_type: 9,
        inputs: vec![],
        outputs: vec![(COIN, vec![0x51])],
        payload: Some(unlock_payload(fee, trailing)),
    }
    .tx()
}

#[test]
fn an_asset_unlocks_fee_is_its_payloads() {
    let empty = Coins::default();
    assert_eq!(check_tx_inputs(&asset_unlock(1_000, 0), &empty, 5), Ok(1_000));
    assert_eq!(
        rejection(check_tx_inputs(&asset_unlock(0, 0), &empty, 5)),
        (TxValidationResult::Consensus, "bad-txns-assetunlock-fee-outofrange".into(), "".into())
    );
    // GetTxPayload reads the payload to its end.
    assert_eq!(
        rejection(check_tx_inputs(&asset_unlock(1_000, 1), &empty, 5)),
        (TxValidationResult::BadSpecial, "bad-assetunlocktx-payload".into(), "".into())
    );
    // Type 9 below the special version is an ordinary transaction: its inputs are checked.
    let plain = Fields {
        version: 2,
        tx_type: 9,
        ..Fields::spending(&[(1, 0)], &[1])
    }
    .tx();
    assert_eq!(rejection(check_tx_inputs(&plain, &empty, 5)).1, "bad-txns-inputs-missingorspent");
}

#[test]
fn an_asset_unlock_counts_sigops_without_coins() {
    // As the special-transaction checks leave it: no inputs, so no coin is read.
    let unlock = asset_unlock(1_000, 0);
    assert_eq!(p2sh_sigop_count(&unlock, &Failing), Ok(0));
    assert_eq!(transaction_sigop_count(&unlock, &Failing, ScriptFlags::P2SH), Ok(0));
}

#[test]
fn a_missing_coin_is_an_error_for_the_sigop_counts_not_a_panic() {
    // An asset unlock with an input: `check_tx_inputs` accepts it without reading the input (as
    // Core does, after the special-transaction checks have rejected it with
    // `bad-assetunlocktx-have-input`); a caller that counts its sigops anyway gets an error.
    let unlock = Fields {
        version: 3,
        tx_type: 9,
        payload: Some(unlock_payload(1_000, 0)),
        ..Fields::spending(&[(1, 0)], &[COIN])
    }
    .tx();
    let empty = Coins::default();
    assert_eq!(check_tx_inputs(&unlock, &empty, 5), Ok(1_000));
    let missing = |input| {
        Err(ViewOr::Reject(MissingCoin {
            input,
        }))
    };
    assert_eq!(p2sh_sigop_count(&unlock, &empty), missing(0));
    assert_eq!(transaction_sigop_count(&unlock, &empty, ScriptFlags::P2SH), missing(0));
    // Without P2SH no coin is read.
    assert_eq!(transaction_sigop_count(&unlock, &empty, ScriptFlags::NONE), Ok(0));
    // The same for any transaction: the second input's coin is missing.
    let coins = Coins::default().with(1, 0, COIN, 1, false);
    let tx = Fields::spending(&[(1, 0), (2, 0)], &[1]).tx();
    assert_eq!(p2sh_sigop_count(&tx, &coins), missing(1));
}

/// A view whose reads fail.
struct Failing;

impl CoinsView for Failing {
    type Error = &'static str;

    fn coin(&self, _: &OutPoint) -> Result<Option<CoinRef<'_>>, &'static str> {
        Err("read failed")
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }
}

#[test]
fn a_read_error_is_never_a_rejection() {
    let tx = Fields::spending(&[(1, 0)], &[1]).tx();
    assert_eq!(check_tx_inputs(&tx, &Failing, 5), Err(ViewOr::View("read failed")));
    assert_eq!(p2sh_sigop_count(&tx, &Failing), Err(ViewOr::View("read failed")));
    assert_eq!(
        transaction_sigop_count(&tx, &Failing, ScriptFlags::P2SH),
        Err(ViewOr::View("read failed"))
    );
}

#[test]
fn p2sh_sigops_count_the_redeem_scripts_of_p2sh_spends_only() {
    let p2sh = {
        let mut s = vec![0xa9, 0x14];
        s.extend([0; 20]);
        s.push(0x87);
        s
    };
    // OP_2 <k1> <k2> <k3> OP_3 OP_CHECKMULTISIG, pushed as the redeem script.
    let mut redeem = vec![0x52];
    for _ in 0..3 {
        redeem.push(33);
        redeem.extend([2; 33]);
    }
    redeem.extend([0x53, 0xae]);
    let mut script_sig = vec![0x00, 0x4c, redeem.len() as u8];
    script_sig.extend(&redeem);

    let mut coins = Coins::default();
    coins.put(1, 0, COIN, p2sh.clone(), 1, false);
    // Spent bare multisig: counted in neither the P2SH count nor (it is not this tx's output)
    // the legacy count.
    coins.put(2, 0, COIN, redeem.clone(), 1, false);
    let mut fields = Fields::spending(&[(1, 0), (2, 0)], &[1]);
    fields.inputs[0].2 = script_sig.clone();
    fields.inputs[1].2 = script_sig;
    let tx = fields.tx();

    assert_eq!(p2sh_sigop_count(&tx, &coins), Ok(3));
    // The legacy count sees the scriptSigs' pushes as data: nothing.
    assert_eq!(tx.legacy_sigop_count(), 0);
    assert_eq!(transaction_sigop_count(&tx, &coins, ScriptFlags::P2SH), Ok(3));
    assert_eq!(transaction_sigop_count(&tx, &coins, ScriptFlags::NONE), Ok(0));
}

#[test]
fn a_coinbase_reads_no_coins_for_its_sigops() {
    let mut fields = Fields::spending(&[(0, u32::MAX)], &[1]);
    fields.inputs[0].2 = vec![0x51, 0x51];
    fields.outputs[0].1 = vec![0xac];
    let coinbase = fields.tx();
    assert!(coinbase.is_coinbase());
    assert_eq!(p2sh_sigop_count(&coinbase, &Failing), Ok(0));
    assert_eq!(transaction_sigop_count(&coinbase, &Failing, ScriptFlags::P2SH), Ok(1));
}

#[test]
fn format_money_matches_cores_unit_tests() {
    // Core: test/util_tests.cpp:317 util_FormatMoney (v24.0.0-rc.3)
    for (n, s) in [
        (0, "0.00"),
        ((COIN / 10_000) * 123_456_789, "12345.6789"),
        (-COIN, "-1.00"),
        (COIN * 100_000_000, "100000000.00"),
        (COIN * 10, "10.00"),
        (COIN, "1.00"),
        (COIN / 10, "0.10"),
        (COIN / 100, "0.01"),
        (COIN / 1_000, "0.001"),
        (COIN / 100_000_000, "0.00000001"),
        (i64::MAX, "92233720368.54775807"),
        (i64::MAX - 3, "92233720368.54775804"),
        (i64::MIN + 1, "-92233720368.54775807"),
        (i64::MIN, "-92233720368.54775808"),
    ] {
        assert_eq!(format_money(n), s, "{n}");
    }
}

// ---- relative lock times ----

/// A regtest window of 40 headers from genesis, 600 s apart (MTP of height h: the time of h - 5,
/// once h ≥ 10).
fn window() -> Window {
    window_with(|h| 1_600_000_000 + 600 * h)
}

/// A regtest window of 40 headers from genesis at increasing `time(h)` (MTP of height h: the
/// time of h - 5, once h ≥ 10).
fn window_with(time: impl Fn(u32) -> u32) -> Window {
    let mut prev = [0; 32];
    let headers = (0..40u32)
        .map(|h| {
            let header = header_bytes(4, prev, time(h), 0x207f_ffff);
            prev = *header_hash(&header).as_bytes();
            header
        })
        .collect();
    Window::from_headers(Network::Regtest, 0, headers, None)
}

/// A transaction of `version` spending one coin per sequence.
fn locked(version: i16, sequences: &[u32]) -> Tx {
    let mut fields = Fields::spending(&[], &[1]);
    fields.version = version;
    fields.inputs = sequences.iter().enumerate().map(|(i, &s)| (1, i as u32, vec![], s)).collect();
    fields.tx()
}

const TYPE: u32 = SEQUENCE_LOCKTIME_TYPE_FLAG;
const DISABLE: u32 = SEQUENCE_LOCKTIME_DISABLE_FLAG;

#[test]
fn height_locks() {
    let window = window();
    let ctx = window.ctx(0); // the block at height 40
    let flags = LockTimeFlags::VERIFY_SEQUENCE;
    // A coin at 30 with a 10-block lock: last invalid height 39, so height 40 is fine.
    let tx = locked(2, &[10]);
    let locks = calculate_sequence_locks(&tx, flags, &mut [30], &ctx);
    assert_eq!(
        locks,
        SequenceLocks {
            min_height: 39,
            min_time: -1
        }
    );
    assert!(evaluate_sequence_locks(&ctx, locks));
    // An 11-block lock is not.
    assert!(!sequence_locks(&locked(2, &[11]), flags, &mut [30], &ctx));
    // The highest requirement over the inputs wins.
    let locks = calculate_sequence_locks(&locked(2, &[3, 11, 0]), flags, &mut [30, 30, 40], &ctx);
    assert_eq!(locks.min_height, 40);
    // The mask is 16 bits; bits 16 to 21 and 23 to 30 are ignored.
    let locks = calculate_sequence_locks(&locked(2, &[0x003f_0005]), flags, &mut [30], &ctx);
    assert_eq!(locks.min_height, 34);
}

#[test]
fn time_locks_run_from_the_median_time_past_before_the_coins_block() {
    let window = window();
    let ctx = window.ctx(0);
    let flags = LockTimeFlags::VERIFY_SEQUENCE;
    // The coin is at 30: its time is the MTP of 29, the time of 24.
    let coin_time = 1_600_000_000 + 600 * 24;
    let locks = calculate_sequence_locks(&locked(2, &[TYPE | 3]), flags, &mut [30], &ctx);
    assert_eq!(
        locks,
        SequenceLocks {
            min_height: -1,
            min_time: coin_time + 3 * 512 - 1
        }
    );
    // The block's cutoff is its parent's MTP (height 39: the time of 34), 6,000 s later.
    assert_eq!(ctx.median_time_past, coin_time + 6_000);
    assert!(sequence_locks(&locked(2, &[TYPE | 11]), flags, &mut [30], &ctx)); // 5,632 s
    assert!(!sequence_locks(&locked(2, &[TYPE | 12]), flags, &mut [30], &ctx)); // 6,144 s
    // A coin at height 0 reads the MTP of height 0.
    let locks = calculate_sequence_locks(&locked(2, &[TYPE]), flags, &mut [0], &ctx);
    assert_eq!(locks.min_time, 1_600_000_000 - 1);
}

#[test]
fn a_time_lock_ending_at_the_cutoff_is_not_met() {
    // EvaluateSequenceLocks: `min_time >= parent MTP` fails, one second less passes.
    let window = window();
    let ctx = window.ctx(0);
    let at = |min_time| SequenceLocks {
        min_height: -1,
        min_time,
    };
    assert!(!evaluate_sequence_locks(&ctx, at(ctx.median_time_past)));
    assert!(evaluate_sequence_locks(&ctx, at(ctx.median_time_past - 1)));

    // The same through a lock: the coin at 30 reads the time of 24, the block's cutoff is the
    // time of 34, and TYPE | 11 ends 5,631 s after the coin's time.
    let flags = LockTimeFlags::VERIFY_SEQUENCE;
    let gap = |last_step| {
        window_with(move |h| match h {
            0..=24 => 1_600_000_000 + 600 * h,
            25..=33 => 1_600_000_000 + 600 * 24 + 563 * (h - 24),
            _ => 1_600_000_000 + 600 * 24 + 563 * 9 + last_step + 600 * (h - 34),
        })
    };
    for (last_step, met) in [(564u32, false), (565, true)] {
        let window = gap(last_step);
        let ctx = window.ctx(0);
        let tx = locked(2, &[TYPE | 11]);
        let locks = calculate_sequence_locks(&tx, flags, &mut [30], &ctx);
        assert_eq!(locks.min_time + i64::from(met), ctx.median_time_past, "{last_step}");
        assert_eq!(sequence_locks(&tx, flags, &mut [30], &ctx), met, "{last_step}");
    }
}

#[test]
fn bip68_needs_the_flag_and_version_2() {
    let window = window();
    let ctx = window.ctx(0);
    let mut heights = [39];
    let tx = locked(2, &[100]);
    assert_eq!(
        calculate_sequence_locks(&tx, LockTimeFlags::NONE, &mut heights, &ctx),
        SequenceLocks::NONE
    );
    for version in [0, 1] {
        assert_eq!(
            calculate_sequence_locks(
                &locked(version, &[100]),
                LockTimeFlags::VERIFY_SEQUENCE,
                &mut heights,
                &ctx
            ),
            SequenceLocks::NONE
        );
    }
    assert_eq!(
        calculate_sequence_locks(
            &locked(3, &[100]),
            LockTimeFlags::VERIFY_SEQUENCE,
            &mut heights,
            &ctx
        )
        .min_height,
        138
    );
    // Undefined flag bits are ignored.
    let other_bits = LockTimeFlags::from_bits(0xffff_fffe);
    assert_eq!(calculate_sequence_locks(&tx, other_bits, &mut heights, &ctx), SequenceLocks::NONE);
    assert_eq!(block_lock_time_flags(true), LockTimeFlags::VERIFY_SEQUENCE);
    assert_eq!(block_lock_time_flags(false), LockTimeFlags::NONE);
}

#[test]
fn the_disable_flag_zeroes_the_inputs_height() {
    let window = window();
    let ctx = window.ctx(0);
    let mut heights = [35, 36, 37];
    let tx = locked(2, &[DISABLE | 1000, 2, DISABLE | TYPE | 7]);
    let locks = calculate_sequence_locks(&tx, LockTimeFlags::VERIFY_SEQUENCE, &mut heights, &ctx);
    assert_eq!(
        locks,
        SequenceLocks {
            min_height: 37,
            min_time: -1
        }
    );
    assert_eq!(heights, [0, 36, 0]);
    // Not enforced: the heights are left alone.
    let mut heights = [35, 36, 37];
    calculate_sequence_locks(&tx, LockTimeFlags::NONE, &mut heights, &ctx);
    assert_eq!(heights, [35, 36, 37]);
}

#[test]
fn a_coin_in_the_block_itself_needs_a_zero_lock() {
    let window = window();
    let ctx = window.ctx(0);
    let flags = LockTimeFlags::VERIFY_SEQUENCE;
    assert!(sequence_locks(&locked(2, &[0]), flags, &mut [40], &ctx));
    assert!(sequence_locks(&locked(2, &[TYPE]), flags, &mut [40], &ctx));
    assert!(!sequence_locks(&locked(2, &[1]), flags, &mut [40], &ctx));
}
