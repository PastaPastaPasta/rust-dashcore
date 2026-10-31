Verdict: **APPROVE-WITH-CHANGES**.

Focused closure check of rev3, `cbc67410`, retaining DEC-50/DEC-54 and the any-reader-accepted-list payee contract. The corrected range search and result-computed certification survive the checks below. Two minor specification corrections and one sharing-contract nit remain; no new major defect was found in those algorithms.

## Numbered findings

### 1. R4-m1 — minor: the unconditional post-build I-P assertion survived R3-F2

**Location:** `designs/EV2-dml.md:271`, `:956`; superseded claims also remain at `:26` and `:1086`.

The design still asserts I-P after every block build, while §3.4 and the new §6.2 fixture correctly say a successful build can violate it. Concrete case: predecessor height 100, valid A/B Evo with `lastPaid=101`, streaks 1/2, and regular C with `lastPaid=1`. Core's block-101 build succeeds, pays C and resets A/B's streaks, leaving **three** valid MNs at `lastPaid=101`. Rev3's lazy check correctly selects the fallback and produces `A A A A A A A A B`; an unconditional `check_invariants()` I-P assertion then fails on the very fixture EV2b-1 requires to pass.

**Core evidence:** `src/evo/specialtxman.cpp`, v23.1.8:486–515 / v24.0.0-rc.3:759–790; projection in `src/evo/deterministicmns.cpp`, v23:235–273 / v24:210–248. The real v24 builder and both pins' DML-method probes reproduce this result; reader acceptance was executed with v24's `snapshot.h:249–297`.

**Fix:** scope I-P assertions to lists with proven from-genesis temporal provenance. For arbitrary decoded lists and their build results, compare the computed P1 with a literal count instead of asserting P1 true. Mark the old “built lists satisfy both by construction” dispositions superseded.

### 2. R4-m2 — minor: the global inequality and future-registration fixture overstate the proof

**Location:** `designs/EV2-dml.md:668`, `:898`, `:1095`, `:1096`.

`height > 0` does **not** imply `pay_height >= lastPaid` for every MN. The canonical reader accepts a height-100 list with a valid MN having `lastPaid=0`, `registered=-2`, `revived=-1`; its pay height is −2. Both pins execute this result. This does not break the specified lazy count: that MN does not satisfy `lastPaid == list.height`.

**Core evidence:** `src/evo/deterministicmns.cpp`, v23.1.8:157–166 / v24.0.0-rc.3:132–141. The v24 reader, `snapshot.h:249–297`, imposes no registration-height bound; v23 has no canonical reader.

**Fix:** use the precise implication already stated at `:672`: **`list.height > 0 && lastPaid == list.height` implies `pay_height >= list.height`**. Here `lastPaid != 0`, so registration substitution cannot fire; revival can only raise the key. This proves completeness for every MN being counted, including future-paid entries when the list height reaches their payment height.

Also correct `:898`'s “future-registration … P2 false” label. With height 0, R `(lastPaid=1, registered=10)` and E `(lastPaid=0, registered=101, streak=1)`, P1 and P2 are both true; the fast range finds E and both pins project `E E E R E`. Future **positive** registration does not make P2 false. Specify a separate negative-registration MN if a P2-fallback fixture is intended.

### 3. R4-n1 — nit: scan reuse contradicts the stated twin-sharing boundary

**Location:** `designs/EV2-dml.md:115`, `:677`, `:1098`.

Rev3 explicitly makes the twin reuse `scan.rs`, but §3.4 still says it shares “only the payload decoder and the canonical encoder” with the fast path. Concrete case: the accepted two-Evo P1-false fixture sends production and the twin through the same `projected_scan`, so that fallback comparison is not an independent check of the scan implementation. Core's required result is `A A A A A A A B` (`src/evo/deterministicmns.cpp`, v23.1.8:235–273 / v24.0.0-rc.3:210–248).

**Fix:** explicitly include literal scan reuse in the sharing contract and retain direct Core differential coverage of fallback inputs. If an independent Rust scan is intended, give the twin its own literal implementation. Production scans must remain unconditional either way. This is a documentation contradiction, not a demonstrated output mismatch.

## Round-3 closure

- **R3-F1: closed.** The unbounded range with exact predicates reproduces `E E E R E` at both pins. Future revival/registration keys remain in range; §2.4/§3.4 no longer require equality with list height. Finding 2 corrects the broader explanatory wording.
- **R3-F2: partial.** Builder certification is removed and the resulting list's lazy count correctly catches the three matches, selecting `A A A A A A A A B`. The retained post-build invariant needs finding 1's correction.
- **R3-m1: closed.** Both pins' actual `PoSePunish` calls in member order `1 2 3 4` preserve bucket `4 3 2 1`; phase-start ordering would reverse it. `HandleQuorumCommitment` is vector-ordered at `specialtxman.cpp` v23:159–174 / v24:283–298; the revised phase boundaries agree with the probes.
- **R3-m2: closed at design level.** `scan.rs` is unconditional and EV2b-1 requires a default-feature build plus docs. There is no completed EV2 implementation whose feature build can be verified here. Finding 3 concerns sharing, not availability.
- **R3-m3: closed.** Actual `ApplyDiff` retains counter 7 after an empty register/spend diff; adding IDs 3 and 9 leaves 7 then advances to 10 at both pins. Citations: `deterministicmns.cpp` v23:396–417, 481–484 / v24:371–392, 512–515.

## Completeness check and evidence

No counterexample to the specified lazy P1/P2 computation or range search was found. For positive height, all **valid same-height** entries lie in the valid-only queue range; future `lastPaid` entries are filtered until their height matches. At height 0, the full pass counts P1 and checks all registration heights for P2; when P2 holds, a same-height key is nonnegative and remains in range. Certification counts regular MNs and Evo MNs with streaks ≥4 too; Evo and streak predicates apply only to continuation selection. Thus P1 supplies uniqueness without hiding additional matches. Keep Core's asymmetric deployment gates: V19 gates payee continuation, while projection continuation depends only on MN_RR.

Reused `/work/scratch/ev2-rev3/{core_probe,rebuild_probe}.cpp`; extended evidence is in `/work/scratch/ev2-gpt4/`: `rev3_model.hpp`, `core_probe.cpp`, `rebuild_probe.cpp`, `build.py`, build commands, `pin-verification.txt` and `core-*.log`. Relevant source files in the three existing Core builds were byte-compared with `git show` at v23.1.8 `728f5055836c6d29806412fc7223ac8fe05af991` and v24.0.0-rc.3 `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`; the reviewed design matches `cbc67410` exactly.

The randomized comparison uses 25,000 lists per pin and actual deployment context: V19 on/MN_RR off, V19 off/MN_RR off, and MN_RR on; six projection lengths each. Lists have 0–6 MNs, regular/Evo types, bans, colliding hashes, streaks 0–6, height 0, future payment/revival/registration heights, negative registration heights and i32 boundary heights. Each context also checks all four rule combinations against the literal source model and compares lazy counts/candidates with full scans. Canonical-reader acceptance executes on v24; v23 checks the same states through its helpers. The full builder executes on v24; v23's changing phases use actual DML methods plus its pinned builder source. These are design probes, not the future Rust implementation or replay gates.

All successful runs used `TMPDIR=/dev/shm/ev2-gpt4-tmp` mode 0700; it was removed afterward. No design, decision or branch was edited, no commit/push/GitHub post was made, and no existing dashd or chain datadir was accessed.

Verdict: **APPROVE-WITH-CHANGES**.
