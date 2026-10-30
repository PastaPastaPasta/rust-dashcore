use super::{parse_hex_lenient, read_filter};
use crate::blockchain::tests::{
    HASH_7, call_method, live_server, method_err, reply, unloaded_server,
};
use crate::blockchain::{BlockReadError, BloomFilterData};
use crate::protocol::{
    RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_MISC_ERROR,
};

/// A filter of three zero bytes, one hash function, tweak 0 and flags 0, as hex.
const FILTER: &str = "03000000010000000000000000";

/// `vData` declared as `size` bytes, followed by `data` and `trailer` zero bytes.
fn raw_filter(size: u64, data: usize, trailer: usize) -> Vec<u8> {
    let mut out = match size {
        0..0xFD => vec![size as u8],
        0xFD..=0xFFFF => [&[0xFD][..], &(size as u16).to_le_bytes()].concat(),
        _ => [&[0xFE][..], &(size as u32).to_le_bytes()].concat(),
    };
    out.resize(out.len() + data + trailer, 0);
    out
}

/// The error's code and object, as `ExecuteCommand` answers it.
fn read_err(bytes: &[u8]) -> (i32, String) {
    let e = read_filter(bytes).unwrap_err();
    (e.code(), e.to_object().write())
}

#[test]
fn parse_hex_is_cores_lenient_parse_hex() {
    assert_eq!(parse_hex_lenient("0a0B"), [0x0a, 0x0b]);
    assert_eq!(parse_hex_lenient(" 0a\t0b\n"), [0x0a, 0x0b]);
    // Each of `IsSpace`'s six characters (r1 m-1).
    for space in [" ", "\t", "\n", "\x0b", "\x0c", "\r"] {
        assert_eq!(parse_hex_lenient(&format!("0a{space}0b{space}")), [0x0a, 0x0b], "{space:?}");
    }
    // Whitespace only between pairs; an odd digit or another character is the empty vector.
    assert_eq!(parse_hex_lenient("0 a"), Vec::<u8>::new());
    assert_eq!(parse_hex_lenient("0a0"), Vec::<u8>::new());
    assert_eq!(parse_hex_lenient("zz"), Vec::<u8>::new());
    assert_eq!(parse_hex_lenient(""), Vec::<u8>::new());
}

/// rpc_getmerkleblocks.py's boundaries and dashd v24.0.0-rc.4's texts for the rest
/// (/work/scratch/rp1a-2c3/probe, D45).
// Core: rpc/blockchain.cpp:850-886
#[test]
fn the_filter_reads_as_cores_with_its_size_precheck() {
    let end = reply(RPC_MISC_ERROR, "DataStream::read(): end of data: iostream error");
    let size = reply(RPC_INVALID_PARAMETER, "Filter is not within size constraints");
    let over = 36_001;
    assert_eq!(read_err(&raw_filter(over, over as usize, 9)), size);
    assert_eq!(read_err(&raw_filter(over, 0, 0)), end);
    assert_eq!(read_err(&raw_filter(over, over as usize, 8)), end);
    // At the limit the precheck passes and the filter reads whole.
    assert!(read_filter(&raw_filter(36_000, 36_000, 9)).is_ok());
    assert_eq!(read_err(&raw_filter(3, 3, 8)), end);
    assert_eq!(read_err(&[]), end);
    assert_eq!(
        read_err(&[0xFD, 3, 0]),
        reply(RPC_MISC_ERROR, "non-canonical ReadCompactSize(): iostream error")
    );
    assert_eq!(
        read_err(&[0xFE, 0, 0, 0, 3]),
        reply(RPC_MISC_ERROR, "ReadCompactSize(): size too large: iostream error")
    );
    let funcs = |n: u32| [&[3, 0, 0, 0][..], &n.to_le_bytes(), &[0; 5]].concat();
    assert_eq!(read_err(&funcs(51)), size);
    assert_eq!(
        read_filter(&[&funcs(50)[..], &[0xab, 0xcd]].concat()).unwrap(),
        BloomFilterData {
            data: vec![0; 3],
            hash_funcs: 50,
            tweak: 0,
            flags: 0,
        }
    );
}

#[test]
fn getmerkleblocks_checks_in_cores_order() {
    let (server, live) = live_server(3);
    let unknown = format!(r#"["{FILTER}", "{HASH_7}", 0]"#);
    assert_eq!(
        method_err(&server, "getmerkleblocks", &unknown),
        reply(RPC_INVALID_ADDRESS_OR_KEY, "Block not found")
    );
    // The filter is read before the hash is looked up.
    assert_eq!(
        method_err(&server, "getmerkleblocks", &format!(r#"["", "{HASH_7}", 0]"#)),
        reply(RPC_MISC_ERROR, "DataStream::read(): end of data: iostream error")
    );
    // ... and before the hash is parsed.
    assert_eq!(
        method_err(&server, "getmerkleblocks", r#"["", "abc", 0]"#),
        reply(RPC_MISC_ERROR, "DataStream::read(): end of data: iostream error")
    );
    assert_eq!(live.lock().unwrap().calls, [format!("lookup {HASH_7}")]);

    live.lock().unwrap().indexed = true;
    for count in ["0", "-1", "2001"] {
        assert_eq!(
            method_err(
                &server,
                "getmerkleblocks",
                &format!(r#"["{FILTER}", "{HASH_7}", {count}]"#)
            ),
            reply(RPC_INVALID_PARAMETER, "Count is out of range")
        );
    }
    assert_eq!(
        method_err(&server, "getmerkleblocks", &format!(r#"["{FILTER}", "{HASH_7}", 1.5]"#)),
        reply(RPC_MISC_ERROR, "JSON integer out of range")
    );
    assert!(!live.lock().unwrap().calls.iter().any(|c| c.starts_with("merkle")));

    live.lock().unwrap().merkle = Some(Ok(vec![vec![0xab], vec![0x01, 0x02]]));
    let got = call_method(&server, "getmerkleblocks", &format!(r#"["{FILTER}", "{HASH_7}"]"#));
    assert_eq!(got.unwrap().write(), r#"["ab","0102"]"#);
    let walk = live.lock().unwrap().calls.last().unwrap().clone();
    assert!(walk.starts_with(&format!("merkle {HASH_7} 2000 BloomFilterData")), "{walk}");
    call_method(&server, "getmerkleblocks", &format!(r#"["{FILTER}", "{HASH_7}", null]"#)).unwrap();
    call_method(&server, "getmerkleblocks", &format!(r#"["{FILTER}", "{HASH_7}", 2000]"#)).unwrap();

    for (e, message) in [
        (BlockReadError::Pruned, "Block not available (pruned data)"),
        (BlockReadError::NotOnDisk, "Block not found on disk"),
    ] {
        live.lock().unwrap().merkle = Some(Err(e));
        assert_eq!(
            method_err(&server, "getmerkleblocks", &format!(r#"["{FILTER}", "{HASH_7}", 1]"#)),
            reply(RPC_MISC_ERROR, message)
        );
    }
    assert_eq!(
        method_err(&unloaded_server(), "getmerkleblocks", &format!(r#"["{FILTER}", "{HASH_7}"]"#)),
        reply(RPC_INTERNAL_ERROR, "Node chainman not found")
    );
}
