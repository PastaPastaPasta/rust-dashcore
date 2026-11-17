# dashd-random test data: provenance

| File | sha256 | Produced by |
|---|---|---|
| `rng_state_vectors.json` | `297af26cbcf89aed50575770c892fd3192efa3022fdf184e6b4366064eeb581c` | `contrib/node/bl502-rng-vectors/rng-vectors.sh ~/workspace/dash <file>`, on 2026-11-05 with g++ 15.2.0 (Ubuntu 15.2.0-16ubuntu1), x86_64 |

`rng_state_vectors.json` is a chain of 54 calls on one Dash Core `RNGState`: `AddEvent`, and `MixExtract` on a `CSHA512` that `SeedEvents` and an input filled first. Each `mix` step records `MixExtract`'s return value, its output, and `m_state` and `m_counter` after the call. The `RNGState` class is cut verbatim out of v24.0.0-rc.5's `src/random.cpp` (commit `8163290aa0307ed4bc2fc0ba15b78c20d61dfa88`; the only edit is `class` → `struct`, which makes the members readable). It is linked with that tag's `crypto/sha256.cpp` (built with `DISABLE_OPTIMIZED_SHA256`), `crypto/sha512.cpp` and `support/cleanse.cpp`. The performance counter `AddEvent` reads is the `perf` each `event` step names. The inputs come from a fixed xorshift64* in `rng_vectors.cpp`, and every input is written into the file. Nothing in it is hand-written.

`src/tests.rs`'s `mix_extract_and_events_match_cores_vectors` replays the chain on a zeroed state.
