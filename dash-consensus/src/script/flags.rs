//! Script verification flags (Core's `SCRIPT_VERIFY_*`, `script/interpreter.h:38-114`), the policy
//! masks that classify a failing input, and `GetBlockScriptFlags`.

/// Core's `unsigned int flags`.
///
/// All 32 bits are kept and passed through, and the undefined ones are ignored, as in Core. Bit 15
/// is the retired `SCRIPT_ENABLE_DIP0020_OPCODES`: the DIP0020 opcodes are enabled at every height,
/// so it has no constant and is ignored like any undefined bit.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct ScriptFlags(u32);

impl ScriptFlags {
    pub const NONE: Self = Self(0);
    /// BIP16: evaluate P2SH subscripts.
    pub const P2SH: Self = Self(1 << 0);
    /// Defined hash types and compressed/uncompressed public keys only.
    pub const STRICTENC: Self = Self(1 << 1);
    /// BIP66: strict DER signatures.
    pub const DERSIG: Self = Self(1 << 2);
    /// Low S values in signatures.
    pub const LOW_S: Self = Self(1 << 3);
    /// BIP147: the extra `CHECKMULTISIG` stack element must be empty.
    pub const NULLDUMMY: Self = Self(1 << 4);
    /// The scriptSig contains only pushes.
    pub const SIGPUSHONLY: Self = Self(1 << 5);
    /// Minimal pushes and minimally encoded numbers.
    pub const MINIMALDATA: Self = Self(1 << 6);
    /// Upgradable NOPs fail.
    pub const DISCOURAGE_UPGRADABLE_NOPS: Self = Self(1 << 7);
    /// Exactly one stack element after evaluation; needs [`Self::P2SH`].
    pub const CLEANSTACK: Self = Self(1 << 8);
    /// BIP65 `OP_CHECKLOCKTIMEVERIFY`.
    pub const CHECKLOCKTIMEVERIFY: Self = Self(1 << 9);
    /// BIP112 `OP_CHECKSEQUENCEVERIFY`.
    pub const CHECKSEQUENCEVERIFY: Self = Self(1 << 10);
    /// A failed signature check requires an empty signature.
    pub const NULLFAIL: Self = Self(1 << 14);
    /// `OP_CODESEPARATOR` and `FindAndDelete` hits fail.
    pub const CONST_SCRIPTCODE: Self = Self(1 << 16);

    /// `policy/policy.h` `STANDARD_SCRIPT_VERIFY_FLAGS`: 0x147DF (v24 re-expresses it on top of its
    /// larger mandatory set).
    pub const STANDARD: Self = Self(0x147DF);
    /// `script/standard.h` `MANDATORY_SCRIPT_VERIFY_FLAGS` of v24.0: P2SH, DERSIG, NULLDUMMY, CLTV
    /// and CSV (A1 §2.25 row 19).
    pub const MANDATORY_V24_0: Self = Self(0x615);

    /// Every flag Core defines (the 13 above).
    pub const DEFINED: Self = Self(0x147FF);

    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Every bit of `f` is set.
    pub const fn contains(self, f: Self) -> bool {
        self.0 & f.0 == f.0
    }

    /// At least one bit of `f` is set.
    pub const fn intersects(self, f: Self) -> bool {
        self.0 & f.0 != 0
    }

    pub const fn union(self, f: Self) -> Self {
        Self(self.0 | f.0)
    }

    pub const fn difference(self, f: Self) -> Self {
        Self(self.0 & !f.0)
    }

    /// Core: `test/util/script.cpp` `IsValidFlagCombination`: `CLEANSTACK` requires `P2SH`.
    ///
    /// `VerifyScript` asserts this (`interpreter.cpp:1710`), so Core aborts on a violation, and
    /// [`super::verify_script`] panics. Flags never come from untrusted input: they are constants or
    /// the block flags, and both always include `P2SH`.
    pub const fn is_valid_combination(self) -> bool {
        !self.contains(Self::CLEANSTACK) || self.contains(Self::P2SH)
    }
}

impl core::ops::BitOr for ScriptFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// The buried deployments `GetBlockScriptFlags` reads: `DeploymentActiveAt(*pindex, …)` for each.
///
/// The caller evaluates them for the block being connected. The mempool's consensus pass uses the
/// **tip**'s state (`validation.cpp:1216`), not the next block's.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct BuriedScriptRules {
    pub dersig: bool,
    pub cltv: bool,
    pub csv: bool,
    pub bip147: bool,
}

// Core: validation.cpp:2419 GetBlockScriptFlags (v23.1.8: 2223)
/// The script flags a block is checked with. P2SH is always on: it is active from genesis on every
/// Dash chain.
// GetBlockScriptFlags is static in validation.cpp, so no oracle entry point calls it: its Core
// evidence is the §7.6 chain sample (`tests/script_chain_sample.rs`) across each activation.
pub const fn block_script_flags(active: BuriedScriptRules) -> ScriptFlags {
    // Start enforcing P2SH (BIP16): always active on Dash chains.
    let mut flags = ScriptFlags::P2SH;
    // Enforce the DERSIG (BIP66) rule.
    if active.dersig {
        flags = flags.union(ScriptFlags::DERSIG);
    }
    // Enforce CHECKLOCKTIMEVERIFY (BIP65).
    if active.cltv {
        flags = flags.union(ScriptFlags::CHECKLOCKTIMEVERIFY);
    }
    // Enforce CHECKSEQUENCEVERIFY (BIP112).
    if active.csv {
        flags = flags.union(ScriptFlags::CHECKSEQUENCEVERIFY);
    }
    // Enforce BIP147 NULLDUMMY.
    if active.bip147 {
        flags = flags.union(ScriptFlags::NULLDUMMY);
    }
    debug_assert!(flags.is_valid_combination());
    flags
}

#[cfg(test)]
mod tests {
    use dash_core_vectors::flags as o4;

    use super::*;

    #[test]
    fn flags_match_the_vector_harness_table() {
        // O4's table is a transcription of Core's `mapFlagNames` (test/util/script.cpp).
        let ours = [
            ("P2SH", ScriptFlags::P2SH),
            ("STRICTENC", ScriptFlags::STRICTENC),
            ("DERSIG", ScriptFlags::DERSIG),
            ("LOW_S", ScriptFlags::LOW_S),
            ("NULLDUMMY", ScriptFlags::NULLDUMMY),
            ("SIGPUSHONLY", ScriptFlags::SIGPUSHONLY),
            ("MINIMALDATA", ScriptFlags::MINIMALDATA),
            ("DISCOURAGE_UPGRADABLE_NOPS", ScriptFlags::DISCOURAGE_UPGRADABLE_NOPS),
            ("CLEANSTACK", ScriptFlags::CLEANSTACK),
            ("CHECKLOCKTIMEVERIFY", ScriptFlags::CHECKLOCKTIMEVERIFY),
            ("CHECKSEQUENCEVERIFY", ScriptFlags::CHECKSEQUENCEVERIFY),
            ("NULLFAIL", ScriptFlags::NULLFAIL),
            ("CONST_SCRIPTCODE", ScriptFlags::CONST_SCRIPTCODE),
        ];
        assert_eq!(ours.len(), o4::FLAG_NAMES.len());
        for (name, flag) in ours {
            assert_eq!(o4::parse_script_flags(name).unwrap(), flag.bits(), "{name}");
        }
        assert_eq!(ScriptFlags::DEFINED.bits(), o4::DEFINED_FLAGS);
    }

    #[test]
    fn policy_masks_are_the_documented_combinations() {
        let standard = ScriptFlags::MANDATORY_V24_0
            | ScriptFlags::STRICTENC
            | ScriptFlags::MINIMALDATA
            | ScriptFlags::DISCOURAGE_UPGRADABLE_NOPS
            | ScriptFlags::CLEANSTACK
            | ScriptFlags::NULLFAIL
            | ScriptFlags::LOW_S
            | ScriptFlags::CONST_SCRIPTCODE;
        assert_eq!(ScriptFlags::STANDARD, standard);
        assert_eq!(
            ScriptFlags::MANDATORY_V24_0,
            ScriptFlags::P2SH
                | ScriptFlags::DERSIG
                | ScriptFlags::NULLDUMMY
                | ScriptFlags::CHECKLOCKTIMEVERIFY
                | ScriptFlags::CHECKSEQUENCEVERIFY
        );
        // STANDARD_NOT_MANDATORY_VERIFY_FLAGS (CS1 §6.2).
        assert_eq!(ScriptFlags::STANDARD.difference(ScriptFlags::MANDATORY_V24_0).bits(), 0x141CA);
    }

    #[test]
    fn block_flags_add_each_buried_rule() {
        let none = BuriedScriptRules::default();
        assert_eq!(block_script_flags(none), ScriptFlags::P2SH);
        let all = BuriedScriptRules {
            dersig: true,
            cltv: true,
            csv: true,
            bip147: true,
        };
        assert_eq!(block_script_flags(all), ScriptFlags::MANDATORY_V24_0);
    }

    #[test]
    fn cleanstack_needs_p2sh() {
        assert!(ScriptFlags::NONE.is_valid_combination());
        assert!(!ScriptFlags::CLEANSTACK.is_valid_combination());
        assert!((ScriptFlags::CLEANSTACK | ScriptFlags::P2SH).is_valid_combination());
    }
}
