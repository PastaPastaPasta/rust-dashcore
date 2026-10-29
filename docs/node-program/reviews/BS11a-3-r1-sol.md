# BS11a-3, round 1 — Sol: Core parity

## Verdict: ACCEPT WITH NITS

I found no confirmed behavioral divergence from Dash Core v24.0.0-rc.3 in the reviewed load/decode, G1/G2 arithmetic, comparison, normalization, serialization, wrapper `core_eq`, or IES key paths. This is a code review supported by targeted differential tests, not a proof over every possible input. The ongoing fuzz certification is outside this round's verdict, as the brief directs.

### Finding

**Nit — equality documentation overstates Core container identity.** `pkgs/pkc/src/bls/wrap_value.rs:166-170` and `pkgs/pkc/src/bls/elem.rs:459-462`, as well as `docs/node-program/designs/BS11a-3-notes.md` D25, say Rust `==` compares what Core keys its containers by. Core rc.3 `src/bls/bls.h:85-87,141-146` orders wrappers solely by `GetHash()` of their serialization; Rust `Element::eq` additionally compares the point (`pkgs/pkc/src/bls/wrap_value.rs:215-223`). A G2 pair can share compressed bytes while carrying distinct points when the sign component used by relic is zero, so Core's ordered container can identify the pair as one key while Rust equality distinguishes it. This does **not** affect `core_eq`, whose separate path models Core's `operator==`. **Fix:** say Rust equality compares validity, serialized bytes, and the point (while its hash uses validity and bytes); state Core's hash-based ordering separately. Amend D25 to match.

### Parity checks

- Checked the wrapper and `BlsElem` dispatch against Core's `CBLSWrapper`, dashbls `elements.cpp`/`schemes.cpp`, and relic `ep_*`/`ep2_*` paths at the rc.3 pin. The Core checkout's inspected wrapper and dashbls source files have no diff from `v24.0.0-rc.3`.
- Reviewed the split between decoded Group values and Relic values, including off-curve and out-of-subgroup reconstruction, `BASIC`/`PROJC` infinity shapes, G2's `PROJC` infinity comparison, G1's crafted zero-z comparison, the sign boundary, scalar reduction above `r`, and IES basic serialization. The D24–D32 notes describe the code's choices; the D25 wording above is the exception.
- A signed, off-curve legacy G2 encoding was considered: relic may temporarily leave the sign marker in `ep2_read_bin`, but its canonical serialization check rejects the input. `g2_decode_legacy` rejects it as well (`pkgs/pkc/src/bls/wrap_codec.rs:303-319`). I found no parity issue there.
- `cargo test -p dash-pkc --all-features` passed: 500 unit tests, 12 Core corpus tests, and 48 wrapper tests; one wrapper fuzz-sample test is intentionally ignored.
- `cargo test -p node-l1` passed. `cargo test -p node-l1 --features oracle --test l1_bls_arith` passed all three tests, including public seeds and 400 deterministic random scripts against the v24 oracle. Prior review work also passed `cargo test -p node-l1 --all-features --lib` (45 tests).

### Bench row

No separate engine `mul` benchmark row is warranted now. The Relic engine handles exotic legacy operands; the common Group path remains on blst. D23 calls for the Group path shape-cost measurement against bare blst; that is the relevant benchmark gate.
