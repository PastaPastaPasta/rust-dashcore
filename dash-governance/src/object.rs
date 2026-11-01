//! `CGovernanceObject`'s pure parts over BS9b's wire type [`GovObject`]: the hashes, the type the
//! data declares, the collateral fee, `IsValidLocally` and `IsCollateralValid`.

use dash_consensus::Tx;
use dash_consensus::coins::compress::is_unspendable;
use dash_consensus::hash::sha256d;
use dash_dml::MnList;
use dash_pkc::bls::BlsSchemeKind;
use dash_pkc::bls::core::{BlsSigValue, BlsVerdict, verify_insecure};
use dash_primitives::{GovObject, OutPoint, TxHash};
use dash_types::codec::BaseCodec;
use dash_types::{CompactSize, Hashable, Numeric};

use crate::GovParams;
use crate::json::{Value, out_of_range};
use crate::proposal::validate_proposal;
use crate::strings::{COIN, hash_hex, hex_str};

/// `GovernanceObject::PROPOSAL`.
pub const PROPOSAL: i32 = 1;
/// `GovernanceObject::TRIGGER`.
pub const TRIGGER: i32 = 2;

/// `GOVERNANCE_PROPOSAL_FEE_TX`.
// Core: governance/object.h:87-89
pub const PROPOSAL_FEE: i64 = COIN;
/// `GOVERNANCE_FEE_CONFIRMATIONS`.
pub const FEE_CONFIRMATIONS: i32 = 6;
/// `GOVERNANCE_MIN_RELAY_FEE_CONFIRMATIONS`.
pub const MIN_RELAY_FEE_CONFIRMATIONS: i32 = 1;
/// `MAX_MONEY`.
pub const MAX_MONEY: i64 = 21_000_000 * COIN;

/// `Governance::Object::MAX_SIGNATURE_SIZE`.
// Core (v24.0.0-rc.4): governance/common.h:37
pub const MAX_SIGNATURE_SIZE: usize = 96;
/// `vchData`'s offset: after `hashParent`, `revision`, `time` and `collateralHash`.
const DATA_OFFSET: usize = 32 + 4 + 8 + 32;
/// From the end of `vchData` to `vchSig`: `type` and `masternodeOutpoint`.
const DATA_TO_SIG: usize = 4 + 36;

/// A network `govobj`: Core's reader, which bounds the signature at [`MAX_SIGNATURE_SIZE`]
/// bytes. Reads the object from the front of `data`, as a stream does; bytes after it are left.
// Core (v24.0.0-rc.4): governance/common.h:73-87
pub fn decode_network(data: &mut &[u8]) -> Option<GovObject> {
    if !sig_count_within_limit(data) {
        return None;
    }
    GovObject::decode(data).ok()
}

/// `LIMITED_VECTOR(obj.vchSig, MAX_SIGNATURE_SIZE)`: the count is read (and range-checked) and
/// bounded before any signature byte is, so an oversized vector is never allocated.
// Core (v24.0.0-rc.4): serialize.h LimitedVectorFormatter; governance/common.h:85
fn sig_count_within_limit(bytes: &[u8]) -> bool {
    let Some(mut rest) = bytes.get(DATA_OFFSET..) else {
        return false;
    };
    let Ok(data_len) = CompactSize::decode(&mut rest) else {
        return false;
    };
    let sig_offset =
        usize::try_from(data_len.to_base()).ok().and_then(|n| n.checked_add(DATA_TO_SIG));
    let Some(mut sig) = sig_offset.and_then(|at| rest.get(at..)) else {
        return false;
    };
    CompactSize::decode(&mut sig).is_ok_and(|n| n.to_base() <= MAX_SIGNATURE_SIZE as u64)
}

/// A transaction id's internal-order bytes.
pub fn txid_bytes(hash: &TxHash) -> [u8; 32] {
    hash.to_lendian()
}

/// `COutPoint::ToStringShort()`: `<txid>-<n>`.
// Core: primitives/transaction.cpp:26-29
pub fn outpoint_short(op: &OutPoint) -> String {
    format!("{}-{}", hash_hex(&txid_bytes(&op.hash)), op.index)
}

/// `COutPoint::ToString()`.
// Core: primitives/transaction.cpp:21-24
fn outpoint_string(op: &OutPoint) -> String {
    format!("COutPoint({}, {})", hash_hex(&txid_bytes(&op.hash)), op.index)
}

/// The object's type field as Core's `int`.
pub fn object_type(obj: &GovObject) -> i32 {
    obj.object_type.to_base()
}

/// `GetHash()` (`Governance::Object::GetHash`): internal order.
// Core: governance/common.cpp:23-39
pub fn hash(obj: &GovObject) -> [u8; 32] {
    *obj.hash().as_bytes()
}

/// `GetDataHash()`: SHA256d of the data's hex as a serialized string.
// Core: governance/object.cpp:543-549
pub fn data_hash(obj: &GovObject) -> [u8; 32] {
    let hex = hex_str(&obj.data);
    let mut buf = Vec::with_capacity(hex.len() + 9);
    push_compact_size(&mut buf, hex.len());
    buf.extend_from_slice(hex.as_bytes());
    sha256d(&buf)
}

/// `GetSignatureHash()`: the network serialization without the signature.
// Core: governance/object.cpp:551-554
pub fn signature_hash(obj: &GovObject) -> [u8; 32] {
    *obj.signature_hash().as_bytes()
}

fn push_compact_size(buf: &mut Vec<u8>, n: usize) {
    let n = n as u64;
    if n < 253 {
        buf.push(n as u8);
    } else if n <= 0xffff {
        buf.push(253);
        buf.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        buf.push(254);
        buf.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        buf.push(255);
        buf.extend_from_slice(&n.to_le_bytes());
    }
}

/// `GetMinCollateralFee()`.
// Core: governance/object.cpp:774-787
pub fn min_collateral_fee(obj_type: i32) -> i64 {
    match obj_type {
        PROPOSAL => PROPOSAL_FEE,
        TRIGGER => 0,
        _ => MAX_MONEY,
    }
}

/// `GetJSONObject()`: the data read as JSON, as an object, or the legacy `[["name", {..}]]`'s
/// inner value; the empty object for empty data. `Err` is the exception's `what()`.
// Core: governance/object.cpp:582-602, 669-675
pub fn json_object(data: &[u8]) -> Result<Value, String> {
    if data.is_empty() {
        return Ok(Value::empty_object());
    }
    let (_, result) = Value::read(data);
    if result.is_object() {
        return Ok(result);
    }
    let arr1 = result.get_values()?;
    let arr2 = arr1.first().ok_or_else(|| out_of_range(0, arr1.len()))?.get_values()?;
    arr2.get(1).cloned().ok_or_else(|| out_of_range(1, arr2.len()))
}

/// The type `LoadData()` gives an object built from its data hex (the RPC and wallet
/// constructors); a decoded network object keeps its wire type and is never unparsable (notes D7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadedType {
    /// Empty data: `LoadData()` returns early and the type stays `UNKNOWN` (0).
    Unchanged,
    /// `GovernanceObject(obj["type"].getInt<int>())`.
    Type(i32),
    /// `fUnparsable`.
    Unparsable,
}

// Core: governance/object.cpp:634-656
pub fn load_data(data: &[u8]) -> LoadedType {
    if data.is_empty() {
        return LoadedType::Unchanged;
    }
    match json_object(data).and_then(|obj| obj.get(b"type").and_then(Value::get_i32)) {
        Ok(t) => LoadedType::Type(t),
        Err(_) => LoadedType::Unparsable,
    }
}

/// `CheckSignature(pubKeyOperator.Get())`: the operator's basic-scheme BLS signature over
/// [`signature_hash`]. The base SDK answers [`BlsVerdict::Unsupported`] only for the legacy scheme
/// (CJ0 D9), which this check never uses.
// Core: governance/object.cpp:566-575
pub fn check_signature(obj: &GovObject, operator_key: &dash_dml::OperatorKey) -> bool {
    let sig = BlsSigValue::from_set_bytes_basic(&obj.sig);
    verify_insecure(&sig, &operator_key.get(), &signature_hash(obj), BlsSchemeKind::Basic)
        == BlsVerdict::Valid
}

/// `CBLSLazyPublicKey::ToString()`: the key's hex in the scheme it was read with (zeros for a key
/// that does not decode).
// Core: bls/bls.h:571-574, 223-227
pub fn operator_key_string(key: &dash_dml::OperatorKey) -> String {
    hex_str(&key.get().to_bytes(key.scheme()))
}

/// The collateral transaction the caller found in its transaction index (`g_txindex->FindTx`),
/// and where it is (notes D9).
#[derive(Clone, Copy)]
pub struct FoundCollateral<'a> {
    pub tx: &'a Tx,
    /// Whether `FindTx` returned a block hash (`nBlockHash != uint256()`).
    pub mined: bool,
    /// `ActiveChain().Height() - pindex->nHeight + 1` when the block is on the active chain,
    /// else 0.
    pub confirmations: i32,
}

/// `IsValidLocally`'s and `IsCollateralValid`'s outputs: the verdict, `strError` and
/// `fMissingConfirmations`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validity {
    pub valid: bool,
    pub error: String,
    pub missing_confirmations: bool,
}

impl Validity {
    fn ok(error: &str) -> Self {
        Validity {
            valid: true,
            error: error.to_owned(),
            missing_confirmations: false,
        }
    }

    fn fail(error: String) -> Self {
        Validity {
            valid: false,
            error,
            missing_confirmations: false,
        }
    }
}

/// What `IsValidLocally` checks beyond the data.
#[derive(Clone, Copy)]
pub enum Check<'a> {
    /// `fCheckCollateral = false` (`UpdateLocalValidity`, `gobject check`): no list is read.
    NoCollateral,
    /// `fCheckCollateral = true` (`ProcessObject`): the proposal's collateral, or the trigger's
    /// signer and signature against the tip's list.
    Collateral {
        list: &'a MnList,
        found: Option<FoundCollateral<'a>>,
    },
}

/// `IsValidLocally(tip_mn_list, chainman, strError, fMissingConfirmations, fCheckCollateral)`
/// for `obj` with `fUnparsable` = `unparsable`.
// Core: governance/object.cpp:711-772
pub fn is_valid_locally(
    obj: &GovObject,
    unparsable: bool,
    check: Check<'_>,
    p: &GovParams,
) -> Validity {
    if unparsable {
        return Validity::fail("Object data unparsable".to_owned());
    }
    match object_type(obj) {
        PROPOSAL => {
            if let Err(e) = validate_proposal(hex_str(&obj.data).as_bytes(), false, true, 0, p) {
                return Validity::fail(format!("Invalid proposal data, error messages: {e}"));
            }
            let Check::Collateral {
                found,
                ..
            } = check
            else {
                return Validity::ok("");
            };
            let collateral = collateral_valid(obj, found);
            if collateral.valid {
                return collateral;
            }
            Validity {
                valid: false,
                error: "Invalid proposal collateral".to_owned(),
                missing_confirmations: collateral.missing_confirmations,
            }
        }
        TRIGGER => {
            let Check::Collateral {
                list,
                ..
            } = check
            else {
                return Validity::ok("");
            };
            let outpoint = outpoint_short(&obj.masternode_outpoint);
            let Some(dmn) = list.get_by_collateral(&obj.masternode_outpoint) else {
                return Validity::fail(format!(
                    "Failed to find Masternode by UTXO, missing masternode={outpoint}"
                ));
            };
            let key = &dmn.cold.pub_key_operator;
            if !check_signature(obj, key) {
                return Validity::fail(format!(
                    "Invalid masternode signature for: {outpoint}, pubkey = {}",
                    operator_key_string(key)
                ));
            }
            Validity::ok("")
        }
        other => Validity::fail(format!("Invalid object type {other}")),
    }
}

/// `CScript::IsPayToPublicKeyHash()`.
// Core: script/script.cpp:202-211
fn is_p2pkh(s: &[u8]) -> bool {
    s.len() == 25 && s[0] == 0x76 && s[1] == 0xa9 && s[2] == 0x14 && s[23] == 0x88 && s[24] == 0xac
}

/// `IsCollateralValid` over the transaction the caller found.
// Core: governance/object.cpp:790-878
pub fn collateral_valid(obj: &GovObject, found: Option<FoundCollateral<'_>>) -> Validity {
    let Some(found) = found else {
        return Validity::fail(format!(
            "Can't find collateral tx {}",
            hash_hex(&txid_bytes(&obj.collateral_hash))
        ));
    };
    let tx = found.tx;
    if !found.mined {
        return Validity::fail(format!("Collateral tx {} is not mined yet", tx_to_string(tx)));
    }
    let outputs = tx.outputs();
    if outputs.is_empty() {
        return Validity::fail("tx vout is empty".to_owned());
    }
    let expected = hash(obj);
    let mut find_script = vec![0x6a, 0x20];
    find_script.extend_from_slice(&expected);
    let fee = min_collateral_fee(object_type(obj));
    let mut found_op_return = false;
    for output in outputs {
        let script = output.script_pubkey.as_bytes();
        if !is_p2pkh(script) && !is_unspendable(script) {
            return Validity::fail(format!("Invalid Script {}", tx_to_string(tx)));
        }
        if script == find_script.as_slice() && output.value >= fee {
            found_op_return = true;
        }
    }
    if !found_op_return {
        return Validity::fail(format!(
            "Couldn't find opReturn {} in {}",
            hash_hex(&expected),
            tx_to_string(tx)
        ));
    }
    let confirmations = found.confirmations;
    if confirmations < FEE_CONFIRMATIONS {
        let mut error = format!(
            "Collateral requires at least {FEE_CONFIRMATIONS} confirmations to be relayed throughout the network (it has only {confirmations})"
        );
        let missing_confirmations = confirmations >= MIN_RELAY_FEE_CONFIRMATIONS;
        if missing_confirmations {
            error.push_str(", pre-accepted -- waiting for required confirmations");
        } else {
            error.push_str(", rejected -- try again later");
        }
        return Validity {
            valid: false,
            error,
            missing_confirmations,
        };
    }
    Validity::ok("valid")
}

/// `CTransaction::ToString()`.
// Core: primitives/transaction.cpp:45-69, 148-164
pub fn tx_to_string(tx: &Tx) -> String {
    let raw = tx.raw();
    let txid = hash_hex(&txid_bytes(&tx.txid()));
    let mut s = format!(
        "CTransaction(hash={}, ver={}, type={}, vin.size={}, vout.size={}, nLockTime={}, vExtraPayload.size={})\n",
        &txid[..10],
        raw.version,
        raw.tx_type.to_base(),
        raw.inputs.len(),
        raw.outputs.len(),
        raw.lock_time,
        raw.extra_payload.len()
    );
    for input in &raw.inputs {
        let script = hex_str(input.script_sig.as_bytes());
        s.push_str("    CTxIn(");
        s.push_str(&outpoint_string(&input.prevout));
        if input.prevout.is_null() {
            s.push_str(&format!(", coinbase {script}"));
        } else {
            s.push_str(&format!(", scriptSig={}", &script[..script.len().min(24)]));
        }
        if input.sequence != u32::MAX {
            s.push_str(&format!(", nSequence={}", input.sequence));
        }
        s.push_str(")\n");
    }
    for output in &raw.outputs {
        let value = output.value;
        let script = hex_str(output.script_pubkey.as_bytes());
        s.push_str(&format!(
            "    CTxOut(nValue={}.{:08}, scriptPubKey={})\n",
            value / COIN,
            value % COIN,
            &script[..script.len().min(30)]
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_object_paths() {
        assert!(json_object(b"").unwrap().is_object());
        let legacy = json_object(br#"[["trigger",{"type":2}]]"#).unwrap();
        assert_eq!(legacy.get(b"type").unwrap().get_i32(), Ok(2));
        assert_eq!(
            json_object(b"1"),
            Err("JSON value is not an object or array as expected".to_owned())
        );
        assert_eq!(json_object(b"[]"), Err(out_of_range(0, 0)));
        assert_eq!(json_object(b"[[1]]"), Err(out_of_range(1, 1)));
        // The legacy inner value can be anything.
        assert_eq!(json_object(br#"[[1,"x"]]"#).unwrap().get_str(), Ok(&b"x"[..]));
    }

    #[test]
    fn loaded_types() {
        assert_eq!(load_data(b""), LoadedType::Unchanged);
        assert_eq!(load_data(br#"{"type":7}"#), LoadedType::Type(7));
        assert_eq!(load_data(br#"{"type":"1"}"#), LoadedType::Unparsable);
        assert_eq!(load_data(b"x"), LoadedType::Unparsable);
    }

    fn object_with(data_len: usize, sig_len: usize) -> GovObject {
        GovObject {
            hash_parent: TxHash::from_lendian([1; 32]),
            revision: 1,
            time: 2,
            collateral_hash: TxHash::from_lendian([3; 32]),
            data: vec![b'x'; data_len],
            object_type: Numeric::from_base(PROPOSAL),
            masternode_outpoint: OutPoint {
                hash: TxHash::from_lendian([4; 32]),
                index: 5,
            },
            sig: vec![6; sig_len],
        }
    }

    fn encoded(obj: &GovObject) -> Vec<u8> {
        let mut bytes = Vec::new();
        obj.encode(&mut bytes);
        bytes
    }

    #[test]
    fn signature_offset() {
        // A one-byte and a three-byte data count.
        for data_len in [0, 300] {
            let bytes = encoded(&object_with(data_len, 0));
            let header = if data_len < 0xfd {
                1
            } else {
                3
            };
            assert_eq!(bytes.len(), DATA_OFFSET + header + data_len + DATA_TO_SIG + 1);
            assert_eq!(bytes[bytes.len() - 1], 0);
        }
    }

    /// rc.4's `LIMITED_VECTOR(obj.vchSig, 96)`: 96 bytes decode, 97 with the full payload do not,
    /// and the count alone rejects (the payload may be absent). Trailing bytes are left unread.
    #[test]
    fn network_signature_bound() {
        for data_len in [0, 300] {
            for (sig_len, ok) in [(0, true), (96, true), (97, false)] {
                let mut bytes = encoded(&object_with(data_len, sig_len));
                assert_eq!(sig_count_within_limit(&bytes), ok, "{data_len} {sig_len}");
                bytes.push(0xff);
                let mut cursor = &bytes[..];
                let obj = decode_network(&mut cursor);
                assert_eq!(obj.is_some(), ok, "{data_len} {sig_len}");
                if ok {
                    assert_eq!(obj, Some(object_with(data_len, sig_len)));
                    assert_eq!(cursor.len(), 1);
                }
            }
        }

        let at_limit = encoded(&object_with(0, 96));
        let sig_at = at_limit.len() - 97;
        // 96 with no payload passes the bound, then fails the decode; no count fails it.
        assert!(sig_count_within_limit(&at_limit[..=sig_at]));
        assert!(decode_network(&mut &at_limit[..=sig_at]).is_none());
        assert!(!sig_count_within_limit(&at_limit[..sig_at]));

        let prefix = &at_limit[..sig_at];
        for count in [
            &[0x61][..],                                             // 97
            &[0xfd, 0x61, 0x00],                                     // 97
            &[0xfe, 0x01, 0x00, 0x00, 0x02],                         // MAX_SIZE + 1
            &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], // u64::MAX
            &[0xfd, 0x60, 0x00],                                     // 96, non-canonical
        ] {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(count);
            bytes.extend_from_slice(&[0; 97]);
            assert!(!sig_count_within_limit(&bytes), "{count:02x?}");
            assert!(decode_network(&mut &bytes[..]).is_none(), "{count:02x?}");
        }

        // A data count pointing past the message, or out of range, fails before the signature.
        let mut huge_data = at_limit[..DATA_OFFSET].to_vec();
        huge_data.extend_from_slice(&[0xff; 9]);
        assert!(!sig_count_within_limit(&huge_data));
        assert!(!sig_count_within_limit(&at_limit[..DATA_OFFSET]));
    }

    #[test]
    fn fees() {
        assert_eq!(min_collateral_fee(PROPOSAL), COIN);
        assert_eq!(min_collateral_fee(TRIGGER), 0);
        assert_eq!(min_collateral_fee(0), MAX_MONEY);
    }
}
