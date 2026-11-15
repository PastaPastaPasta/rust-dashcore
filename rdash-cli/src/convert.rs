//! Command-line arguments to JSON-RPC parameters.
//!
//! Core: `RPCConvertValues`, `RPCConvertNamedValues` and `CRPCConvertTable` of src/rpc/client.cpp
//! at v24.0.0-rc.3. The table (`vRPCConvertParams`) is `dash_rpc_spec::CONVERT_PARAMS`, generated
//! from that file; nothing is hand-written here.

use dash_rpc_spec::{convert_param, convert_param_by_name, has_convert_params};

use crate::json::{UniValue, UniValueExt};

/// Core: `MaybeUnquoteString`: one pair of matching single or double quotes is removed.
fn maybe_unquote(arg: &str) -> &str {
    let b = arg.as_bytes();
    if b.len() >= 2
        && ((b[0] == b'\'' && b[b.len() - 1] == b'\'') || (b[0] == b'"' && b[b.len() - 1] == b'"'))
    {
        &arg[1..arg.len() - 1]
    } else {
        arg
    }
}

/// Core: `LikelyJSONType`.
fn likely_json_type(arg: &str) -> bool {
    let b = maybe_unquote(arg).as_bytes();
    b.len() >= 2
        && ((b[0] == b'[' && b[b.len() - 1] == b']') || (b[0] == b'{' && b[b.len() - 1] == b'}'))
}

/// Core: `Parse`: the `std::runtime_error` text is the error.
fn parse(raw: &str) -> Result<UniValue, String> {
    UniValue::read(raw.as_bytes()).ok_or_else(|| format!("Error parsing JSON: {raw}"))
}

/// Core: `CRPCConvertTable::ArgToUniValue`. `rule` is the table entry for the argument:
/// `Some(preserve_str)` when the argument is not a plain string.
fn arg_to_univalue(arg: &str, rule: Option<bool>) -> Result<UniValue, String> {
    match rule {
        Some(preserve_str) if !preserve_str || likely_json_type(arg) => parse(maybe_unquote(arg)),
        _ => Ok(UniValue::from(arg)),
    }
}

/// Core: `RPCConvertValues`: positional arguments as a JSON array.
pub fn rpc_convert_values(method: &str, params: &[String]) -> Result<UniValue, String> {
    let mut out = UniValue::new_array();

    // If we are using a subcommand that is in the table, update the method name.
    let candidate;
    let method = match params.first() {
        Some(first) if !method.contains(' ') => {
            candidate = format!("{method} {first}");
            if has_convert_params(&candidate) {
                candidate.as_str()
            } else {
                method
            }
        }
        _ => method,
    };

    for (idx, value) in params.iter().enumerate() {
        let rule = convert_param(method, idx).map(|p| p.preserve_str);
        out.push_back(arg_to_univalue(value, rule)?);
    }
    Ok(out)
}

/// Core: `RPCConvertNamedValues`: `name=value` arguments as a JSON object; positional
/// arguments go under `args`.
pub fn rpc_convert_named_values(method: &str, params: &[String]) -> Result<UniValue, String> {
    let mut out = UniValue::new_object();
    let mut positional = UniValue::new_array();

    // If we are using a subcommand that is in the table, update the method name.
    let candidate;
    let method = match params.first() {
        Some(first) if !method.contains(' ') && !first.contains('=') => {
            candidate = format!("{method} {first}");
            if has_convert_params(&candidate) {
                candidate.as_str()
            } else {
                method
            }
        }
        _ => method,
    };

    for s in params {
        let Some((name, value)) = s.split_once('=') else {
            let rule = convert_param(method, positional.size()).map(|p| p.preserve_str);
            positional.push_back(arg_to_univalue(s, rule)?);
            continue;
        };
        // Later named values overwrite earlier ones (`pushKV`).
        let rule = convert_param_by_name(method, name).map(|p| p.preserve_str);
        out.push_kv(name, arg_to_univalue(value, rule)?);
    }

    if !positional.is_empty_container() {
        // `pushKVEnd`, so an explicit "args" value is not overwritten by the implicit one.
        match &mut out {
            UniValue::Obj(entries) => entries.push(("args".to_string(), positional)),
            _ => unreachable!("created as an object"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    // The cases of Core's src/test/rpc_tests.cpp `rpc_parse_monetary_values`-neighbour tests
    // for the converter (`rpc_convert_values`, `rpc_convert_values_generatetoaddress`,
    // `rpc_convert_values_generatetoaddress`) at v24.

    #[test]
    fn positional_conversion() {
        let v = rpc_convert_values("getblock", &strings(&["abc", "2"])).unwrap();
        assert_eq!(v.write(), r#"["abc",2]"#);
        let v = rpc_convert_values(
            "generatetoaddress",
            &strings(&["101", "mkESjLZW66TmHhiFX8MCaBjrhZ543PPh9a"]),
        )
        .unwrap();
        assert_eq!(v.write(), r#"[101,"mkESjLZW66TmHhiFX8MCaBjrhZ543PPh9a"]"#);
    }

    #[test]
    fn preserve_str_arguments_parse_only_json_containers() {
        // `sendmany`'s amounts argument is an object and takes JSON; a plain string stays one.
        let v = rpc_convert_values("sendmany", &strings(&["", "{\"a\":1}"])).unwrap();
        assert_eq!(v.write(), r#"["",{"a":1}]"#);
    }

    #[test]
    fn bad_json_is_an_error() {
        assert_eq!(
            rpc_convert_values("getblock", &strings(&["x", "{"])).unwrap_err(),
            "Error parsing JSON: {"
        );
    }

    #[test]
    fn named_conversion() {
        let v = rpc_convert_named_values("getblock", &strings(&["blockhash=abc", "verbosity=2"]))
            .unwrap();
        assert_eq!(v.write(), r#"{"blockhash":"abc","verbosity":2}"#);
        let v = rpc_convert_named_values("getblock", &strings(&["abc", "verbosity=2"])).unwrap();
        assert_eq!(v.write(), r#"{"verbosity":2,"args":["abc"]}"#);
        let v = rpc_convert_named_values("getblock", &[]).unwrap();
        assert_eq!(v.write(), "{}");
    }

    #[test]
    fn subcommand_rule_applies() {
        // `protx register_fund` is in the table as a two-word method.
        assert!(has_convert_params("protx register_fund"));
        let v = rpc_convert_values("protx", &strings(&["register_fund", "a", "b", "1"])).unwrap();
        // Position 0 is the sub-command itself.
        assert_eq!(v.at(0).write(), r#""register_fund""#);
    }
}
