//! Core's `nStatus` (SB4 §3.2).

/// A block index entry's status, bit for bit Core's `BlockStatus` (`chain.h:81-135`).
///
/// SPV's context entries are a property of their record ([`crate::RecordKind`]), never a status
/// bit (SB0 §4.1.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Status(pub u32);

impl Status {
    /// `BLOCK_VALID_UNKNOWN`.
    pub const VALID_UNKNOWN: u32 = 0;
    /// `BLOCK_VALID_RESERVED`.
    pub const VALID_RESERVED: u32 = 1;
    /// `BLOCK_VALID_TREE`: the header is valid and all parents are at least `VALID_TREE`.
    pub const VALID_TREE: u32 = 2;
    /// `BLOCK_VALID_TRANSACTIONS`.
    pub const VALID_TRANSACTIONS: u32 = 3;
    /// `BLOCK_VALID_CHAIN`.
    pub const VALID_CHAIN: u32 = 4;
    /// `BLOCK_VALID_SCRIPTS`.
    pub const VALID_SCRIPTS: u32 = 5;
    /// `BLOCK_VALID_MASK`.
    pub const VALID_MASK: u32 = 7;
    /// `BLOCK_HAVE_DATA`.
    pub const HAVE_DATA: u32 = 8;
    /// `BLOCK_HAVE_UNDO`.
    pub const HAVE_UNDO: u32 = 16;
    /// `BLOCK_HAVE_MASK`.
    pub const HAVE_MASK: u32 = Status::HAVE_DATA | Status::HAVE_UNDO;
    /// `BLOCK_FAILED_VALID`.
    pub const FAILED_VALID: u32 = 32;
    /// `BLOCK_FAILED_CHILD`.
    pub const FAILED_CHILD: u32 = 64;
    /// `BLOCK_FAILED_MASK`.
    pub const FAILED_MASK: u32 = Status::FAILED_VALID | Status::FAILED_CHILD;
    /// `BLOCK_CONFLICT_CHAINLOCK` (`chain.h:118`).
    pub const CONFLICT_CHAINLOCK: u32 = 128;
    /// `BLOCK_ASSUMED_VALID`: reserved (A1 §2.9), never set.
    pub const ASSUMED_VALID: u32 = 256;

    /// The raw word.
    pub fn bits(self) -> u32 {
        self.0
    }

    /// Whether any bit of `mask` is set.
    pub fn has(self, mask: u32) -> bool {
        self.0 & mask != 0
    }

    /// `IsValid(nUpTo)`: false when any FAILED bit is set, otherwise whether the validity level
    /// reaches `up_to`. `CONFLICT_CHAINLOCK` does not affect it.
    // Core: chain.h:300-308 CBlockIndex::IsValid
    pub fn is_valid(self, up_to: u32) -> bool {
        debug_assert!(up_to & !Status::VALID_MASK == 0, "only validity values allowed");
        if self.has(Status::FAILED_MASK) {
            return false;
        }
        self.0 & Status::VALID_MASK >= up_to
    }

    /// `RaiseValidity(nUpTo)`: the status with its validity level raised to `up_to`, and
    /// whether it changed. A failed status is never raised.
    // Core: chain.h:320-336 CBlockIndex::RaiseValidity (BLOCK_ASSUMED_VALID is never set here)
    pub fn raise_validity(self, up_to: u32) -> (Status, bool) {
        debug_assert!(up_to & !Status::VALID_MASK == 0, "only validity values allowed");
        if self.has(Status::FAILED_MASK) {
            return (self, false);
        }
        if self.0 & Status::VALID_MASK < up_to {
            (Status((self.0 & !Status::VALID_MASK) | up_to), true)
        } else {
            (self, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_ignores_conflict() {
        let s = Status(Status::VALID_TREE | Status::CONFLICT_CHAINLOCK);
        assert!(s.is_valid(Status::VALID_TREE));
        assert!(!s.is_valid(Status::VALID_TRANSACTIONS));
        assert!(!Status(Status::VALID_SCRIPTS | Status::FAILED_CHILD).is_valid(Status::VALID_TREE));
    }

    #[test]
    fn raise_validity_keeps_flags() {
        let (s, changed) = Status(Status::VALID_TREE | Status::HAVE_DATA).raise_validity(3);
        assert!(changed);
        assert_eq!(s, Status(Status::VALID_TRANSACTIONS | Status::HAVE_DATA));
        assert_eq!(
            Status(Status::FAILED_VALID).raise_validity(3),
            (Status(Status::FAILED_VALID), false)
        );
    }
}
