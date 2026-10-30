# test-vectors

Dash Core's test vectors, vectors transcribed from Core's C++ unit tests, and loaders that follow
Core's own test-harness semantics. Test infrastructure for the Rust Dash node (node program task
O4, requirements NFR-CEQ-01/02). Crate `dash-core-vectors`; it depends on nothing in the workspace,
so any crate can use it as a dev-dependency without a cycle.

```
test-vectors/
  core/<commit>/           vectors pinned to one Dash Core commit
    PROVENANCE.md          source, how every file was produced, harness semantics and quirks
    src/test/data/...      verbatim copies of Core's JSON/hex test data (FILES.sha256)
    test/functional/data/  verbatim copies of the functional-test data
    test/util/data/        verbatim copies of the dash-tx golden files
    transcribed/           vectors Core's own unit tests compute (SHA256SUMS)
    instrumentation/       the Dash Core patch that records them
  golden/                  what Core computed from Core-produced mnlistdiff/qrinfo captures
  src/                     the loaders (ParseScript, flags, error names, per-file row rules)
  tests/                   checks of the data and of the cross-references between files
```

Rules:

- Everything is produced by Dash Core (its data files, its tests, a Core node's messages). Nothing is
  hand-written. Every file has an entry in a `PROVENANCE.md`.
- Loaders reproduce what Core's harness does and say where they deliberately do not (the flag
  perturbation defects documented in `core/<commit>/PROVENANCE.md`).
- Licence: MIT, with Dash Core's notice (`core/<commit>/COPYING`).

Update the pin:

```
contrib/node/sync-core-vectors.sh ~/workspace/dash <commit-ish>      # verbatim files
contrib/node/regen-core-vectors.sh transcribed <test_dash>           # needs the instrumented Core build
contrib/node/regen-core-vectors.sh golden <test_dash>
# then: update CORE_COMMIT/CORE_DIR in src/lib.rs, `core_commit` in contrib/node/regen-core-vectors.sh,
# the file counts the tests pin, PROVENANCE.md, and review the diff
```

Using the vectors from another crate:

```toml
[dev-dependencies]
dash-core-vectors = { path = "../test-vectors" }
```

```rust
let cases = dash_core_vectors::cases::load_script_tests(
    &dash_core_vectors::read_core_text("src/test/data/script_tests.json"),
)?;
```

`dash/tests/core_vectors.rs` is a worked example.
