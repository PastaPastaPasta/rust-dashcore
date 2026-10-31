//! `governance::ValidateProposal`: the proposal data rules, with Core's messages in Core's order.
//!
//! Every message ends in `;`. A field's getter appends its exception text before the field's own
//! message, so the same data gives the same string as Core.

use crate::GovParams;
use crate::json::{Kind, Value};
use crate::strings::{Destination, decode_destination, is_space, parse_hex};

/// `MAX_DATA_SIZE`.
// Core: governance/object.cpp:33
pub const MAX_DATA_SIZE: usize = 512;
/// `MAX_NAME_SIZE`.
// Core: governance/object.cpp:34
pub const MAX_NAME_SIZE: usize = 40;

/// `GetDataValue(obj, key, std::string&, errors)`.
// Core: governance/object.cpp:36-48
fn data_str<'a>(obj: &'a Value, key: &[u8], errors: &mut String) -> Option<&'a [u8]> {
    match obj.get(key).and_then(Value::get_str) {
        Ok(s) => Some(s),
        Err(e) => {
            errors.push_str(&e);
            errors.push(';');
            None
        }
    }
}

/// `GetDataValue` for a number: only a number is read (with `get`), anything else is absent.
// Core: governance/object.cpp:50-80
fn data_num<T>(
    obj: &Value,
    key: &[u8],
    errors: &mut String,
    get: impl FnOnce(&Value) -> Result<T, String>,
) -> Option<T> {
    let read = obj.get(key).and_then(|v| {
        if v.kind() == Kind::Num {
            get(v).map(Some)
        } else {
            Ok(None)
        }
    });
    match read {
        Ok(v) => v,
        Err(e) => {
            errors.push_str(&e);
            errors.push(';');
            None
        }
    }
}

// Core: governance/object.cpp:82-97
fn validate_type(obj: &Value, errors: &mut String) -> bool {
    let Some(kind) = data_num(obj, b"type", errors, Value::get_i64) else {
        errors.push_str("type field not found;");
        return false;
    };
    if kind != 1 {
        errors.push_str("type is not 1;");
        return false;
    }
    true
}

// Core: governance/object.cpp:99-127
fn validate_name(obj: &Value, errors: &mut String) -> bool {
    let Some(name) = data_str(obj, b"name", errors) else {
        errors.push_str("name field not found;");
        return false;
    };
    if name.len() > MAX_NAME_SIZE {
        errors.push_str("name exceeds 40 characters;");
        return false;
    }
    if name.is_empty() {
        errors.push_str("name cannot be empty;");
        return false;
    }
    let allowed =
        |c: &u8| matches!(c.to_ascii_lowercase(), b'-' | b'_' | b'a'..=b'z' | b'0'..=b'9');
    if !name.iter().all(allowed) {
        errors.push_str("name contains invalid characters;");
        return false;
    }
    true
}

// Core: governance/object.cpp:129-155
fn validate_start_end_epoch(
    obj: &Value,
    check_expiration: bool,
    adjusted_now: i64,
    errors: &mut String,
) -> bool {
    let Some(start) = data_num(obj, b"start_epoch", errors, Value::get_i64) else {
        errors.push_str("start_epoch field not found;");
        return false;
    };
    let Some(end) = data_num(obj, b"end_epoch", errors, Value::get_i64) else {
        errors.push_str("end_epoch field not found;");
        return false;
    };
    if end <= start {
        errors.push_str("end_epoch <= start_epoch;");
        return false;
    }
    if check_expiration && end <= adjusted_now {
        errors.push_str("expired;");
        return false;
    }
    true
}

// Core: governance/object.cpp:157-175
fn validate_payment_amount(obj: &Value, errors: &mut String) -> bool {
    let Some(amount) = data_num(obj, b"payment_amount", errors, Value::get_real) else {
        errors.push_str("payment_amount field not found;");
        return false;
    };
    if amount <= 0.0 {
        errors.push_str("payment_amount is negative;");
        return false;
    }
    true
}

// Core: governance/object.cpp:177-204
fn validate_payment_address(
    obj: &Value,
    allow_script: bool,
    p: &GovParams,
    errors: &mut String,
) -> bool {
    let Some(address) = data_str(obj, b"payment_address", errors) else {
        errors.push_str("payment_address field not found;");
        return false;
    };
    if address.iter().copied().any(is_space) {
        errors.push_str("payment_address can't have whitespaces;");
        return false;
    }
    let Some(dest) = decode_destination(address, p.chain()) else {
        errors.push_str("payment_address is invalid;");
        return false;
    };
    if !allow_script && matches!(dest, Destination::ScriptHash(_)) {
        errors.push_str("script addresses are not supported;");
        return false;
    }
    true
}

/// `CheckURL`: sentinel's `urlparse` failures, unbalanced brackets in the netloc.
// Core: governance/object.cpp:206-243
fn check_url(url: &[u8]) -> bool {
    let rest = match url.iter().position(|&c| c == b':') {
        Some(colon) => &url[colon + 1..],
        None => url,
    };
    if rest.len() > 2 && rest.starts_with(b"//") {
        let rest = &rest[2..];
        let netloc = match rest.iter().position(|c| b"/?#".contains(c)) {
            Some(end) => &rest[..end],
            None => rest,
        };
        let (open, close) = (netloc.contains(&b'['), netloc.contains(&b']'));
        if open != close {
            return false;
        }
    }
    true
}

// Core: governance/object.cpp:245-270
fn validate_url(obj: &Value, errors: &mut String) -> bool {
    let Some(url) = data_str(obj, b"url", errors) else {
        errors.push_str("url field not found;");
        return false;
    };
    if url.iter().copied().any(is_space) {
        errors.push_str("url can't have whitespaces;");
        return false;
    }
    if url.len() < 4 {
        errors.push_str("url too short;");
        return false;
    }
    if !check_url(url) {
        errors.push_str("url invalid;");
        return false;
    }
    true
}

/// `ParseProposalJSON`: the data as an object, from whatever `UniValue::read` built (its result
/// is not checked, notes D5).
// Core: governance/object.cpp:272-299
fn parse_proposal_json(data_hex: &[u8], errors: &mut String) -> Option<Value> {
    if data_hex.is_empty() {
        return None;
    }
    let data = parse_hex(data_hex);
    if data.len() > MAX_DATA_SIZE {
        // An assignment in Core, not an append.
        *errors = "data exceeds 512 characters;".to_owned();
        return None;
    }
    if data.is_empty() {
        return None;
    }
    let (_, value) = Value::read(&data);
    if !value.is_object() {
        errors.push_str("Proposal must be a JSON object;");
        return None;
    }
    Some(value)
}

/// `governance::ValidateProposal(data_hex, errors, check_expiration, allow_script)` with
/// `GetAdjustedTime()` = `adjusted_now`: `Err` holds Core's `strErrorOut`.
// Core: governance/object.cpp:999-1033
pub fn validate_proposal(
    data_hex: &[u8],
    check_expiration: bool,
    allow_script: bool,
    adjusted_now: i64,
    p: &GovParams,
) -> Result<(), String> {
    let mut errors = String::new();
    let Some(obj) = parse_proposal_json(data_hex, &mut errors) else {
        errors.push_str("JSON parsing error;");
        return Err(errors);
    };
    let failure = if !validate_type(&obj, &mut errors) {
        "Invalid type;"
    } else if !validate_name(&obj, &mut errors) {
        "Invalid name;"
    } else if !validate_start_end_epoch(&obj, check_expiration, adjusted_now, &mut errors) {
        "Invalid start:end range;"
    } else if !validate_payment_amount(&obj, &mut errors) {
        "Invalid payment amount;"
    } else if !validate_payment_address(&obj, allow_script, p, &mut errors) {
        "Invalid payment address;"
    } else if !validate_url(&obj, &mut errors) {
        "Invalid URL;"
    } else {
        return Ok(());
    };
    errors.push_str(failure);
    Err(errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::hex_str;
    use crate::test_support::params;
    use dash_params::Network;

    fn validate(json: &str) -> Result<(), String> {
        let p = params(Network::Main);
        validate_proposal(hex_str(json.as_bytes()).as_bytes(), false, false, 1, &p)
    }

    #[test]
    fn partial_objects_are_validated() {
        // A truncated object still reads `type`, so the name check reports the missing value.
        assert_eq!(
            validate(r#"{"type":1,"name":"#),
            Err(format!("{};name field not found;Invalid name;", crate::json::out_of_range(1, 1)))
        );
        assert_eq!(
            validate("[1]"),
            Err("Proposal must be a JSON object;JSON parsing error;".to_owned())
        );
        assert_eq!(
            validate(r#"{"type":"1"}"#),
            Err("type field not found;Invalid type;".to_owned())
        );
        assert_eq!(validate(r#"{"type":2}"#), Err("type is not 1;Invalid type;".to_owned()));
        assert_eq!(
            validate(r#"{"type":1,"name":1}"#),
            Err("JSON value of type number is not of expected type string;name field not found;Invalid name;"
                .to_owned())
        );
    }

    #[test]
    fn data_limits() {
        let p = params(Network::Main);
        assert_eq!(
            validate_proposal(b"", false, false, 1, &p),
            Err("JSON parsing error;".to_owned())
        );
        assert_eq!(
            validate_proposal(b"zz", false, false, 1, &p),
            Err("JSON parsing error;".to_owned())
        );
        let most = hex_str(&[b' '; MAX_DATA_SIZE]);
        assert_eq!(
            validate_proposal(most.as_bytes(), false, false, 1, &p),
            Err("Proposal must be a JSON object;JSON parsing error;".to_owned())
        );
        let long = hex_str(&[b' '; MAX_DATA_SIZE + 1]);
        assert_eq!(
            validate_proposal(long.as_bytes(), false, false, 1, &p),
            Err("data exceeds 512 characters;JSON parsing error;".to_owned())
        );
    }

    /// `proposals_valid.json`'s first case with `name` and `url` replaced.
    fn proposal(name: &str, url: &str) -> Result<(), String> {
        validate(&format!(
            r#"{{"end_epoch":1491368400,"name":"{name}","payment_address":"XpG61qAVhdyN7AqVZQsHfJL7AEk4dPVinc","payment_amount":25.75,"start_epoch":1474261086,"type":1,"url":"{url}"}}"#
        ))
    }

    #[test]
    fn name_and_url_bounds() {
        assert_eq!(proposal(&"a".repeat(MAX_NAME_SIZE), "abcd"), Ok(()));
        assert_eq!(
            proposal(&"a".repeat(MAX_NAME_SIZE + 1), "abcd"),
            Err("name exceeds 40 characters;Invalid name;".to_owned())
        );
        assert_eq!(proposal("n", "abc"), Err("url too short;Invalid URL;".to_owned()));
        // A bracket outside a `//` netloc is not checked.
        assert_eq!(proposal("n", "aaaa["), Ok(()));
        assert_eq!(proposal("n", "x:ab["), Ok(()));
        assert_eq!(proposal("n", "x://ab["), Err("url invalid;Invalid URL;".to_owned()));
    }

    #[test]
    fn urls() {
        assert!(check_url(b"https://x/[y"));
        assert!(!check_url(b"https://[x/y"));
        assert!(!check_url(b"//x]"));
        assert!(check_url(b"a:b"));
    }
}
