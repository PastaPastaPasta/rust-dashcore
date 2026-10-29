//! Core's `CBloomFilter` (`common/bloom.cpp:29-305`): BIP37 filters with Dash's special-transaction
//! matching (P4b D8).

use dash_consensus::primitives::{OutPoint, Tx, TxHash, TxOut};
use dash_consensus::script::interpreter::MAX_PUBKEYS_PER_MULTISIG;
use dash_consensus::script::opcodes::{OP_1, OP_16, OP_CHECKMULTISIG, OP_CHECKSIG, OP_PUSHDATA4};
use dash_consensus::script::parse::{check_minimal_push, get_op};
use dash_consensus::script::{DEFAULT_MAX_NUM_SIZE, ScriptNum};
use dash_p2p::bloom::murmur3;
use dash_p2p_core::FilterLoad;
use dash_p2p_core::limits::{MAX_BLOOM_FILTER_SIZE, MAX_HASH_FUNCS};
use dash_primitives::{PayoutShare, SpecialPayload};

/// `BLOOM_UPDATE_ALL`: a matched output's outpoint is added to the filter.
pub const BLOOM_UPDATE_ALL: u8 = 1;
/// `BLOOM_UPDATE_P2PUBKEY_ONLY`: only a matched pay-to-pubkey or bare multisig output's.
pub const BLOOM_UPDATE_P2PUBKEY_ONLY: u8 = 2;
/// `BLOOM_UPDATE_MASK`.
pub const BLOOM_UPDATE_MASK: u8 = 3;

/// A peer's filter, Core's `CBloomFilter` as `filterload` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BloomFilter {
    data: Vec<u8>,
    hash_funcs: u32,
    tweak: u32,
    flags: u8,
}

impl From<FilterLoad> for BloomFilter {
    fn from(f: FilterLoad) -> Self {
        Self {
            data: f.data,
            hash_funcs: f.hash_funcs,
            tweak: f.tweak,
            flags: f.flags,
        }
    }
}

/// Core's `COutPoint` serialization: the hash, then the index, both little-endian.
fn outpoint_bytes(hash: &TxHash, index: u32) -> [u8; 36] {
    let mut out = [0u8; 36];
    out[..32].copy_from_slice(hash.as_bytes());
    out[32..].copy_from_slice(&index.to_le_bytes());
    out
}

impl BloomFilter {
    // Core: common/bloom.cpp:29-45 CBloomFilter::CBloomFilter
    /// Core's constructor: the size and hash-function count for `elements` at false-positive rate
    /// `fp_rate`, capped at the protocol limits. Clients build filters this way; the node only
    /// reads them, so this is for tests and the sim's clients.
    pub fn new(elements: u32, fp_rate: f64, tweak: u32, flags: u8) -> Self {
        const LN2SQUARED: f64 = 0.480_453_013_918_201_4;
        const LN2: f64 = std::f64::consts::LN_2;
        let bits = (-1.0 / LN2SQUARED * f64::from(elements) * fp_rate.ln()) as u32;
        let size = bits.min(MAX_BLOOM_FILTER_SIZE as u32 * 8) / 8;
        let hash_funcs = (f64::from(size * 8 / elements) * LN2) as u32;
        Self {
            data: vec![0; size as usize],
            hash_funcs: hash_funcs.min(MAX_HASH_FUNCS),
            tweak,
            flags,
        }
    }

    /// The `filterload` message carrying this filter.
    pub fn to_msg(&self) -> FilterLoad {
        FilterLoad {
            data: self.data.clone(),
            hash_funcs: self.hash_funcs,
            tweak: self.tweak,
            flags: self.flags,
        }
    }

    // Core: common/bloom.cpp:93-96 CBloomFilter::IsWithinSizeConstraints
    /// The `filterload` limits: at most 36,000 bytes and 50 hash functions.
    pub fn is_within_size_constraints(&self) -> bool {
        self.data.len() <= MAX_BLOOM_FILTER_SIZE && self.hash_funcs <= MAX_HASH_FUNCS
    }

    // Core: common/bloom.cpp:47-51 CBloomFilter::Hash
    fn bit(&self, n: u32, key: &[u8]) -> usize {
        let h = murmur3(n.wrapping_mul(0xFBA4_C795).wrapping_add(self.tweak), key);
        h as usize % (self.data.len() * 8)
    }

    // Core: common/bloom.cpp:53-63 CBloomFilter::insert
    /// Adds `key`. An empty filter stays empty (CVE-2013-5700).
    pub fn insert(&mut self, key: &[u8]) {
        if self.data.is_empty() {
            return;
        }
        for n in 0..self.hash_funcs {
            let i = self.bit(n, key);
            self.data[i >> 3] |= 1 << (i & 7);
        }
    }

    // Core: common/bloom.cpp:72-84 CBloomFilter::contains
    /// Whether `key` may be in the filter. An empty filter contains everything.
    pub fn contains(&self, key: &[u8]) -> bool {
        if self.data.is_empty() {
            return true;
        }
        (0..self.hash_funcs).all(|n| {
            let i = self.bit(n, key);
            self.data[i >> 3] & (1 << (i & 7)) != 0
        })
    }

    fn contains_outpoint(&self, out: &OutPoint) -> bool {
        self.contains(&outpoint_bytes(&out.hash, out.index))
    }

    fn update_all(&self) -> bool {
        self.flags & BLOOM_UPDATE_MASK == BLOOM_UPDATE_ALL
    }

    // Core: common/bloom.cpp:98-111 CBloomFilter::CheckScript
    /// Whether the filter contains a non-empty push of `script`, read up to its first bad
    /// instruction.
    fn check_script(&self, script: &[u8]) -> bool {
        let mut pc = 0;
        while pc < script.len() {
            let Some(op) = get_op(script, &mut pc) else {
                break;
            };
            if !op.data.is_empty() && self.contains(op.data) {
                return true;
            }
        }
        false
    }

    // Core: common/bloom.cpp:248-269 CBloomFilter::ProcessTxOut
    fn process_tx_out(&mut self, out: &TxOut, hash: &TxHash, index: u32) -> bool {
        let script = out.script_pubkey.as_bytes();
        if !self.check_script(script) {
            return false;
        }
        let mode = self.flags & BLOOM_UPDATE_MASK;
        if mode == BLOOM_UPDATE_ALL
            || mode == BLOOM_UPDATE_P2PUBKEY_ONLY
                && (is_pay_to_pubkey(script) || is_multisig(script))
        {
            self.insert(&outpoint_bytes(hash, index));
        }
        true
    }

    // Core: common/bloom.cpp:271-305 CBloomFilter::IsRelevantAndUpdate
    /// Whether `tx` matches, adding what the filter's update mode asks for.
    pub fn is_relevant_and_update(&mut self, tx: &Tx) -> bool {
        if self.data.is_empty() {
            return true;
        }
        let hash = tx.txid();
        let mut found = self.contains(hash.as_bytes());
        found = found || self.special_tx_matches_and_update(tx);
        for (i, out) in tx.outputs().iter().enumerate() {
            found = self.process_tx_out(out, &hash, i as u32) || found;
        }
        if found {
            return true;
        }
        tx.inputs().iter().any(|input| {
            self.contains_outpoint(&input.prevout) || self.check_script(input.script_sig.as_bytes())
        })
    }

    /// `GetOwnerPayouts` (evo/providertx.h:94-97) tested with `CheckScript`: the payout list from
    /// `ProTxVersion::ExtAddr` (3), else `LegacyPayoutAsList(scriptPayout)`
    /// (evo/providertx_util.cpp:19-22).
    fn owner_payout_matches(
        &self,
        version: u16,
        script_payout: &[u8],
        payouts: &[PayoutShare],
    ) -> bool {
        if version >= 3 {
            payouts.iter().any(|p| self.check_script(p.script_payout.as_bytes()))
        } else {
            self.check_script(script_payout)
        }
    }

    // Core: common/bloom.cpp:127-246 CBloomFilter::CheckSpecialTransactionMatchesAndUpdate
    /// Dash's special-transaction fields. A payload that does not decode, or that the decoder does
    /// not read to the end, matches nothing (`GetTxPayload`, evo/specialtx.h:17-35).
    fn special_tx_matches_and_update(&mut self, tx: &Tx) -> bool {
        let Some(Ok(decoded)) = tx.payload() else {
            return false;
        };
        if !decoded.is_complete() {
            return false;
        }
        match &decoded.payload {
            SpecialPayload::ProviderRegister(p) => {
                let payout =
                    self.owner_payout_matches(p.version, p.script_payout.as_bytes(), &p.payouts);
                let share = p.shares.iter().any(|s| {
                    // `CCollateralShare::RewardScript()` (evo/providertx.h:80).
                    let reward = if s.script_reward.is_empty() {
                        &s.script_refund
                    } else {
                        &s.script_reward
                    };
                    self.check_script(s.script_refund.as_bytes())
                        || self.check_script(reward.as_bytes())
                        || self.contains(s.key_id_owner.as_bytes())
                });
                let found = self.contains(&outpoint_bytes(&p.collateral_hash, p.collateral_index))
                    || self.contains(p.key_id_owner.as_bytes())
                    || self.contains(p.key_id_voting.as_bytes())
                    || payout
                    || share;
                if found && self.update_all() {
                    self.insert(tx.txid().as_bytes());
                }
                found
            }
            SpecialPayload::ProviderUpdateService(p) => self
                .update_payload_matches(&p.pro_tx_hash, |f| {
                    f.check_script(p.script_operator_payout.as_bytes())
                }),
            SpecialPayload::ProviderUpdateRegistrar(p) => {
                self.update_payload_matches(&p.pro_tx_hash, |f| {
                    f.contains(p.key_id_voting.as_bytes())
                        || f.owner_payout_matches(p.version, p.script_payout.as_bytes(), &p.payouts)
                })
            }
            SpecialPayload::ProviderUpdateRevoke(p) => self.contains(p.pro_tx_hash.as_bytes()),
            // The refunds are ordinary outputs, which the output loop matches.
            SpecialPayload::ProviderDissolve(p) => self.contains(p.pro_tx_hash.as_bytes()),
            SpecialPayload::ProviderUpdateShare(p) => self
                .update_payload_matches(&p.pro_tx_hash, |f| {
                    f.check_script(p.script_reward.as_bytes())
                }),
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => self
                .update_payload_matches(&p.pro_tx_hash, |f| f.contains(p.key_id_voting.as_bytes())),
            SpecialPayload::AssetLock(p) => {
                // `ProcessTxOut` with the credit output's index among the credit outputs.
                let hash = tx.txid();
                let mut found = false;
                for (i, out) in p.credit_outputs.iter().enumerate() {
                    found = self.process_tx_out(out, &hash, i as u32) || found;
                }
                found
            }
            SpecialPayload::AssetUnlock(_)
            | SpecialPayload::CoinbaseCommitment(_)
            | SpecialPayload::QuorumCommitment(_)
            | SpecialPayload::MnhfSignal(_) => false,
            // A provider payload of version 0 or above `ExtAddr`: Core reads only the version, so
            // a complete decode is a 2-byte payload whose other fields Core leaves at their
            // defaults. Consensus refuses it (`bad-protx-version`), so no served block holds one;
            // it matches nothing here.
            SpecialPayload::UnknownProTxVersion {
                ..
            } => false,
            SpecialPayload::Unknown {
                ..
            } => {
                tracing::info!("Unknown special transaction type in Bloom filter check.");
                false
            }
        }
    }

    /// The shape Core repeats for every update payload: it matches when the filter holds the ProTx
    /// hash, else when `fields` says so, and a match on the fields inserts the ProTx hash under
    /// `BLOOM_UPDATE_ALL`.
    fn update_payload_matches(
        &mut self,
        pro_tx_hash: &TxHash,
        fields: impl FnOnce(&Self) -> bool,
    ) -> bool {
        if self.contains(pro_tx_hash.as_bytes()) {
            return true;
        }
        let found = fields(self);
        if found && self.update_all() {
            self.insert(pro_tx_hash.as_bytes());
        }
        found
    }
}

// Core: pubkey.h:59-78 CPubKey::GetLen, ValidSize
fn pubkey_valid_size(data: &[u8]) -> bool {
    let len = match data.first() {
        Some(2 | 3) => 33,
        Some(4 | 6 | 7) => 65,
        _ => return false,
    };
    data.len() == len
}

// Core: script/standard.cpp:42-53 MatchPayToPubkey
/// `Solver`'s `PUBKEY`: a 65- or 33-byte push of a key of that size, then `OP_CHECKSIG`.
fn is_pay_to_pubkey(script: &[u8]) -> bool {
    [65usize, 33].iter().any(|&size| {
        script.len() == size + 2
            && script[0] as usize == size
            && script[size + 1] == OP_CHECKSIG
            && pubkey_valid_size(&script[1..=size])
    })
}

// Core: script/standard.cpp:70-93 IsValidMultisigKeyCount, GetMultisigKeyCount
fn multisig_key_count(opcode: u8, data: &[u8]) -> Option<i64> {
    let count = if (OP_1..=OP_16).contains(&opcode) {
        i64::from(opcode - (OP_1 - 1))
    } else if opcode <= OP_PUSHDATA4 {
        if !check_minimal_push(data, opcode) {
            return None;
        }
        i64::from(ScriptNum::decode(data, true, DEFAULT_MAX_NUM_SIZE).ok()?.getint())
    } else {
        return None;
    };
    (count > 0 && count <= i64::from(MAX_PUBKEYS_PER_MULTISIG)).then_some(count)
}

// Core: script/standard.cpp:95-113 MatchMultisig
/// `Solver`'s `MULTISIG`: `m <keys> n OP_CHECKMULTISIG`, `n` the number of keys and `m ≤ n`.
fn is_multisig(script: &[u8]) -> bool {
    if script.last() != Some(&OP_CHECKMULTISIG) {
        return false;
    }
    let mut pc = 0;
    let Some(first) = get_op(script, &mut pc) else {
        return false;
    };
    let Some(required) = multisig_key_count(first.opcode, first.data) else {
        return false;
    };
    let mut keys = 0i64;
    // Core's loop stops at the first instruction that is not a key. A failed read leaves
    // `OP_INVALIDOPCODE`, which is no key count.
    let last = loop {
        match get_op(script, &mut pc) {
            Some(op) if pubkey_valid_size(op.data) => keys += 1,
            Some(op) => break op,
            None => return false,
        }
    };
    let Some(count) = multisig_key_count(last.opcode, last.data) else {
        return false;
    };
    keys == count && count >= required && pc + 1 == script.len()
}

/// Helpers the bloom and merkle tests share: Core's `bloom_tests.cpp` objects, extracted by
/// `contrib/node/p4b-bloom-vectors.py` into `tests/fixtures/core-bloom`.
#[cfg(test)]
pub(super) mod vectors {
    use dash_consensus::primitives::{Block, Tx, decode_block, decode_tx_exact};

    pub fn unhex(s: &str) -> Vec<u8> {
        let s = s.trim();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// `uint256S`: a display-order hex string as the little-endian bytes Core stores.
    pub fn u256(s: &str) -> [u8; 32] {
        let mut b: [u8; 32] = unhex(s.trim_start_matches("0x")).try_into().unwrap();
        b.reverse();
        b
    }

    pub fn tx(hex: &str) -> Tx {
        decode_tx_exact(&unhex(hex)).unwrap().into_value()
    }

    pub fn block(hex: &str) -> Block {
        decode_block(&mut &unhex(hex)[..]).unwrap().into_value()
    }

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../../tests/fixtures/core-bloom/", $name, ".hex"))
        };
    }
    pub(crate) use fixture;
}

#[cfg(test)]
mod tests {
    use dash_types::Numeric as _;

    use super::vectors::{fixture, tx, u256, unhex};
    use super::*;

    const BLOOM_UPDATE_NONE: u8 = 0;

    fn core_filter(flags: u8) -> BloomFilter {
        BloomFilter::new(10, 0.000_001, 0, flags)
    }

    fn outpoint(hash: &str, n: u32) -> [u8; 36] {
        outpoint_bytes(&TxHash::from_lendian(u256(hash)), n)
    }

    fn wire(f: &BloomFilter) -> String {
        use dash_types::codec::BaseCodec as _;
        let mut out = Vec::new();
        f.to_msg().encode(&mut out);
        out.iter().map(|b| format!("{b:02x}")).collect()
    }

    // Core: src/test/bloom_tests.cpp bloom_create_insert_serialize(_with_tweak)
    #[test]
    fn create_insert_serialize() {
        for (tweak, expected) in
            [(0, "03614e9b050000000000000001"), (2_147_483_649, "03ce4299050000000100008001")]
        {
            let mut f = BloomFilter::new(3, 0.01, tweak, BLOOM_UPDATE_ALL);
            let a = unhex("99108ad8ed9bb6274d3980bab5a85c048f0950c8");
            assert!(tweak != 0 || !f.contains(&a), "empty");
            f.insert(&a);
            assert!(f.contains(&a));
            assert!(!f.contains(&unhex("19108ad8ed9bb6274d3980bab5a85c048f0950c8")));
            for k in [
                "b5a2c786d9ef4658287ced5914b37a1b4aa32eee",
                "b9300670b4c5366e95b2699e8b18bc75e5f729c5",
            ] {
                f.insert(&unhex(k));
                assert!(f.contains(&unhex(k)));
            }
            assert_eq!(wire(&f), expected);
        }
    }

    // Core: src/test/bloom_tests.cpp bloom_match
    #[test]
    fn bloom_match() {
        let t = tx(fixture!("bloom_match-tx"));
        let spending = tx(fixture!("bloom_match-spending"));
        let prev = "0x90c122d70786e899529d71dbeba91ba216982fb6ba58f3bdaab65e73b7e9260b";
        let matches = |key: &[u8], t: &Tx| {
            let mut f = core_filter(BLOOM_UPDATE_ALL);
            f.insert(key);
            f.is_relevant_and_update(t)
        };
        assert!(
            matches(
                &u256("0xb4749f017444b051c44dfd2720e88f314ff94f3dd6d56d40ef65854fcd7fff6b"),
                &t
            ),
            "tx hash"
        );
        assert!(
            matches(&unhex("6bff7fcd4f8565ef406dd5d63d4ff94f318fe82027fd4dc451b04474019f74b4"), &t),
            "serialized hash"
        );
        assert!(
            matches(
                &unhex(
                    "30450220070aca44506c5cef3a16ed519d7c3c39f8aab192c4e1c90d065f37b8a4af6141022100a8e160b856c2d43d27d8fba71e5aef6405b8643ac4cb7cb3c462aced7f14711a01"
                ),
                &t
            ),
            "input signature"
        );
        assert!(
            matches(
                &unhex(
                    "046d11fee51b0e60666d5049a9101a72741df480b96ee26488a4d3466b95c9a40ac5eeef87e10a5cd336c19a84565f80fa6c547957b7700ff4dfbdefe76036c339"
                ),
                &t
            ),
            "input pubkey"
        );
        let mut f = core_filter(BLOOM_UPDATE_ALL);
        f.insert(&unhex("04943fdd508053c75000106d3bc6e2754dbcff19"));
        assert!(f.is_relevant_and_update(&t), "output address");
        assert!(f.is_relevant_and_update(&spending), "the matched output was added");
        assert!(
            matches(&unhex("a266436d2965547608b9e15d9032a7b9d64fa431"), &t),
            "output address 2"
        );
        assert!(matches(&outpoint(prev, 0), &t), "COutPoint");
        assert!(
            !matches(&u256("00000009e784f32f62ef849763d4f45b98e07ba658647343b915ff832b110436"), &t),
            "random hash"
        );
        assert!(!matches(&unhex("0000006d2965547608b9e15d9032a7b9d64fa431"), &t), "random address");
        assert!(!matches(&outpoint(prev, 1), &t), "other output");
        assert!(
            !matches(
                &outpoint("0x000000d70786e899529d71dbeba91ba216982fb6ba58f3bdaab65e73b7e9260b", 0),
                &t
            ),
            "other outpoint"
        );
    }

    fn dip2() -> [Tx; 4] {
        [
            tx(fixture!("dip2-proregtx")),
            tx(fixture!("dip2-proupservtx")),
            tx(fixture!("dip2-proupregtx")),
            tx(fixture!("dip2-prouprevtx")),
        ]
    }

    // Core: src/test/bloom_tests.cpp dip2_bloom_match
    #[test]
    fn dip2_bloom_match() {
        let [reg, serv, upreg, rev] = dip2();
        let protx = u256("0x39a1339d9bf26de701345beecc5de75a690bc9533741a3dbe90f2fd88b8ed461");
        let payout = unhex("e54445646929fac8b7d6c71715913af443249784");
        let voting = unhex("359c348a574176c210c37a25d4ffd917866fb0a3");
        let matches = |key: &[u8], t: &Tx| {
            let mut f = core_filter(BLOOM_UPDATE_ALL);
            f.insert(key);
            f.is_relevant_and_update(t)
        };
        assert!(
            matches(
                &outpoint("0x23464abc2f724de235e69e72ef5068f1b2701521b88e7b2740b93978ff54909b", 1),
                &reg
            ),
            "collateral"
        );
        assert!(matches(&unhex("891cbc8a94fa7fea64ca9994870dca0f75bbd075"), &reg), "owner");
        assert!(matches(&voting, &reg), "voting");
        assert!(matches(&payout, &reg), "payout");
        assert!(matches(&protx, &serv), "ProUpServ hash");
        assert!(matches(&payout, &serv), "operator payout");
        assert!(matches(&protx, &upreg), "ProUpReg hash");
        assert!(matches(&voting, &upreg), "ProUpReg voting");
        assert!(matches(&payout, &upreg), "ProUpReg payout");
        assert!(matches(&protx, &rev), "ProUpRev hash");
        let mut f = core_filter(BLOOM_UPDATE_ALL);
        let mut longer = protx.to_vec();
        longer.insert(0, 0);
        f.insert(&longer);
        f.insert(&[payout.clone(), vec![0]].concat());
        f.insert(&[voting.clone(), vec![0]].concat());
        assert!(!f.is_relevant_and_update(&rev), "unrelated data");
        assert!(!f.is_relevant_and_update(&upreg), "unrelated data");
    }

    // Core: src/test/bloom_tests.cpp dip2_bloom_update
    #[test]
    fn dip2_bloom_update() {
        let [reg, serv, upreg, _] = dip2();
        let mut f = core_filter(BLOOM_UPDATE_ALL);
        f.insert(&outpoint(
            "0x23464abc2f724de235e69e72ef5068f1b2701521b88e7b2740b93978ff54909b",
            1,
        ));
        assert!(!f.is_relevant_and_update(&serv), "before the update");
        assert!(f.is_relevant_and_update(&reg));
        assert!(f.is_relevant_and_update(&serv), "the ProRegTx hash was added");

        let mut f = core_filter(BLOOM_UPDATE_ALL);
        f.insert(&unhex("359c348a574176c210c37a25d4ffd917866fb0a3"));
        assert!(!f.is_relevant_and_update(&serv), "before the update");
        assert!(f.is_relevant_and_update(&upreg));
        assert!(f.is_relevant_and_update(&serv), "the ProTx hash was added");

        // Not under BLOOM_UPDATE_NONE.
        let mut f = core_filter(BLOOM_UPDATE_NONE);
        f.insert(&unhex("359c348a574176c210c37a25d4ffd917866fb0a3"));
        assert!(f.is_relevant_and_update(&upreg));
        assert!(!f.is_relevant_and_update(&serv));
    }

    /// Dash Core v24's special transactions past the four DIP2 types (P4b review r1 M-3): each
    /// as dashd v24.0.0-rc.3 built it in one of Core's functional-test chains (`v24-*.hex`,
    /// `contrib/node/p4b-special-tx-vectors.py`), or that transaction with its payload changed
    /// through base-sdk's codecs. Each key is a value Core printed for the payload.
    mod v24 {
        use dash_consensus::primitives::Tx;
        use dash_primitives::{AssetLock, OutPoint, ProRegTx, ProUpRevTx, Transaction, TxOut};
        use dash_types::codec::BaseCodec;

        use super::*;

        fn raw(hex: &str) -> Transaction {
            Transaction::decode(&mut &unhex(hex)[..]).unwrap()
        }

        /// `hex` with its payload, read as `P`, changed by `edit`.
        fn with_payload<P: BaseCodec>(hex: &str, edit: impl FnOnce(&mut P)) -> Tx {
            let mut raw = raw(hex);
            let mut payload = P::decode(&mut &raw.extra_payload[..]).unwrap();
            edit(&mut payload);
            raw.extra_payload.clear();
            payload.encode(&mut raw.extra_payload);
            Tx::new(raw)
        }

        /// A 1 DASH output paying `script`.
        fn txout(script: &[u8]) -> TxOut {
            let mut bytes = 100_000_000i64.to_le_bytes().to_vec();
            bytes.push(script.len() as u8);
            bytes.extend_from_slice(script);
            TxOut::decode(&mut &bytes[..]).unwrap()
        }

        fn p2pkh(key_id: &[u8]) -> Vec<u8> {
            [&[0x76, 0xa9, 0x14][..], key_id, &[0x88, 0xac]].concat()
        }

        /// A ProUpRevTx naming `hash`: it matches when the filter holds `hash`.
        fn names(hash: &TxHash) -> Tx {
            with_payload::<ProUpRevTx>(fixture!("dip2-prouprevtx"), |p| p.pro_tx_hash = *hash)
        }

        /// A transaction spending `(hash, index)`: it matches when the filter holds that outpoint.
        fn spends(hash: &TxHash, index: u32) -> Tx {
            let mut raw = raw(fixture!("bloom_match-spending"));
            raw.inputs[0].prevout = OutPoint {
                hash: *hash,
                index,
            };
            Tx::new(raw)
        }

        fn protx(hash: &str) -> TxHash {
            TxHash::from_lendian(u256(hash))
        }

        /// `tx` matches a filter holding `key`: under `BLOOM_UPDATE_ALL` the match inserts what
        /// `probe` matches on, under `BLOOM_UPDATE_P2PUBKEY_ONLY` and `BLOOM_UPDATE_NONE` nothing
        /// (the special arms insert only under `BLOOM_UPDATE_ALL`, bloom.cpp:148, :161, :177,
        /// :204, :216; the probes' outputs are not pay-to-pubkey).
        fn matches_and_inserts(tx: &Tx, key: &[u8], probe: &Tx) {
            for (flags, inserted) in [
                (BLOOM_UPDATE_ALL, true),
                (BLOOM_UPDATE_P2PUBKEY_ONLY, false),
                (BLOOM_UPDATE_NONE, false),
            ] {
                let mut f = core_filter(flags);
                f.insert(key);
                assert!(!f.clone().is_relevant_and_update(probe), "the probe alone");
                assert!(f.is_relevant_and_update(tx), "flags {flags}: no match");
                assert_eq!(f.is_relevant_and_update(probe), inserted, "flags {flags}: insert");
            }
        }

        /// `tx` matches a filter holding `key` and inserts nothing `probe` matches on, under
        /// any update mode.
        fn matches_only(tx: &Tx, key: &[u8], probe: &Tx) {
            for flags in [BLOOM_UPDATE_ALL, BLOOM_UPDATE_P2PUBKEY_ONLY, BLOOM_UPDATE_NONE] {
                let mut f = core_filter(flags);
                f.insert(key);
                assert!(f.is_relevant_and_update(tx), "flags {flags}: no match");
                assert!(!f.is_relevant_and_update(probe), "flags {flags}: inserted");
            }
        }

        // Core: common/bloom.cpp:132-151; evo/providertx.h:94-97 GetOwnerPayouts
        #[test]
        fn proregtx_v3_payouts() {
            let reg = tx(fixture!("v24-proregtx-payouts"));
            let probe = names(&reg.txid());
            // The second of Core's two `payouts`; `scriptPayout` is empty from v3.
            matches_and_inserts(&reg, &unhex("bf2088d38438dcdcbcddd1992e3a038e01947750"), &probe);
            matches_and_inserts(&reg, &unhex("230ed8083d34c8e12afc770280eb0619ade84ee1"), &probe);
        }

        /// `GetOwnerPayouts` reads the list from `ExtAddr` (3) up. base-sdk decodes a provider
        /// payload above 3 as `UnknownProTxVersion`, which matches nothing, so only this call
        /// gives the version test a 4.
        #[test]
        fn owner_payouts_from_ext_addr_up() {
            let reg = raw(fixture!("v24-proregtx-payouts"));
            let p = ProRegTx::decode(&mut &reg.extra_payload[..]).unwrap();
            assert!(p.script_payout.is_empty());
            let mut f = core_filter(BLOOM_UPDATE_ALL);
            f.insert(&unhex("bf2088d38438dcdcbcddd1992e3a038e01947750"));
            for (version, found) in [(1, false), (2, false), (3, true), (4, true)] {
                let got = f.owner_payout_matches(version, p.script_payout.as_bytes(), &p.payouts);
                assert_eq!(got, found, "version {version}");
            }
        }

        // Core: common/bloom.cpp:136-150; evo/providertx.h:80 CCollateralShare::RewardScript
        #[test]
        fn proregtx_v3_shares() {
            let reg = tx(fixture!("v24-proregtx-shares"));
            let probe = names(&reg.txid());
            // The second share's `scriptRefund`, then the two shares' `keyIDOwner`s.
            matches_and_inserts(&reg, &unhex("bcc1beef6d6ad2a781c39adcb72a446b839e29b9"), &probe);
            matches_and_inserts(&reg, &unhex("c374c77db723510a2717e27c91033e1c9431342a"), &probe);
            matches_and_inserts(&reg, &unhex("5a71973a879b5cd702e4ebad0eb96244e84e0627"), &probe);
            // Core built both shares with an empty `scriptReward` (`RewardScript()` is then
            // `scriptRefund`, which is tested first anyway). A share paying its reward
            // elsewhere matches on that script too.
            let reward = [0x5e; 20];
            let paid = with_payload::<ProRegTx>(fixture!("v24-proregtx-shares"), |p| {
                assert!(p.shares[0].script_reward.is_empty());
                p.shares[0].script_reward = txout(&p2pkh(&reward)).script_pubkey;
            });
            matches_and_inserts(&paid, &reward, &names(&paid.txid()));
            let mut f = core_filter(BLOOM_UPDATE_ALL);
            f.insert(&reward);
            assert!(!f.is_relevant_and_update(&reg), "Core's, without that script");
        }

        /// The operator payout matches and, under `BLOOM_UPDATE_ALL`, inserts the ProTx hash.
        // Core: common/bloom.cpp:153-166
        #[test]
        fn proupservtx_operator_payout_inserts_the_protx_hash() {
            let serv = tx(fixture!("v24-proupservtx-v3"));
            let hash = protx("5bb21f874f50967df6faf711521c13d0723991d8d1e8c00669c9f49eea248a01");
            let payout = unhex("d32aa4c2b35062de318b326ead281d0fd3a0dd48");
            matches_and_inserts(&serv, &payout, &names(&hash));
        }

        // Core: common/bloom.cpp:167-181
        #[test]
        fn proupregtx_v3_payouts() {
            let upreg = tx(fixture!("v24-proupregtx-payouts"));
            let hash = protx("5bb21f874f50967df6faf711521c13d0723991d8d1e8c00669c9f49eea248a01");
            // The last of Core's eight `payouts`.
            let payout = unhex("65e261ad7c3a55813c71b69030f01a911cab966e");
            matches_and_inserts(&upreg, &payout, &names(&hash));
        }

        /// ProDis matches on its ProTx hash only and inserts nothing; its refunds are outputs,
        /// which the output loop matches and inserts.
        // Core: common/bloom.cpp:189-197
        #[test]
        fn prodistx_hash_only() {
            let dis = tx(fixture!("v24-prodistx"));
            let hash = protx("7573c036d58e4c07b517a9469069c9b7cb3b1696e4657d2161636f29b4a38497");
            matches_only(&dis, hash.as_bytes(), &names(&dis.txid()));
            let refund = unhex("1911a6ec416466ca71a31d079e8f31faab249ad8");
            matches_and_inserts(&dis, &refund, &spends(&dis.txid(), 1));
        }

        // Core: common/bloom.cpp:198-210
        #[test]
        fn proupsharetx_reward_inserts_the_protx_hash() {
            let share = tx(fixture!("v24-proupsharetx"));
            let hash = protx("d49cd0723c76141530c7538a900f02acc0acd4b8c64be3a2160931414d7d5942");
            matches_only(&share, hash.as_bytes(), &names(&share.txid()));
            let reward = unhex("e95d37ab592405b93308cf78a5b159ce159a6e87");
            matches_and_inserts(&share, &reward, &names(&hash));
        }

        // Core: common/bloom.cpp:211-223
        #[test]
        fn proupsharedregtx_voting_inserts_the_protx_hash() {
            let shared = tx(fixture!("v24-proupsharedregtx"));
            let hash = protx("268a0feca109e52657331b4f5c4aada9e2f3d7013a6876cdcd988ff77ac2eb71");
            matches_only(&shared, hash.as_bytes(), &names(&shared.txid()));
            let voting = unhex("9c38dd39fd5fee325c06620bd3f62fb37d27a70a");
            matches_and_inserts(&shared, &voting, &names(&hash));
        }

        /// A credit output is `ProcessTxOut`'d with its index among the credit outputs, so the
        /// outpoint inserted is `(txid, credit index)`, which no output of the transaction has
        /// (the second credit output here is `(txid, 1)`; the vouts are 0 and 1, the credit
        /// outputs would follow them at 2 and 3).
        // Core: common/bloom.cpp:224-236
        #[test]
        fn assetlock_credit_outputs_insert_their_credit_index() {
            let key = [0x6b; 20];
            let lock = with_payload::<AssetLock>(fixture!("v24-assetlocktx"), |p| {
                assert_eq!(p.credit_outputs.len(), 1);
                p.credit_outputs.push(txout(&p2pkh(&key)));
            });
            assert_eq!(lock.outputs().len(), 2);
            matches_and_inserts(&lock, &key, &spends(&lock.txid(), 1));
            let mut f = core_filter(BLOOM_UPDATE_ALL);
            f.insert(&key);
            assert!(f.is_relevant_and_update(&lock));
            assert!(!f.is_relevant_and_update(&spends(&lock.txid(), 3)), "not after the vouts");
            // Core's own credit output, the first.
            let core = tx(fixture!("v24-assetlocktx"));
            let first = unhex("c71b73778695ccd1dc6b072f9f8d6ff027a10fcf");
            matches_and_inserts(&core, &first, &spends(&core.txid(), 0));
        }

        /// `BLOOM_UPDATE_P2PUBKEY_ONLY` on credit outputs: a pay-to-pubkey one inserts its
        /// outpoint, a pay-to-pubkey-hash one only matches.
        // Core: common/bloom.cpp:248-269 ProcessTxOut
        #[test]
        fn assetlock_credit_outputs_under_p2pubkey_only() {
            let hash = [0x6b; 20];
            let key = [&[0x02u8][..], &[0x77; 32]].concat();
            let p2pk = [&[33u8][..], &key, &[OP_CHECKSIG]].concat();
            let lock = with_payload::<AssetLock>(fixture!("v24-assetlocktx"), |p| {
                p.credit_outputs = vec![txout(&p2pkh(&hash)), txout(&p2pk)];
            });
            for (data, index, inserted) in [(&key[..], 1, true), (&hash[..], 0, false)] {
                let mut f = core_filter(BLOOM_UPDATE_P2PUBKEY_ONLY);
                f.insert(data);
                assert!(f.is_relevant_and_update(&lock));
                assert_eq!(f.is_relevant_and_update(&spends(&lock.txid(), index)), inserted);
            }
        }
    }

    #[test]
    fn size_constraints() {
        let f = |len: usize, hash_funcs: u32| BloomFilter {
            data: vec![0; len],
            hash_funcs,
            tweak: 0,
            flags: 0,
        };
        assert!(f(36_000, 50).is_within_size_constraints());
        assert!(!f(36_001, 50).is_within_size_constraints());
        assert!(!f(36_000, 51).is_within_size_constraints());
    }

    #[test]
    fn empty_filter_matches_everything_and_stays_empty() {
        let mut f = BloomFilter::from(FilterLoad {
            data: Vec::new(),
            hash_funcs: 10,
            tweak: 0,
            flags: BLOOM_UPDATE_ALL,
        });
        f.insert(b"x");
        assert!(f.data.is_empty());
        assert!(f.contains(b"anything"));
        assert!(f.is_relevant_and_update(&tx(fixture!("bloom_match-tx"))));
    }

    // Core: script/standard.cpp:42-113 MatchPayToPubkey, MatchMultisig
    #[test]
    fn solver_shapes() {
        let key33 = [&[0x02u8][..], &[7; 32]].concat();
        let key65 = [&[0x04u8][..], &[7; 64]].concat();
        let p2pk = |k: &[u8]| [&[k.len() as u8][..], k, &[OP_CHECKSIG]].concat();
        assert!(is_pay_to_pubkey(&p2pk(&key33)));
        assert!(is_pay_to_pubkey(&p2pk(&key65)));
        let mut bad = key33.clone();
        bad[0] = 0x04;
        assert!(!is_pay_to_pubkey(&p2pk(&bad)), "a 33-byte key with an uncompressed prefix");
        let ms = |m: u8, keys: &[&[u8]], n: u8| {
            let mut s = vec![m];
            for k in keys {
                s.push(k.len() as u8);
                s.extend_from_slice(k);
            }
            s.extend([n, OP_CHECKMULTISIG]);
            s
        };
        assert!(is_multisig(&ms(OP_1, &[&key33, &key65], OP_1 + 1)));
        assert!(!is_multisig(&ms(OP_1 + 2, &[&key33, &key65], OP_1 + 1)), "m > n");
        assert!(!is_multisig(&ms(OP_1, &[&key33], OP_1 + 1)), "n ≠ keys");
        let mut trailing = ms(OP_1, &[&key33], OP_1);
        trailing.insert(trailing.len() - 1, OP_CHECKSIG);
        assert!(!is_multisig(&trailing), "an instruction before OP_CHECKMULTISIG");
        // A minimal push of the count: 1 as `01 01` is not minimal (OP_1 is), so it fails.
        assert!(!is_multisig(
            &[&[1u8, 1][..], &p2pk(&key33)[..34], &[OP_1, OP_CHECKMULTISIG]].concat()
        ));
    }
}
