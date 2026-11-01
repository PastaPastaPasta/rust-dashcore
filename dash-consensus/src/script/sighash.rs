//! The legacy signature hash (`script/interpreter.cpp:1347-1525`) and Core's
//! `PrecomputedTransactionData` (CS1 §4.8).
//!
//! Every transaction field is written through [`crate::primitives::canonical`]'s field writers
//! (PLAN §7 handling). This module writes only bytes that are not transaction fields: the input and
//! output counts, the scriptCode and the hash type.

use dash_primitives::Transaction;

use super::encoding::{SIGHASH_ANYONECANPAY, SIGHASH_NONE, SIGHASH_SINGLE};
use super::opcodes::OP_CODESEPARATOR;
use super::parse::get_op;
use crate::hash::HashWriter;
use crate::primitives::canonical;
use crate::primitives::{CoreWrite, write_compact_size};

/// `uint256::ONE` in Core's internal byte order: what the SIGHASH_SINGLE bug signs.
const ONE: [u8; 32] = {
    let mut one = [0; 32];
    one[0] = 1;
    one
};

// Core: script/interpreter.cpp:1366 CTransactionSignatureSerializer::SerializeScriptCode
/// The scriptCode with every `OP_CODESEPARATOR` removed.
///
/// The compact size counts the whole script less the separators the parse reached, and the bytes
/// stop where the parse stopped. On a malformed tail that is [`get_op`]'s failure cursor, so the
/// compact size can exceed the bytes written (quirk 39, CS1 §5.2).
pub fn serialize_script_code(script_code: &[u8], w: &mut impl CoreWrite) {
    let mut it = 0;
    let mut code_separators = 0;
    while let Some(op) = get_op(script_code, &mut it) {
        if op.opcode == OP_CODESEPARATOR {
            code_separators += 1;
        }
    }
    write_compact_size((script_code.len() - code_separators) as u64, w);
    let (mut it, mut begin) = (0, 0);
    while let Some(op) = get_op(script_code, &mut it) {
        if op.opcode == OP_CODESEPARATOR {
            w.write(&script_code[begin..it - 1]);
            begin = it;
        }
    }
    if begin != script_code.len() {
        w.write(&script_code[begin..it]);
    }
}

// Core: script/interpreter.cpp:1347 CTransactionSignatureSerializer::Serialize (with
// SerializeInput :1388 and SerializeOutput :1410)
/// `txTmp` of `SignatureHash`: the transaction as the hash type commits to it.
fn write_signed_tx(
    script_code: &[u8],
    tx: &Transaction,
    n_in: usize,
    hash_type: i32,
    w: &mut impl CoreWrite,
) {
    let anyone_can_pay = hash_type & SIGHASH_ANYONECANPAY != 0;
    let hash_single = hash_type & 0x1f == SIGHASH_SINGLE;
    let hash_none = hash_type & 0x1f == SIGHASH_NONE;

    canonical::write_version_and_type(tx, w);
    let n_inputs = if anyone_can_pay {
        1
    } else {
        tx.inputs.len()
    };
    write_compact_size(n_inputs as u64, w);
    for n_input in 0..n_inputs {
        // In case of SIGHASH_ANYONECANPAY, only the input being signed is serialized.
        let n_input = if anyone_can_pay {
            n_in
        } else {
            n_input
        };
        let input = &tx.inputs[n_input];
        let sequence = if n_input != n_in && (hash_single || hash_none) {
            // Let the others update at will: Core's `int{0}`.
            0
        } else {
            input.sequence
        };
        if n_input != n_in {
            // Blank out other inputs' signatures.
            canonical::write_input(&input.prevout, &[], sequence, w);
        } else {
            canonical::write_prevout(&input.prevout, w);
            serialize_script_code(script_code, w);
            canonical::write_sequence(sequence, w);
        }
    }
    let n_outputs = if hash_none {
        0
    } else if hash_single {
        n_in + 1
    } else {
        tx.outputs.len()
    };
    write_compact_size(n_outputs as u64, w);
    for n_output in 0..n_outputs {
        if hash_single && n_output != n_in {
            // Do not lock in the payee at other indices: Core's `CTxOut()`.
            canonical::write_output(-1, &[], w);
        } else {
            let output = &tx.outputs[n_output];
            canonical::write_output(output.value, output.script_pubkey.as_bytes(), w);
        }
    }
    canonical::write_lock_time(tx.lock_time, w);
    canonical::write_extra_payload(tx, w);
}

// Core: script/interpreter.cpp:1512 SignatureHash (SigVersion::BASE)
/// The legacy signature hash of input `n_in` of `tx` for `script_code` and the full 32-bit
/// `hash_type`, in Core's internal byte order. The plain serializer: the line-by-line port, and
/// the reference twin of the precomputed path ([`signature_hash_with`]).
///
/// Core's `amount` and `cache` arguments are ignored by Dash's `SignatureHash` (it has no BIP143),
/// so they are absent. `SIGHASH_SINGLE` with no matching output returns `uint256::ONE`.
///
/// # Panics
///
/// If `n_in >= tx.inputs.len()`: Core asserts it, and every caller guarantees it.
pub fn signature_hash(
    script_code: &[u8],
    tx: &Transaction,
    n_in: usize,
    hash_type: i32,
) -> [u8; 32] {
    assert!(n_in < tx.inputs.len(), "SignatureHash: input index out of range");
    // Check for invalid use of SIGHASH_SINGLE.
    if hash_type & 0x1f == SIGHASH_SINGLE && n_in >= tx.outputs.len() {
        return ONE;
    }
    let mut ss = HashWriter::new();
    write_signed_tx(script_code, tx, n_in, hash_type, &mut ss);
    ss.write(&hash_type.to_le_bytes());
    ss.finish()
}

// Core: script/interpreter.h:118 PrecomputedTransactionData (Init at interpreter.cpp:1471)
/// Core's `PrecomputedTransactionData`: built once per transaction where Core calls
/// `txdata.Init` (`CheckInputScripts`, after the script-execution-cache miss) and shared by the
/// transaction's inputs, which may verify on different threads.
///
/// It borrows the transaction, so it cannot be paired with another one. Dash's `SignatureHash`
/// ignores Core's cache, so this holds only the reference for now; CS1 phase C keeps the per-input
/// sighash midstates here (C3, CS1 §8.3) behind the same API.
#[derive(Clone, Copy)]
pub struct TxSigData<'a> {
    tx: &'a Transaction,
}

impl<'a> TxSigData<'a> {
    pub fn new(tx: &'a Transaction) -> Self {
        Self {
            tx,
        }
    }

    /// The transaction.
    pub fn tx(&self) -> &'a Transaction {
        self.tx
    }
}

/// The signature hash the transaction checker uses: [`signature_hash`] over `txdata`'s
/// transaction. Phase C adds the precomputed path here, checked against [`signature_hash`].
///
/// # Panics
///
/// If `n_in` is out of range, as [`signature_hash`].
pub fn signature_hash_with(
    script_code: &[u8],
    txdata: &TxSigData<'_>,
    n_in: usize,
    hash_type: i32,
) -> [u8; 32] {
    signature_hash(script_code, txdata.tx(), n_in, hash_type)
}

#[cfg(test)]
mod tests {
    use dash_core_vectors::script::reference_serialize_script_code;
    use dash_primitives::{OutPoint, TxHash, TxIn, TxType};
    use dash_types::Numeric as _;

    use super::*;
    use crate::primitives::decode_tx_exact;
    use crate::script::parse::find_and_delete;

    // Core: test/sighash_tests.cpp:24 SignatureHashOld
    /// Bitcoin's original algorithm, Core's second witness: a copy of the transaction with the
    /// other scripts blanked, the scriptCode's separators removed by `FindAndDelete` and put in the
    /// signed input, serialized whole (payload included) with the hash type. Each field of the copy
    /// is written as `ss << txTmp` writes it.
    fn signature_hash_old(
        script_code: &[u8],
        tx: &Transaction,
        n_in: usize,
        hash_type: i32,
    ) -> [u8; 32] {
        if n_in >= tx.inputs.len() {
            return ONE;
        }
        let mut code = script_code.to_vec();
        find_and_delete(&mut code, &[OP_CODESEPARATOR]);
        let base = hash_type & 0x1f;
        let (none, single) = (base == SIGHASH_NONE, base == SIGHASH_SINGLE);
        if single && n_in >= tx.outputs.len() {
            return ONE;
        }
        let inputs: Vec<usize> = if hash_type & SIGHASH_ANYONECANPAY != 0 {
            vec![n_in]
        } else {
            (0..tx.inputs.len()).collect()
        };
        let mut ss = HashWriter::new();
        canonical::write_version_and_type(tx, &mut ss);
        write_compact_size(inputs.len() as u64, &mut ss);
        for i in inputs {
            let input = &tx.inputs[i];
            canonical::write_prevout(&input.prevout, &mut ss);
            let script: &[u8] = if i == n_in {
                &code
            } else {
                &[]
            };
            write_compact_size(script.len() as u64, &mut ss);
            ss.write(script);
            let sequence = if i != n_in && (none || single) {
                0
            } else {
                input.sequence
            };
            canonical::write_sequence(sequence, &mut ss);
        }
        let outputs = if none {
            0
        } else if single {
            n_in + 1
        } else {
            tx.outputs.len()
        };
        write_compact_size(outputs as u64, &mut ss);
        for (i, output) in tx.outputs.iter().take(outputs).enumerate() {
            if single && i < n_in {
                canonical::write_output(-1, &[], &mut ss);
            } else {
                canonical::write_output(output.value, output.script_pubkey.as_bytes(), &mut ss);
            }
        }
        canonical::write_lock_time(tx.lock_time, &mut ss);
        canonical::write_extra_payload(tx, &mut ss);
        ss.write(&hash_type.to_le_bytes());
        ss.finish()
    }

    /// splitmix64.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// The malformed tails of quirk 39 (CS1 §5.2). Every one but the lone `4c` leaves bytes after
    /// `GetOp`'s failure cursor; `4c` ends there, so nothing is dropped.
    const TAILS: [&[u8]; 6] = [
        &[0x05, 0xaa],
        &[0x4c],
        &[0x4c, 0x02, 0xaa],
        &[0x4d, 0x01],
        &[0x4e, 0x01, 0x00],
        &[0x4d, 0x03, 0x00, 0xaa],
    ];

    // Core: test/sighash_tests.cpp:86 RandomScript
    /// Core's nine-opcode scripts, and with `tail` a tail from [`TAILS`] (Core never draws one).
    /// Returns whether `SerializeScriptCode` drops bytes of the script.
    fn random_script(rng: &mut Rng, tail: bool, out: &mut Vec<u8>) -> bool {
        const OPS: [u8; 9] = [0x00, 0x51, 0x52, 0x53, 0xac, 0x63, 0x65, 0x6a, OP_CODESEPARATOR];
        for _ in 0..rng.below(10) {
            out.push(OPS[rng.below(9) as usize]);
        }
        if !tail {
            return false;
        }
        let tail = TAILS[rng.below(TAILS.len() as u64) as usize];
        out.extend_from_slice(tail);
        tail != [0x4c]
    }

    fn script_field(rng: &mut Rng, out: &mut Vec<u8>) {
        let mut script = Vec::new();
        random_script(rng, false, &mut script);
        write_compact_size(script.len() as u64, out);
        out.extend(script);
    }

    // Core: test/sighash_tests.cpp:94 RandomTransaction
    /// Core's random transaction (versions 1 and 2), and also version 3 of a random special type
    /// with a random payload, which Core's generator never makes.
    fn random_transaction(rng: &mut Rng, single: bool) -> Transaction {
        let mut out = Vec::new();
        let special = rng.below(3) == 0;
        let version: i16 = if special {
            3
        } else {
            rng.below(2) as i16 + 1
        };
        let tx_type: u16 = if special {
            1 + rng.below(12) as u16
        } else {
            0
        };
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&tx_type.to_le_bytes());
        let lock_time = if rng.below(2) == 0 {
            rng.next() as u32
        } else {
            0
        };
        let ins = rng.below(4) + 1;
        let outs = if single {
            ins
        } else {
            rng.below(4) + 1
        };
        write_compact_size(ins, &mut out);
        for _ in 0..ins {
            for _ in 0..4 {
                out.extend_from_slice(&rng.next().to_le_bytes());
            }
            out.extend_from_slice(&(rng.below(4) as u32).to_le_bytes());
            script_field(rng, &mut out);
            let sequence = if rng.below(2) == 0 {
                rng.next() as u32
            } else {
                u32::MAX
            };
            out.extend_from_slice(&sequence.to_le_bytes());
        }
        write_compact_size(outs, &mut out);
        for _ in 0..outs {
            out.extend_from_slice(&(rng.below(21_000_000 * 100_000_000 + 1) as i64).to_le_bytes());
            script_field(rng, &mut out);
        }
        out.extend_from_slice(&lock_time.to_le_bytes());
        if special {
            let len = rng.below(80);
            write_compact_size(len, &mut out);
            out.extend((0..len).map(|_| rng.next() as u8));
        }
        match decode_tx_exact(&out) {
            Ok(tx) => tx.into_value().into_raw(),
            Err(_) => panic!("the generator writes decodable transactions"),
        }
    }

    // Core: test/sighash_tests.cpp:120 sighash_test
    /// `signature_hash` against `signature_hash_old`: equal on Core's well-formed scriptCodes, and
    /// different on a malformed tail that `SerializeScriptCode` truncates while `FindAndDelete`
    /// keeps it whole (quirk 39), unless both sign the SINGLE bug's `ONE`.
    #[test]
    fn sighash_test() {
        let mut rng = Rng(0x5167_4a54);
        let (mut agreed, mut differed) = (0, 0);
        for _ in 0..50_000 {
            let hash_type = rng.next() as u32 as i32;
            let tx = random_transaction(&mut rng, hash_type & 0x1f == SIGHASH_SINGLE);
            let mut script_code = Vec::new();
            let tail = rng.below(4) == 0;
            let dropped = random_script(&mut rng, tail, &mut script_code);
            let n_in = rng.below(tx.inputs.len() as u64) as usize;
            let (new, old) = (
                signature_hash(&script_code, &tx, n_in, hash_type),
                signature_hash_old(&script_code, &tx, n_in, hash_type),
            );
            if dropped && new != ONE {
                assert_ne!(new, old, "a malformed tail signs the same preimage");
                differed += 1;
            } else {
                assert_eq!(new, old);
                agreed += 1;
            }
        }
        assert!(agreed > 30_000 && differed > 5_000, "{agreed} agreed, {differed} differed");
    }

    #[test]
    fn script_code_matches_the_reference_port_on_truncated_tails() {
        for script in [
            &[0x51, 0xab, 0x52][..],
            &[0xab, 0xab],
            &[0x05, 0xaa],
            &[0xab, 0x4d, 0x01],
            &[0x51, 0x4e, 0x01, 0x02],
            &[0x4c, 0x03, 0xab],
            &[],
        ] {
            let mut ours = Vec::new();
            serialize_script_code(script, &mut ours);
            assert_eq!(ours, reference_serialize_script_code(script));
        }
    }

    #[test]
    fn single_without_a_matching_output_signs_one() {
        let input = TxIn {
            prevout: OutPoint {
                hash: TxHash::from_lendian([7; 32]),
                index: 0,
            },
            script_sig: Default::default(),
            sequence: u32::MAX,
        };
        let tx = Transaction {
            version: 1,
            tx_type: TxType::new(0),
            inputs: vec![input.clone(), input],
            outputs: vec![],
            lock_time: 0,
            extra_payload: vec![],
        };
        assert_eq!(signature_hash(&[], &tx, 1, SIGHASH_SINGLE), ONE);
        assert_eq!(signature_hash(&[], &tx, 1, SIGHASH_SINGLE | 0x20), ONE);
        assert_ne!(signature_hash(&[], &tx, 1, 1), ONE);
    }
}
