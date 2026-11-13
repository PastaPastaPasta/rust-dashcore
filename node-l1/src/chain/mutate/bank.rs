//! `chain-v2`'s builder bank (O2b-4 §3a.2, D55; slice 4e1): the builders the census overlay
//! requires that [`OPERATORS`] lacks, reached by `MutateX`/`ProposeX` slot. Append only: a
//! slot's builder never moves or goes, so an append changes only inputs that selected a slot
//! no builder filled (`unknown_slot`, G-ENC-4).
//!
//! Slots 0-12 are diff_chain carriers of L3 group operators ([`L3_OPERATORS`]): each builds what
//! the L3 operator's rejecting variant builds at `Site::Reg`, fixed, with no L3 trial. Slots
//! 13-39 each plant one script error Core raises in a block (`GetBlockScriptFlags`'s P2SH,
//! DERSIG, CLTV, CSV and NULLDUMMY; DIP0020's opcodes unconditional): one funding transaction
//! whose output holds the error's scriptPubKey, spent in the same block by its scriptSig.

use super::*;
use crate::chain::build::{OP_1, OP_CHECKMULTISIG, OP_CLTV, OP_DROP, OP_EQUALVERIFY, OP_VERIFY};

const OP_2: u8 = 0x52;
const OP_3: u8 = 0x53;
const OP_1NEGATE: u8 = 0x4f;
const OP_RESERVED: u8 = 0x50;
const OP_NOP: u8 = 0x61;
const OP_ENDIF: u8 = 0x68;
const OP_FROMALTSTACK: u8 = 0x6c;
const OP_SPLIT: u8 = 0x7f;
const OP_NUM2BIN: u8 = 0x80;
const OP_BIN2NUM: u8 = 0x81;
const OP_AND: u8 = 0x84;
const OP_1ADD: u8 = 0x8b;
const OP_2MUL: u8 = 0x8d;
const OP_DIV: u8 = 0x96;
const OP_MOD: u8 = 0x97;
const OP_NUMEQUALVERIFY: u8 = 0x9d;
const OP_CHECKSIGVERIFY: u8 = 0xad;
const OP_CHECKMULTISIGVERIFY: u8 = 0xaf;
const OP_CHECKDATASIGVERIFY: u8 = 0xbb;
const OP_PUSHDATA2: u8 = 0x4d;

/// The operators that build from the parent block's transactions ([`Cx::prior`]): a block whose
/// parent has none is not mutated.
pub const NEEDS_PRIOR: &[&str] = &["x_bip30"];

/// One `@Reg` variant expecting `$reason` at `$occurrence` in every mode and at every `-par`.
macro_rules! bank_reg {
    ($reason:expr, $occurrence:expr) => {
        &[Variant {
            name: "reg",
            sites: REG,
            expect: |_, _, _| Expect::Reject {
                reason: $reason,
                occurrence: $occurrence,
            },
            carrier: Carrier::OpReturn,
            full_only: false,
        }]
    };
}

/// A script error's builder: inline (`-par=1`) Core names the error, queued (`-par=4`) the
/// queue's verdict (v:2791) names none.
macro_rules! script_error {
    ($name:expr, $error:expr, $sig:expr, $pubkey:expr) => {
        Operator {
            name: $name,
            rejects: &[SCRIPT, "block-validation-failed"],
            stage: Stage::Connect,
            par: false,
            apply: |p, cx| raw_spend(p, cx, $sig, $pubkey),
            variants: &[Variant {
                name: "reg",
                sites: REG,
                expect: |_, par, _| match par {
                    Par::One => Expect::Reject {
                        reason: concat!("mandatory-script-verify-flag-failed (", $error, ")"),
                        occurrence: CIS_SCRIPT,
                    },
                    Par::Four => script_reject(par),
                },
                carrier: Carrier::OpReturn,
                full_only: false,
            }],
        }
    };
}

/// Coin 1's value locked behind `pubkey` in one transaction, spent by `sig` in the next.
fn raw_spend(p: &mut BlockPlan, cx: &Cx<'_>, sig: Vec<u8>, pubkey: Vec<u8>) {
    let mut fund = spend(cx, 1, 1);
    fund.outputs[0].1 = pubkey;
    let value = fund.outputs[0].0;
    let mut tx = TxPlan::simple(
        vec![Input::new((fund.txid(), 0), sig)],
        vec![(value - 1_000, Lock::True.script_pubkey())],
    );
    cx.carry(&mut tx);
    p.txs.push(fund);
    p.txs.push(tx);
}

/// A 33-byte compressed-looking key: only STRICTENC checks a key's encoding.
fn key() -> Vec<u8> {
    push(&[0x02; 33])
}

/// `n` bytes pushed with `OP_PUSHDATA2`.
fn pushdata2(n: u16) -> Vec<u8> {
    [vec![OP_PUSHDATA2], n.to_le_bytes().to_vec(), vec![0x01; usize::from(n)]].concat()
}

/// The block grown to `size` bytes on a DIP0001 chain: a chain of spends from coin 0, each
/// under `MAX_STANDARD_TX_SIZE`'s 100 kB, the last filled to the byte (L3's `sized_block` at
/// `Site::Reg`, but the chain's coinbase carries its tag already).
fn sized_block_dip1(p: &mut BlockPlan, cx: &Cx<'_>, size: usize) {
    let cap = 100_000;
    let target = size;
    let mut tx = spend(cx, 0, 1);
    while target - block_size(p) > cap {
        let full = fill_tx(&tx, cap - 1_000).expect("a filled link");
        let (prevout, value) = ((full.txid(), 0), full.outputs[0].0);
        p.txs.push(full);
        tx = TxPlan::simple(
            vec![Input::new(prevout, Lock::True.script_sig())],
            vec![(value - 1_000, Lock::True.script_pubkey())],
        );
        cx.carry(&mut tx);
    }
    p.txs.push(tx);
    *p = fill_block(p, p.txs.len() - 1, target).expect("a block filled to the byte");
}

pub const BANK: &[Operator] = &[
    // 0
    Operator {
        name: "x_block_size",
        rejects: &["bad-blk-length"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| sized_block_dip1(p, cx, 2_000_001),
        variants: bank_reg!("bad-blk-length", CB_LENGTH),
    },
    // 1
    Operator {
        name: "x_sigops_check",
        rejects: &["bad-blk-sigops"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| legacy_block(p, cx, 40_001),
        variants: bank_reg!("bad-blk-sigops", CB_SIGOPS),
    },
    // 2
    Operator {
        name: "x_tx_size_100k",
        rejects: &["bad-txns-oversize"],
        stage: Stage::Contextual,
        par: false,
        apply: |p, cx| sized_tx(p, cx, 100_001),
        variants: bank_reg!("bad-txns-oversize", CCT_OVERSIZE),
    },
    // 3
    Operator {
        name: "x_bip30",
        rejects: &["bad-txns-BIP30"],
        stage: Stage::Connect,
        par: false,
        // The first of the parent's transactions the generator offers (`bip30_priors`: one whose
        // spendable outputs the parent leaves unspent) again: ConnectBlock's BIP30 check finds
        // its outputs in the coins view and refuses it before any inputs are looked up.
        apply: |p, cx| {
            if let Some(tx) = cx.prior.first() {
                p.txs.push(tx.clone());
            }
        },
        variants: bank_reg!("bad-txns-BIP30", CONN_BIP30),
    },
    // 4
    Operator {
        name: "x_sigops_connect",
        rejects: &["bad-blk-sigops"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            legacy_block(p, cx, 40_000 - u32::from(SIGOPS_MAX) + 1);
            locked_spend(p, cx, Lock::Sigops(SIGOPS_MAX));
        },
        variants: bank_reg!("bad-blk-sigops", CONN_SIGOPS),
    },
    // 5
    Operator {
        name: "x_vin_empty",
        rejects: &["bad-txns-vin-empty"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = TxPlan::simple(Vec::new(), vec![(0, Lock::True.script_pubkey())]);
            cx.carry(&mut tx);
            p.txs.push(tx);
        },
        variants: bank_reg!("bad-txns-vin-empty", CT_VIN_EMPTY),
    },
    // 6
    Operator {
        name: "x_vout_empty",
        rejects: &["bad-txns-vout-empty"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.outputs.clear();
            p.txs.push(tx);
        },
        variants: bank_reg!("bad-txns-vout-empty", CT_VOUT_EMPTY),
    },
    // 7
    Operator {
        name: "x_tx_size_1m",
        rejects: &["bad-txns-oversize"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| sized_tx(p, cx, 1_000_001),
        variants: bank_reg!("bad-txns-oversize", CT_OVERSIZE),
    },
    // 8
    Operator {
        name: "x_payload_size",
        rejects: &["bad-txns-payload-oversize"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.version = 3;
            tx.tx_type = 1;
            tx.extra_payload = vec![0; 10_001];
            p.txs.push(tx);
        },
        variants: bank_reg!("bad-txns-payload-oversize", CT_PAYLOAD),
    },
    // 9
    Operator {
        name: "x_txouttotal",
        rejects: &["bad-txns-txouttotal-toolarge"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            let half = MAX_MONEY / 2;
            tx.outputs = vec![
                (half, Lock::True.script_pubkey()),
                (MAX_MONEY - half + 1, Lock::True.script_pubkey()),
            ];
            p.txs.push(tx);
        },
        variants: bank_reg!("bad-txns-txouttotal-toolarge", CT_TXOUTTOTAL),
    },
    // 10
    Operator {
        name: "x_cb_length",
        rejects: &["bad-cb-length"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let height = script_num(i64::from(cx.height));
            p.txs[0].inputs[0].script_sig =
                [height.clone(), vec![OP_0; 101 - height.len()]].concat();
            p.txs.push(spend(cx, 0, 1));
        },
        variants: bank_reg!("bad-cb-length", CT_CB_LENGTH),
    },
    // 11
    Operator {
        name: "x_prevout_null",
        rejects: &["bad-txns-prevout-null"],
        stage: Stage::Check,
        par: false,
        apply: |p, cx| {
            let mut tx = spend(cx, 0, 1);
            tx.inputs.push(Input::new(([0; 32], u32::MAX), Vec::new()));
            p.txs.push(tx);
        },
        variants: bank_reg!("bad-txns-prevout-null", CT_PREVOUT_NULL),
    },
    // 12
    Operator {
        name: "x_cb_type5",
        rejects: &["bad-cbtx-payload"],
        stage: Stage::Connect,
        par: false,
        apply: |p, cx| {
            let cb = &mut p.txs[0];
            (cb.version, cb.tx_type) = (1, TRANSACTION_COINBASE);
            p.txs.push(spend(cx, 0, 1));
        },
        variants: bank_reg!("bad-cbtx-payload", STX_CB_PAYLOAD),
    },
    // 13
    script_error!(
        "se_verify",
        "Script failed an OP_VERIFY operation",
        Vec::new(),
        vec![OP_0, OP_VERIFY]
    ),
    // 14
    script_error!(
        "se_equalverify",
        "Script failed an OP_EQUALVERIFY operation",
        Vec::new(),
        vec![OP_1, OP_2, OP_EQUALVERIFY, OP_1]
    ),
    // 15
    script_error!(
        "se_checkmultisigverify",
        "Script failed an OP_CHECKMULTISIGVERIFY operation",
        Vec::new(),
        [vec![OP_0, OP_0, OP_1], key(), vec![OP_1, OP_CHECKMULTISIGVERIFY, OP_1]].concat()
    ),
    // 16
    script_error!(
        "se_checksigverify",
        "Script failed an OP_CHECKSIGVERIFY operation",
        Vec::new(),
        [vec![OP_0], key(), vec![OP_CHECKSIGVERIFY, OP_1]].concat()
    ),
    // 17
    script_error!(
        "se_checkdatasigverify",
        "Script failed an OP_CHECKDATASIGVERIFY operation",
        Vec::new(),
        [vec![OP_0, OP_0], key(), vec![OP_CHECKDATASIGVERIFY, OP_1]].concat()
    ),
    // 18
    script_error!(
        "se_numequalverify",
        "Script failed an OP_NUMEQUALVERIFY operation",
        Vec::new(),
        vec![OP_1, OP_2, OP_NUMEQUALVERIFY, OP_1]
    ),
    // 19: the oversized script is the spend's scriptSig, which EvalScript sizes before it runs. A
    // scriptPubKey above MAX_SCRIPT_SIZE is unspendable, so it never becomes a coin to spend.
    script_error!("se_script_size", "Script is too big", pushdata2(520).repeat(20), vec![OP_1]),
    // 20
    script_error!(
        "se_push_size",
        "Push value size limit exceeded",
        Vec::new(),
        [pushdata2(521), vec![OP_1]].concat()
    ),
    // 21
    script_error!(
        "se_op_count",
        "Operation limit exceeded",
        Vec::new(),
        [vec![OP_NOP; 202], vec![OP_1]].concat()
    ),
    // 22
    script_error!("se_stack_size", "Stack size limit exceeded", Vec::new(), vec![OP_1; 1_001]),
    // 23
    script_error!(
        "se_sig_count",
        "Signature count negative or greater than pubkey count",
        Vec::new(),
        vec![OP_0, OP_0, OP_1, OP_0, OP_CHECKMULTISIG]
    ),
    // 24
    script_error!(
        "se_pubkey_count",
        "Pubkey count negative or limit exceeded",
        Vec::new(),
        [push(&[21]), vec![OP_CHECKMULTISIG]].concat()
    ),
    // 25
    script_error!(
        "se_bad_opcode",
        "Opcode missing or not understood",
        Vec::new(),
        vec![OP_1, OP_RESERVED]
    ),
    // 26
    script_error!(
        "se_disabled_opcode",
        "Attempted to use a disabled opcode",
        Vec::new(),
        vec![OP_1, OP_2MUL]
    ),
    // 27
    script_error!(
        "se_invalid_stack_operation",
        "Operation not valid with the current stack size",
        Vec::new(),
        vec![OP_DROP]
    ),
    // 28
    script_error!(
        "se_invalid_altstack_operation",
        "Operation not valid with the current altstack size",
        Vec::new(),
        vec![OP_FROMALTSTACK]
    ),
    // 29: OP_RETURN after a push, as a scriptPubKey that starts with it is unspendable and never
    // becomes a coin to spend.
    script_error!("se_op_return", "OP_RETURN was encountered", Vec::new(), vec![OP_1, OP_RETURN]),
    // 30
    script_error!(
        "se_unbalanced_conditional",
        "Invalid OP_IF construction",
        Vec::new(),
        vec![OP_ENDIF]
    ),
    // 31
    script_error!(
        "se_split_range",
        "Invalid OP_SPLIT range",
        Vec::new(),
        [push(&[1, 2]), vec![OP_3, OP_SPLIT]].concat()
    ),
    // 32
    script_error!(
        "se_invalid_operand_size",
        "Invalid operand size",
        Vec::new(),
        [push(&[1]), push(&[1, 2]), vec![OP_AND]].concat()
    ),
    // 33
    script_error!(
        "se_invalid_number_range",
        "Given operand is not a number within the valid range [-2^31...2^31]",
        Vec::new(),
        [push(&[1; 5]), vec![OP_BIN2NUM]].concat()
    ),
    // 34
    script_error!(
        "se_impossible_encoding",
        "The requested encoding is impossible to satisfy",
        Vec::new(),
        [push(&[1, 2]), vec![OP_1, OP_NUM2BIN]].concat()
    ),
    // 35
    script_error!("se_div_by_zero", "Division by zero error", Vec::new(), vec![OP_1, OP_0, OP_DIV]),
    // 36
    script_error!("se_mod_by_zero", "Modulo by zero error", Vec::new(), vec![OP_1, OP_0, OP_MOD]),
    // 37
    script_error!(
        "se_negative_locktime",
        "Negative locktime",
        Vec::new(),
        vec![OP_1NEGATE, OP_CLTV]
    ),
    // 38
    script_error!(
        "se_sig_pushonly",
        "Only push operators allowed in signatures",
        [vec![OP_NOP], Lock::P2shTrue.script_sig()].concat(),
        Lock::P2shTrue.script_pubkey()
    ),
    // 39
    script_error!(
        "se_unknown_error",
        "unknown error",
        Vec::new(),
        [push(&[1; 5]), vec![OP_1ADD]].concat()
    ),
];

#[cfg(test)]
mod tests {
    use dash_consensus::script::sigops::{legacy_sigop_count, script_sigop_count_with_sig};
    use dash_consensus::script::{NoSignatureChecker, ScriptFlags, verify_script};

    use super::*;

    fn coin(n: u8) -> Coin {
        Coin {
            outpoint: ([n; 32], 0),
            value: 1_000_000,
            lock: Lock::True,
            height: 1,
            coinbase: false,
        }
    }

    fn built(op: &Operator, prior: &[TxPlan]) -> BlockPlan {
        let coins = [coin(1), coin(2), coin(3)];
        let cx = Cx {
            height: 300,
            mtp: 1_000,
            mocktime: 2_000,
            coins: &coins,
            immature: None,
            l3: None,
            prior,
        };
        let mut plan = BlockPlan {
            prev: [0; 32],
            version: 0x2000_0000,
            time: 1_500,
            bits: REGTEST_BITS,
            txs: vec![coinbase(300, 5_000, 1)],
            merkle_override: None,
        };
        (op.apply)(&mut plan, &cx);
        plan
    }

    fn op(name: &str) -> &'static Operator {
        BANK.iter().find(|o| o.name == name).expect("a bank operator")
    }

    /// The bank's ids are new (they share the catalogue's namespace), each has its one `@Reg`
    /// variant, and it fits the slots.
    #[test]
    fn the_bank_is_forty_new_ids_with_one_variant_each() {
        assert_eq!(BANK.len(), 40);
        assert!(BANK.len() <= usize::from(BANK_CAPACITY));
        for o in BANK {
            assert!(catalogue().all(|c| c.name != o.name), "{} is a catalogue id", o.name);
            assert_eq!(o.variants.len(), 1, "{}", o.name);
            assert_eq!(o.variants[0].sites, REG, "{}", o.name);
        }
    }

    /// Slots 0-12 build exactly the number or shape their occurrence needs on a `diff_chain`
    /// block (its coinbase carries its tag already).
    #[test]
    fn the_carriers_build_their_numbers() {
        let legacy =
            |p: &BlockPlan| -> u32 { p.txs.iter().map(|t| legacy_sigop_count(t.tx().raw())).sum() };
        let p = built(op("x_block_size"), &[]);
        assert_eq!(p.build().bytes.len(), 2_000_001);
        assert!(p.txs.iter().all(|t| tx_size(t) <= 100_000));
        assert_eq!(legacy(&built(op("x_sigops_check"), &[])), 40_001);
        let p = built(op("x_sigops_connect"), &[]);
        let (fund, spend) = (&p.txs[p.txs.len() - 2], p.txs.last().expect("a spend"));
        let p2sh = script_sigop_count_with_sig(&fund.outputs[0].1, &spend.inputs[0].script_sig);
        assert_eq!((legacy(&p), p2sh), (40_000 - 13 + 1, 13));
        assert_eq!(tx_size(built(op("x_tx_size_100k"), &[]).txs.last().expect("a tx")), 100_001);
        assert_eq!(tx_size(built(op("x_tx_size_1m"), &[]).txs.last().expect("a tx")), 1_000_001);
        let p = built(op("x_payload_size"), &[]);
        let tx = p.txs.last().expect("a tx");
        assert_eq!((tx.version, tx.tx_type, tx.extra_payload.len()), (3, 1, 10_001));
        let p = built(op("x_txouttotal"), &[]);
        assert_eq!(
            p.txs.last().expect("a tx").outputs.iter().map(|o| o.0).sum::<i64>(),
            MAX_MONEY + 1
        );
        assert_eq!(built(op("x_cb_length"), &[]).txs[0].inputs[0].script_sig.len(), 101);
        assert!(built(op("x_vin_empty"), &[]).txs.last().expect("a tx").inputs.is_empty());
        assert!(built(op("x_vout_empty"), &[]).txs.last().expect("a tx").outputs.is_empty());
        let p = built(op("x_prevout_null"), &[]);
        assert_eq!(p.txs.last().expect("a tx").inputs[1].prevout, ([0; 32], u32::MAX));
        let p = built(op("x_cb_type5"), &[]);
        assert_eq!((p.txs[0].version, p.txs[0].tx_type), (1, TRANSACTION_COINBASE));
    }

    /// `x_bip30` repeats the parent's first transaction, byte for byte, and adds nothing when
    /// the parent has none (the generator then leaves the block plain: `NEEDS_PRIOR`).
    #[test]
    fn bip30_repeats_the_parents_transaction() {
        let prior = TxPlan::simple(
            vec![Input::new(([7; 32], 0), Vec::new())],
            vec![(5_000, Lock::True.script_pubkey())],
        );
        let p = built(op("x_bip30"), std::slice::from_ref(&prior));
        assert_eq!(p.txs.last().expect("a tx").txid(), prior.txid());
        assert_eq!(built(op("x_bip30"), &[]).txs.len(), 1);
        assert_eq!(NEEDS_PRIOR, &["x_bip30"]);
    }

    /// Slots 13-39 each fail with their error under a block's flags (dash-consensus's
    /// interpreter, `GetBlockScriptFlags`'s five), the error string being the one the variant
    /// expects at `-par=1`, and the funding output itself passes `CheckTransaction`'s size.
    #[test]
    fn script_builders_fail_with_their_error() {
        let flags = ScriptFlags::P2SH
            .union(ScriptFlags::DERSIG)
            .union(ScriptFlags::CHECKLOCKTIMEVERIFY)
            .union(ScriptFlags::CHECKSEQUENCEVERIFY)
            .union(ScriptFlags::NULLDUMMY);
        let scripts: Vec<&Operator> = BANK.iter().filter(|o| o.name.starts_with("se_")).collect();
        assert_eq!(scripts.len(), 27);
        let mut errors = Vec::new();
        for o in scripts {
            let p = built(o, &[]);
            let (fund, spend) = (&p.txs[p.txs.len() - 2], p.txs.last().expect("a spend"));
            assert_eq!(spend.inputs[0].prevout, (fund.txid(), 0), "{}", o.name);
            let got = verify_script(
                &spend.inputs[0].script_sig,
                &fund.outputs[0].1,
                flags,
                &NoSignatureChecker,
            )
            .expect_err(o.name);
            let Expect::Reject {
                reason,
                occurrence,
            } = (o.variants[0].expect)(Mode::Submit, Par::One, Site::Reg)
            else {
                panic!("{}: not a reject", o.name);
            };
            assert_eq!(
                reason,
                format!("mandatory-script-verify-flag-failed ({got})"),
                "{}",
                o.name
            );
            assert_eq!(occurrence, CIS_SCRIPT);
            assert_eq!(
                (o.variants[0].expect)(Mode::Submit, Par::Four, Site::Reg),
                script_reject(Par::Four),
                "{}",
                o.name
            );
            assert!(script_failure(o), "{}", o.name);
            errors.push(got.to_string());
        }
        errors.sort();
        errors.dedup();
        assert_eq!(errors.len(), 27, "each builder its own error");
    }
}
