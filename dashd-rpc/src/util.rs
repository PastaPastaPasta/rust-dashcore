//! The `util` commands that need no wallet and no chain (RP1a-3a): `rpc/output_script.cpp`'s
//! `validateaddress`, `createmultisig`, `getdescriptorinfo` and `deriveaddresses`, and
//! `rpc/signmessage.cpp`'s `verifymessage` and `signmessagewithprivkey`, over the chain's
//! [`KeyIoParams`].

use std::sync::Arc;

use crate::UniValue;
use crate::descriptor::{
    ParsedDescriptor, SigningProvider, get_descriptor_checksum, hex, infer_p2sh_descriptor,
    multisig_script,
};
use crate::key_io::{
    Destination, KeyIoParams, decode_destination_with_error, decode_platform_destination,
    decode_secret, encode_destination, encode_platform_destination, extract_destination, hash160,
    parse_fully_valid,
};
use crate::message::{MessageVerification, message_sign, message_verify};
use crate::protocol::{
    RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RPC_TYPE_ERROR, RpcError,
};
use crate::request::{find_value, get_int_i32, get_int_i64, param, parse_hex, values};
use crate::server::{Command, RpcServer};

/// `MAX_PUBKEYS_PER_MULTISIG`.
const MAX_PUBKEYS_PER_MULTISIG: usize = 20;
/// `MAX_SCRIPT_ELEMENT_SIZE`.
const MAX_SCRIPT_ELEMENT_SIZE: usize = 520;

/// Registers the six commands on `server`.
// Core: rpc/output_script.cpp:284-294, rpc/signmessage.cpp:106-115
pub fn register_util_commands(server: &RpcServer, key_io: KeyIoParams) {
    let p = Arc::new(key_io);
    type Handler = fn(&KeyIoParams, &UniValue) -> Result<UniValue, RpcError>;
    let commands: [(&str, Handler); 6] = [
        ("validateaddress", validateaddress),
        ("createmultisig", createmultisig),
        ("deriveaddresses", deriveaddresses),
        ("getdescriptorinfo", getdescriptorinfo),
        ("verifymessage", verifymessage),
        ("signmessagewithprivkey", signmessagewithprivkey),
    ];
    for (name, handler) in commands {
        let p = Arc::clone(&p);
        server.append_command(Command::new(name, move |r| handler(&p, &r.params)));
    }
}

/// `DescribeAddress`.
// Core: rpc/util.cpp:277-300
fn describe_address(ret: &mut UniValue, dest: &Destination) {
    ret.push_kv_end("isscript", matches!(dest, Destination::ScriptHash(_)).into());
}

/// Core's `validateaddress`: an L1 address, else a DIP-18 Platform address described by its
/// credit output script, else the L1 decoder's error and its locations.
// Core: rpc/output_script.cpp:26-97
fn validateaddress(p: &KeyIoParams, params: &UniValue) -> Result<UniValue, RpcError> {
    let address = param(params, 0).get_str()?;
    let mut ret = UniValue::new_object();
    match decode_destination_with_error(address, p) {
        Ok(dest) => {
            ret.push_kv_end("isvalid", true.into());
            ret.push_kv_end("address", encode_destination(&dest, p).into());
            ret.push_kv_end("scriptPubKey", hex(&dest.script()).into());
            describe_address(&mut ret, &dest);
        }
        Err((error, locations)) => match decode_platform_destination(address, p) {
            Ok(platform) => {
                ret.push_kv_end("isvalid", true.into());
                ret.push_kv_end("isplatform", true.into());
                ret.push_kv_end("address", encode_platform_destination(&platform, p).into());
                let script = platform.script();
                ret.push_kv_end("scriptPubKey", hex(&script).into());
                if let Some(l1) = extract_destination(&script) {
                    describe_address(&mut ret, &l1);
                }
            }
            Err(_) => {
                ret.push_kv_end("isvalid", false.into());
                let mut indices = UniValue::new_array();
                for i in locations {
                    indices.push_back(i64::from(i).into());
                }
                ret.push_kv_end("error_locations", indices);
                ret.push_kv_end("error", error.into());
            }
        },
    }
    Ok(ret)
}

/// `HexToPubKey`.
// Core: rpc/util.cpp:217-231
fn hex_to_pubkey(hex_in: &str) -> Result<Vec<u8>, RpcError> {
    let invalid =
        |why: &str| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, format!("Pubkey \"{hex_in}\" {why}"));
    let data = parse_hex(hex_in).ok_or_else(|| invalid("must be a hex string"))?;
    if hex_in.len() != 66 && hex_in.len() != 130 {
        return Err(invalid("must have a length of either 33 or 65 bytes"));
    }
    parse_fully_valid(&data).ok_or_else(|| invalid("must be cryptographically valid."))?;
    Ok(data)
}

/// Core's `createmultisig`: the P2SH address of `nrequired`-of-the-keys, its redeem script, and
/// the descriptor `InferDescriptor` gives the address with that script known.
// Core: rpc/output_script.cpp:99-153, rpc/util.cpp:254-275
fn createmultisig(p: &KeyIoParams, params: &UniValue) -> Result<UniValue, RpcError> {
    let required = get_int_i32(param(params, 0))?;
    let pubkeys = values(param(params, 1))
        .into_iter()
        .map(|k| hex_to_pubkey(k.get_str()?))
        .collect::<Result<Vec<_>, _>>()?;
    let invalid = |msg: String| RpcError::new(RPC_INVALID_PARAMETER, msg);
    if required < 1 {
        return Err(invalid(
            "a multisignature address must require at least one key to redeem".into(),
        ));
    }
    if pubkeys.len() < required as usize {
        return Err(invalid(format!(
            "not enough keys supplied (got {} keys, but need at least {required} to redeem)",
            pubkeys.len()
        )));
    }
    if pubkeys.len() > MAX_PUBKEYS_PER_MULTISIG {
        return Err(invalid(format!(
            "Number of keys involved in the multisignature address creation > {MAX_PUBKEYS_PER_MULTISIG}\nReduce the number"
        )));
    }
    let redeem = multisig_script(required as usize, &pubkeys);
    if redeem.len() > MAX_SCRIPT_ELEMENT_SIZE {
        return Err(invalid(format!(
            "redeemScript exceeds size limit: {} > {MAX_SCRIPT_ELEMENT_SIZE}",
            redeem.len()
        )));
    }
    let dest = Destination::ScriptHash(hash160(&redeem));
    let mut ret = UniValue::new_object();
    ret.push_kv_end("address", encode_destination(&dest, p).into());
    ret.push_kv_end("redeemScript", hex(&redeem).into());
    ret.push_kv_end("descriptor", infer_p2sh_descriptor(&redeem, p).into());
    Ok(ret)
}

/// Core's `getdescriptorinfo`.
// Core: rpc/output_script.cpp:155-198
fn getdescriptorinfo(p: &KeyIoParams, params: &UniValue) -> Result<UniValue, RpcError> {
    let descriptor = param(params, 0).get_str()?;
    let desc = ParsedDescriptor::parse(descriptor, p, false)
        .map_err(|e| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, e))?;
    let mut ret = UniValue::new_object();
    ret.push_kv_end("descriptor", desc.to_string(p).into());
    ret.push_kv_end("checksum", get_descriptor_checksum(descriptor).into());
    ret.push_kv_end("isrange", desc.is_range().into());
    ret.push_kv_end("issolvable", desc.is_solvable().into());
    ret.push_kv_end("hasprivatekeys", desc.has_private_keys().into());
    Ok(ret)
}

/// `ParseRange` then `ParseDescriptorRange`: `end` or `[begin, end]`, within `[0, 2^31)` and
/// fewer than a million positions.
// Core: rpc/util.cpp:1126-1153
pub(crate) fn parse_descriptor_range(value: &UniValue) -> Result<(i64, i64), RpcError> {
    let invalid = |msg: &str| Err(RpcError::new(RPC_INVALID_PARAMETER, msg));
    let (low, high) = match value {
        UniValue::Num(_) => (0, get_int_i64(value)?),
        UniValue::Arr(a) if a.len() == 2 && a[0].is_num() && a[1].is_num() => {
            let (low, high) = (get_int_i64(&a[0])?, get_int_i64(&a[1])?);
            if low > high {
                return invalid("Range specified as [begin,end] must not have begin after end");
            }
            (low, high)
        }
        _ => return invalid("Range must be specified as end or as [begin,end]"),
    };
    if low < 0 {
        return invalid("Range should be greater or equal than 0");
    }
    if high >> 31 != 0 {
        return invalid("End of range is too high");
    }
    if high >= low + 1_000_000 {
        return invalid("Range is too large");
    }
    Ok((low, high))
}

/// `EvalDescriptorStringOrObject`: a scan object's scripts, a descriptor string or a
/// `{"desc", "range"}` object (range `[0, 1000]` by default, `0` alone for an unranged
/// descriptor), each position's expansion in turn. The descriptor needs no checksum; its
/// expansions fill `provider` (`scantxoutset` gives each scan object its own).
// Core: rpc/util.cpp:1184-1215
pub(crate) fn eval_descriptor_string_or_object(
    scanobject: &UniValue,
    provider: &mut SigningProvider,
    p: &KeyIoParams,
) -> Result<Vec<Vec<u8>>, RpcError> {
    let mut range = (0, 1000);
    let desc_str = match scanobject {
        UniValue::Str(s) => s.as_str(),
        UniValue::Obj(o) => {
            let desc_uni = find_value(o, "desc");
            if desc_uni.is_null() {
                return Err(RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "Descriptor needs to be provided in scan object",
                ));
            }
            let desc_str = desc_uni.get_str()?;
            let range_uni = find_value(o, "range");
            if !range_uni.is_null() {
                range = parse_descriptor_range(range_uni)?;
            }
            desc_str
        }
        _ => {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "Scan object needs to be either a string or an object",
            ));
        }
    };
    let desc = ParsedDescriptor::parse(desc_str, p, false)
        .map_err(|e| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, e))?;
    if !desc.is_range() {
        range = (0, 0);
    }
    let mut ret = Vec::new();
    for i in range.0..=range.1 {
        let pos = u32::try_from(i).expect("the range is within [0, 2^31)");
        let scripts = desc.expand_into(pos, provider).ok_or_else(|| {
            RpcError::new(
                RPC_INVALID_ADDRESS_OR_KEY,
                format!("Cannot derive script without private keys: '{desc_str}'"),
            )
        })?;
        ret.extend(scripts);
    }
    Ok(ret)
}

/// Core's `deriveaddresses`: the descriptor needs its checksum, and a range exactly when it is
/// ranged (a present argument counts, even a null one).
// Core: rpc/output_script.cpp:200-282
fn deriveaddresses(p: &KeyIoParams, params: &UniValue) -> Result<UniValue, RpcError> {
    let desc_str = param(params, 0).get_str()?;
    let n_params = values(params).len();
    let (begin, end) = match param(params, 1) {
        UniValue::Null => (0, 0),
        range => parse_descriptor_range(range)?,
    };
    let desc = ParsedDescriptor::parse(desc_str, p, true)
        .map_err(|e| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, e))?;
    if !desc.is_range() && n_params > 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "Range should not be specified for an un-ranged descriptor",
        ));
    }
    if desc.is_range() && n_params == 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "Range must be specified for a ranged descriptor",
        ));
    }
    let mut addresses = UniValue::new_array();
    for i in begin..=end {
        let pos = u32::try_from(i).expect("the range is within [0, 2^31)");
        let scripts = desc.expand(pos).ok_or_else(|| {
            RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Cannot derive script without private keys")
        })?;
        for script in scripts {
            let dest = extract_destination(&script).ok_or_else(|| {
                RpcError::new(
                    RPC_INVALID_ADDRESS_OR_KEY,
                    "Descriptor does not have a corresponding address",
                )
            })?;
            addresses.push_back(encode_destination(&dest, p).into());
        }
    }
    // Core: "This should not be possible, but an assert seems overkill".
    if values(&addresses).is_empty() {
        return Err(RpcError::new(RPC_MISC_ERROR, "Unexpected empty result"));
    }
    Ok(addresses)
}

/// Core's `verifymessage`.
// Core: rpc/signmessage.cpp:18-62
fn verifymessage(p: &KeyIoParams, params: &UniValue) -> Result<UniValue, RpcError> {
    let address = param(params, 0).get_str()?;
    let signature = param(params, 1).get_str()?;
    let message = param(params, 2).get_str()?;
    match message_verify(address, signature, message, p) {
        MessageVerification::InvalidAddress => {
            Err(RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Invalid address"))
        }
        MessageVerification::AddressNoKey => {
            Err(RpcError::new(RPC_TYPE_ERROR, "Address does not refer to key"))
        }
        MessageVerification::MalformedSignature => {
            Err(RpcError::new(RPC_TYPE_ERROR, "Malformed base64 encoding"))
        }
        MessageVerification::PubkeyNotRecovered | MessageVerification::NotSigned => {
            Ok(false.into())
        }
        MessageVerification::Ok => Ok(true.into()),
    }
}

/// Core's `signmessagewithprivkey`. `MessageSign` cannot fail for a valid key, so Core's
/// "Sign failed" is unreachable.
// Core: rpc/signmessage.cpp:65-104
fn signmessagewithprivkey(p: &KeyIoParams, params: &UniValue) -> Result<UniValue, RpcError> {
    let privkey = param(params, 0).get_str()?;
    let message = param(params, 1).get_str()?;
    let key = decode_secret(privkey, p)
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Invalid private key"))?;
    Ok(message_sign(&key, message).into())
}

#[cfg(test)]
mod tests;
