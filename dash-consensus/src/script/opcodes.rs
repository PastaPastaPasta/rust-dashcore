//! Core's `opcodetype` (`script/script.h:55-198`) and `GetOpName` (`script/script.cpp:11`).

// Push value
pub const OP_0: u8 = 0x00;
pub const OP_FALSE: u8 = OP_0;
pub const OP_PUSHDATA1: u8 = 0x4c;
pub const OP_PUSHDATA2: u8 = 0x4d;
pub const OP_PUSHDATA4: u8 = 0x4e;
pub const OP_1NEGATE: u8 = 0x4f;
pub const OP_RESERVED: u8 = 0x50;
pub const OP_1: u8 = 0x51;
pub const OP_TRUE: u8 = OP_1;
pub const OP_2: u8 = 0x52;
pub const OP_3: u8 = 0x53;
pub const OP_4: u8 = 0x54;
pub const OP_5: u8 = 0x55;
pub const OP_6: u8 = 0x56;
pub const OP_7: u8 = 0x57;
pub const OP_8: u8 = 0x58;
pub const OP_9: u8 = 0x59;
pub const OP_10: u8 = 0x5a;
pub const OP_11: u8 = 0x5b;
pub const OP_12: u8 = 0x5c;
pub const OP_13: u8 = 0x5d;
pub const OP_14: u8 = 0x5e;
pub const OP_15: u8 = 0x5f;
pub const OP_16: u8 = 0x60;

// Control
pub const OP_NOP: u8 = 0x61;
pub const OP_VER: u8 = 0x62;
pub const OP_IF: u8 = 0x63;
pub const OP_NOTIF: u8 = 0x64;
pub const OP_VERIF: u8 = 0x65;
pub const OP_VERNOTIF: u8 = 0x66;
pub const OP_ELSE: u8 = 0x67;
pub const OP_ENDIF: u8 = 0x68;
pub const OP_VERIFY: u8 = 0x69;
pub const OP_RETURN: u8 = 0x6a;

// Stack ops
pub const OP_TOALTSTACK: u8 = 0x6b;
pub const OP_FROMALTSTACK: u8 = 0x6c;
pub const OP_2DROP: u8 = 0x6d;
pub const OP_2DUP: u8 = 0x6e;
pub const OP_3DUP: u8 = 0x6f;
pub const OP_2OVER: u8 = 0x70;
pub const OP_2ROT: u8 = 0x71;
pub const OP_2SWAP: u8 = 0x72;
pub const OP_IFDUP: u8 = 0x73;
pub const OP_DEPTH: u8 = 0x74;
pub const OP_DROP: u8 = 0x75;
pub const OP_DUP: u8 = 0x76;
pub const OP_NIP: u8 = 0x77;
pub const OP_OVER: u8 = 0x78;
pub const OP_PICK: u8 = 0x79;
pub const OP_ROLL: u8 = 0x7a;
pub const OP_ROT: u8 = 0x7b;
pub const OP_SWAP: u8 = 0x7c;
pub const OP_TUCK: u8 = 0x7d;

// Splice ops (DIP0020)
pub const OP_CAT: u8 = 0x7e;
pub const OP_SPLIT: u8 = 0x7f;
pub const OP_SIZE: u8 = 0x82;

// Conversion ops (DIP0020)
pub const OP_NUM2BIN: u8 = 0x80;
pub const OP_BIN2NUM: u8 = 0x81;

// Bit logic (AND, OR and XOR are DIP0020)
pub const OP_INVERT: u8 = 0x83;
pub const OP_AND: u8 = 0x84;
pub const OP_OR: u8 = 0x85;
pub const OP_XOR: u8 = 0x86;
pub const OP_EQUAL: u8 = 0x87;
pub const OP_EQUALVERIFY: u8 = 0x88;
pub const OP_RESERVED1: u8 = 0x89;
pub const OP_RESERVED2: u8 = 0x8a;

// Numeric (DIV and MOD are DIP0020)
pub const OP_1ADD: u8 = 0x8b;
pub const OP_1SUB: u8 = 0x8c;
pub const OP_2MUL: u8 = 0x8d;
pub const OP_2DIV: u8 = 0x8e;
pub const OP_NEGATE: u8 = 0x8f;
pub const OP_ABS: u8 = 0x90;
pub const OP_NOT: u8 = 0x91;
pub const OP_0NOTEQUAL: u8 = 0x92;
pub const OP_ADD: u8 = 0x93;
pub const OP_SUB: u8 = 0x94;
pub const OP_MUL: u8 = 0x95;
pub const OP_DIV: u8 = 0x96;
pub const OP_MOD: u8 = 0x97;
pub const OP_LSHIFT: u8 = 0x98;
pub const OP_RSHIFT: u8 = 0x99;
pub const OP_BOOLAND: u8 = 0x9a;
pub const OP_BOOLOR: u8 = 0x9b;
pub const OP_NUMEQUAL: u8 = 0x9c;
pub const OP_NUMEQUALVERIFY: u8 = 0x9d;
pub const OP_NUMNOTEQUAL: u8 = 0x9e;
pub const OP_LESSTHAN: u8 = 0x9f;
pub const OP_GREATERTHAN: u8 = 0xa0;
pub const OP_LESSTHANOREQUAL: u8 = 0xa1;
pub const OP_GREATERTHANOREQUAL: u8 = 0xa2;
pub const OP_MIN: u8 = 0xa3;
pub const OP_MAX: u8 = 0xa4;
pub const OP_WITHIN: u8 = 0xa5;

// Crypto
pub const OP_RIPEMD160: u8 = 0xa6;
pub const OP_SHA1: u8 = 0xa7;
pub const OP_SHA256: u8 = 0xa8;
pub const OP_HASH160: u8 = 0xa9;
pub const OP_HASH256: u8 = 0xaa;
pub const OP_CODESEPARATOR: u8 = 0xab;
pub const OP_CHECKSIG: u8 = 0xac;
pub const OP_CHECKSIGVERIFY: u8 = 0xad;
pub const OP_CHECKMULTISIG: u8 = 0xae;
pub const OP_CHECKMULTISIGVERIFY: u8 = 0xaf;

// Expansion
pub const OP_NOP1: u8 = 0xb0;
pub const OP_CHECKLOCKTIMEVERIFY: u8 = 0xb1;
pub const OP_NOP2: u8 = OP_CHECKLOCKTIMEVERIFY;
pub const OP_CHECKSEQUENCEVERIFY: u8 = 0xb2;
pub const OP_NOP3: u8 = OP_CHECKSEQUENCEVERIFY;
pub const OP_NOP4: u8 = 0xb3;
pub const OP_NOP5: u8 = 0xb4;
pub const OP_NOP6: u8 = 0xb5;
pub const OP_NOP7: u8 = 0xb6;
pub const OP_NOP8: u8 = 0xb7;
pub const OP_NOP9: u8 = 0xb8;
pub const OP_NOP10: u8 = 0xb9;

// Data signatures
pub const OP_CHECKDATASIG: u8 = 0xba;
pub const OP_CHECKDATASIGVERIFY: u8 = 0xbb;

pub const OP_INVALIDOPCODE: u8 = 0xff;

/// `CScript::DecodeOP_N` (`script/script.h:488`): `OP_0` is 0, `OP_1..=OP_16` are 1..=16. Only
/// called on those opcodes. Core asserts that, and Core's asserts are always on (it cannot be built
/// without them); here it is a debug assertion, because the only caller (sigop counting) passes
/// `OP_1..=OP_16`. In a release build another opcode gives a meaningless value.
pub const fn decode_op_n(opcode: u8) -> u32 {
    debug_assert!(opcode == OP_0 || (opcode >= OP_1 && opcode <= OP_16));
    if opcode == OP_0 {
        0
    } else {
        (opcode - (OP_1 - 1)) as u32
    }
}

// Core: script/script.cpp:11 GetOpName
/// Core's `GetOpName`: `"0"`, `"-1"` and `"1".."16"` for the small integers, `OP_*` for named
/// opcodes, `"OP_UNKNOWN"` for the rest. Direct pushes `0x01..=0x4b` are unknown.
pub const fn op_name(opcode: u8) -> &'static str {
    match opcode {
        OP_0 => "0",
        OP_PUSHDATA1 => "OP_PUSHDATA1",
        OP_PUSHDATA2 => "OP_PUSHDATA2",
        OP_PUSHDATA4 => "OP_PUSHDATA4",
        OP_1NEGATE => "-1",
        OP_RESERVED => "OP_RESERVED",
        OP_1 => "1",
        OP_2 => "2",
        OP_3 => "3",
        OP_4 => "4",
        OP_5 => "5",
        OP_6 => "6",
        OP_7 => "7",
        OP_8 => "8",
        OP_9 => "9",
        OP_10 => "10",
        OP_11 => "11",
        OP_12 => "12",
        OP_13 => "13",
        OP_14 => "14",
        OP_15 => "15",
        OP_16 => "16",
        OP_NOP => "OP_NOP",
        OP_VER => "OP_VER",
        OP_IF => "OP_IF",
        OP_NOTIF => "OP_NOTIF",
        OP_VERIF => "OP_VERIF",
        OP_VERNOTIF => "OP_VERNOTIF",
        OP_ELSE => "OP_ELSE",
        OP_ENDIF => "OP_ENDIF",
        OP_VERIFY => "OP_VERIFY",
        OP_RETURN => "OP_RETURN",
        OP_TOALTSTACK => "OP_TOALTSTACK",
        OP_FROMALTSTACK => "OP_FROMALTSTACK",
        OP_2DROP => "OP_2DROP",
        OP_2DUP => "OP_2DUP",
        OP_3DUP => "OP_3DUP",
        OP_2OVER => "OP_2OVER",
        OP_2ROT => "OP_2ROT",
        OP_2SWAP => "OP_2SWAP",
        OP_IFDUP => "OP_IFDUP",
        OP_DEPTH => "OP_DEPTH",
        OP_DROP => "OP_DROP",
        OP_DUP => "OP_DUP",
        OP_NIP => "OP_NIP",
        OP_OVER => "OP_OVER",
        OP_PICK => "OP_PICK",
        OP_ROLL => "OP_ROLL",
        OP_ROT => "OP_ROT",
        OP_SWAP => "OP_SWAP",
        OP_TUCK => "OP_TUCK",
        OP_CAT => "OP_CAT",
        OP_SPLIT => "OP_SPLIT",
        OP_SIZE => "OP_SIZE",
        OP_NUM2BIN => "OP_NUM2BIN",
        OP_BIN2NUM => "OP_BIN2NUM",
        OP_INVERT => "OP_INVERT",
        OP_AND => "OP_AND",
        OP_OR => "OP_OR",
        OP_XOR => "OP_XOR",
        OP_EQUAL => "OP_EQUAL",
        OP_EQUALVERIFY => "OP_EQUALVERIFY",
        OP_RESERVED1 => "OP_RESERVED1",
        OP_RESERVED2 => "OP_RESERVED2",
        OP_1ADD => "OP_1ADD",
        OP_1SUB => "OP_1SUB",
        OP_2MUL => "OP_2MUL",
        OP_2DIV => "OP_2DIV",
        OP_NEGATE => "OP_NEGATE",
        OP_ABS => "OP_ABS",
        OP_NOT => "OP_NOT",
        OP_0NOTEQUAL => "OP_0NOTEQUAL",
        OP_ADD => "OP_ADD",
        OP_SUB => "OP_SUB",
        OP_MUL => "OP_MUL",
        OP_DIV => "OP_DIV",
        OP_MOD => "OP_MOD",
        OP_LSHIFT => "OP_LSHIFT",
        OP_RSHIFT => "OP_RSHIFT",
        OP_BOOLAND => "OP_BOOLAND",
        OP_BOOLOR => "OP_BOOLOR",
        OP_NUMEQUAL => "OP_NUMEQUAL",
        OP_NUMEQUALVERIFY => "OP_NUMEQUALVERIFY",
        OP_NUMNOTEQUAL => "OP_NUMNOTEQUAL",
        OP_LESSTHAN => "OP_LESSTHAN",
        OP_GREATERTHAN => "OP_GREATERTHAN",
        OP_LESSTHANOREQUAL => "OP_LESSTHANOREQUAL",
        OP_GREATERTHANOREQUAL => "OP_GREATERTHANOREQUAL",
        OP_MIN => "OP_MIN",
        OP_MAX => "OP_MAX",
        OP_WITHIN => "OP_WITHIN",
        OP_RIPEMD160 => "OP_RIPEMD160",
        OP_SHA1 => "OP_SHA1",
        OP_SHA256 => "OP_SHA256",
        OP_HASH160 => "OP_HASH160",
        OP_HASH256 => "OP_HASH256",
        OP_CODESEPARATOR => "OP_CODESEPARATOR",
        OP_CHECKSIG => "OP_CHECKSIG",
        OP_CHECKSIGVERIFY => "OP_CHECKSIGVERIFY",
        OP_CHECKMULTISIG => "OP_CHECKMULTISIG",
        OP_CHECKMULTISIGVERIFY => "OP_CHECKMULTISIGVERIFY",
        OP_NOP1 => "OP_NOP1",
        OP_CHECKLOCKTIMEVERIFY => "OP_CHECKLOCKTIMEVERIFY",
        OP_CHECKSEQUENCEVERIFY => "OP_CHECKSEQUENCEVERIFY",
        OP_NOP4 => "OP_NOP4",
        OP_NOP5 => "OP_NOP5",
        OP_NOP6 => "OP_NOP6",
        OP_NOP7 => "OP_NOP7",
        OP_NOP8 => "OP_NOP8",
        OP_NOP9 => "OP_NOP9",
        OP_NOP10 => "OP_NOP10",
        OP_CHECKDATASIG => "OP_CHECKDATASIG",
        OP_CHECKDATASIGVERIFY => "OP_CHECKDATASIGVERIFY",
        OP_INVALIDOPCODE => "OP_INVALIDOPCODE",
        _ => "OP_UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use dash_core_vectors::script::OPCODE_NAMES;

    use super::*;

    #[test]
    fn names_match_the_parse_script_table() {
        // O4's table is `GetOpName` for OP_RESERVED and OP_NOP..=OP_CHECKDATASIGVERIFY, the opcodes
        // Core's `ParseScript` can name.
        for &(opcode, name) in OPCODE_NAMES {
            assert_eq!(op_name(opcode), name, "{opcode:#04x}");
        }
        let named = (OP_NOP..=OP_CHECKDATASIGVERIFY).count() + 1;
        assert_eq!(OPCODE_NAMES.len(), named);
    }

    #[test]
    fn small_integers_and_unknowns() {
        assert_eq!(op_name(OP_0), "0");
        assert_eq!(op_name(OP_1NEGATE), "-1");
        assert_eq!(op_name(OP_16), "16");
        assert_eq!(op_name(0x01), "OP_UNKNOWN");
        assert_eq!(op_name(0x4b), "OP_UNKNOWN");
        assert_eq!(op_name(0xbc), "OP_UNKNOWN");
        assert_eq!(op_name(0xfe), "OP_UNKNOWN");
        assert_eq!(decode_op_n(OP_0), 0);
        assert_eq!(decode_op_n(OP_16), 16);
    }
}
