//! `CSuperblock`: the trigger a governance object's data describes, its format, heights, payments
//! limit, expiry and validity (EV3's `IsValid`, notes D2).

use dash_consensus::Tx;
use dash_consensus::subsidy::{SubsidyError, superblock_payments_limit};
use dash_evo::payments::superblock_valid;
use dash_primitives::{GovObject, TxOut};

use crate::GovParams;
use crate::json::Value;
use crate::object::{MAX_MONEY, TRIGGER, json_object, object_type};
use crate::strings::{
    Destination, decode_destination, hash_hex, hex_str, parse_fixed_point, parse_hash_str,
    split_string, value_from_amount,
};

/// `SeenObjectStatus`, as far as a held trigger uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Unknown,
    Valid,
    Executed,
    ErrorInvalid,
}

/// `CGovernancePayment`: a destination (its script is `GetScriptForDestination`'s), the amount and
/// the proposal paid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payment {
    pub dest: Destination,
    pub amount: i64,
    /// Internal order.
    pub proposal_hash: [u8; 32],
}

/// A parsed trigger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Superblock {
    /// `nBlockHeight`: `event_block_height`.
    pub height: i32,
    pub payments: Vec<Payment>,
    pub status: Status,
}

/// `ParsePaymentAmount`: Core's amount text (digits and one inner point, at most 20 characters)
/// within the money range.
// Core: governance/superblock.cpp:22-64
pub fn parse_payment_amount(s: &[u8]) -> Result<i64, String> {
    const F: &str = "ParsePaymentAmount";
    if s.is_empty() {
        return Err(format!("{F} -- Amount is empty"));
    }
    if s.len() > 20 {
        return Err(format!("{F} -- Amount string too long"));
    }
    if !s.iter().all(|&c| c == b'.' || c.is_ascii_digit()) {
        return Err(format!("{F} -- Amount string contains invalid character"));
    }
    let mut points = s.iter().enumerate().filter(|&(_, &c)| c == b'.').map(|(i, _)| i);
    if let Some(first) = points.next() {
        if first == 0 {
            return Err(format!("{F} -- Invalid amount string, leading decimal point not allowed"));
        }
        if points.next().is_some() {
            return Err(format!("{F} -- Invalid amount string, too many decimal points"));
        }
    }
    // ASCII by the character check above.
    let text = String::from_utf8_lossy(s);
    let amount = parse_fixed_point(s, 8)
        .ok_or_else(|| format!("{F} -- ParseFixedPoint failed for string \"{text}\""))?;
    if !(0..=MAX_MONEY).contains(&amount) {
        return Err(format!("{F} -- Invalid amount string, value outside of valid money range"));
    }
    Ok(amount)
}

/// A string field of the trigger's JSON (`obj[key].get_str()`).
fn field<'a>(obj: &'a Value, key: &[u8]) -> Result<&'a [u8], String> {
    obj.get(key)?.get_str()
}

impl Superblock {
    /// `CSuperblock(govObj, nHash)`: the trigger `obj` describes, or the exception's `what()`.
    /// Core's messages quote the address and hash strings; bytes that are not UTF-8 are replaced
    /// with U+FFFD here (notes D4).
    // Core: governance/superblock.cpp:75-105
    pub fn parse(obj: &GovObject, p: &GovParams) -> Result<Superblock, String> {
        if object_type(obj) != TRIGGER {
            return Err("CSuperblock: Governance Object not a trigger".to_owned());
        }
        let json = json_object(&obj.data)?;
        if json.get(b"type")?.get_i32()? != TRIGGER {
            return Err("CSuperblock: invalid data type".to_owned());
        }
        let height = json.get(b"event_block_height")?.get_i32()?;
        let addresses = field(&json, b"payment_addresses")?;
        let amounts = field(&json, b"payment_amounts")?;
        let hashes = field(&json, b"proposal_hashes")?;
        Ok(Superblock {
            height,
            payments: parse_payment_schedule(addresses, amounts, hashes, p)?,
            status: Status::Unknown,
        })
    }

    /// `GetPaymentsTotalAmount()`: Core's unchecked sum, which wraps in the release builds.
    // Core: governance/superblock.cpp:237-240
    pub fn total(&self) -> i64 {
        self.payments.iter().fold(0i64, |sum, payment| sum.wrapping_add(payment.amount))
    }

    /// The payments as `CTxOut`s, in order (`GetSuperblockPayments`).
    // Core: governance/superblock.cpp:633-666
    pub fn tx_outs(&self) -> Vec<TxOut> {
        self.payments
            .iter()
            .map(|payment| TxOut {
                value: payment.amount,
                script_pubkey: payment.dest.script().into(),
            })
            .collect()
    }

    /// `IsValid(active_chain, txNew, block_height, blockReward, is_v24)`: EV3's port.
    // Core: governance/superblock.cpp:248-334
    pub fn is_valid(
        &self,
        coinbase: &Tx,
        height: u32,
        block_reward: i64,
        is_v24: bool,
        p: &GovParams,
    ) -> Result<bool, SubsidyError> {
        superblock_valid(coinbase, height, &self.tx_outs(), block_reward, is_v24, p.payments())
    }

    /// `IsExpired(heightToTest)`; `None` where an `int` sum in it overflows (undefined behaviour
    /// in Core, notes D13).
    // Core: governance/superblock.cpp:339-375
    pub fn is_expired(&self, height_to_test: i32, p: &GovParams) -> Option<bool> {
        let cycle = p.superblock_cycle();
        let blocks = match self.status {
            Status::Executed => cycle,
            Status::Valid => cycle.min(576),
            _ => cycle.min(24),
        };
        let expiration = self.height.checked_add(blocks)?;
        if height_to_test > expiration {
            return Some(true);
        }
        if !p.is_mainnet() && height_to_test.checked_add(cycle.checked_mul(2)?)? < self.height {
            return Some(true);
        }
        Some(false)
    }

    /// `GetHexStrData()`: the trigger's data as Core writes it, hex-encoded.
    // Core: governance/superblock.cpp:389-413
    pub fn hex_str_data(&self, p: &GovParams) -> String {
        let join = |f: &dyn Fn(&Payment) -> String| {
            self.payments.iter().map(f).collect::<Vec<_>>().join("|")
        };
        let addresses = join(&|payment| payment.dest.encode(p.chain()));
        let amounts = join(&|payment| value_from_amount(payment.amount));
        let hashes = join(&|payment| hash_hex(&payment.proposal_hash));
        let json = format!(
            "{{\"event_block_height\": {}, \"payment_addresses\": \"{addresses}\", \"payment_amounts\": \"{amounts}\", \"proposal_hashes\": \"{hashes}\", \"type\":2}}",
            self.height
        );
        hex_str(json.as_bytes())
    }
}

/// `ParsePaymentSchedule`. `GetScriptForDestination` never throws for the destinations
/// `DecodeDestination` returns, so its "Invalid payment found" branch is not reachable.
// Core: governance/superblock.cpp:165-225
fn parse_payment_schedule(
    addresses: &[u8],
    amounts: &[u8],
    hashes: &[u8],
    p: &GovParams,
) -> Result<Vec<Payment>, String> {
    const F: &str = "CSuperblock::ParsePaymentSchedule";
    let addresses = split_string(addresses, b'|');
    let amounts = split_string(amounts, b'|');
    let hashes = split_string(hashes, b'|');
    if addresses.len() != amounts.len() || addresses.len() != hashes.len() {
        return Err(format!("{F} -- Mismatched payments, amounts and proposalHashes"));
    }
    if addresses.is_empty() {
        return Err(format!("{F} -- Error no payments"));
    }
    let mut payments = Vec::with_capacity(addresses.len());
    for ((address, amount), hash) in addresses.into_iter().zip(amounts).zip(hashes) {
        let dest = decode_destination(address, p.chain()).ok_or_else(|| {
            format!("{F} -- Invalid Dash Address: {}", String::from_utf8_lossy(address))
        })?;
        let amount = parse_payment_amount(amount)?;
        let proposal_hash = parse_hash_str(hash).ok_or_else(|| {
            format!("{F} -- Invalid proposal hash: {}", String::from_utf8_lossy(hash))
        })?;
        payments.push(Payment {
            dest,
            amount,
            proposal_hash,
        });
    }
    Ok(payments)
}

/// `IsValidBlockHeight`: at or after `nSuperblockStartBlock`, a multiple of the cycle.
// Core: governance/superblock.cpp:119-124
pub fn is_valid_block_height(height: i32, p: &GovParams) -> bool {
    p.payments().subsidy().config().superblocks.is_superblock_height(height)
}

/// `GetNearestSuperblocksHeights`: the last and the next superblock heights.
// Core: governance/superblock.cpp:126-143
pub fn nearest_superblock_heights(height: i32, p: &GovParams) -> (i32, i32) {
    let (start, cycle) = (p.superblock_start(), p.superblock_cycle());
    let first = start + (cycle - start % cycle) % cycle;
    if height < first {
        (0, first)
    } else {
        let last = height - height % cycle;
        (last, last + cycle)
    }
}

/// `GetPaymentsLimit(active_chain, height)`: 0 off a superblock height. `Err` where Core's
/// subsidy read asserts or overflows (EV3, the oracle's -1).
// Core: governance/superblock.cpp:145-163
pub fn payments_limit(height: i32, p: &GovParams) -> Result<i64, SubsidyError> {
    superblock_payments_limit(height, p.payments().subsidy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::params;
    use dash_params::Network;

    #[test]
    fn amounts() {
        assert_eq!(parse_payment_amount(b"1.5"), Ok(150_000_000));
        assert_eq!(
            parse_payment_amount(b""),
            Err("ParsePaymentAmount -- Amount is empty".to_owned())
        );
        assert_eq!(
            parse_payment_amount(b"123456789012345678901"),
            Err("ParsePaymentAmount -- Amount string too long".to_owned())
        );
        // 20 characters are read (and `ParseFixedPoint` takes at most 18 integer digits).
        assert_eq!(
            parse_payment_amount(b"12345678901234567890"),
            Err("ParsePaymentAmount -- ParseFixedPoint failed for string \"12345678901234567890\""
                .to_owned())
        );
        assert_eq!(
            parse_payment_amount(b"1e5"),
            Err("ParsePaymentAmount -- Amount string contains invalid character".to_owned())
        );
        assert_eq!(
            parse_payment_amount(b".5"),
            Err("ParsePaymentAmount -- Invalid amount string, leading decimal point not allowed"
                .to_owned())
        );
        assert_eq!(
            parse_payment_amount(b"1.2.3"),
            Err("ParsePaymentAmount -- Invalid amount string, too many decimal points".to_owned())
        );
        assert_eq!(
            parse_payment_amount(b"1."),
            Err("ParsePaymentAmount -- ParseFixedPoint failed for string \"1.\"".to_owned())
        );
        assert_eq!(
            parse_payment_amount(b"21000001"),
            Err("ParsePaymentAmount -- Invalid amount string, value outside of valid money range"
                .to_owned())
        );
    }

    #[test]
    fn heights() {
        let p = params(Network::Main);
        let cycle = p.superblock_cycle();
        let (_, first) = nearest_superblock_heights(0, &p);
        assert!(first >= p.superblock_start() && first % cycle == 0);
        assert!(is_valid_block_height(first, &p));
        assert!(!is_valid_block_height(first + 1, &p));
        assert!(!is_valid_block_height(first - cycle, &p));
        assert_eq!(nearest_superblock_heights(first + 1, &p), (first, first + cycle));
        assert_eq!(nearest_superblock_heights(first, &p), (first, first + cycle));
        assert_eq!(nearest_superblock_heights(first - 1, &p), (0, first));
        assert_eq!(payments_limit(first + 1, &p), Ok(0));
        assert!(payments_limit(first, &p).unwrap() > 0);
    }

    #[test]
    fn expiry() {
        let p = params(Network::Testnet3);
        let cycle = p.superblock_cycle();
        let mut sb = Superblock {
            height: 1000,
            payments: Vec::new(),
            status: Status::Valid,
        };
        let horizon = cycle.min(576);
        assert_eq!(sb.is_expired(1000 + horizon, &p), Some(false));
        assert_eq!(sb.is_expired(1000 + horizon + 1, &p), Some(true));
        // Off mainnet (testnet, devnet and regtest: `NetworkIDString() != MAIN`,
        // superblock.cpp:366-371) a trigger far in the future expires too.
        for network in [Network::Testnet3, Network::Devnet, Network::Regtest] {
            let p = params(network);
            let cycle = p.superblock_cycle();
            assert_eq!(sb.is_expired(1000 - 2 * cycle - 1, &p), Some(true), "{network:?}");
            assert_eq!(sb.is_expired(1000 - 2 * cycle, &p), Some(false), "{network:?}");
        }
        sb.height = i32::MAX;
        assert_eq!(sb.is_expired(0, &p), None);
        sb.status = Status::Executed;
        sb.height = 1000;
        assert_eq!(sb.is_expired(1000 + cycle, &p), Some(false));
        // Mainnet's cycle tells the three horizons apart, and never expires a future trigger.
        let p = params(Network::Main);
        let cycle = p.superblock_cycle();
        assert!(cycle > 576);
        for (status, horizon) in
            [(Status::Executed, cycle), (Status::Valid, 576), (Status::Unknown, 24)]
        {
            sb.status = status;
            assert_eq!(sb.is_expired(1000 + horizon, &p), Some(false), "{status:?}");
            assert_eq!(sb.is_expired(1000 + horizon + 1, &p), Some(true), "{status:?}");
        }
        assert_eq!(sb.is_expired(0, &p), Some(false));
    }

    /// A mainnet trigger object paying 1 address `amount` at `height` (JSON text, as Core writes it).
    fn trigger_object(height: &str, amount: &str) -> GovObject {
        use dash_primitives::{OutPoint, TxHash};
        use dash_types::Numeric;
        let data = format!(
            "{{\"event_block_height\": {height}, \"payment_addresses\": \"XwyHjrAVXFSs2uHKw5BB2U31m8D41izE24\", \"payment_amounts\": \"{amount}\", \"proposal_hashes\": \"{}\", \"type\":2}}",
            "11".repeat(32)
        );
        GovObject {
            hash_parent: TxHash::from_lendian([0; 32]),
            revision: 1,
            time: 0,
            collateral_hash: TxHash::from_lendian([0; 32]),
            data: data.into_bytes(),
            object_type: Numeric::from_base(TRIGGER),
            masternode_outpoint: OutPoint {
                hash: TxHash::from_lendian([0; 32]),
                index: u32::MAX,
            },
            sig: vec![],
        }
    }

    /// `event_block_height` is read with `getInt<int>`: a value above `INT_MAX` throws
    /// (`superblock.cpp:95`, `univalue.h:136-147`).
    #[test]
    fn event_height_is_an_int() {
        let p = params(Network::Main);
        let parse =
            |height: &str| Superblock::parse(&trigger_object(height, "1"), &p).map(|sb| sb.height);
        assert_eq!(parse("2147483647"), Ok(i32::MAX));
        assert_eq!(parse("2147483648"), Err("JSON integer out of range".to_owned()));
        assert_eq!(parse("-2147483649"), Err("JSON integer out of range".to_owned()));
    }

    /// `IsValid`'s height gate alone (`superblock.cpp:254-258`): a trigger paying 0 (Core's
    /// `ParsePaymentAmount` takes `"0"`, `CGovernancePayment` is valid for any amount,
    /// `superblock.cpp:415-424`) passes the payments limit off a superblock height too, where
    /// `GetPaymentsLimit` is 0 (`:149-151`), so only the gate rejects at `h + 1`.
    #[test]
    fn height_gate_alone_rejects_off_cycle() {
        use dash_consensus::primitives::Transaction;
        use dash_primitives::TxType;
        use dash_types::Numeric;
        let p = params(Network::Main);
        let (_, h) = nearest_superblock_heights(p.superblock_start(), &p);
        let sb = Superblock::parse(&trigger_object(&h.to_string(), "0"), &p)
            .expect("a 0 payment parses");
        assert_eq!(sb.total(), 0);
        let reward = 5 * crate::strings::COIN;
        let mut outputs = vec![TxOut {
            value: reward,
            script_pubkey: vec![0x51].into(),
        }];
        outputs.extend(sb.tx_outs());
        let coinbase = Tx::new(Transaction {
            version: 3,
            tx_type: TxType::from_base(5),
            inputs: vec![],
            outputs,
            lock_time: 0,
            extra_payload: vec![],
        });
        let at = |height: i32| sb.is_valid(&coinbase, height as u32, reward, false, &p);
        assert_eq!(payments_limit(h + 1, &p), Ok(0));
        assert_eq!(at(h), Ok(true));
        assert_eq!(at(h + 1), Ok(false));
    }

    #[test]
    fn schedule_lengths() {
        let p = params(Network::Main);
        let mismatched = Err(
            "CSuperblock::ParsePaymentSchedule -- Mismatched payments, amounts and proposalHashes"
                .to_owned(),
        );
        assert_eq!(parse_payment_schedule(b"a|b", b"1|2", b"h", &p), mismatched);
        assert_eq!(parse_payment_schedule(b"a", b"1|2", b"h|h", &p), mismatched);
    }
}
