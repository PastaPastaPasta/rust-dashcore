# BS11a-4 4a Opus-h review, round 1 (relic Miller loop port, pairing probe)

**Verdict: ACCEPT WITH NITS.** No Blocker or Major. The port matches relic's C source line for line on every path that runs at the pin. I built relic myself from the pinned `src/dashbls`, and the committed corpus reproduces from it byte for byte. Independent replays (a fresh seed, and an edge-value probe variant) match relic exactly. All 27 of my non-equivalent hand mutants are killed by the relic-reference test. There are four Minors (commit wording, corpus-test pinning, probe classes, and the CodeQL wrapper's cross-thread race) and some nits.

Heads reviewed:
- fork `node/bsdk-bs11a-4` `4a6a9e45..f4b17c3a`: af5738e1 (probe and corpus), f4b17c3a (port);
- rdc `node/bs11a-4` b301cdb5a, 3c1ee4557 and 8b9094a71 (`docs/node-program/designs/BS11a-4-notes.md`);
- `/work/scratch/bs11a-4/tools/bin/codeql` (read only).

relic is cited at Core v24.0.0-rc.3 (`da567d7db` → `0364ed626e`), `src/dashbls/depends/relic/src/`. All work was done in `/work/scratch/bs11a4-4a-opus-r1` (removed at the end).

## Findings

### m1 (Minor): both commit messages say "Core", not "reference implementation"

- **Where.**
  - af5738e1's subject is "record Core's relic pairing on every point class", and its body says "Core's librelic.a".
  - f4b17c3a's body starts "Core verifies legacy signatures …".
  - The files say "Dash Core" too: `contrib/vectors/bls_pairing/probe.c:6` and `README.md:3`.
- **Why it matters.** It breaks the base SDK wording policy the brief names. Earlier fork commits also say "Core's" (03a7acf8, 705277be, dd45c534), and PROVENANCE's `bls_dashbls` entry names "Dash Core v24.0.0-rc.3". So this is consistent with the fork, but not with the policy, and it has to be fixed before anything goes upstream.
- **Fix.** Reword, for example:
  - `pkc%test: record the reference relic pairing on every point class`, with "the reference implementation's librelic.a" in the body;
  - "The reference implementation verifies legacy signatures …" in f4b17c3a.

  Do the same in probe.c and README if the policy covers files as well as messages. PROVENANCE needs a version anchor, so it can keep the tag.

### m2 (Minor): the replay test doesn't pin two paths the committed corpus happens to cover

- **Where.** `pkgs/pkc/src/bls/relic/pp.rs:338-344`.
- **What.** The test asserts the point classes and `zeros > 0`, but not these two paths:
  - (a) A call where **every** pair has an infinity side. relic then returns 1 without `pp_exp_k12` (`relic_pp_map_k12.c:553-555`, `fp12_set_dig(r, 1)` and the `j > 0` guard). The committed corpus has 2 such vectors.
  - (b) Multi-pair calls. The corpus has m = 1..4 (9/17/11/11).
- **Failure scenario.** If the corpus is regenerated with another seed or count, it can lose (a). The only thing pinning (a) would then be `infinity_pairs_are_dropped_and_none_left_gives_one`, an engine-only test (see n5). My "empty gives zero" mutant is killed today by both that test and the corpus test; with such a corpus it would be killed only by the engine-only one.
- **Fix.** Count the vectors whose pairs all have an infinity side and assert at least one. Assert the largest m is at least 2.

### m3 (Minor): the probe has no off-curve projective points and no edge-value coordinates

- **Where.** `contrib/vectors/bls_pairing/probe.c:142-253`.
- **What.**
  - "projective" is always a subgroup point that has been rescaled. Points off the curve or outside the subgroup are only ever affine.
  - Coordinates are `fp_rand`, apart from (0, ±2) and y = 0.
  - 4b will pass the engine's arithmetic outputs to `map_sim`. Those are PROJC points off the curve or outside the subgroup, with arbitrary z.
  - relic's lazy-reduction paths (`fp2_subc_low`, `fp2_rdcn_low`, `fp2_nord_low` in the lines and in `fp12_mul_dxs_lazyr`) are where an inexact relic result would hide, and uniform random field elements almost never reach their boundary values.
- **Evidence (passes).** I added an "edge" class to a copy of the probe:
  - each coordinate is drawn from {0, 1, 2, p−1, p−2, (p−1)/2, (p+1)/2, −4, random};
  - z is 1 (BASIC), or an edge value (PROJC, never 0), on both G1 and G2.

  `probe-edge pairs 6000 opus-r1-edge` has 3,752 edge points, and it replays exactly through `the_engine_reproduces_relics_pairings` (`BLS_PAIRING_DIR`, release, 16 s). So the claim holds. The committed probe just doesn't carry the evidence.
- **Fix.**
  - Add an off-curve PROJC class (G1 homogeneous, G2 Jacobian, as the existing "projective" cases are) and an edge-coordinate class.
  - Regenerate the corpus and update its sha in PROVENANCE, and have the test assert the new classes.
  - The edge variant I ran is ~40 lines on top of `probe.c` (an `edge_fp` helper plus one `case 7` in each class switch).

### m4 (Minor): two wrappers in different threads can both start in the same poll window

- **Where.** `/work/scratch/bs11a-4/tools/bin/codeql:8-16`.
- **What.**
  - The check (pgrep plus `free -g`) and the launch aren't serialized across wrappers, and a waiting wrapper isn't counted (its own cmdline is `tools/bin/codeql`, which the pattern doesn't match).
  - Suppose two gated combs from different threads wait on one live run. When it exits, both see `others = 0` in the same 30/60 s window and both start.
  - Likewise, with one run live and ≥ 150G available, two waiters can both see `others = 1` and start, which makes three.
- **Within its own scope it is fine.**
  - The rule is counted correctly: 0 → go; 1 with `avail ≥ 150` → go; 2 or more → wait. `free -g` `$7` is "available".
  - It can't deadlock with itself. Only `create` and `analyze` are gated, and they run one after the other. The nested verbs aren't gated. Waiting wrappers don't count each other.
  - The pattern does see live runs: the same pattern in the netinfo-b3 copy logged "other CodeQL runs 1" at 10:19Z (journal).
- **Fix.** Wrap the check-and-launch in `flock` on one shared lock file, for example `/work/scratch/.codeql-gate.lock`. Have every gated copy hold it until its own `codeql-*/db` shows up in pgrep, or for 60 s at most, then release it. Without the lock, the rule holds only while combs start more than one poll apart.

### Nits

- **n1.** `pkgs/pkc/corpus/PROVENANCE.md:37`: "10⁶" is the only non-Latin-1 character in any `.md` in the tree; it was 0 before this slice. The guide asks for Latin-1. Use "10^6" or "1,000,000".
- **n2. af5738e1 refers ahead to a test that doesn't exist yet.**
  - Its PROVENANCE (`:39`, "Test: `src/bls/relic/pp.rs`, `the_engine_reproduces_relics_pairings`") and its README replay command name a test that only exists in f4b17c3a.
  - Each commit builds and passes on its own: af5738e1 alone gives `cargo test -p dash-pkc --features full` 559/0. Only the reference dangles.
  - Fix: put the port first (with its model tests), then the probe, corpus and replay test as one commit. Neither commit then edits the other's files.
- **n3.** `pp.rs:143-145`, "The value equals relic's only up to factors the final exponentiation removes": the port forms exactly relic's pre-exponentiation `r`. It uses the same formulas and exact field arithmetic, and `fp12_mul_dxs_lazyr` is the full product with the sparse line. What is true is that the evidence (probe output after `pp_exp_k12`) shows equality only after the final exponentiation. Suggested wording: "It is checked against relic only after the final exponentiation, so use it only through [`Fp12::final_exp`] …".
- **n4. Three citation spans are off.**
  - In pp.rs:19, `fp12_mul_dxs_lazyr` is cited as `relic_fp12_mul.c:320-410`; the function ends at `:420`.
  - In the rdc notes:
    - `BS11a-4-notes.md:35` cites `pp_mil_k12` as `:51-125`; it is `:51-133`, as pp.rs:93 has it.
    - `:33` cites `pp_map_sim_oatep_k12` as `:523-575`; it is `:523-599`, as pp.rs:138 has it.
- **n5. Two tests check the engine against itself.** `projective_inputs_are_normalized` and `infinity_pairs_are_dropped_and_none_left_gives_one` compare the engine with itself, on behaviour relic defines.
  - They aren't harmful: every mutant they kill is also killed by the corpus test.
  - But they read as parity tests. A one-line doc saying they are model checks would help, with relic's behaviour pinned by the corpus test (m2).
- **n6. Seed truncation.** `probe.c:40-46`: `seed()` silently truncates seeds longer than 32 bytes, so two seeds that share a 32-byte prefix collide. Die when `n > 32`, or hash the seed.
- **n7. The corpus file doesn't say how it was generated.** `probe.c:288`: the JSON5 header doesn't record the command, so the file can't say how to regenerate it on its own. Print `argv[1..3]` in the header comment.
- **n8.** The wrapper only counts temp-mode `codeql-*/db` dirs.
  - `lint_codeql.py --cache` runs use `maint/codeql/<lang>/.cache/db` (`lint_codeql.py:249,353`), so they aren't counted.
  - Neither are CodeQL installs that aren't named `codeql-<ver>/codeql/codeql`.
  - The wrapper `exec`s a binary in another task's scratch dir (`/work/scratch/bs11b/…`). If that dir is cleaned, the comb fails at the CodeQL step. It fails loudly, so nothing passes silently.

## Checks that found nothing

### Parity with relic (focus 1): traced against the C source, not the notes

**Configuration.** dashbls `configure.ac:647,715` sets `FP_PRIME 381` and `PP_METHD "LAZYR;OATEP"`. In `relic_pp.h:120-138,258-277`, `pp_dbl_k12` resolves to `pp_dbl_k12_projc_lazyr`, `pp_add_k12` to `pp_add_k12_projc_lazyr`, and `fp12_mul_dxs` to `fp12_mul_dxs_lazyr`. The probe's `ep_param_set_any_pairf` → `B12_P381` (`relic_ep_param.c:1301-1302`) is dashbls's own init (`bls.cpp:36-43`).

**Doubling** (`relic_pp_dbl_k12.c:237-399`).
- G1's b = 4 gives `detect_opt` → `RLC_TINY` (`relic_ep_curve.c:44-66`), so the general-b branch runs.
- A, B, C, D = 3b′C, E, F, G = 3D, x3 = E(B−G), y3 = (B+G)² − 12D² (relic's dbl/dbl/dbl/add chain is 12D²) and z3 = 4B·F all match `pp.rs:41-53`.
- M-type flips `one`/`zero`, so `l[one][one]` = `[0][0]` = D−B, `l[one][zero]` = `[0][1]` = 3x_P·A, and `l[zero][zero]` = `[1][1]` = F·(−y_P). These match pp.rs:55-57.
- `r` aliases `q`, but every read of `q` comes before the first write to `r`.
- `TWIST_B` = 4(1+u) is checked against `Fp::from_u64(4)` by `field.rs` tests.

**Addition** (`relic_pp_add_k12.c:173-255`). Going through t0 … r->z step by step, including the aliasing (`r->x` is reused as X·t0² before `t3`), gives `pp.rs:70-80` exactly. The lines are `[0][1]` = t1·(−x_P), `[0][0]` = x_Q·t1 − y_Q·t0 and `[1][1]` = t0·y_P. The addition evaluates at the original P, not `_p`, and reads only x and y of the affine Q.

**`fp12_mul_dxs_lazyr`** (`relic_fp12_mul.c:320-420`), M-type, PROJC.
- It reads `b[0][0]`, `b[0][1]` (through `fp6_mul_dxs`) and `b[1][1]`. `t0[2]` is never set and never read.
- The Karatsuba recombination (`v·(a1·b11·v)` folded with `nord`) is the full product with the other slots zero.
- `c = a` aliasing is safe: `a[1]` is last read before `c[1]` is written.
- So `r * l` with the port's `l` (zero apart from the three slots, which both line functions write) is relic's product exactly.

**Loop** (`relic_pp_map_k12.c:51-133`).
- `_p = (3x, −y)` (PROJC branch) and `_q = −q`.
- The first doubling writes into `r` = 1. It overwrites the `[0][0]` slot that holds the 1, so `r` becomes the line. My mutant that multiplies it in through `l` instead is equivalent, as expected.
- Then `fp12_sqr` and per-pair dbl, add on +1 and add of −Q on −1, for `i = len−3 … 0`. `s[..len−2].rev()` in pp.rs is the same index set.
- **The declined first-stage addition.** I ran relic's `bn_rec_naf` (w = 2) on |x| in Python, independently of `recode.rs`: 65 digits, top three (MSB first) 1, 0, −1. So `s[len−2] = 0`, and neither first-stage branch runs. `x_naf_has_a_zero_second_digit` pins exactly that, and the mutant that ports the branch literally survives, as an equivalent should.
- `bn_rec_naf` takes `bn_abs` (`relic_bn_rec.c:162`), so `Bn::pos(&[X_ABS])` is the right input.

**Map** (`:523-599`).
- Infinity is tested on the raw input (z = 0) before `ep_norm`/`ep2_norm`. A BASIC point is copied, G1 PROJC is homogeneous, and G2 PROJC is Jacobian (`relic_ep_norm.c`, `relic_ep2_norm.c`); these match `Ep`/`Ep2::norm`.
- `j = 0` → `r = 1` with no `pp_exp_k12`.
- The B12 branch is `pp_mil_k12`, then `fp12_inv_cyc` (`relic_fpx_inv.c:607-610`, `c0, −c1`, which is `blst_fp12_conjugate`), then `pp_exp_k12`.
- Degenerate inputs (y = 0, order 3, a mid-loop t with z = 0) take no branches in relic's line functions, and none in the port.

**blst primitives.** `blst_fp12_sqr` is the general `sqr_fp12`, not the cyclotomic one. My mutant that swaps in `blst_fp12_cyclotomic_sqr` is killed.

### Probe and corpus (focus 2)

- **Rebuilt from scratch.** I built `librelic.a` myself from `git archive da567d7db src/dashbls` (`autogen` + `configure` + `make librelic.la`; sha `01d4b473…`, which differs from PROVENANCE's `be445a04…` as expected for a separate build). I then built the probe with `build.sh` against blst 0.3.17's C sources. `probe pairs 48 bs11a4-corpus` is **identical** to the committed `bls_pairing.json5`, whose sha256 `b0752797…2d72` matches PROVENANCE. I couldn't check `be445a04…` itself, because that archive lives only in other tasks' scratch dirs. Since an independent build of the pinned source reproduces the file, that sha is informational.
- **It calls relic.** `nm` shows relic's `pp_map_sim_oatep_k12`, `pp_exp_k12`, `pp_dbl_k12_projc_lazyr` and `fp12_mul_dxs_lazyr` linked from the archive. blst is used only for `blst_final_exp`, `blst_fp_from_bendian` and comparisons.
- **The seeding fix is sound.** With `seeded = 0`, `rand_seed` overwrites V and C from the seed alone (`relic_rand_hashd.c:176-215`) and resets `counter = 1`. `rand_bytes` never reseeds by itself. `core_init` and `ep_param_set_any_pairf` run before `seed()`, so whatever randomness they consume doesn't matter. That the corpus reproduced on a separate build confirms it.
- **The final exponentiations agree.** `probe fe 40000 opus-r1-fe`: equal 40000/40000, conjugate 0, other 0, exit 0.
- **A replay of my own.** `probe pairs 6000 opus-r1-replay` (fresh seed), replayed through `the_engine_reproduces_relics_pairings`: pass. A one-hex-digit corruption of one `out` value in a copy fails at `pp.rs:336`, so the replay really reads `BLS_PAIRING_DIR`.

### Tests (focus 3)

- **Tip.**
  - `cargo test -p dash-pkc --features full --lib` 565/0, and `--features bls --lib` 458/0.
  - clippy `--features full -D warnings`, `--no-default-features` and `--features bls`: clean.
- **Hand mutants** (30, beyond the implementer's cargo-mutants set): each run executed the `bls::relic::` tests.
  - **27 killed. All 27 are killed by `the_engine_reproduces_relics_pairings`.** They include:
    - D-type slot placement (in dbl alone, and in dbl and add);
    - t0/t1 swapped in `l[0][0]`, the addition evaluated at `_p`, the doubling at P;
    - an affine-t addition, a missing conjugate, `conjugate` as the identity;
    - squaring per pair, NAF ranges that are off by one in either direction, a −Q step that adds +Q;
    - wrong y3, z3 and x3 in dbl and in add, an untwisted b, `TWIST_B` changed;
    - no G1 or G2 normalization, the infinity filter changed four ways;
    - a cyclotomic sqr, `set_coeff` transposed.
  - Bilinearity can't see either conjugate mutant, because an inverted pairing is still bilinear. "No conjugate" is killed by the corpus test and the blst model test. "Conjugate is identity" is killed by the corpus test alone.
  - **3 survived, all equivalent:**
    - a literal port of the first-stage addition, which never runs;
    - the first line multiplied in through `l` rather than written into `r` (1 · l = l);
    - a fresh zeroed `l` before each line (both line functions write the same three slots).
- **No tautologies where relic is the reference.**
  - `engine_and_blst_…` compares the engine with blst for Lemma U, where blst is the right reference.
  - Bilinearity is a model property.
  - The two engine-only tests are covered under n5.
  - `want` and the lines both go through `set_coeff`, but the tower and index mapping are pinned independently: by the probe's `to_blst` plus `fe` equality, and by the transposed-`set_coeff` mutant being killed.

### Commit hygiene (focus 4)

- Each commit builds and passes on its own: af5738e1 gives 559/0 with `--features full`.
- `lint_commit.py -r 4a6a9e45..f4b17c3a`: both commits PASS (format, 73 columns).
- f4b17c3a touches no file af5738e1 adds, so the two commits don't churn each other.
- Every added line is Latin-1 except n1. The program references (Lemma E/U, D-numbers) follow existing fork precedent (`elem.rs`, `ep.rs`, `recode.rs`). I found no history or conversational comments.
- The corpus's citation is file-level PROVENANCE: the command, the seed, the archive, and the blst version. That fits a generated file.

### rdc docs

- D3–D6's "landed" text matches the code and the evidence above, apart from n4's spans.
- DEC-465's rulings (D2, D9, D13, D14) are recorded as ruled.
- The 250-pair split is moved to 4b, which is out of scope here.

### CodeQL wrapper (focus 5)

The counting rule, the no-self-deadlock property and the live matching are all verified; the details are under m4. Its only defect is the cross-thread race (m4).

## Not done

No comb, CodeQL, gate or fuzzer runs. I didn't read other `/work/scratch` dirs, so the `be445a04…` sha and the implementer's 10⁵ replay weren't checked directly. My own 6,000 fresh-seed and 6,000 edge-value replays stand in for them.
