//! Script verification flags as the vector files name them, and the flag perturbations Core's
//! test harness is *meant* to perform.
//!
//! Core's `transaction_tests.cpp` and `script_tests.cpp` check that a valid vector stays valid
//! when flags are removed and an invalid one stays invalid when flags are added (every flag is a
//! soft fork). The loops there have three defects, none of which this module copies
//! (see `PROVENANCE.md`, "Harness quirks"):
//!
//! 1. They enumerate `1 << i` for `i < mapFlagNames.size()` (13), covering bits 0..=12. The
//!    defined flags are bits 0..=10, 14 (`NULLFAIL`) and 16 (`CONST_SCRIPTCODE`), so `NULLFAIL`
//!    and `CONST_SCRIPTCODE` are never perturbed and bits 11 and 12 (undefined) are.
//!    Random combinations draw 13 bits for the same reason; `script_tests.cpp` draws 16 bits,
//!    which reaches the retired bit 15 but not bit 16.
//! 2. `ExcludeIndividualFlags` tests `find(..) != end()` where it needs `== end()`, so it never
//!    returns anything and the "flags are maximal" (`tx_valid`) and "flags are minimal"
//!    (`tx_invalid`) checks do nothing. [`restore_one_excluded`] and [`exclude_individual_flags`]
//!    are the intended functions. Run against Core's interpreter
//!    (`transcribed/harness_audit.json`), the repaired maximality check holds for every `tx_valid`
//!    vector, but the repaired minimality check fails for 61 of the 66 script-checked `tx_invalid`
//!    vectors. Nine of them are invalid whatever flags are set; in the others a listed flag is
//!    redundant (a row lists `P2SH` next to the flag the transaction depends on, and dropping
//!    `P2SH` still fails). So `tx_invalid` is only checked for monotonicity.
//! 3. `tx_valid` passes `~verify_flags` (all 32 bits, including undefined ones) to the verifier.
//!    [`DEFINED_FLAGS`] masks that down.

use std::fmt;

pub const SCRIPT_VERIFY_NONE: u32 = 0;
pub const SCRIPT_VERIFY_P2SH: u32 = 1 << 0;
pub const SCRIPT_VERIFY_STRICTENC: u32 = 1 << 1;
pub const SCRIPT_VERIFY_DERSIG: u32 = 1 << 2;
pub const SCRIPT_VERIFY_LOW_S: u32 = 1 << 3;
pub const SCRIPT_VERIFY_NULLDUMMY: u32 = 1 << 4;
pub const SCRIPT_VERIFY_SIGPUSHONLY: u32 = 1 << 5;
pub const SCRIPT_VERIFY_MINIMALDATA: u32 = 1 << 6;
pub const SCRIPT_VERIFY_DISCOURAGE_UPGRADABLE_NOPS: u32 = 1 << 7;
pub const SCRIPT_VERIFY_CLEANSTACK: u32 = 1 << 8;
pub const SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY: u32 = 1 << 9;
pub const SCRIPT_VERIFY_CHECKSEQUENCEVERIFY: u32 = 1 << 10;
pub const SCRIPT_VERIFY_NULLFAIL: u32 = 1 << 14;
/// Bit 15 was `SCRIPT_ENABLE_DIP0020_OPCODES`; it is retired and ignored by the interpreter.
pub const SCRIPT_ENABLE_DIP0020_OPCODES_RETIRED: u32 = 1 << 15;
pub const SCRIPT_VERIFY_CONST_SCRIPTCODE: u32 = 1 << 16;

/// `mapFlagNames` (`transaction_tests.cpp`). This is the complete set of names the vector files
/// use; anything else is a bad test.
pub const FLAG_NAMES: [(&str, u32); 13] = [
    ("P2SH", SCRIPT_VERIFY_P2SH),
    ("STRICTENC", SCRIPT_VERIFY_STRICTENC),
    ("DERSIG", SCRIPT_VERIFY_DERSIG),
    ("LOW_S", SCRIPT_VERIFY_LOW_S),
    ("SIGPUSHONLY", SCRIPT_VERIFY_SIGPUSHONLY),
    ("MINIMALDATA", SCRIPT_VERIFY_MINIMALDATA),
    ("NULLDUMMY", SCRIPT_VERIFY_NULLDUMMY),
    ("DISCOURAGE_UPGRADABLE_NOPS", SCRIPT_VERIFY_DISCOURAGE_UPGRADABLE_NOPS),
    ("CLEANSTACK", SCRIPT_VERIFY_CLEANSTACK),
    ("NULLFAIL", SCRIPT_VERIFY_NULLFAIL),
    ("CHECKLOCKTIMEVERIFY", SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY),
    ("CHECKSEQUENCEVERIFY", SCRIPT_VERIFY_CHECKSEQUENCEVERIFY),
    ("CONST_SCRIPTCODE", SCRIPT_VERIFY_CONST_SCRIPTCODE),
];

/// Every flag bit with a name, as a mask.
pub const DEFINED_FLAGS: u32 = {
    let mut mask = 0;
    let mut i = 0;
    while i < FLAG_NAMES.len() {
        mask |= FLAG_NAMES[i].1;
        i += 1;
    }
    mask
};

/// The error `ParseScriptFlags` reports as `BOOST_ERROR("Bad test: unknown verification flag ..")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownFlag(pub String);

impl fmt::Display for UnknownFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown verification flag '{}'", self.0)
    }
}

impl std::error::Error for UnknownFlag {}

/// `ParseScriptFlags`: `""` and `"NONE"` are 0, otherwise a comma-separated list of names with no
/// surrounding whitespace. Empty items (`"P2SH,,DERSIG"`) are unknown flags, as in Core.
pub fn parse_script_flags(s: &str) -> Result<u32, UnknownFlag> {
    if s.is_empty() || s == "NONE" {
        return Ok(0);
    }
    let mut flags = 0;
    for word in s.split(',') {
        match FLAG_NAMES.iter().find(|(name, _)| *name == word) {
            Some(&(_, bit)) => flags |= bit,
            None => return Err(UnknownFlag(word.to_owned())),
        }
    }
    Ok(flags)
}

/// `FormatScriptFlags`: names joined by `,` in name order (Core iterates a `std::map`).
pub fn format_script_flags(flags: u32) -> String {
    let mut names: Vec<&str> =
        FLAG_NAMES.iter().filter(|(_, bit)| flags & bit != 0).map(|(name, _)| *name).collect();
    names.sort_unstable();
    names.join(",")
}

/// `TrimFlags`: `CLEANSTACK` requires `P2SH`, so drop it when `P2SH` is absent.
pub fn trim_flags(flags: u32) -> u32 {
    if flags & SCRIPT_VERIFY_P2SH == 0 {
        flags & !SCRIPT_VERIFY_CLEANSTACK
    } else {
        flags
    }
}

/// `FillFlags`: `CLEANSTACK` implies `P2SH`, so add it.
pub fn fill_flags(flags: u32) -> u32 {
    if flags & SCRIPT_VERIFY_CLEANSTACK != 0 {
        flags | SCRIPT_VERIFY_P2SH
    } else {
        flags
    }
}

/// The single-bit masks of all defined flags, lowest bit first.
pub fn defined_flag_bits() -> impl Iterator<Item = u32> {
    let mut bits: Vec<u32> = FLAG_NAMES.iter().map(|(_, b)| *b).collect();
    bits.sort_unstable();
    bits.into_iter()
}

/// `ExcludeIndividualFlags` as Core intends it, for a set of *enabled* flags (the listed flags of
/// `tx_invalid`): each defined flag removed in turn, trimmed to a valid combination, skipping
/// combinations that equal `flags` or were already produced. Core's version never returns anything
/// (see the module docs for where the check built on it holds).
pub fn exclude_individual_flags(flags: u32) -> Vec<u32> {
    let mut combos = Vec::new();
    for bit in defined_flag_bits() {
        let excluded = trim_flags(flags & !bit);
        if flags != excluded && !combos.contains(&excluded) {
            combos.push(excluded);
        }
    }
    combos
}

/// The `tx_valid` counterpart: `excluded` is the vector's list of flags that must be *off*; the
/// enabled set is every other defined flag. Returns, for each excluded flag, the enabled set with
/// that flag switched back on (`CLEANSTACK` together with `P2SH`, which it needs, so the combination
/// is valid for a verifier). The vector has to fail under each of them: the exclusions are maximal.
///
/// The `CLEANSTACK`-needs-`P2SH` rule is a rule about enabled flags, so it is applied there, not to
/// the excluded list.
pub fn restore_one_excluded(excluded: u32) -> Vec<u32> {
    let enabled = DEFINED_FLAGS & !excluded;
    defined_flag_bits()
        .filter(|bit| excluded & bit != 0)
        .map(|bit| fill_flags(enabled | bit))
        .collect()
}

/// A small deterministic generator (xorshift64*) for the random flag combinations; the vector
/// harness only needs "arbitrary but reproducible" and must not depend on a `rand` crate.
#[derive(Clone, Debug)]
pub struct FlagRng(u64);

impl FlagRng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }

    /// A random subset of the *defined* flags.
    pub fn random_defined_flags(&mut self) -> u32 {
        self.next_u32() & DEFINED_FLAGS
    }
}

/// The flag sets a *valid* script vector must also pass under: `flags` with each single defined
/// bit removed, plus `random` random subsets removed. Combinations are fixed up with
/// [`trim_flags`] so `CLEANSTACK` never appears without `P2SH`.
///
/// Used by `script_tests` (`flags` is the vector's flag set) and by `tx_valid` (where Core passes
/// `~verify_flags`; call with `flags = !verify_flags & DEFINED_FLAGS`).
pub fn removal_sets(flags: u32, rng: &mut FlagRng, random: usize) -> Vec<u32> {
    let mut sets: Vec<u32> = defined_flag_bits().map(|bit| trim_flags(flags & !bit)).collect();
    for _ in 0..random {
        sets.push(trim_flags(flags & !rng.random_defined_flags()));
    }
    sets
}

/// The flag sets an *invalid* vector must also fail under: `flags` with each single defined bit
/// added, plus `random` random subsets added. Combinations are fixed up with [`fill_flags`].
///
/// Used by `script_tests` and by `tx_invalid`.
pub fn addition_sets(flags: u32, rng: &mut FlagRng, random: usize) -> Vec<u32> {
    let mut sets: Vec<u32> = defined_flag_bits().map(|bit| fill_flags(flags | bit)).collect();
    for _ in 0..random {
        sets.push(fill_flags(flags | rng.random_defined_flags()));
    }
    sets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cover_nullfail_and_const_scriptcode() {
        assert_eq!(DEFINED_FLAGS.count_ones(), 13);
        assert_ne!(DEFINED_FLAGS & SCRIPT_VERIFY_NULLFAIL, 0);
        assert_ne!(DEFINED_FLAGS & SCRIPT_VERIFY_CONST_SCRIPTCODE, 0);
        // Core's `1 << i, i < 13` loop reaches bits 11 and 12, which are not flags, and misses
        // exactly the two flags above.
        let core_loop: u32 = (0..13).fold(0, |m, i| m | (1 << i));
        assert_eq!(
            DEFINED_FLAGS & !core_loop,
            SCRIPT_VERIFY_NULLFAIL | SCRIPT_VERIFY_CONST_SCRIPTCODE
        );
        assert_eq!(core_loop & !DEFINED_FLAGS, (1 << 11) | (1 << 12));
    }

    #[test]
    fn parse_and_format_round_trip() {
        assert_eq!(parse_script_flags(""), Ok(0));
        assert_eq!(parse_script_flags("NONE"), Ok(0));
        let f = parse_script_flags("P2SH,STRICTENC,NULLFAIL").unwrap();
        assert_eq!(f, 1 | 2 | (1 << 14));
        assert_eq!(format_script_flags(f), "NULLFAIL,P2SH,STRICTENC");
        assert_eq!(parse_script_flags("P2SH,,DERSIG"), Err(UnknownFlag(String::new())));
        assert_eq!(parse_script_flags("p2sh"), Err(UnknownFlag("p2sh".into())));
        assert_eq!(parse_script_flags("NONE,P2SH"), Err(UnknownFlag("NONE".into())));
        assert_eq!(parse_script_flags("BADTX"), Err(UnknownFlag("BADTX".into())));
    }

    #[test]
    fn exclude_individual_flags_is_not_a_no_op() {
        let f = SCRIPT_VERIFY_P2SH | SCRIPT_VERIFY_CLEANSTACK | SCRIPT_VERIFY_NULLFAIL;
        let combos = exclude_individual_flags(f);
        // Removing P2SH drags CLEANSTACK with it.
        assert_eq!(
            combos,
            vec![
                SCRIPT_VERIFY_NULLFAIL,
                SCRIPT_VERIFY_P2SH | SCRIPT_VERIFY_NULLFAIL,
                SCRIPT_VERIFY_P2SH | SCRIPT_VERIFY_CLEANSTACK
            ]
        );
        assert!(exclude_individual_flags(0).is_empty());
    }

    #[test]
    fn restore_one_excluded_works_in_enabled_flag_space() {
        // Excluding CLEANSTACK and SIGPUSHONLY: switching SIGPUSHONLY back on must not also
        // switch CLEANSTACK on (it stays excluded), and the two sets differ.
        let excluded = SCRIPT_VERIFY_CLEANSTACK | SCRIPT_VERIFY_SIGPUSHONLY;
        let enabled = DEFINED_FLAGS & !excluded;
        let sets = restore_one_excluded(excluded);
        assert_eq!(sets.len(), 2);
        assert!(sets.contains(&(enabled | SCRIPT_VERIFY_SIGPUSHONLY)));
        // CLEANSTACK back on while P2SH is enabled stays valid.
        assert!(sets.contains(&(enabled | SCRIPT_VERIFY_CLEANSTACK)));
        // Excluding P2SH and CLEANSTACK: restoring CLEANSTACK pulls P2SH in with it.
        let excluded = SCRIPT_VERIFY_P2SH | SCRIPT_VERIFY_CLEANSTACK;
        let enabled = DEFINED_FLAGS & !excluded;
        let sets = restore_one_excluded(excluded);
        assert_eq!(
            sets,
            vec![
                enabled | SCRIPT_VERIFY_P2SH,
                enabled | SCRIPT_VERIFY_P2SH | SCRIPT_VERIFY_CLEANSTACK
            ]
        );
        for s in sets {
            assert!(s & SCRIPT_VERIFY_CLEANSTACK == 0 || s & SCRIPT_VERIFY_P2SH != 0);
        }
    }

    #[test]
    fn perturbations_reach_every_defined_flag() {
        let mut rng = FlagRng::new(1);
        let added = addition_sets(0, &mut rng, 0);
        let mut seen = 0;
        for s in &added {
            seen |= s;
        }
        assert_eq!(seen & DEFINED_FLAGS, DEFINED_FLAGS);
        let removed = removal_sets(DEFINED_FLAGS, &mut rng, 0);
        assert!(removed.iter().any(|s| s & SCRIPT_VERIFY_NULLFAIL == 0));
        assert!(removed.iter().any(|s| s & SCRIPT_VERIFY_CONST_SCRIPTCODE == 0));
        for s in removed.iter().chain(&added) {
            assert_eq!(*s & !DEFINED_FLAGS, 0);
            if s & SCRIPT_VERIFY_CLEANSTACK != 0 {
                assert_ne!(s & SCRIPT_VERIFY_P2SH, 0);
            }
        }
        // A random set also stays within the defined flags.
        for _ in 0..100 {
            assert_eq!(rng.random_defined_flags() & !DEFINED_FLAGS, 0);
        }
    }
}
