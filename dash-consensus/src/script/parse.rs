//! Core's script parsing helpers (`script/script.{h,cpp}`) and `FindAndDelete`
//! (`script/interpreter.cpp`).

use super::opcodes::*;

/// One instruction as Core's `GetScriptOp` returns it: the opcode and its push data.
///
/// `data` is empty for every opcode that is not a push (`0x00..=0x4e`), including `OP_1NEGATE`,
/// `OP_RESERVED` and `OP_1..=OP_16`: Core clears the data vector on every call, which
/// [`super::script_sigop_count_with_sig`] observes.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Op<'a> {
    pub opcode: u8,
    pub data: &'a [u8],
}

// Core: script/script.cpp:259 GetScriptOp
/// Reads the instruction at `*pc`.
///
/// On success `*pc` is past the whole instruction. On failure (`None`) it is left exactly where
/// Core's iterator is left, which `SerializeScriptCode` observes (quirk 39, CS1 §4.3):
/// - at the end of the script: unchanged;
/// - otherwise the opcode byte is consumed; a `PUSHDATA1/2/4` whose 1/2/4-byte size field is
///   incomplete stops right after the opcode (Core reads none of the field); a push whose data is
///   short stops after the size field.
pub fn get_op<'a>(script: &'a [u8], pc: &mut usize) -> Option<Op<'a>> {
    if *pc >= script.len() {
        return None;
    }
    let opcode = script[*pc];
    *pc += 1;
    if opcode > OP_PUSHDATA4 {
        return Some(Op {
            opcode,
            data: &[],
        });
    }
    let size = if opcode < OP_PUSHDATA1 {
        opcode as usize
    } else {
        let width = match opcode {
            OP_PUSHDATA1 => 1,
            OP_PUSHDATA2 => 2,
            _ => 4,
        };
        let field = script.get(*pc..*pc + width)?;
        *pc += width;
        // ReadLE16 / ReadLE32 into an unsigned int.
        field.iter().rev().fold(0u32, |n, b| n << 8 | u32::from(*b)) as usize
    };
    if script.len() - *pc < size {
        return None;
    }
    let data = &script[*pc..*pc + size];
    *pc += size;
    Some(Op {
        opcode,
        data,
    })
}

/// The instructions of `script` in order, stopping for good at the end or at the first
/// instruction [`get_op`] cannot read. Use [`get_op`] directly where a parse failure matters.
pub fn instructions(script: &[u8]) -> impl Iterator<Item = Op<'_>> {
    let mut pc = 0;
    // Fused: after a failure `pc` can sit inside the failed push (Core's cursor), and reading on
    // from there would take its length bytes for instructions.
    core::iter::from_fn(move || get_op(script, &mut pc)).fuse()
}

// Core: script/script.cpp:237 CScript::IsPushOnly
/// Every instruction parses and none is above `OP_16`. `OP_RESERVED` counts as a push here (its
/// execution fails anyway).
pub fn is_push_only(script: &[u8]) -> bool {
    let mut pc = 0;
    while pc < script.len() {
        match get_op(script, &mut pc) {
            Some(op) if op.opcode <= OP_16 => {}
            _ => return false,
        }
    }
    true
}

// Core: script/script.cpp:213 CScript::IsPayToScriptHash
/// Exactly `OP_HASH160 <20 bytes> OP_EQUAL` (`a9 14 <20> 87`).
pub fn is_pay_to_script_hash(script: &[u8]) -> bool {
    script.len() == 23 && script[0] == OP_HASH160 && script[1] == 0x14 && script[22] == OP_EQUAL
}

/// `OP_HASH160 <hash> OP_EQUAL`, for this crate's tests.
#[cfg(test)]
pub(crate) fn p2sh_script_pubkey(hash: &[u8; 20]) -> Vec<u8> {
    let mut spk = vec![OP_HASH160, 0x14];
    spk.extend_from_slice(hash);
    spk.push(OP_EQUAL);
    spk
}

// Core: script/script.cpp:386 CheckMinimalPush
/// The push of `data` used the smallest push opcode for it. Only called for `opcode` in
/// `0x00..=OP_PUSHDATA4`. Core asserts that, and Core's asserts are always on (it cannot be built
/// without them); here it is a debug assertion, because the interpreter only calls this for push
/// opcodes.
pub fn check_minimal_push(data: &[u8], opcode: u8) -> bool {
    debug_assert!(opcode <= OP_PUSHDATA4);
    let len = data.len();
    if len == 0 {
        // Should have used OP_0.
        opcode == OP_0
    } else if len == 1 && (1..=16).contains(&data[0]) {
        // Should have used OP_1 .. OP_16.
        false
    } else if len == 1 && data[0] == 0x81 {
        // Should have used OP_1NEGATE.
        false
    } else if len <= 75 {
        // Must have used a direct push (opcode indicating number of bytes pushed + those bytes).
        usize::from(opcode) == len
    } else if len <= 255 {
        // Must have used OP_PUSHDATA.
        opcode == OP_PUSHDATA1
    } else if len <= 65535 {
        // Must have used OP_PUSHDATA2.
        opcode == OP_PUSHDATA2
    } else {
        true
    }
}

// Core: script/script.h:448 CScript::operator<<(const std::vector<unsigned char>&)
/// Appends a push of `data` with the smallest push opcode for its length. Never uses `OP_N`
/// (one byte `01` is pushed as `01 01`), and the empty vector is `OP_0`.
pub fn push_data(script: &mut Vec<u8>, data: &[u8]) {
    let len = data.len();
    if len < usize::from(OP_PUSHDATA1) {
        script.push(len as u8);
    } else if len <= 0xff {
        script.push(OP_PUSHDATA1);
        script.push(len as u8);
    } else if len <= 0xffff {
        script.push(OP_PUSHDATA2);
        script.extend_from_slice(&(len as u16).to_le_bytes());
    } else {
        script.push(OP_PUSHDATA4);
        script.extend_from_slice(&(len as u32).to_le_bytes());
    }
    script.extend_from_slice(data);
}

// Core: script/interpreter.cpp:224 FindAndDelete
/// Deletes occurrences of `pattern` from `script` and returns how many were deleted. The script is
/// rewritten only if the count is above zero; an empty pattern deletes nothing.
///
/// This is Core's single cursor pass, not a search (CS1 §4.3):
/// - a match is tested only at the cursor, and adjacent matches are consumed one after another, so
///   after a match the cursor need not be on a boundary of the original parse (`01 01` minus `01`
///   deletes both bytes);
/// - after the matches the cursor moves by one [`get_op`]; when that fails the pass ends and the
///   rest of the script is kept verbatim;
/// - it is not idempotent in general (`51 51 52 52` minus `51 52` leaves `51 52`), so it must
///   never be repeated until stable. For a pattern that is exactly one complete instruction, which
///   every consensus call's pattern is, a second call finds nothing.
pub fn find_and_delete(script: &mut Vec<u8>, pattern: &[u8]) -> usize {
    if pattern.is_empty() {
        return 0;
    }
    let mut found = 0;
    let mut result = Vec::new();
    let (mut pc, mut pc2) = (0, 0);
    loop {
        result.extend_from_slice(&script[pc2..pc]);
        while script.len() - pc >= pattern.len() && script[pc..pc + pattern.len()] == *pattern {
            pc += pattern.len();
            found += 1;
        }
        pc2 = pc;
        // `pc2` stays at the last cursor when this fails: the tail from there is kept.
        if get_op(script, &mut pc).is_none() {
            break;
        }
    }
    if found > 0 {
        result.extend_from_slice(&script[pc2..]);
        *script = result;
    }
    found
}

#[cfg(test)]
mod tests {
    use dash_core_vectors::script::{push_data as reference_push_data, reference_get_op};
    use proptest::prelude::*;

    use super::*;

    /// Scripts biased to pushes, truncated pushes and the opcodes the parser treats specially.
    fn script_strategy() -> impl Strategy<Value = Vec<u8>> {
        let piece = prop_oneof![
            any::<u8>().prop_map(|b| vec![b]),
            (0u8..=0x4b, proptest::collection::vec(any::<u8>(), 0..80)).prop_map(|(n, d)| {
                let mut v = vec![n];
                v.extend(d.iter().take(n as usize));
                v
            }),
            (
                prop_oneof![Just(OP_PUSHDATA1), Just(OP_PUSHDATA2), Just(OP_PUSHDATA4)],
                proptest::collection::vec(any::<u8>(), 0..8)
            )
                .prop_map(|(op, rest)| {
                    let mut v = vec![op];
                    v.extend(rest);
                    v
                }),
            proptest::collection::vec(any::<u8>(), 0..300).prop_map(|d| {
                let mut v = Vec::new();
                push_data(&mut v, &d);
                v
            }),
        ];
        proptest::collection::vec(piece, 0..12).prop_map(|p| p.concat())
    }

    /// One complete instruction: a non-push opcode, a canonical push, or a push through a
    /// PUSHDATA opcode wider than needed.
    fn one_instruction() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            (OP_1NEGATE..=u8::MAX).prop_map(|op| vec![op]),
            proptest::collection::vec(any::<u8>(), 0..80).prop_map(|d| {
                let mut v = Vec::new();
                push_data(&mut v, &d);
                v
            }),
            (
                prop_oneof![
                    Just((OP_PUSHDATA1, 1usize)),
                    Just((OP_PUSHDATA2, 2)),
                    Just((OP_PUSHDATA4, 4))
                ],
                proptest::collection::vec(any::<u8>(), 0..10)
            )
                .prop_map(|((op, width), d)| {
                    let mut v = vec![op];
                    v.extend_from_slice(&(d.len() as u32).to_le_bytes()[..width]);
                    v.extend(d);
                    v
                }),
        ]
    }

    /// Lengths around every push-opcode boundary.
    fn push_len() -> impl Strategy<Value = usize> {
        prop_oneof![0usize..=80, 250usize..=260, 65_530usize..=65_540]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn push_data_matches_the_reference_port(data in push_len().prop_flat_map(|n| proptest::collection::vec(any::<u8>(), n))) {
            let (mut ours, mut theirs) = (vec![0xab], vec![0xab]);
            push_data(&mut ours, &data);
            reference_push_data(&mut theirs, &data);
            prop_assert_eq!(ours, theirs);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// The opcode, the success and the cursor — including after a failure — equal O4's
        /// independent port, at every position a parse can start from.
        #[test]
        fn get_op_matches_the_reference_port(script in script_strategy(), start in 0usize..64) {
            let start = start.min(script.len());
            let (mut ours, mut theirs) = (start, start);
            loop {
                let a = get_op(&script, &mut ours);
                let b = reference_get_op(&script, &mut theirs);
                prop_assert_eq!(a.map(|op| op.opcode), b);
                prop_assert_eq!(ours, theirs);
                if a.is_none() {
                    break;
                }
            }
        }

        /// The restricted idempotence property of CS1 §4.3: a pattern that parses as exactly one
        /// complete instruction is never found again in FindAndDelete's own output. Half the
        /// patterns are taken from the script itself, so deletions happen.
        #[test]
        fn find_and_delete_of_one_instruction_is_idempotent(
            script in script_strategy(),
            generated in one_instruction(),
            pick in any::<proptest::sample::Index>(),
            from_script in any::<bool>(),
        ) {
            let own: Vec<Op> = instructions(&script).collect();
            let pattern = match (from_script, own.is_empty()) {
                (true, false) => {
                    let op = own[pick.index(own.len())];
                    let mut bytes = Vec::new();
                    if op.opcode <= OP_PUSHDATA4 {
                        push_data(&mut bytes, op.data);
                    } else {
                        bytes.push(op.opcode);
                    }
                    bytes
                }
                _ => generated,
            };
            let mut pc = 0;
            prop_assert!(get_op(&pattern, &mut pc).is_some() && pc == pattern.len());
            let mut once = script.clone();
            find_and_delete(&mut once, &pattern);
            let mut twice = once.clone();
            prop_assert_eq!(find_and_delete(&mut twice, &pattern), 0);
            prop_assert_eq!(twice, once);
        }

        /// FindAndDelete never grows a script and reports a count only when it rewrote it.
        #[test]
        fn find_and_delete_count_matches_the_rewrite(script in script_strategy(), pattern in one_instruction()) {
            let mut out = script.clone();
            let n = find_and_delete(&mut out, &pattern);
            prop_assert_eq!(script.len() - out.len(), n * pattern.len());
        }
    }

    #[test]
    fn failure_cursor_follows_core() {
        // (script, start, cursor after the failing call)
        let cases: [(&[u8], usize); 9] = [
            (&[], 0),                 // at the end: unchanged
            (&[0x05, 0xaa], 1),       // direct push, short data: after the opcode
            (&[0x4c], 1),             // PUSHDATA1 without its size byte
            (&[0x4c, 0x02, 0xaa], 2), // PUSHDATA1, short data: after the size field
            (&[0x4d, 0x01], 1),       // PUSHDATA2 with half a size field: after the opcode
            (&[0x4d, 0x02, 0x00, 0xaa], 3),
            (&[0x4e, 0x01, 0x02], 1), // PUSHDATA4 with 2 of 4 size bytes: after the opcode
            (&[0x4e, 0x01, 0x02, 0x03], 1),
            (&[0x4e, 0x01, 0x00, 0x00, 0x00], 5),
        ];
        for (script, cursor) in cases {
            let mut pc = 0;
            assert_eq!(get_op(script, &mut pc), None, "{script:02x?}");
            assert_eq!(pc, cursor, "{script:02x?}");
        }
    }

    #[test]
    fn small_integer_opcodes_carry_no_data() {
        let mut pc = 0;
        let script = [OP_1NEGATE, OP_RESERVED, OP_1, OP_16, 0x01, 0x07];
        for expected in [OP_1NEGATE, OP_RESERVED, OP_1, OP_16] {
            assert_eq!(
                get_op(&script, &mut pc),
                Some(Op {
                    opcode: expected,
                    data: &[]
                })
            );
        }
        assert_eq!(
            get_op(&script, &mut pc),
            Some(Op {
                opcode: 0x01,
                data: &[0x07]
            })
        );
    }

    #[test]
    fn push_data_uses_pushdata4_above_65535_bytes() {
        let data = vec![7; 65_536];
        let (mut ours, mut theirs) = (Vec::new(), Vec::new());
        push_data(&mut ours, &data);
        reference_push_data(&mut theirs, &data);
        assert_eq!(ours[..5], [OP_PUSHDATA4, 0x00, 0x00, 0x01, 0x00]);
        assert_eq!(ours, theirs);
    }

    #[test]
    fn pushes_over_65535_bytes_are_always_minimal() {
        // Core's last case. EvalScript never gets there: it rejects pushes over 520 bytes first.
        assert!(check_minimal_push(&[0; 65_536], OP_PUSHDATA4));
        assert!(!check_minimal_push(&[0; 65_535], OP_PUSHDATA4));
    }

    #[test]
    fn push_only_counts_reserved_and_stops_at_a_parse_failure() {
        assert!(is_push_only(&[]));
        assert!(is_push_only(&[OP_0, OP_RESERVED, OP_16, 0x01, 0xff]));
        assert!(!is_push_only(&[OP_NOP]));
        assert!(!is_push_only(&[0x02, 0x01]));
    }

    #[test]
    fn instructions_stop_for_good_at_a_parse_failure() {
        // PUSHDATA2 with half its size field: get_op fails with the cursor after 0x4d, where a
        // second call would read the size byte (OP_1) as an instruction.
        let script = [OP_PUSHDATA2, OP_1];
        let mut ops = instructions(&script);
        assert_eq!(ops.next(), None);
        assert_eq!(ops.next(), None);
        assert_eq!(ops.next(), None);
        let mut pc = 0;
        assert_eq!(get_op(&script, &mut pc), None);
        assert_eq!(get_op(&script, &mut pc).map(|op| op.opcode), Some(OP_1));
    }

    #[test]
    fn p2sh_is_the_exact_template() {
        let mut spk = p2sh_script_pubkey(&[0; 20]);
        assert!(is_pay_to_script_hash(&spk));
        spk.push(OP_NOP);
        assert!(!is_pay_to_script_hash(&spk));
    }
}
