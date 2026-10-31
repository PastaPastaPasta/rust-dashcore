//! One JSON-RPC call (`JSONRPCRequest`, `rpc/request.cpp`).

use crate::UniValue;
use crate::protocol::{RPC_INVALID_REQUEST, RpcError};

/// Core's `JSONRPCRequest`.
#[derive(Clone, Debug)]
pub struct JsonRpcRequest {
    pub id: UniValue,
    pub method: String,
    /// An array, or an object of named arguments.
    pub params: UniValue,
    /// The HTTP request's URI (`/`, `/wallet/<name>`).
    pub uri: String,
    pub auth_user: String,
    /// The client, as `ToStringAddrPort` prints it.
    pub peer_addr: String,
}

impl Default for JsonRpcRequest {
    fn default() -> Self {
        JsonRpcRequest {
            id: UniValue::Null,
            method: String::new(),
            params: UniValue::Null,
            uri: String::new(),
            auth_user: String::new(),
            peer_addr: String::new(),
        }
    }
}

impl JsonRpcRequest {
    /// Core's `JSONRPCRequest::parse`. `log_ips` is `-logips`.
    // Core: rpc/request.cpp:170-199
    pub fn parse(&mut self, value: &UniValue, log_ips: bool) -> Result<(), RpcError> {
        let UniValue::Obj(request) = value else {
            return Err(RpcError::new(RPC_INVALID_REQUEST, "Invalid Request object"));
        };
        // Parse id now so errors from here on are reported with it.
        self.id = find_value(request, "id").clone();
        self.method = match find_value(request, "method") {
            UniValue::Null => return Err(RpcError::new(RPC_INVALID_REQUEST, "Missing method")),
            UniValue::Str(m) => m.clone(),
            _ => return Err(RpcError::new(RPC_INVALID_REQUEST, "Method must be a string")),
        };
        if self.method != "getblocktemplate" {
            let method = sanitize_string(&self.method);
            if log_ips {
                tracing::debug!(
                    target: "rpc",
                    "ThreadRPCServer method={method} user={} peeraddr={}",
                    self.auth_user,
                    self.peer_addr
                );
            } else {
                tracing::debug!(target: "rpc", "ThreadRPCServer method={method} user={}", self.auth_user);
            }
        }
        self.params = match find_value(request, "params") {
            p @ (UniValue::Arr(_) | UniValue::Obj(_)) => p.clone(),
            UniValue::Null => UniValue::new_array(),
            _ => {
                return Err(RpcError::new(
                    RPC_INVALID_REQUEST,
                    "Params must be an array or object",
                ));
            }
        };
        Ok(())
    }

    /// Core's `JSONRPCRequest::squashed`: `<method> <sub>` called as `<method><sub>` with the
    /// sub-command dropped from the arguments. Core takes the first *value*, so named arguments
    /// lose their names here, as in Core.
    // Core: rpc/request.cpp:201-213
    pub fn squashed(&self) -> Result<JsonRpcRequest, RpcError> {
        let values = values(&self.params);
        let Some(first) = values.first() else {
            return Ok(self.clone());
        };
        let mut new_request = self.clone();
        new_request.method = format!("{}{}", self.method, first.get_str()?);
        new_request.params = UniValue::Arr(values[1..].iter().map(|v| (*v).clone()).collect());
        Ok(new_request)
    }
}

/// Core's `UniValue::find_value`: the first member named `key`, or null.
pub fn find_value<'a>(object: &'a [(String, UniValue)], key: &str) -> &'a UniValue {
    object.iter().find(|(k, _)| k == key).map_or(&UniValue::Null, |(_, v)| v)
}

/// Core's `UniValue::getValues` without the type check: the elements of an array or the values
/// of an object, empty otherwise.
pub fn values(v: &UniValue) -> Vec<&UniValue> {
    v.get_values().unwrap_or_default()
}

/// Core's `UniValue::operator[](size_t)`: the `i`th element or value, null past the end and for
/// scalars.
pub fn param(v: &UniValue, i: usize) -> &UniValue {
    match v {
        UniValue::Arr(a) => a.get(i).unwrap_or(&UniValue::Null),
        UniValue::Obj(o) => o.get(i).map_or(&UniValue::Null, |(_, v)| v),
        _ => &UniValue::Null,
    }
}

/// Core's `SanitizeString(str, SAFE_CHARS_DEFAULT)`: drops every byte outside the safe set.
// Core: util/strencodings.cpp:21-37
pub fn sanitize_string(s: &str) -> String {
    const SAFE: &str = " abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.,;-_/:?@()";
    s.chars().filter(|c| SAFE.contains(*c)).collect()
}

/// Core's `SanitizeString(str, SAFE_CHARS_URI)`.
pub fn sanitize_uri(s: &str) -> String {
    const SAFE: &str =
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!*'();:@&=+$,/?#[]-_.~%";
    s.chars().filter(|c| SAFE.contains(*c)).collect()
}

/// Core's `UniValue::getInt<int>`: a number whose whole text is an `int`. A value of another
/// type is a `type_error`; a number that is not an `int` a plain `runtime_error`.
pub fn get_int_i32(v: &UniValue) -> Result<i32, RpcError> {
    i32::try_from(get_int_i64(v)?)
        .map_err(|_| RpcError::Runtime("JSON integer out of range".into()))
}

/// Core's `UniValue::getInt<uint64_t>`: `from_chars` over the whole number's text, so a sign,
/// a fraction or an exponent is "JSON integer out of range".
// Core: univalue/include/univalue.h:136-147
pub fn get_int_u64(v: &UniValue) -> Result<u64, RpcError> {
    if !v.is_num() {
        v.get_int64()?;
    }
    let text = v.get_val_str();
    text.bytes()
        .all(|c| c.is_ascii_digit())
        .then(|| text.parse().ok())
        .flatten()
        .ok_or_else(|| RpcError::Runtime("JSON integer out of range".into()))
}

/// Core's `UniValue::getInt<uint32_t>`, as [`get_int_u64`] for a `uint32_t`.
// Core: univalue/include/univalue.h:136-147
pub fn get_int_u32(v: &UniValue) -> Result<u32, RpcError> {
    u32::try_from(get_int_u64(v)?)
        .map_err(|_| RpcError::Runtime("JSON integer out of range".into()))
}

/// Core's `UniValue::getInt<int64_t>`, as [`get_int_i32`] for an `int64_t`.
// Core: univalue/include/univalue.h:136-147
pub fn get_int_i64(v: &UniValue) -> Result<i64, RpcError> {
    if !v.is_num() {
        v.get_int64()?;
    }
    v.get_int64().map_err(|e| RpcError::Runtime(e.0))
}

/// `IsHex(str)` then `ParseHex(str)`: a non-empty, even run of hex digits.
// Core: util/strencodings.cpp:62-68, 81-97
pub(crate) fn parse_hex(hex: &str) -> Option<Vec<u8>> {
    if hex.is_empty() || !hex.len().is_multiple_of(2) || !hex.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return None;
    }
    let digit = |c: u8| char::from(c).to_digit(16).expect("a hex digit") as u8;
    Some(
        hex.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[hi, lo]| digit(*hi) << 4 | digit(*lo))
            .collect(),
    )
}
