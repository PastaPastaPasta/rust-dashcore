//! The protocol tables of the `dash` crate equal Dash Core v24's (BS18).
//!
//! `tests/data/core_v24_protocol_tables.json` is extracted from Core's sources at the recorded tag
//! by `contrib/node/core-protocol-tables.py` (rerun it with `--check` to see whether the file is
//! current). Each test compares one table of the crate with the extraction, in both directions:
//! a Core entry the crate lacks and a crate entry Core lacks both fail.

use dashcore::blockdata::opcodes::{self, All, get_op_name};
use dashcore::consensus::Decodable;
use dashcore::consensus::encode::{
    Error as EncodeError, MAX_COMPACT_SIZE, VarInt, deserialize, deserialize_partial, serialize,
};
use dashcore::network::constants::{self, ServiceFlags};
use dashcore::network::message::{
    MAX_MSG_SIZE, MAX_PROTOCOL_MESSAGE_LENGTH, MESSAGE_HEADER_SIZE, NetworkMessage,
    RawNetworkMessage,
};
use dashcore::network::message_blockdata::{INVENTORY_TYPES, Inventory, inv_type_command};
use serde_json::{Value, json};
use std::borrow::Cow;

fn tables() -> Value {
    serde_json::from_str(include_str!("data/core_v24_protocol_tables.json")).unwrap()
}

fn pairs(v: &Value) -> Vec<(String, u64)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| (e[0].as_str().unwrap().to_string(), e[1].as_u64().unwrap()))
        .collect()
}

/// The name/value pairs of `constants` that Core's `version.h` also defines, in Core's order.
fn crate_versions() -> Vec<(&'static str, u32)> {
    vec![
        ("PROTOCOL_VERSION", constants::PROTOCOL_VERSION),
        ("INIT_PROTO_VERSION", constants::INIT_PROTO_VERSION),
        ("MIN_PEER_PROTO_VERSION", constants::MIN_PEER_PROTO_VERSION),
        ("MIN_MASTERNODE_PROTO_VERSION", constants::MIN_MASTERNODE_PROTO_VERSION),
        ("ADDRV2_PROTO_VERSION", constants::ADDRV2_PROTO_VERSION),
        ("BLS_SCHEME_PROTO_VERSION", constants::BLS_SCHEME_PROTO_VERSION),
        ("DMN_TYPE_PROTO_VERSION", constants::DMN_TYPE_PROTO_VERSION),
        ("SMNLE_VERSIONED_PROTO_VERSION", constants::SMNLE_VERSIONED_PROTO_VERSION),
        ("MNLISTDIFF_VERSION_ORDER", constants::MNLISTDIFF_VERSION_ORDER),
        ("MNLISTDIFF_CHAINLOCKS_PROTO_VERSION", constants::MNLISTDIFF_CHAINLOCKS_PROTO_VERSION),
        ("NO_LEGACY_ISLOCK_PROTO_VERSION", constants::NO_LEGACY_ISLOCK_PROTO_VERSION),
        ("DSQ_INV_VERSION", constants::DSQ_INV_VERSION),
        ("INCREASE_MAX_HEADERS2_VERSION", constants::INCREASE_MAX_HEADERS2_VERSION),
        ("BIP324_DASH_BASELINE_VERSION", constants::BIP324_DASH_BASELINE_VERSION),
        ("EFFICIENT_QRINFO_VERSION", constants::EFFICIENT_QRINFO_VERSION),
        ("ISDLOCK_CYCLEHASH_UPDATE_VERSION", constants::ISDLOCK_CYCLEHASH_UPDATE_VERSION),
        ("PLATFORM_BAN_VERSION", constants::PLATFORM_BAN_VERSION),
        ("QFCOMMIT_STALE_REPROP_BAN_VERSION", constants::QFCOMMIT_STALE_REPROP_BAN_VERSION),
        ("PLATFORMBAN_V2_SHORT_ID_VERSION", constants::PLATFORMBAN_V2_SHORT_ID_VERSION),
        ("COINJOIN_REBALANCE_VERSION", constants::COINJOIN_REBALANCE_VERSION),
        ("ASSET_UNLOCK_INV_VERSION", constants::ASSET_UNLOCK_INV_VERSION),
    ]
}

#[test]
fn protocol_version_constants_equal_core() {
    let core = pairs(&tables()["version"]);
    let ours: Vec<(String, u64)> =
        crate_versions().into_iter().map(|(n, v)| (n.to_string(), v as u64)).collect();
    assert_eq!(ours, core);
    assert_eq!(constants::PROTOCOL_VERSION, 70242);
}

#[test]
fn inventory_type_table_equals_core() {
    let core = pairs(&tables()["inventory"]["members"]);
    assert_eq!(core.len(), 20, "Core's GetDataMsg has twenty members, UNDEFINED included");
    let ours: Vec<(String, u64)> =
        INVENTORY_TYPES.iter().map(|(n, v)| (n.to_string(), *v as u64)).collect();
    assert_eq!(ours, core);
}

#[test]
fn inventory_commands_and_strings_equal_core() {
    let t = tables();
    let members = pairs(&t["inventory"]["members"]);
    let commands: Vec<(String, String)> = t["inventory"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e[0].as_str().unwrap().to_string(), e[1].as_str().unwrap().to_string()))
        .collect();
    // Exactly Core's rows: a command row for a member the crate gives no command (or the reverse)
    // fails, as does an extra row on either side.
    // (Core lists them in its `switch` order, the crate in enum order: compare the row sets.)
    let mut ours: Vec<(String, String)> = INVENTORY_TYPES
        .iter()
        .filter_map(|(name, value)| {
            inv_type_command(*value).map(|command| (name.to_string(), command.to_string()))
        })
        .collect();
    let mut core_rows = commands.clone();
    ours.sort();
    core_rows.sort();
    assert_eq!(ours, core_rows);
    assert_eq!(t["inventory"]["to_string_known"], "%s %s");
    assert_eq!(t["inventory"]["to_string_unknown"], "0x%08x %s");

    let hash = {
        let mut h = [0u8; 32];
        for (i, b) in h.iter_mut().enumerate() {
            *b = i as u8 + 1;
        }
        h
    };
    let shown_hash: String = hash.iter().rev().map(|b| format!("{b:02x}")).collect();

    // Every number up to well past the table, and the BIP144 flags.
    let mut numbers: Vec<u32> = (0..=64).collect();
    numbers.extend([0x4000_0001, 0x4000_0002, u32::MAX]);
    for number in numbers {
        let name = members.iter().find(|(_, v)| *v == number as u64).map(|(n, _)| n.as_str());
        let command = name.and_then(|n| commands.iter().find(|(c, _)| c == n)).map(|(_, c)| c);

        assert_eq!(inv_type_command(number), command.map(String::as_str), "type {number}");

        let mut bytes = number.to_le_bytes().to_vec();
        bytes.extend_from_slice(&hash);
        let inv: Inventory = deserialize(&bytes).unwrap();
        assert_eq!(inv.inv_type(), number);
        assert_eq!(inv.command(), command.map(String::as_str));
        assert_eq!(inv.is_known_type(), command.is_some());
        assert_eq!(serialize(&inv), bytes, "type {number} round-trips");
        let expected = match command {
            Some(cmd) => format!("{cmd} {shown_hash}"),
            None => format!("0x{number:08x} {shown_hash}"),
        };
        assert_eq!(inv.to_string(), expected, "type {number}");
        // Only the numbers with a command are typed variants.
        assert_eq!(!matches!(inv, Inventory::Unknown { .. }), command.is_some(), "type {number}");
    }
}

/// Core's `ServiceFlags`, by Core's identifiers.
fn crate_service_flags() -> Vec<(&'static str, ServiceFlags)> {
    vec![
        ("NODE_NONE", ServiceFlags::NONE),
        ("NODE_NETWORK", ServiceFlags::NETWORK),
        ("NODE_BLOOM", ServiceFlags::BLOOM),
        ("NODE_COMPACT_FILTERS", ServiceFlags::COMPACT_FILTERS),
        ("NODE_NETWORK_LIMITED", ServiceFlags::NETWORK_LIMITED),
        ("NODE_HEADERS_COMPRESSED", ServiceFlags::HEADERS_COMPRESSED),
        ("NODE_P2P_V2", ServiceFlags::P2P_V2),
    ]
}

#[test]
fn service_flags_equal_core() {
    let t = tables();
    let ours: Vec<(String, u64)> =
        crate_service_flags().into_iter().map(|(n, f)| (n.to_string(), f.as_u64())).collect();
    assert_eq!(ours, pairs(&t["service_flags"]["members"]));
    assert_eq!(ServiceFlags::NODE_HEADERS_COMPRESSED, ServiceFlags::HEADERS_COMPRESSED);
    assert_eq!(constants::NODE_HEADERS_COMPRESSED, ServiceFlags::HEADERS_COMPRESSED);

    // `serviceFlagToStr` for every bit; the bits Core does not name are UNKNOWN[2^N].
    assert_eq!(t["service_flags"]["unknown_format"], "UNKNOWN[2^%u]");
    let names: Vec<(String, String)> = t["service_flags"]["names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e[0].as_str().unwrap().to_string(), e[1].as_str().unwrap().to_string()))
        .collect();
    // Exactly Core's rows: one name per named flag, in Core's order, and no other.
    let ours: Vec<(String, String)> = crate_service_flags()
        .into_iter()
        .filter(|(_, flag)| *flag != ServiceFlags::NONE)
        .map(|(id, flag)| (id.to_string(), flag.to_strings().remove(0)))
        .collect();
    assert_eq!(ours, names);
    let members = pairs(&t["service_flags"]["members"]);
    for bit in 0..64u32 {
        let value = 1u64 << bit;
        let core_name = members
            .iter()
            .find(|(_, v)| *v == value)
            .and_then(|(n, _)| names.iter().find(|(m, _)| m == n))
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| format!("UNKNOWN[2^{bit}]"));
        assert_eq!(ServiceFlags::from(value).to_string(), format!("ServiceFlags({core_name})"));
        assert_eq!(ServiceFlags::from(value).to_strings(), [core_name], "bit {bit}");
    }

    // `serviceFlagsToStr` lists the set bits from the lowest.
    let all = ServiceFlags::NETWORK
        | ServiceFlags::BLOOM
        | ServiceFlags::COMPACT_FILTERS
        | ServiceFlags::NETWORK_LIMITED
        | ServiceFlags::HEADERS_COMPRESSED
        | ServiceFlags::P2P_V2;
    assert_eq!(
        all.to_strings(),
        ["NETWORK", "BLOOM", "COMPACT_FILTERS", "NETWORK_LIMITED", "HEADERS_COMPRESSED", "P2P_V2"]
    );
    assert!(ServiceFlags::NONE.to_strings().is_empty());
}

#[test]
fn message_size_limits_equal_core() {
    let t = tables();
    let limits = &t["limits"];
    assert_eq!(MAX_PROTOCOL_MESSAGE_LENGTH as u64, limits["MAX_PROTOCOL_MESSAGE_LENGTH"]);
    assert_eq!(MAX_PROTOCOL_MESSAGE_LENGTH, 3 * 1024 * 1024);
    assert_eq!(MAX_COMPACT_SIZE, limits["MAX_SIZE"]);
    assert_eq!(MAX_COMPACT_SIZE, 32 * 1024 * 1024);
    // Core compares the announced size with both and so the smaller one decides.
    assert_eq!(MAX_MSG_SIZE as u64, (MAX_PROTOCOL_MESSAGE_LENGTH as u64).min(MAX_COMPACT_SIZE));
    assert_eq!(MESSAGE_HEADER_SIZE, 24);
    // Where Core applies them: the v1 header check, the v2 packet check and `ReadCompactSize`.
    assert_eq!(
        limits["applied_at"],
        json!({
            "v1_header": ["src/net.cpp", 738],
            "v2_contents": ["src/net.cpp", 1299],
            "compact_size": ["src/serialize.h", 318],
        })
    );
}

#[test]
fn the_extraction_is_of_the_pinned_core_release_and_has_no_other_tables() {
    let t = tables();
    // Every other test compares the crate with this file, so the file's provenance and shape are
    // part of the proof (`contrib/node/core-protocol-tables.py --check` regenerates it).
    assert_eq!(
        t["source"],
        json!({
            "repo": "dashpay/dash",
            "tag": "v24.0.0-rc.3",
            "commit": "0364ed626ec2eaa518bf1ae4a0566ce638c7fe18",
        })
    );
    let keys = |v: &Value| -> Vec<String> {
        let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };
    assert_eq!(keys(&t), ["inventory", "limits", "opcodes", "service_flags", "source", "version"]);
    assert_eq!(
        keys(&t["inventory"]),
        ["commands", "members", "to_string_known", "to_string_unknown"]
    );
    assert_eq!(keys(&t["service_flags"]), ["members", "names", "unknown_format"]);
    assert_eq!(keys(&t["limits"]), ["MAX_PROTOCOL_MESSAGE_LENGTH", "MAX_SIZE", "applied_at"]);
    assert_eq!(keys(&t["opcodes"]), ["get_op_name", "members"]);
}

/// A message with `len` announced payload bytes, `payload` of which follow.
fn message_bytes(command: &str, len: u32, payload: &[u8]) -> Vec<u8> {
    use dashcore::hashes::{Hash, sha256d};
    let mut bytes = 0xBD6B0CBFu32.to_le_bytes().to_vec();
    let mut cmd = [0u8; 12];
    cmd[..command.len()].copy_from_slice(command.as_bytes());
    bytes.extend_from_slice(&cmd);
    bytes.extend_from_slice(&len.to_le_bytes());
    bytes.extend_from_slice(&sha256d::Hash::hash(payload)[..4]);
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn a_message_payload_above_core_limit_is_refused_at_the_header() {
    // The header announces one byte too many and nothing follows: Core refuses on the size
    // (`Header error: Size too large`), so the error is the size, not a short read.
    let header_only = message_bytes("foo", MAX_PROTOCOL_MESSAGE_LENGTH as u32 + 1, &[]);
    assert_eq!(header_only.len(), MESSAGE_HEADER_SIZE);
    match deserialize::<RawNetworkMessage>(&header_only) {
        Err(EncodeError::OversizedVectorAllocation {
            requested,
            max,
        }) => {
            assert_eq!(requested, MAX_PROTOCOL_MESSAGE_LENGTH + 1);
            assert_eq!(max, MAX_PROTOCOL_MESSAGE_LENGTH);
        }
        other => panic!("expected the size error, got {other:?}"),
    }

    // A payload of exactly the limit is read (the old 5,000,000 byte limit would take 3 MiB + 1).
    let payload = vec![0xa5u8; MAX_PROTOCOL_MESSAGE_LENGTH];
    let bytes = message_bytes("foo", payload.len() as u32, &payload);
    let msg: RawNetworkMessage = deserialize(&bytes).unwrap();
    match msg.payload {
        NetworkMessage::Unknown {
            payload: p,
            ..
        } => assert_eq!(p.len(), MAX_PROTOCOL_MESSAGE_LENGTH),
        other => panic!("expected an unknown message, got {other:?}"),
    }

    // One byte more, even with the whole payload present, is refused.
    let payload = vec![0xa5u8; MAX_PROTOCOL_MESSAGE_LENGTH + 1];
    let bytes = message_bytes("foo", payload.len() as u32, &payload);
    assert!(matches!(
        deserialize::<RawNetworkMessage>(&bytes),
        Err(EncodeError::OversizedVectorAllocation { .. })
    ));
}

#[test]
fn a_streamed_message_of_exactly_the_core_limit_is_read() {
    // dash-spv reads from a stream with `consensus_decode`, which bounds the read at the header
    // plus the payload limit: a message of exactly the limit must fit.
    let payload = vec![0x5au8; MAX_PROTOCOL_MESSAGE_LENGTH];
    let mut bytes = message_bytes("foo", payload.len() as u32, &payload);
    bytes.extend_from_slice(b"next");
    let mut cursor = std::io::Cursor::new(bytes);
    let msg = RawNetworkMessage::consensus_decode(&mut cursor).unwrap();
    match msg.payload {
        NetworkMessage::Unknown {
            payload: p,
            ..
        } => assert_eq!(p.len(), MAX_PROTOCOL_MESSAGE_LENGTH),
        other => panic!("expected an unknown message, got {other:?}"),
    }
    // What follows the message is left in the stream.
    assert_eq!(cursor.position() as usize, MESSAGE_HEADER_SIZE + MAX_PROTOCOL_MESSAGE_LENGTH);

    let payload = vec![0x5au8; MAX_PROTOCOL_MESSAGE_LENGTH + 1];
    let bytes = message_bytes("foo", payload.len() as u32, &payload);
    assert!(matches!(
        RawNetworkMessage::consensus_decode(&mut std::io::Cursor::new(bytes)),
        Err(EncodeError::OversizedVectorAllocation { .. })
    ));
}

/// Decodes `bytes` through both entry points: the finite-reader one `deserialize_partial` uses and the
/// streaming `consensus_decode`, which wraps the reader in a budget unless the type opts out.
fn both_entry_points<T: Decodable>(bytes: &[u8]) -> [Result<T, EncodeError>; 2] {
    [
        deserialize_partial::<T>(bytes).map(|(value, _)| value),
        T::consensus_decode(&mut std::io::Cursor::new(bytes)),
    ]
}

fn oversized<T>(r: &Result<T, EncodeError>) -> bool {
    matches!(r, Err(EncodeError::OversizedVectorAllocation { .. }))
}

#[test]
fn a_length_above_max_size_is_refused_without_reading_data() {
    // `ReadCompactSize`'s range check: a vector or string length above MAX_SIZE fails on the
    // length alone; at MAX_SIZE it passes and the missing data is what fails.
    let announce = |len: u64| serialize(&VarInt(len));
    for len in [MAX_COMPACT_SIZE + 1, u64::from(u32::MAX) + 1, u64::MAX] {
        let bytes = announce(len);
        for r in both_entry_points::<Vec<u8>>(&bytes) {
            assert!(oversized(&r), "Vec<u8> length {len}");
        }
        for r in both_entry_points::<Box<[u8]>>(&bytes) {
            assert!(oversized(&r), "Box<[u8]> length {len}");
        }
        for r in both_entry_points::<Vec<Inventory>>(&bytes) {
            assert!(oversized(&r), "Vec<Inventory> length {len}");
        }
        for r in both_entry_points::<String>(&bytes) {
            assert!(oversized(&r), "String length {len}");
        }
    }
    for r in both_entry_points::<Vec<u8>>(&announce(MAX_COMPACT_SIZE)) {
        assert!(matches!(r, Err(EncodeError::Io(_))));
    }
    for r in both_entry_points::<String>(&announce(MAX_COMPACT_SIZE)) {
        assert!(matches!(r, Err(EncodeError::Io(_))));
    }
}

#[test]
fn byte_vectors_and_strings_are_read_to_the_core_range() {
    // Core reads a byte vector or string of any length `ReadCompactSize` accepts (serialize.h
    // `Unserialize` of `std::vector<unsigned char>` and `std::string`), in chunks. The crate's
    // `MAX_VEC_SIZE` (4,000,000) is only the allocation guard, not a limit on the data read.
    for len in [4_000_000usize, 4_000_001, MAX_COMPACT_SIZE as usize] {
        let mut bytes = serialize(&VarInt(len as u64));
        bytes.resize(bytes.len() + len, b'a');
        bytes.extend_from_slice(b"next");

        for r in both_entry_points::<Vec<u8>>(&bytes) {
            assert_eq!(r.unwrap().len(), len, "Vec<u8> {len}");
        }
        for r in both_entry_points::<Box<[u8]>>(&bytes) {
            assert_eq!(r.unwrap().len(), len, "Box<[u8]> {len}");
        }
        for r in both_entry_points::<String>(&bytes) {
            let s = r.unwrap();
            assert_eq!((s.len(), s.as_bytes()[len - 1]), (len, b'a'), "String {len}");
        }
        for r in both_entry_points::<Cow<'static, str>>(&bytes) {
            assert_eq!(r.unwrap().len(), len, "Cow<str> {len}");
        }

        // The streaming read stops after the data: what follows stays in the stream.
        let mut cursor = std::io::Cursor::new(&bytes);
        String::consensus_decode(&mut cursor).unwrap();
        assert_eq!(&bytes[cursor.position() as usize..], b"next");
    }

    // A lying length does not read (or allocate) the announced size: the data runs out.
    let mut bytes = serialize(&VarInt(MAX_COMPACT_SIZE));
    bytes.extend_from_slice(&[1, 2, 3]);
    for r in both_entry_points::<Vec<u8>>(&bytes) {
        assert!(matches!(r, Err(EncodeError::Io(_))));
    }
}

#[test]
fn other_vectors_are_read_to_the_core_range() {
    // 1,000,001 u32s are 4,000,004 bytes, over the old streaming budget.
    let count = 1_000_001u32;
    let values: Vec<u32> = (0..count).collect();
    let bytes = serialize(&values);
    assert!(bytes.len() > 4_000_000);
    for r in both_entry_points::<Vec<u32>>(&bytes) {
        assert_eq!(r.unwrap(), values);
    }
    // Vectors of vectors go through the same macro.
    let nested: Vec<Vec<u8>> = (0..200).map(|i| vec![i as u8; 20_000]).collect();
    let bytes = serialize(&nested);
    assert!(bytes.len() > 4_000_000);
    for r in both_entry_points::<Vec<Vec<u8>>>(&bytes) {
        assert_eq!(r.unwrap(), nested);
    }
}

/// Dash's names for the opcodes Core's enum defines, where they differ from Core's.
fn rust_opcode(core_name: &str) -> All {
    use opcodes::all::*;
    match core_name {
        "OP_0" => OP_PUSHBYTES_0,
        "OP_FALSE" => OP_PUSHBYTES_0,
        "OP_1NEGATE" => OP_PUSHNUM_NEG1,
        "OP_1" | "OP_TRUE" => OP_PUSHNUM_1,
        "OP_2" => OP_PUSHNUM_2,
        "OP_3" => OP_PUSHNUM_3,
        "OP_4" => OP_PUSHNUM_4,
        "OP_5" => OP_PUSHNUM_5,
        "OP_6" => OP_PUSHNUM_6,
        "OP_7" => OP_PUSHNUM_7,
        "OP_8" => OP_PUSHNUM_8,
        "OP_9" => OP_PUSHNUM_9,
        "OP_10" => OP_PUSHNUM_10,
        "OP_11" => OP_PUSHNUM_11,
        "OP_12" => OP_PUSHNUM_12,
        "OP_13" => OP_PUSHNUM_13,
        "OP_14" => OP_PUSHNUM_14,
        "OP_15" => OP_PUSHNUM_15,
        "OP_16" => OP_PUSHNUM_16,
        "OP_CHECKLOCKTIMEVERIFY" | "OP_NOP2" => OP_CLTV,
        "OP_CHECKSEQUENCEVERIFY" | "OP_NOP3" => OP_CSV,
        "OP_SPLIT" => opcodes::OP_SPLIT,
        "OP_NUM2BIN" => opcodes::OP_NUM2BIN,
        "OP_BIN2NUM" => opcodes::OP_BIN2NUM,
        "OP_CHECKDATASIG" => opcodes::OP_CHECKDATASIG,
        "OP_CHECKDATASIGVERIFY" => opcodes::OP_CHECKDATASIGVERIFY,
        other => {
            // The rest are named alike: look the name up through the byte Core gives it.
            let table = tables();
            let value = pairs(&table["opcodes"]["members"])
                .into_iter()
                .find(|(n, _)| n == other)
                .unwrap()
                .1;
            let candidate = All::from(value as u8);
            assert_eq!(candidate.to_string(), other, "{other} is named alike in the crate");
            candidate
        }
    }
}

#[test]
fn opcode_names_equal_core_for_every_byte() {
    let t = tables();
    let names: Vec<String> = t["opcodes"]["get_op_name"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_string())
        .collect();
    assert_eq!(names.len(), 256);
    for byte in 0..=255u8 {
        assert_eq!(get_op_name(byte), names[byte as usize], "byte {byte:#04x}");
        assert_eq!(All::from(byte).core_name(), names[byte as usize]);
    }
    // The DIP-0020 names, by their bytes.
    assert_eq!(get_op_name(0x7e), "OP_CAT");
    assert_eq!(get_op_name(0x7f), "OP_SPLIT");
    assert_eq!(get_op_name(0x80), "OP_NUM2BIN");
    assert_eq!(get_op_name(0x81), "OP_BIN2NUM");
    assert_eq!(get_op_name(0x83), "OP_INVERT");
    assert_eq!(get_op_name(0xba), "OP_CHECKDATASIG");
    assert_eq!(get_op_name(0xbb), "OP_CHECKDATASIGVERIFY");
    // Bytes Core's enum leaves out are unknown, the direct pushes among them.
    assert_eq!(get_op_name(0x01), "OP_UNKNOWN");
    assert_eq!(get_op_name(0x4b), "OP_UNKNOWN");
    assert_eq!(get_op_name(0xbc), "OP_UNKNOWN");
    assert_eq!(get_op_name(0xfe), "OP_UNKNOWN");
    assert_eq!(get_op_name(0xff), "OP_INVALIDOPCODE");
}

#[test]
fn opcode_values_equal_core() {
    // Every opcode of Core's `opcodetype` has the same byte under its Dash name.
    for (name, value) in pairs(&tables()["opcodes"]["members"]) {
        assert_eq!(rust_opcode(&name).to_u8() as u64, value, "{name}");
    }
}
