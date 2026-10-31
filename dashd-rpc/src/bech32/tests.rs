use super::*;

// Core: src/test/bech32_tests.cpp:14-33
#[test]
fn core_bech32_vectors_decode_and_recode() {
    for (encoding, cases) in [
        (
            Encoding::Bech32,
            [
                "A12UEL5L",
                "a12uel5l",
                "an83characterlonghumanreadablepartthatcontainsthenumber1andtheexcludedcharactersbio1tt5tgs",
                "abcdef1qpzry9x8gf2tvdw0s3jn54khce6mua7lmqqqxw",
                "11qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqc8247j",
                "split1checkupstagehandshakeupstreamerranterredcaperred2y9e3w",
                "?1ezyfcl",
            ],
        ),
        // Core: src/test/bech32_tests.cpp:35-54
        (
            Encoding::Bech32m,
            [
                "A1LQFN3A",
                "a1lqfn3a",
                "an83characterlonghumanreadablepartthatcontainsthetheexcludedcharactersbioandnumber11sg7hg6",
                "abcdef1l7aum6echk45nj3s0wdvt2fg8x9yrzpqzd3ryx",
                "11llllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllllludsr8",
                "split1checkupstagehandshakeupstreamerranterredcaperredlc445v",
                "?1v759aa",
            ],
        ),
    ] {
        for s in cases {
            let dec = decode(s.as_bytes()).unwrap_or_else(|| panic!("{s}"));
            assert_eq!(dec.encoding, encoding, "{s}");
            assert!(encode(encoding, &dec.hrp, &dec.data).eq_ignore_ascii_case(s), "{s}");
            assert_eq!(locate_errors(s.as_bytes()), (String::new(), vec![]), "{s}");
        }
    }
}

// Core: src/test/bech32_tests.cpp:56-107
#[test]
fn core_bech32_invalid_vectors_locate_their_errors() {
    let cases: &[(&[u8], &str, &[i32])] = &[
        (b" 1nwldj5", "Invalid character or mixed case", &[0]),
        (b"\x7f1axkwrx", "Invalid character or mixed case", &[0]),
        (b"\x801eym55h", "Invalid character or mixed case", &[0]),
        (
            b"an84characterslonghumanreadablepartthatcontainsthenumber1andtheexcludedcharactersbio1569pvx",
            "Bech32 string too long",
            &[90],
        ),
        (b"pzry9x0s0muk", "Missing separator", &[]),
        (b"1pzry9x0s0muk", "Invalid separator position", &[0]),
        (b"x1b4n0q5v", "Invalid Base 32 character", &[2]),
        (b"li1dgmt3", "Invalid separator position", &[2]),
        (b"de1lg7wt\xff", "Invalid character or mixed case", &[8]),
        (b"A1G7SGD8", "Invalid checksum", &[]),
        (b"10a06t8", "Invalid separator position", &[0]),
        (b"1qzzfhee", "Invalid separator position", &[0]),
        (b"a12UEL5L", "Invalid character or mixed case", &[3, 4, 5, 7]),
        (b"A12uEL5L", "Invalid character or mixed case", &[3]),
        (b"abcdef1qpzrz9x8gf2tvdw0s3jn54khce6mua7lmqqqxw", "Invalid Bech32 checksum", &[11]),
        (b"test1zg69w7y6hn0aqy352euf40x77qddq3dc", "Invalid Bech32 checksum", &[9, 16]),
        // Core: src/test/bech32_tests.cpp:109-156
        (b" 1xj0phk", "Invalid character or mixed case", &[0]),
        (b"\x7f1g6xzxy", "Invalid character or mixed case", &[0]),
        (b"\x801vctc34", "Invalid character or mixed case", &[0]),
        (
            b"an84characterslonghumanreadablepartthatcontainsthetheexcludedcharactersbioandnumber11d6pts4",
            "Bech32 string too long",
            &[90],
        ),
        (b"qyrz8wqd2c9m", "Missing separator", &[]),
        (b"1qyrz8wqd2c9m", "Invalid separator position", &[0]),
        (b"y1b0jsk6g", "Invalid Base 32 character", &[2]),
        (b"lt1igcx5c0", "Invalid Base 32 character", &[3]),
        (b"in1muywd", "Invalid separator position", &[2]),
        (b"mm1crxm3i", "Invalid Base 32 character", &[8]),
        (b"au1s5cgom", "Invalid Base 32 character", &[7]),
        (b"M1VUXWEZ", "Invalid checksum", &[]),
        (b"16plkw9", "Invalid separator position", &[0]),
        (b"1p2gdwpf", "Invalid separator position", &[0]),
        (b"abcdef1l7aum6echk45nj2s0wdvt2fg8x9yrzpqzd3ryx", "Invalid Bech32m checksum", &[21]),
        (b"test1zg69v7y60n00qy352euf40x77qcusag6", "Invalid Bech32m checksum", &[13, 32]),
        // Two typos in a Platform address: both encodings locate two errors, and Core keeps
        // the first tried, Bech32 (dashd v24.0.0-rc.4's validateaddress; DIFF `edge_va_tie`).
        (b"tdash1q3g3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zy6dv7vl", "Invalid Bech32 checksum", &[19, 34]),
    ];
    for &(s, error, locations) in cases {
        let shown = String::from_utf8_lossy(s);
        assert_eq!(decode(s), None, "{shown}");
        assert_eq!(locate_errors(s), (error.to_owned(), locations.to_vec()), "{shown}");
    }
}

/// `ConvertBits`' refusals: leftover bits of a whole input group, or non-zero padding.
#[test]
fn convert_bits_refuses_cores_bad_padding() {
    assert_eq!(convert_bits(&[0xff], 8, 5, true), Some(vec![31, 28]));
    assert_eq!(convert_bits(&[31, 28], 5, 8, false), Some(vec![0xff]));
    assert_eq!(convert_bits(&[31, 29], 5, 8, false), None);
    assert_eq!(convert_bits(&[31, 28, 0], 5, 8, false), None);
}
