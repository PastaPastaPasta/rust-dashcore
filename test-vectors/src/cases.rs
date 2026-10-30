//! Typed loaders for the Core JSON files. Each one applies the row rules of the C++ test that
//! consumes the file (which rows are comments, what makes a row a "bad test", which accessors
//! throw), so that a consumer sees exactly the cases Core's harness runs.

use std::fmt;

use crate::flags::{UnknownFlag, parse_script_flags};
use crate::hex::{parse_hex, uint256_from_hex};
use crate::script::{parse_script, push_compact_size};
use crate::univalue::{ParseError, UniValue};

/// A row Core's harness would report as `Bad test` (or abort on, when an accessor throws).
#[derive(Debug, Clone, PartialEq)]
pub struct LoadError {
    pub file: &'static str,
    pub index: usize,
    pub reason: String,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} row {}: {}", self.file, self.index, self.reason)
    }
}

impl std::error::Error for LoadError {}

fn bad(file: &'static str, index: usize, reason: impl fmt::Display) -> LoadError {
    LoadError {
        file,
        index,
        reason: reason.to_string(),
    }
}

/// `read_json` plus the top-level array requirement every harness has.
fn top_level_array(file: &'static str, text: &str) -> Result<Vec<UniValue>, LoadError> {
    match UniValue::parse(text) {
        Ok(UniValue::Arr(rows)) => Ok(rows),
        Ok(_) => Err(bad(file, 0, "top-level value is not an array")),
        Err(ParseError {
            offset,
            message,
        }) => Err(bad(file, 0, format!("JSON parse error at byte {offset}: {message}"))),
    }
}

fn row_err<E: fmt::Display>(file: &'static str, index: usize) -> impl Fn(E) -> LoadError {
    move |e| bad(file, index, e)
}

fn flag_err(file: &'static str, index: usize) -> impl Fn(UnknownFlag) -> LoadError {
    move |e| bad(file, index, format!("bad test: {e}"))
}

// ---------------------------------------------------------------------------------------------
// script_tests.json  (script_tests.cpp, `script_json_test`)
// ---------------------------------------------------------------------------------------------

/// One `[scriptSig, scriptPubKey, flags, expected_error, comment?]` row.
///
/// Core evaluates it with `VerifyScript` against a crediting/spending transaction pair with
/// value 0 (see [`credit_tx`] and [`spend_tx`]) and expects exactly `expected_error`
/// (`"OK"` means success).
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptCase {
    /// Row index in the file (comment rows count).
    pub index: usize,
    pub script_sig: Vec<u8>,
    pub script_pub_key: Vec<u8>,
    pub flags: u32,
    pub expected_error: String,
    /// The fifth element, if any. Core ignores it.
    pub comment: Option<String>,
    /// The original strings, for diagnostics.
    pub script_sig_text: String,
    pub script_pub_key_text: String,
    pub flags_text: String,
}

/// Comment rows (a single string) are skipped; any other row with fewer than four elements is a
/// bad test. Rows with more than four are accepted and the extras ignored.
pub fn load_script_tests(text: &str) -> Result<Vec<ScriptCase>, LoadError> {
    const FILE: &str = "script_tests.json";
    let rows = top_level_array(FILE, text)?;
    let mut cases = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if row.len() < 4 {
            if row.len() != 1 {
                return Err(bad(FILE, index, "bad test: fewer than 4 elements"));
            }
            continue;
        }
        let e = row_err(FILE, index);
        let sig_text = row.at(0).get_str().map_err(&e)?;
        let pub_text = row.at(1).get_str().map_err(&e)?;
        let flags_text = row.at(2).get_str().map_err(&e)?;
        let error = row.at(3).get_str().map_err(&e)?;
        if !crate::script_error::is_known_script_error(error) {
            return Err(bad(FILE, index, format!("unknown scripterror \"{error}\"")));
        }
        cases.push(ScriptCase {
            index,
            script_sig: parse_script(sig_text).map_err(row_err(FILE, index))?,
            script_pub_key: parse_script(pub_text).map_err(row_err(FILE, index))?,
            flags: parse_script_flags(flags_text).map_err(flag_err(FILE, index))?,
            expected_error: error.to_owned(),
            comment: row.at(4).get_str().ok().map(str::to_owned),
            script_sig_text: sig_text.to_owned(),
            script_pub_key_text: pub_text.to_owned(),
            flags_text: flags_text.to_owned(),
        });
    }
    Ok(cases)
}

/// A version-1 transaction with one input and one output of value 0, as `BuildCreditingTransaction`
/// and `BuildSpendingTransaction` (`test/util/transaction_utils.cpp`) make them.
fn one_in_one_out_tx(
    prev_txid: &[u8; 32],
    prev_vout: u32,
    script_sig: &[u8],
    script_pub_key: &[u8],
) -> Vec<u8> {
    let mut tx = Vec::new();
    tx.extend_from_slice(&1u32.to_le_bytes()); // version 1, type 0
    tx.push(1); // one input
    tx.extend_from_slice(prev_txid);
    tx.extend_from_slice(&prev_vout.to_le_bytes());
    push_compact_size(&mut tx, script_sig.len());
    tx.extend_from_slice(script_sig);
    tx.extend_from_slice(&u32::MAX.to_le_bytes()); // sequence
    tx.push(1); // one output
    tx.extend_from_slice(&0i64.to_le_bytes());
    push_compact_size(&mut tx, script_pub_key.len());
    tx.extend_from_slice(script_pub_key);
    tx.extend_from_slice(&0u32.to_le_bytes()); // lock time
    tx
}

/// `BuildCreditingTransaction(scriptPubKey, nValue = 0)`, serialized: one input spending the null
/// outpoint with scriptSig `OP_0 OP_0` (`CScriptNum(0)` pushes the empty vector), one output of
/// value 0 paying `script_pub_key`.
pub fn credit_tx(script_pub_key: &[u8]) -> Vec<u8> {
    one_in_one_out_tx(&[0; 32], u32::MAX, &[0x00, 0x00], script_pub_key)
}

/// `BuildSpendingTransaction(scriptSig, txCredit)`, serialized: one input spending output 0 of
/// the crediting transaction (`credit_txid` in internal byte order), one empty output of value 0.
pub fn spend_tx(script_sig: &[u8], credit_txid: &[u8; 32]) -> Vec<u8> {
    one_in_one_out_tx(credit_txid, 0, script_sig, &[])
}

// ---------------------------------------------------------------------------------------------
// tx_valid.json / tx_invalid.json  (transaction_tests.cpp)
// ---------------------------------------------------------------------------------------------

/// `[txid, vout, scriptPubKey]` of a spent output.
#[derive(Debug, Clone, PartialEq)]
pub struct Prevout {
    /// `uint256S(..)`: internal byte order.
    pub txid: [u8; 32],
    /// `uint32_t(getInt<int>())`, so `-1` is `0xffffffff`.
    pub vout: u32,
    pub script_pub_key: Vec<u8>,
}

/// What the third element of a tx vector means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxFlags {
    /// `tx_valid`: the flags that are *excluded* (the transaction must verify with every other
    /// flag set, `!flags`). `tx_invalid`: the flags to verify with (must fail).
    Script(u32),
    /// `tx_invalid` only: `CheckTransaction` must reject the transaction ("BADTX").
    BadTx,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TxCase {
    pub index: usize,
    pub prevouts: Vec<Prevout>,
    /// The serialized transaction. Core deserializes it from the front and does not require the
    /// stream to be fully consumed.
    pub tx: Vec<u8>,
    pub tx_hex: String,
    pub flags: TxFlags,
}

impl TxCase {
    /// `mapprevOutScriptPubKeys.at(prevout)`: the `std::map` assignment loop means the *last*
    /// listing of an outpoint wins. `None` is Core's "Bad test" (`std::out_of_range`).
    pub fn prevout_script(&self, txid: &[u8; 32], vout: u32) -> Option<&[u8]> {
        self.prevouts
            .iter()
            .rev()
            .find(|p| &p.txid == txid && p.vout == vout)
            .map(|p| p.script_pub_key.as_slice())
    }
}

fn load_tx_cases(
    file: &'static str,
    text: &str,
    allow_badtx: bool,
) -> Result<Vec<TxCase>, LoadError> {
    let rows = top_level_array(file, text)?;
    let mut cases = Vec::new();
    for (index, test) in rows.iter().enumerate() {
        // Rows whose first element is not an array are comments.
        if !matches!(test.at(0), UniValue::Arr(_)) {
            continue;
        }
        let e = row_err(file, index);
        let (3, Ok(tx_hex), Ok(flags_text)) =
            (test.len(), test.at(1).get_str(), test.at(2).get_str())
        else {
            return Err(bad(file, index, "bad test: expected [inputs, tx, flags]"));
        };
        let mut prevouts = Vec::new();
        for input in test.at(0).get_array().map_err(&e)? {
            let fields = input
                .get_array()
                .map_err(|_| bad(file, index, "bad test: input is not an array"))?;
            if fields.len() != 3 {
                return Err(bad(file, index, "bad test: input does not have 3 elements"));
            }
            prevouts.push(Prevout {
                txid: uint256_from_hex(fields[0].get_str().map_err(&e)?),
                vout: fields[1].get_i32().map_err(&e)? as u32,
                script_pub_key: parse_script(fields[2].get_str().map_err(&e)?)
                    .map_err(row_err(file, index))?,
            });
        }
        // Core's "Bad test flags" check on `tx_valid`: `~verify_flags` must survive `FillFlags`,
        // i.e. a test may not exclude P2SH while leaving CLEANSTACK enabled.
        if !allow_badtx
            && let Ok(excluded) = parse_script_flags(flags_text)
            && crate::flags::fill_flags(!excluded) != !excluded
        {
            return Err(bad(file, index, "Bad test flags: CLEANSTACK enabled without P2SH"));
        }
        let flags = if allow_badtx && flags_text == "BADTX" {
            TxFlags::BadTx
        } else {
            TxFlags::Script(parse_script_flags(flags_text).map_err(flag_err(file, index))?)
        };
        cases.push(TxCase {
            index,
            prevouts,
            tx: parse_hex(tx_hex),
            tx_hex: tx_hex.to_owned(),
            flags,
        });
    }
    Ok(cases)
}

/// `tx_valid.json`. Every transaction must pass `CheckTransaction`, then verify with all flags
/// except `flags` (and with any further subset removed; see `flags::removal_sets`), and fail
/// whenever one of the excluded flags is switched back on (`flags::restore_one_excluded`).
pub fn load_tx_valid(text: &str) -> Result<Vec<TxCase>, LoadError> {
    load_tx_cases("tx_valid.json", text, false)
}

/// `tx_invalid.json`. A transaction either fails `CheckTransaction` (`TxFlags::BadTx`, and the
/// harness requires that to be the *only* reason a transaction is accepted as bad), or fails
/// script verification with exactly `flags` and with any superset (`flags::addition_sets`).
pub fn load_tx_invalid(text: &str) -> Result<Vec<TxCase>, LoadError> {
    load_tx_cases("tx_invalid.json", text, true)
}

// ---------------------------------------------------------------------------------------------
// sighash.json  (sighash_tests.cpp, `sighash_from_data`)
// ---------------------------------------------------------------------------------------------

/// `[raw_tx, script, input_index, hashType, signature_hash]`
#[derive(Debug, Clone, PartialEq)]
pub struct SighashCase {
    pub index: usize,
    pub tx: Vec<u8>,
    /// The script code, `ParseHex` of the second element (raw script bytes, not `ParseScript`).
    pub script_code: Vec<u8>,
    /// `getInt<int>`. Core's `SignatureHash` asserts that it is in range of the inputs
    /// (`interpreter.cpp:1514`); no row of the file is out of range. (`uint256::ONE` is the
    /// `SIGHASH_SINGLE` result for an input without a matching output, which no row hits either.)
    pub n_in: i32,
    /// `getInt<int>`, the full signed 32-bit hash type (not just the low byte).
    pub hash_type: i32,
    /// `sh.GetHex()` form: the display (reversed) hex the harness string-compares.
    pub expected: String,
}

/// A row of one element is a comment (the first row is the format description).
pub fn load_sighash(text: &str) -> Result<Vec<SighashCase>, LoadError> {
    const FILE: &str = "sighash.json";
    let rows = top_level_array(FILE, text)?;
    let mut cases = Vec::new();
    for (index, test) in rows.iter().enumerate() {
        if test.is_empty() {
            return Err(bad(FILE, index, "bad test: empty row"));
        }
        if test.len() == 1 {
            continue;
        }
        let e = row_err(FILE, index);
        cases.push(SighashCase {
            index,
            tx: parse_hex(test.at(0).get_str().map_err(&e)?),
            script_code: parse_hex(test.at(1).get_str().map_err(&e)?),
            n_in: test.at(2).get_i32().map_err(&e)?,
            hash_type: test.at(3).get_i32().map_err(&e)?,
            expected: test.at(4).get_str().map_err(&e)?.to_owned(),
        });
    }
    Ok(cases)
}

// ---------------------------------------------------------------------------------------------
// trivially_valid.json / trivially_invalid.json  (evo_trivialvalidation.cpp)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlsScheme {
    /// Validate against the chain at height 100, where the V19 (basic scheme) rules are active.
    Basic,
    /// Validate against the chain at height 98, before V19 (legacy scheme).
    Legacy,
}

/// `[txhash, type, scheme, txhex, expected_error?, notes?]`. The runner executes each row under
/// both global BLS-scheme settings; `scheme` only selects the chain tip it validates against.
#[derive(Debug, Clone, PartialEq)]
pub struct TrivialCase {
    pub index: usize,
    pub tx_hash: [u8; 32],
    /// `proregtx`, `proupservtx`, `proupregtx` or `prouprevtx`.
    pub tx_type: String,
    pub scheme: BlsScheme,
    pub tx: Vec<u8>,
    /// `Some(reject reason)` for `trivially_invalid.json`.
    pub expected_error: Option<String>,
    pub notes: Option<String>,
}

/// Row 0 is the column header and is skipped (the runner starts at index 1).
pub fn load_trivial(file: &'static str, text: &str) -> Result<Vec<TrivialCase>, LoadError> {
    let rows = top_level_array(file, text)?;
    let mut cases = Vec::new();
    for (index, test) in rows.iter().enumerate().skip(1) {
        let e = row_err(file, index);
        let scheme = match test.at(2).get_str().map_err(&e)? {
            "basic" => BlsScheme::Basic,
            "legacy" => BlsScheme::Legacy,
            other => return Err(bad(file, index, format!("unknown scheme {other:?}"))),
        };
        cases.push(TrivialCase {
            index,
            tx_hash: uint256_from_hex(test.at(0).get_str().map_err(&e)?),
            tx_type: test.at(1).get_str().map_err(&e)?.to_owned(),
            scheme,
            tx: parse_hex(test.at(3).get_str().map_err(&e)?),
            expected_error: if test.len() > 4 {
                Some(test.at(4).get_str().map_err(&e)?.to_owned())
            } else {
                None
            },
            notes: test.at(5).get_str().ok().map(str::to_owned),
        });
    }
    Ok(cases)
}

// ---------------------------------------------------------------------------------------------
// base58_encode_decode.json, key_io_*.json, bip39_vectors.json, proposals_*.json
// ---------------------------------------------------------------------------------------------

/// `[hex, base58]`, both directions. Every row (there are no comment rows) needs two elements.
#[derive(Debug, Clone, PartialEq)]
pub struct Base58Case {
    pub index: usize,
    pub data: Vec<u8>,
    pub base58: String,
}

pub fn load_base58(text: &str) -> Result<Vec<Base58Case>, LoadError> {
    const FILE: &str = "base58_encode_decode.json";
    let rows = top_level_array(FILE, text)?;
    let mut cases = Vec::new();
    for (index, test) in rows.iter().enumerate() {
        if test.len() < 2 {
            return Err(bad(FILE, index, "bad test: fewer than 2 elements"));
        }
        let e = row_err(FILE, index);
        cases.push(Base58Case {
            index,
            data: parse_hex(test.at(0).get_str().map_err(&e)?),
            base58: test.at(1).get_str().map_err(&e)?.to_owned(),
        });
    }
    Ok(cases)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    Main,
    Test,
    Regtest,
}

/// `key_io_valid.json`: `[address-or-WIF, payload hex, {chain, isPrivkey, isCompressed?, tryCaseFlip?}]`.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyIoValid {
    pub index: usize,
    pub encoded: String,
    /// For an address: the expected `scriptPubKey`. For a WIF key: the 32-byte secret.
    pub payload: Vec<u8>,
    pub chain: Chain,
    pub is_privkey: bool,
    /// Only meaningful for private keys.
    pub is_compressed: bool,
    /// Whether the case-flipped string is also a valid address (default false).
    pub try_case_flip: bool,
}

pub fn load_key_io_valid(text: &str) -> Result<Vec<KeyIoValid>, LoadError> {
    const FILE: &str = "key_io_valid.json";
    let rows = top_level_array(FILE, text)?;
    let mut cases = Vec::new();
    for (index, test) in rows.iter().enumerate() {
        if test.len() < 3 {
            return Err(bad(FILE, index, "bad test: fewer than 3 elements"));
        }
        let e = row_err(FILE, index);
        let meta = test.at(2);
        meta.get_obj().map_err(&e)?;
        let chain = match meta.find_value("chain").get_str().map_err(&e)? {
            "main" => Chain::Main,
            "test" => Chain::Test,
            "regtest" => Chain::Regtest,
            other => return Err(bad(FILE, index, format!("unknown chain {other:?}"))),
        };
        let is_privkey = meta.find_value("isPrivkey").get_bool().map_err(&e)?;
        let is_compressed = if is_privkey {
            meta.find_value("isCompressed").get_bool().map_err(&e)?
        } else {
            false
        };
        let flip = meta.find_value("tryCaseFlip");
        cases.push(KeyIoValid {
            index,
            encoded: test.at(0).get_str().map_err(&e)?.to_owned(),
            payload: parse_hex(test.at(1).get_str().map_err(&e)?),
            chain,
            is_privkey,
            is_compressed,
            try_case_flip: if flip.is_null() {
                false
            } else {
                flip.get_bool().map_err(&e)?
            },
        });
    }
    Ok(cases)
}

/// `key_io_invalid.json`: strings that must be neither a valid address nor a valid WIF key on
/// mainnet, testnet and regtest. Extra elements are ignored.
pub fn load_key_io_invalid(text: &str) -> Result<Vec<String>, LoadError> {
    const FILE: &str = "key_io_invalid.json";
    let rows = top_level_array(FILE, text)?;
    let mut out = Vec::new();
    for (index, test) in rows.iter().enumerate() {
        if test.is_empty() {
            return Err(bad(FILE, index, "bad test: empty row"));
        }
        out.push(test.at(0).get_str().map_err(row_err(FILE, index))?.to_owned());
    }
    Ok(out)
}

/// `bip39_vectors.json`: `[entropy hex, mnemonic, seed hex, xprv]`, Trezor's vectors with the
/// passphrase `"TREZOR"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Bip39Case {
    pub index: usize,
    pub entropy: Vec<u8>,
    pub mnemonic: String,
    pub seed_hex: String,
    pub xprv: String,
}

pub const BIP39_PASSPHRASE: &str = "TREZOR";

pub fn load_bip39(text: &str) -> Result<Vec<Bip39Case>, LoadError> {
    const FILE: &str = "bip39_vectors.json";
    let rows = top_level_array(FILE, text)?;
    let mut cases = Vec::new();
    for (index, test) in rows.iter().enumerate() {
        if test.len() < 4 {
            return Err(bad(FILE, index, "bad test: fewer than 4 elements"));
        }
        let e = row_err(FILE, index);
        cases.push(Bip39Case {
            index,
            entropy: parse_hex(test.at(0).get_str().map_err(&e)?),
            mnemonic: test.at(1).get_str().map_err(&e)?.to_owned(),
            seed_hex: test.at(2).get_str().map_err(&e)?.to_owned(),
            xprv: test.at(3).get_str().map_err(&e)?.to_owned(),
        });
    }
    Ok(cases)
}

/// `proposals_valid.json`: `[proposal object, fAllowScript]`. Every proposal is valid but
/// *expired*: it validates with `fCheckExpiration = false` and fails with `true`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalCase {
    pub index: usize,
    pub proposal: UniValue,
    pub allow_script: bool,
}

pub fn load_proposals_valid(text: &str) -> Result<Vec<ProposalCase>, LoadError> {
    const FILE: &str = "proposals_valid.json";
    let rows = top_level_array(FILE, text)?;
    if rows.is_empty() {
        return Err(bad(FILE, 0, "Empty `tests`"));
    }
    let mut cases = Vec::new();
    for (index, test) in rows.iter().enumerate() {
        cases.push(ProposalCase {
            index,
            proposal: test.at(0).clone(),
            allow_script: test.at(1).get_bool().map_err(row_err(FILE, index))?,
        });
    }
    Ok(cases)
}

/// `proposals_invalid.json`: proposal objects that are invalid with or without the expiry check
/// and without `fAllowScript`.
pub fn load_proposals_invalid(text: &str) -> Result<Vec<UniValue>, LoadError> {
    const FILE: &str = "proposals_invalid.json";
    let rows = top_level_array(FILE, text)?;
    if rows.is_empty() {
        return Err(bad(FILE, 0, "Empty `tests`"));
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crediting_and_spending_transactions_have_the_layout_core_builds() {
        // version 1, one input (null outpoint, scriptSig OP_0 OP_0, final sequence), one output.
        let credit = credit_tx(&[0x51]);
        assert_eq!(
            crate::hex::hex_str(&credit),
            concat!(
                "01000000",
                "01",
                "00000000000000000000000000000000000000000000000000000000000000",
                "00",
                "ffffffff",
                "02",
                "0000",
                "ffffffff",
                "01",
                "0000000000000000",
                "01",
                "51",
                "00000000"
            )
        );
        let spend = spend_tx(&[0x01, 0x02], &[7; 32]);
        assert_eq!(&spend[5..37], &[7u8; 32]);
        assert_eq!(&spend[37..41], &[0; 4]);
        assert_eq!(&spend[41..44], &[0x02, 0x01, 0x02]);
        // an empty output script, then the lock time
        assert_eq!(&spend[spend.len() - 5..], &[0, 0, 0, 0, 0]);
    }
}
