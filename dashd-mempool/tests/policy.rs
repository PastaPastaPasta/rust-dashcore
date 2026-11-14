use dash_consensus::coins::{Coin, CoinRef, CoinWrite, CoinsView, MemoryCoins};
use dash_consensus::primitives::{BlockHash, write_bytes};
use dash_consensus::script::interpreter::MAX_SCRIPT_SIZE;
use dash_consensus::script::opcodes::*;
use dash_consensus::script::push_data;
use dash_consensus::{Tx, decode_tx_exact};
use dash_primitives::{OutPoint, SpecialPayload, Transaction, TxHash, TxIn, TxOut, TxType};
use dashd_mempool::options::MemPoolOptions;
use dashd_mempool::policy::consts::*;
use dashd_mempool::policy::*;

const P2PKH: [u8; 25] = [
    OP_DUP,
    OP_HASH160,
    20,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    OP_EQUALVERIFY,
    OP_CHECKSIG,
];
const P2SH: [u8; 23] =
    [OP_HASH160, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, OP_EQUAL];
const SHARED: [u8; 7] = [4, b'D', b'S', b'H', b'C', OP_DROP, OP_1];

fn input(script: &[u8]) -> TxIn {
    TxIn::new(OutPoint::new(TxHash::from([1; 32]), 0), script.to_vec().into(), u32::MAX)
}

fn output(value: i64, script: &[u8]) -> TxOut {
    TxOut::from_duffs(value, script.to_vec().into())
}

fn transaction(version: i16, script_sig: &[u8], outputs: Vec<TxOut>) -> Tx {
    Tx::new(Transaction::new(version, TxType::new(0), vec![input(script_sig)], outputs, 0, vec![]))
}

fn special(tx_type: TxType, inputs: Vec<TxIn>, outputs: Vec<TxOut>, payload: Vec<u8>) -> Tx {
    Tx::new(Transaction::new(3, tx_type, inputs, outputs, 0, payload))
}

fn standard(tx: &Tx) -> Result<(), &'static str> {
    let options = MemPoolOptions::default();
    is_standard_tx(
        tx,
        options.max_datacarrier_bytes,
        options.permit_bare_multisig,
        options.dust_relay_feerate,
    )
}

fn key() -> Vec<u8> {
    let mut key = vec![1; 33];
    key[0] = 2;
    key
}

fn count(script: &mut Vec<u8>, n: u8) {
    if (1..=16).contains(&n) {
        script.push(OP_1 + n - 1);
    } else {
        push_data(script, &[n]);
    }
}

fn multisig(m: u8, n: u8) -> Vec<u8> {
    let mut script = Vec::new();
    count(&mut script, m);
    for _ in 0..n {
        push_data(&mut script, &key());
    }
    count(&mut script, n);
    script.push(OP_CHECKMULTISIG);
    script
}

fn coins(script: &[u8]) -> MemoryCoins {
    let mut view = MemoryCoins::new();
    view.write(input(&[]).prevout(), CoinWrite::Put(Coin::new(1_000, script, 1, false)));
    view
}

#[test]
fn policy_constants_and_option_defaults_match_core_v24() {
    assert_eq!(DEFAULT_BLOCK_MAX_SIZE, 2_000_000);
    assert_eq!(DEFAULT_BLOCK_MIN_TX_FEE, 1_000);
    assert_eq!(MAX_STANDARD_TX_SIZE, 100_000);
    assert_eq!(MIN_STANDARD_TX_SIZE, 65);
    assert_eq!(MAX_P2SH_SIGOPS, 15);
    assert_eq!(MAX_STANDARD_TX_SIGOPS, 4_000);
    assert_eq!(MAX_STANDARD_SCRIPTSIG_SIZE, 1_650);
    assert_eq!(EXTRA_DESCENDANT_TX_SIZE_LIMIT, 10_000);
    assert_eq!(TX_MAX_STANDARD_VERSION, 3);
    assert_eq!(STANDARD_SCRIPT_VERIFY_FLAGS.bits(), 0x147df);
    assert_eq!(STANDARD_NOT_MANDATORY_VERIFY_FLAGS.bits(), 0x141ca);
    assert_eq!(STANDARD_LOCKTIME_VERIFY_FLAGS.bits(), 1);
    let options = MemPoolOptions::default();
    assert_eq!(options.bytes_per_sigop, DEFAULT_BYTES_PER_SIGOP);
    assert_eq!(options.permit_bare_multisig, DEFAULT_PERMIT_BAREMULTISIG);
    assert_eq!(options.dust_relay_feerate, i64::from(DUST_RELAY_TX_FEE));
    assert_eq!(options.min_relay_feerate, i64::from(DEFAULT_MIN_RELAY_TX_FEE));
    assert_eq!(options.incremental_relay_feerate, i64::from(DEFAULT_INCREMENTAL_RELAY_FEE));
    assert_eq!(options.limits.ancestor_count, i64::from(DEFAULT_ANCESTOR_LIMIT));
    assert_eq!(
        options.limits.ancestor_size_vbytes,
        i64::from(DEFAULT_ANCESTOR_SIZE_LIMIT_KVB) * 1_000
    );
    assert_eq!(options.limits.descendant_count, i64::from(DEFAULT_DESCENDANT_LIMIT));
    assert_eq!(
        options.limits.descendant_size_vbytes,
        i64::from(DEFAULT_DESCENDANT_SIZE_LIMIT_KVB) * 1_000
    );
}

// Core v24.0.0: test/transaction_tests.cpp:413-451 test_IsStandard
#[test]
fn dust_fee_rounding_and_exact_threshold() {
    for (rate, threshold) in [(3_000, 546), (3_702, 674), (1, 1), (0, 0), (-1, -1), (-3_702, -673)]
    {
        assert_eq!(get_dust_threshold(&output(0, &P2PKH), rate), threshold);
        assert!(is_dust(&output(threshold - 1, &P2PKH), rate));
        assert!(!is_dust(&output(threshold, &P2PKH), rate));
    }
    assert_eq!(get_dust_threshold(&output(0, &P2SH), 3_000), 540);
    for script in [vec![OP_RETURN], vec![OP_0; MAX_SCRIPT_SIZE + 1]] {
        assert_eq!(get_dust_threshold(&output(0, &script), 3_000), 0);
    }
    for size in [252, 253, MAX_SCRIPT_SIZE] {
        let prefix = if size < 253 {
            1
        } else {
            3
        };
        assert_eq!(
            get_dust_threshold(&output(0, &vec![OP_0; size]), 3_000),
            3 * (8 + prefix + size as i64 + 148)
        );
    }
    let tx = transaction(2, &[], vec![output(545, &P2PKH)]);
    assert_eq!(standard(&tx), Err("dust"));
    assert_eq!(standard(&transaction(2, &[], vec![output(546, &P2PKH)])), Ok(()));
}

// Core v24.0.0: test/transaction_tests.cpp:422-440 test_IsStandard
#[test]
fn transaction_version_boundary_table() {
    for (version, expected) in [
        (-1, Err("version")),
        (0, Err("version")),
        (1, Ok(())),
        (2, Ok(())),
        (3, Ok(())),
        (4, Err("version")),
    ] {
        assert_eq!(standard(&transaction(version, &[], vec![output(546, &P2PKH)])), expected);
    }
}

#[test]
fn minimum_size_belongs_to_atmp_and_maximum_is_exclusive() {
    for size in [64, 65] {
        let mut script = vec![OP_RETURN];
        script.resize(size - 60, OP_0);
        let tx = transaction(2, &[], vec![output(0, &script)]);
        assert_eq!(tx.size(), size as u32);
        assert_eq!(standard(&tx), Ok(()));
        assert_eq!(tx.size() < MIN_STANDARD_TX_SIZE, size == 64);
    }
    for (size, expected) in [(99_999, Ok(())), (100_000, Err("tx-size"))] {
        let base = special(
            TxType::ProviderUpdateService,
            vec![input(&[])],
            vec![output(546, &P2PKH)],
            vec![0; 65_536],
        );
        let payload_size = 65_536 + size - base.size() as usize;
        let tx = special(
            TxType::ProviderUpdateService,
            vec![input(&[])],
            vec![output(546, &P2PKH)],
            vec![0; payload_size],
        );
        assert_eq!(tx.size(), size as u32);
        assert_eq!(standard(&tx), expected);
    }
}

// Core v24.0.0: test/transaction_tests.cpp:502-544 test_IsStandard
#[test]
fn scriptsig_size_pushonly_and_reject_precedence() {
    for (data_size, expected) in [(1_647, Ok(())), (1_648, Err("scriptsig-size"))] {
        let mut script = Vec::new();
        push_data(&mut script, &vec![0; data_size]);
        assert_eq!(script.len(), data_size + 3);
        assert_eq!(standard(&transaction(2, &script, vec![output(546, &P2PKH)])), expected);
    }
    for script in [vec![OP_NOP], vec![OP_PUSHDATA1], vec![2, 0]] {
        assert_eq!(
            standard(&transaction(2, &script, vec![output(546, &P2PKH)])),
            Err("scriptsig-not-pushonly")
        );
    }
    let pushes = [OP_0, OP_1NEGATE, OP_RESERVED, OP_1, OP_16];
    assert_eq!(standard(&transaction(2, &pushes, vec![output(546, &P2PKH)])), Ok(()));
    assert_eq!(standard(&transaction(0, &[OP_NOP; 1_651], vec![output(0, &[])])), Err("version"));
    assert_eq!(
        standard(&transaction(2, &[OP_NOP; 1_651], vec![output(0, &[])])),
        Err("scriptsig-size")
    );
    assert_eq!(
        standard(&transaction(2, &[OP_NOP], vec![output(0, &[])])),
        Err("scriptsig-not-pushonly")
    );
}

// Core v24.0.0: test/transaction_tests.cpp:453-500,546-556 test_IsStandard
#[test]
fn output_policy_boundary_table() {
    for (keys, expected) in [(3, Ok(())), (4, Err("scriptpubkey"))] {
        let tx = transaction(2, &[], vec![output(100_000, &multisig(1, keys))]);
        assert_eq!(standard(&tx), expected);
        assert_eq!(
            is_standard_tx(&tx, Some(83), false, 3_000),
            if keys == 3 {
                Err("bare-multisig")
            } else {
                expected
            }
        );
    }
    for (size, expected) in [(83, Ok(())), (84, Err("scriptpubkey"))] {
        let mut script = vec![OP_RETURN];
        push_data(&mut script, &vec![0; size - 3]);
        assert_eq!(script.len(), size);
        let tx = transaction(2, &[], vec![output(0, &script)]);
        assert_eq!(standard(&tx), expected);
        assert_eq!(is_standard_tx(&tx, None, true, 3_000), Err("scriptpubkey"));
    }
    for script in [
        vec![OP_RETURN],
        vec![OP_RETURN, OP_RESERVED, OP_1NEGATE, OP_0, OP_16],
        vec![OP_RETURN, OP_PUSHDATA1, 0],
    ] {
        assert_eq!(standard(&transaction(2, &[], vec![output(0, &script)])), Ok(()));
        assert_eq!(is_standard(&script, None), (false, TxoutType::NullData));
    }
    assert_eq!(is_standard(&[OP_RETURN], Some(0)), (false, TxoutType::NullData));
    assert_eq!(is_standard(&[OP_RETURN, OP_RETURN], Some(83)), (false, TxoutType::Nonstandard));
    let mut outputs = vec![output(0, &[OP_RETURN]), output(0, &[OP_RETURN])];
    assert_eq!(standard(&transaction(2, &[], outputs.clone())), Err("multi-op-return"));
    outputs.push(output(545, &P2PKH));
    assert_eq!(standard(&transaction(2, &[], outputs)), Err("dust"));
    let bare_dust = transaction(2, &[], vec![output(0, &multisig(1, 1))]);
    assert_eq!(is_standard_tx(&bare_dust, Some(83), false, 3_000), Err("bare-multisig"));
}

// Core v24.0.0: test/script_standard_tests.cpp:21-152; same vectors as RPC's Solver tests.
#[test]
fn solver_solutions_and_malformed_shapes() {
    let key = key();
    let mut pubkey = Vec::new();
    push_data(&mut pubkey, &key);
    pubkey.push(OP_CHECKSIG);
    assert_eq!(solver(&pubkey), (TxoutType::Pubkey, vec![key.clone()]));
    assert_eq!(solver(&P2PKH), (TxoutType::PubkeyHash, vec![vec![0; 20]]));
    assert_eq!(solver(&P2SH), (TxoutType::ScriptHash, vec![vec![0; 20]]));
    assert_eq!(
        solver(&multisig(1, 2)),
        (TxoutType::Multisig, vec![vec![1], key.clone(), key, vec![2]])
    );
    for script in [vec![OP_RETURN], vec![OP_RETURN, 0, OP_RESERVED], vec![OP_RETURN, 1, 75]] {
        assert_eq!(solver(&script), (TxoutType::NullData, vec![]));
    }
    for script in [
        vec![],
        vec![OP_9, OP_ADD, OP_11, OP_EQUAL],
        vec![30; 32],
        vec![OP_1, OP_1, OP_CHECKMULTISIG],
        vec![OP_RETURN, 1, 75, OP_ADD],
        vec![OP_RETURN, OP_PUSHDATA1],
    ] {
        assert_eq!(solver(&script), (TxoutType::Nonstandard, vec![]));
    }
    let good = multisig(1, 1);
    for first in [OP_0, OP_2, OP_1NEGATE] {
        let mut bad = good.clone();
        bad[0] = first;
        assert_eq!(solver(&bad), (TxoutType::Nonstandard, vec![]));
    }
    let mut bad = good.clone();
    let count_pos = bad.len() - 2;
    bad[count_pos] = OP_2;
    assert_eq!(solver(&bad), (TxoutType::Nonstandard, vec![]));
    for (header, size) in [(2, 33), (3, 33), (4, 65), (6, 65), (7, 65)] {
        let mut key = vec![0; size];
        key[0] = header;
        let mut script = Vec::new();
        push_data(&mut script, &key);
        script.push(OP_CHECKSIG);
        assert_eq!(solver(&script), (TxoutType::Pubkey, vec![key]));
        script[1] = 5;
        assert_eq!(solver(&script).0, TxoutType::Nonstandard);
    }
    let mut nonminimal_pubkey = vec![OP_PUSHDATA1];
    nonminimal_pubkey.extend(&good[1..good.len() - 2]);
    nonminimal_pubkey.push(OP_CHECKSIG);
    assert_eq!(solver(&nonminimal_pubkey).0, TxoutType::Nonstandard);
}

#[test]
fn solver_multisig_counts_are_minimal_but_key_pushes_need_not_be() {
    for n in 1..=20 {
        assert_eq!(solver(&multisig(n, n)).0, TxoutType::Multisig);
    }
    assert_eq!(solver(&multisig(1, 21)).0, TxoutType::Nonstandard);
    for prefix in
        [vec![OP_PUSHDATA1, 33], vec![OP_PUSHDATA2, 33, 0], vec![OP_PUSHDATA4, 33, 0, 0, 0]]
    {
        let mut script = vec![OP_1];
        script.extend(prefix);
        script.extend(key());
        script.extend([OP_1, OP_CHECKMULTISIG]);
        assert_eq!(solver(&script).0, TxoutType::Multisig);
    }
    for bad_count in
        [vec![1, 1], vec![OP_PUSHDATA1, 1, 17], vec![2, 17, 0], vec![1, 0x80], vec![1, 21]]
    {
        let good = multisig(1, 17);
        let mut script = good[..good.len() - 3].to_vec();
        script.extend(bad_count);
        script.push(OP_CHECKMULTISIG);
        assert_eq!(solver(&script), (TxoutType::Nonstandard, vec![]));
    }
    let mut truncated = vec![OP_1, OP_PUSHDATA1, 34];
    truncated.extend(key());
    truncated.extend([OP_1, OP_CHECKMULTISIG]);
    assert_eq!(solver(&truncated).0, TxoutType::Nonstandard);
}

#[test]
fn p2sh_sigop_boundary_and_evalscript_failures() {
    let view = coins(&P2SH);
    for (sigops, expected) in [(15, true), (16, false)] {
        let mut script_sig = Vec::new();
        push_data(&mut script_sig, &vec![OP_CHECKSIG; sigops]);
        assert_eq!(are_inputs_standard(&transaction(2, &script_sig, vec![]), &view), Ok(expected));
    }
    for (count, expected) in [(15, true), (16, false)] {
        let mut script_sig = Vec::new();
        push_data(&mut script_sig, &[OP_1 + count - 1, OP_CHECKMULTISIG]);
        assert_eq!(are_inputs_standard(&transaction(2, &script_sig, vec![]), &view), Ok(expected));
    }
    for script_sig in
        [vec![], vec![OP_RETURN], vec![OP_RESERVED], vec![OP_PUSHDATA1], vec![OP_0, OP_DROP]]
    {
        assert_eq!(are_inputs_standard(&transaction(2, &script_sig, vec![]), &view), Ok(false));
    }
    let mut oversized_element = Vec::new();
    push_data(&mut oversized_element, &[0; 521]);
    assert_eq!(are_inputs_standard(&transaction(2, &oversized_element, vec![]), &view), Ok(false));
    assert_eq!(are_inputs_standard(&transaction(2, &[OP_0; 1_001], vec![]), &view), Ok(false));
    assert_eq!(are_inputs_standard(&transaction(2, &[OP_0, OP_DUP], vec![]), &view), Ok(true));
    assert_eq!(
        are_inputs_standard(&transaction(2, &[OP_RETURN], vec![]), &coins(&P2PKH)),
        Ok(true)
    );
    assert_eq!(are_inputs_standard(&transaction(2, &[], vec![]), &coins(&[OP_1])), Ok(false));
    assert_eq!(are_inputs_standard(&transaction(2, &[], vec![]), &MemoryCoins::new()), Ok(false));
}

#[test]
fn input_standardness_propagates_storage_faults_and_skips_coinbase() {
    struct Fault;
    impl CoinsView for Fault {
        type Error = &'static str;
        fn coin(&self, _: &OutPoint) -> Result<Option<CoinRef<'_>>, Self::Error> {
            Err("read-fault")
        }
        fn best_block(&self) -> Option<BlockHash> {
            None
        }
    }
    assert_eq!(are_inputs_standard(&transaction(2, &[], vec![]), &Fault), Err("read-fault"));
    let coinbase = Tx::new(Transaction::new(
        2,
        TxType::new(0),
        vec![TxIn::new(OutPoint::new(TxHash::from([0; 32]), u32::MAX), vec![].into(), u32::MAX)],
        vec![],
        0,
        vec![],
    ));
    assert!(coinbase.is_coinbase());
    assert_eq!(are_inputs_standard(&coinbase, &Fault), Ok(true));
}

fn registration_payload(shared: bool, collateral_hash: [u8; 32], collateral_index: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(3u16.to_le_bytes());
    bytes.extend([0; 4]); // type, mode
    bytes.extend(collateral_hash);
    bytes.extend(collateral_index.to_le_bytes());
    bytes.extend([1, 0]); // NetInfo version and purpose count
    bytes.extend([0; 20 + 48 + 20 + 2]); // owner, operator, voting, reward
    bytes.push(0); // payouts
    bytes.push(u8::from(shared));
    if shared {
        bytes.extend(100_000_000i64.to_le_bytes());
        write_bytes(&P2PKH, &mut bytes);
        write_bytes(&[], &mut bytes);
        bytes.extend([0; 20 + 65]); // share owner, join signature
    }
    bytes.extend([0; 4 + 8 + 32]); // early period, penalty, inputs hash
    bytes.push(0); // collateral signature
    bytes
}

#[test]
fn shared_registration_output_exception_is_limited_to_declared_internal_collateral() {
    let shared_output = || output(0, &SHARED);
    for (shared, hash, index, expected) in [
        (true, [0; 32], 0, Ok(())),
        (false, [0; 32], 0, Err("scriptpubkey")),
        (true, [1; 32], 0, Err("scriptpubkey")),
        (true, [0; 32], 1, Err("scriptpubkey")),
    ] {
        let tx = special(
            TxType::ProviderRegister,
            vec![input(&[])],
            vec![shared_output()],
            registration_payload(shared, hash, index),
        );
        let decoded = tx.payload().unwrap().unwrap();
        assert!(decoded.is_complete());
        assert!(matches!(decoded.payload(), SpecialPayload::ProviderRegister(_)));
        assert_eq!(standard(&tx), expected);
    }
    let payload = registration_payload(true, [0; 32], 1);
    let tx = special(
        TxType::ProviderRegister,
        vec![input(&[])],
        vec![output(546, &P2PKH), shared_output()],
        payload.clone(),
    );
    assert_eq!(standard(&tx), Ok(()));
    let tx = special(
        TxType::ProviderRegister,
        vec![input(&[])],
        vec![shared_output(), shared_output()],
        payload,
    );
    assert_eq!(standard(&tx), Err("scriptpubkey"));
    for payload in [vec![], vec![3], {
        let mut p = registration_payload(true, [0; 32], 0);
        p.push(0);
        p
    }] {
        let tx =
            special(TxType::ProviderRegister, vec![input(&[])], vec![shared_output()], payload);
        assert_eq!(standard(&tx), Err("scriptpubkey"));
    }
    let mut almost = SHARED.to_vec();
    almost.push(OP_0);
    let tx = special(
        TxType::ProviderRegister,
        vec![input(&[])],
        vec![output(0, &almost)],
        registration_payload(true, [0; 32], 0),
    );
    assert_eq!(standard(&tx), Err("scriptpubkey"));
    assert_eq!(standard(&transaction(2, &[], vec![shared_output()])), Err("scriptpubkey"));
}

#[test]
fn dissolve_input_exception_uses_only_version_type_and_exact_template() {
    for (version, tx_type, script, expected) in [
        (3, TxType::ProviderDissolve, SHARED.to_vec(), true),
        (2, TxType::ProviderDissolve, SHARED.to_vec(), false),
        (3, TxType::ProviderRegister, SHARED.to_vec(), false),
        (3, TxType::ProviderDissolve, vec![OP_1], false),
        (
            3,
            TxType::ProviderDissolve,
            {
                let mut s = SHARED.to_vec();
                s.push(OP_0);
                s
            },
            false,
        ),
    ] {
        let tx = Tx::new(Transaction::new(version, tx_type, vec![input(&[])], vec![], 0, vec![]));
        assert_eq!(are_inputs_standard(&tx, &coins(&script)), Ok(expected));
    }
    let tx = special(TxType::ProviderDissolve, vec![input(&[OP_RETURN])], vec![], vec![]);
    assert_eq!(are_inputs_standard(&tx, &coins(&SHARED)), Ok(true));
}

#[test]
fn asset_lock_input_size_version_boundaries_and_reject_order() {
    for (inputs, expected) in [(100, Ok(())), (101, Err("assetlocktx-too-many-inputs"))] {
        let tx = special(TxType::AssetLock, vec![input(&[]); inputs], vec![], vec![1, 0]);
        assert_eq!(is_standard_special_tx(&tx), expected);
    }
    for (size, expected) in [(20_480, Ok(())), (20_481, Err("assetlocktx-too-big"))] {
        let base =
            special(TxType::AssetLock, vec![input(&[])], vec![output(0, &[0; 253])], vec![1, 0]);
        let padding = 253 + size - base.size() as usize;
        let tx = special(
            TxType::AssetLock,
            vec![input(&[])],
            vec![output(0, &vec![0; padding])],
            vec![1, 0],
        );
        assert_eq!(tx.size(), size as u32);
        assert_eq!(is_standard_special_tx(&tx), expected);
    }
    for (payload, expected) in [
        (vec![1, 0], Ok(())),
        (vec![2, 0], Err("assetlocktx-version-2")),
        (vec![3, 0], Err("assetlocktx-version-2")),
        (vec![], Ok(())),
        (vec![2], Ok(())),
        (vec![2, 0, 0], Ok(())),
    ] {
        let tx = special(TxType::AssetLock, vec![input(&[])], vec![], payload);
        assert_eq!(is_standard_special_tx(&tx), expected);
    }
    let tx = special(
        TxType::AssetLock,
        vec![input(&[]); 101],
        vec![output(0, &[0; 20_480])],
        vec![2, 0],
    );
    assert_eq!(is_standard_special_tx(&tx), Err("assetlocktx-too-many-inputs"));
    let tx =
        special(TxType::AssetLock, vec![input(&[])], vec![output(0, &[0; 20_480])], vec![2, 0]);
    assert_eq!(is_standard_special_tx(&tx), Err("assetlocktx-too-big"));
    for (version, tx_type) in [(2, TxType::AssetLock), (3, TxType::ProviderRegister)] {
        let tx = Tx::new(Transaction::new(
            version,
            tx_type,
            vec![input(&[]); 101],
            vec![],
            0,
            vec![2, 0],
        ));
        assert_eq!(is_standard_special_tx(&tx), Ok(()));
    }
}

#[test]
fn virtual_size_widens_the_normalized_unsigned_option() {
    let tx = transaction(2, &[], vec![output(546, &P2PKH)]);
    for (argument, normalized, expected) in [
        (-1i64, u32::MAX, 20 * i64::from(u32::MAX)),
        (0, 0, 100),
        (1i64 << 32, 0, 100),
        (20, 20, 400),
        (30, 30, 600),
    ] {
        let options = MemPoolOptions {
            bytes_per_sigop: argument as u32,
            ..MemPoolOptions::default()
        };
        assert_eq!(options.bytes_per_sigop, normalized);
        assert_eq!(get_virtual_transaction_size(100, 20, options.bytes_per_sigop), expected);
        let expected_tx = if normalized == 0 {
            i64::from(tx.size())
        } else {
            expected
        };
        assert_eq!(
            get_virtual_transaction_size_for_tx(&tx, 20, options.bytes_per_sigop),
            expected_tx
        );
        assert_eq!(
            get_virtual_transaction_size_for_tx(&tx, 0, options.bytes_per_sigop),
            i64::from(tx.size())
        );
    }
}

fn hex(text: &str) -> Vec<u8> {
    let text = text.split_whitespace().collect::<String>();
    let (pairs, trailing) = text.as_bytes().as_chunks::<2>();
    assert!(trailing.is_empty());
    pairs
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn core_test_vectors_classify_transaction_outputs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../test-vectors/core/0364ed626ec2/test/util/data");
    for (name, expected) in [
        ("txcreateoutpubkey1", vec![TxoutType::Pubkey]),
        ("txcreatemultisig1", vec![TxoutType::Multisig]),
        ("txcreatescript1", vec![TxoutType::Nonstandard]),
        ("txcreatedata1", vec![TxoutType::PubkeyHash, TxoutType::NullData]),
    ] {
        let text = std::fs::read_to_string(root.join(format!("{name}.hex"))).unwrap();
        let tx = decode_tx_exact(&hex(&text)).unwrap().into_value();
        assert_eq!(
            tx.outputs()
                .iter()
                .map(|out| solver(out.script_pubkey().as_bytes()).0)
                .collect::<Vec<_>>(),
            expected,
            "{name}"
        );
    }
}
