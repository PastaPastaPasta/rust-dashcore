//! Known-answer tests: NIST's CAVP SHA-256 vectors and Dash Core's `crypto_tests`, against every
//! implementation this CPU runs. Provenance: `tests/data/PROVENANCE.md`.

mod common;

use common::{Rng, data, hex, implementations, unhex};
use dash_sha256::{Implementation, Sha256, Sha256d};

/// `(message, digest)` pairs of a CAVP `SHA256ShortMsg.rsp` / `SHA256LongMsg.rsp` file.
fn cavp_messages(file: &str) -> Vec<(Vec<u8>, String)> {
    let mut out = Vec::new();
    let mut len = None;
    let mut msg = None;
    for line in data(file).lines().map(str::trim) {
        if let Some(v) = line.strip_prefix("Len = ") {
            len = Some(v.parse::<usize>().unwrap());
        } else if let Some(v) = line.strip_prefix("Msg = ") {
            let mut m = unhex(v);
            // A zero-length message is written as one zero byte.
            m.truncate(len.unwrap() / 8);
            msg = Some(m);
        } else if let Some(v) = line.strip_prefix("MD = ") {
            out.push((msg.take().unwrap(), v.to_owned()));
        }
    }
    out
}

#[test]
fn nist_short_and_long_messages() {
    for (file, count) in [("nist/SHA256ShortMsg.rsp", 65), ("nist/SHA256LongMsg.rsp", 64)] {
        let cases = cavp_messages(file);
        assert_eq!(cases.len(), count, "{file}");
        for imp in implementations() {
            for (msg, md) in &cases {
                let mut h = Sha256::with_implementation(imp);
                h.update(msg);
                assert_eq!(hex(&h.finalize()), *md, "{file} {} bytes, {}", msg.len(), imp.name());
            }
        }
    }
}

/// The CAVP Monte Carlo test: 100 checkpoints, each 1000 chained hashes of three digests.
#[test]
fn nist_monte_carlo() {
    let rsp = data("nist/SHA256Monte.rsp");
    let mut lines = rsp.lines().map(str::trim);
    let seed: [u8; 32] =
        unhex(lines.find_map(|l| l.strip_prefix("Seed = ")).unwrap()).try_into().unwrap();
    let expected: Vec<&str> = lines.filter_map(|l| l.strip_prefix("MD = ")).collect();
    assert_eq!(expected.len(), 100);
    for imp in implementations() {
        let mut seed = seed;
        for md in &expected {
            let mut window = [seed; 3];
            for _ in 3..1003 {
                let mut h = Sha256::with_implementation(imp);
                h.update(window.as_flattened());
                window = [window[1], window[2], h.finalize()];
            }
            seed = window[2];
            assert_eq!(hex(&seed), *md, "{}", imp.name());
        }
    }
}

/// Core's `LongTestString()` (`src/test/crypto_tests.cpp`).
fn long_test_string() -> Vec<u8> {
    (0..200_000u32).flat_map(|i| [i, i >> 4, i >> 8, i >> 12, i >> 16].map(|x| x as u8)).collect()
}

/// Core's `TestVector`: the whole input, then `passes` times in random pieces, also finishing a
/// copy of the hasher once the rest is short.
fn test_vector(imp: &'static Implementation, input: &[u8], digest: &str, passes: usize) {
    let mut h = Sha256::with_implementation(imp);
    h.update(input);
    assert_eq!(hex(&h.finalize()), digest, "whole, {}", imp.name());

    let mut rng = Rng(input.len() as u64);
    for _ in 0..passes {
        let mut hasher = Sha256::with_implementation(imp);
        let mut pos = 0;
        for len in rng.splits(input.len()) {
            hasher.update(&input[pos..pos + len]);
            pos += len;
            if pos > 0 && pos + 2 * 32 > input.len() && pos < input.len() {
                let mut copy = hasher.clone();
                copy.update(&input[pos..]);
                assert_eq!(hex(&copy.finalize()), digest, "copy, {}", imp.name());
            }
        }
        assert_eq!(hex(&hasher.finalize()), digest, "pieces, {}", imp.name());
    }
}

/// Core's `sha256_testvectors`, extracted from `crypto_tests.cpp` with their source lines.
#[test]
fn core_sha256_testvectors() {
    let json: serde_json::Value = serde_json::from_str(&data("core-crypto-tests.json")).unwrap();
    let vectors = json["vectors"].as_array().unwrap();
    assert_eq!(vectors.len(), 11);
    for v in vectors {
        let input = &v["input"];
        let bytes = if let Some(text) = input["text"].as_str() {
            text.as_bytes().to_vec()
        } else if let Some(c) = input["repeat"].as_str() {
            c.repeat(input["count"].as_u64().unwrap() as usize).into_bytes()
        } else {
            assert_eq!(input["generator"], "LongTestString");
            long_test_string()
        };
        // Core makes 32 passes in pieces; the two 1 MB inputs get 2 here to keep debug builds fast.
        let passes = if bytes.len() > 100_000 {
            2
        } else {
            32
        };
        for imp in implementations() {
            test_vector(imp, &bytes, v["digest"].as_str().unwrap(), passes);
        }
    }
}

/// Core's `sha256d64` test: `SHA256D64` of 0 to 32 random inputs equals `CHash256` of each.
/// The in-place form gives the same digests.
#[test]
fn core_sha256d64() {
    let mut rng = Rng(64);
    for imp in implementations() {
        for i in 0..=32 {
            let bytes = rng.bytes(64 * i);
            let input = bytes.as_chunks::<64>().0;
            let mut out = vec![[0; 32]; i];
            imp.sha256d64(&mut out, input);
            let mut level = bytes.as_chunks::<32>().0.to_vec();
            assert_eq!(imp.sha256d64_in_place(&mut level), i);
            assert_eq!(level[..i], out[..], "in place, {i} inputs, {}", imp.name());
            for (o, chunk) in out.iter().zip(input) {
                let mut h = Sha256d::with_implementation(imp);
                h.update(chunk);
                assert_eq!(*o, h.finalize(), "{i} inputs, {}", imp.name());
            }
        }
    }
}

/// The free functions use the automatic implementation.
#[test]
fn free_functions() {
    assert_eq!(
        hex(&dash_sha256::sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let mut h = Sha256::new();
    h.update(&dash_sha256::sha256(b"abc"));
    assert_eq!(dash_sha256::sha256d(b"abc"), h.finalize());

    let input = [[7u8; 64]; 9];
    let mut out = [[0; 32]; 9];
    dash_sha256::sha256d64(&mut out, &input);
    assert!(out.iter().all(|o| *o == dash_sha256::sha256d(&[7; 64])));
}

/// The in-place level equals the out-of-place one across the first, copied group of 8 pairs
/// and the doubling groups after it.
#[test]
fn sha256d64_in_place_matches() {
    let mut rng = Rng(32);
    for imp in implementations() {
        for pairs in [0, 1, 7, 8, 9, 15, 16, 17, 24, 31, 33, 100, 1000] {
            let bytes = rng.bytes(64 * pairs);
            let mut expected = vec![[0; 32]; pairs];
            imp.sha256d64(&mut expected, bytes.as_chunks::<64>().0);
            let mut level = bytes.as_chunks::<32>().0.to_vec();
            assert_eq!(imp.sha256d64_in_place(&mut level), pairs);
            assert_eq!(level[..pairs], expected[..], "{pairs} pairs, {}", imp.name());
            assert_eq!(level[pairs..], bytes.as_chunks::<32>().0[pairs..], "{pairs} pairs, tail");
        }
    }
}

#[test]
#[should_panic(expected = "odd number of hashes")]
fn sha256d64_in_place_odd() {
    dash_sha256::sha256d64_in_place(&mut [[0; 32]; 3]);
}

#[test]
#[should_panic(expected = "lengths differ")]
fn sha256d64_length_mismatch() {
    dash_sha256::sha256d64(&mut [[0; 32]; 2], &[[0; 64]; 3])
}

/// `Implementation::auto` is the detection with everything allowed; `reset` restarts a hasher.
#[test]
fn auto_and_reset() {
    let auto = Implementation::auto();
    assert_eq!(auto.name(), Implementation::detect(dash_sha256::Use::ALL).name());
    assert_eq!(Implementation::standard().name(), "standard");

    let mut h = Sha256::new();
    h.update(b"something else");
    h.reset();
    h.update(b"abc");
    assert_eq!(h.finalize(), dash_sha256::sha256(b"abc"));
}
