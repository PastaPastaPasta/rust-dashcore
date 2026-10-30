# AB-AR1-s independent review

**Verdict: CHANGES-REQUIRED**

Reviewed `node/ab-ar1-s` at `1dde8b14e1a6df17c585484a9478aed95775d59f`, against AB-AR1.md, AB-REVIEW.md, REVIEW-COMMON.md, PLAN §4 AR1 and A1 §2.18. Used a detached scratch worktree; made no changes to the implementer's worktree, commits, or GitHub. No other arm's implementation was inspected.

Reference: Dash Core `v24.0.0-rc.3`, peeled commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`. The normal Core checkout has a different HEAD, so source comparisons used `git show v24.0.0-rc.3:<file>`. Live comparisons used the cached release binaries, whose version reports `v24.0.0-rc.3`.

## Result and acceptance evidence

The ordinary argument and settings merge implementation is convincing. The mandatory unit-test ports, option recognition, workspace integration and reviewer checks pass. However, help generation is incomplete, valid Windows UNC paths are damaged, and documented edge-case deviations violate the faithful-port / byte-identical-error acceptance. A known-gaps document is useful disclosure, but does not waive those requirements.

Reviewed commits, oldest first:

- `d9609d79` — generate the Core v24 option table from source.
- `39dd9230` — port ArgsManager and util/settings.
- `7d57bc1b` — port Core argument tests and functional behavior coverage.
- `33a8e395` — add the CI group.
- `d531098d` — wallet chain options and boolean help defaults.
- `e93e1ee4` — filesystem quoting, equivalence and absolute paths.
- `1dde8b14` — document known gaps.

Independent validation ran with `TMPDIR=/dev/shm/ab-review-ar1-s` (mode 700), `CARGO_BUILD_JOBS=6`, and this worktree's own `target/`:

- `cargo fmt --all --check`: PASS.
- `cargo clippy -p dash-args --all-features --all-targets -- -D warnings`: PASS.
- `cargo test -p dash-args --all-features`: **72 passed, 0 failed**, plus 0 doctests.
- `python3 -I dash-args/tools/gen_option_table.py --check`: PASS.
- `.github/scripts/ci_config.py verify-groups`: PASS; all 29 crates assigned.
- `contrib/node/check-r2.sh`: PASS. The shared-library classification in deny.toml is appropriate: this dependency-free library has no forbidden-stack edge and needs no exception wrapper.
- Pinned Core `test_dash --run_test=getarg_tests,settings_tests,argsman_tests`: **26 cases passed**, no errors.
- Independent CLI differential: **822 probes, 0 differences**, covering each of the 274 daemon registrations in bare, `=1`, and `-no` forms. A deliberately invalid final parameter made both parsers report their first parsing error without starting a node. This checks recognition and flag errors; includeconf's end-of-parse rule was checked separately through source and the functional test port.
- Help differential: all **262 visible daemon option names/help parameters** match Core `-help-debug`; **20 help bodies differ**, all containing unresolved placeholders. The complete generated table has **371 per-program entries**, including **33 dynamic-help entries**; this is not 371 distinct daemon options.

The three merge transcript claims were independently verified by dumping both Core and Rust transcripts. They are **byte-identical**, not merely equal to a copied hash assertion:

- Settings: 162,062 bytes; SHA256d `79db02d74e3e193196541b67c068b40ebd0c124a24b3ecbe9cbf7e85b1c4ba7a`.
- Args: 3,028,040 bytes; SHA256d `c78db4195fcbe4cf3f535f9a72b963bba77fd36099da29dfb8845291a2cf760a`.
- Chain: 79,655 bytes; SHA256d `1cd2697d1665d086f15ed18b3333bc356c3d39ba490589dc58afd8fc2f457731`.

The implementer's **full workspace gate, EXIT 0 / 3201 passed**, is a reported result, not an independently rerun full gate. The reviewer-required touched-crate gates above were independently rerun successfully. The tests visibly port the named getarg/settings cases and map the three functional suites to Rust tests. Node/network behavior cannot be proved by these tests, as the brief explicitly anticipates.

## Core spot checks

The following exceed the required ten named behaviors; compared to the pinned source and the passing ported tests, with live CLI/help/settings checks where described above:

1. `-foo`, `-foo=`, `-nofoo`, `-nofoo=1`, `-nofoo=0`, and nonnumeric negation: match `InterpretValue` and `getarg_tests`.
2. Double-negative warning text and true/false conversion: match, including `-noconnect=0`.
3. DISALLOW_ELISION / DISALLOW_NEGATION errors: match the Core test cases and registration flags; the live bare/negated option probes match.
4. Duplicate CLI values are last-wins; ordinary duplicate config values are first-wins: match. List behavior preserves Core's negation reset and revived lower-priority config values.
5. NETWORK_ONLY settings ignore positive default-section values off mainnet, while a terminal negation is still effective: matches `GetSetting` / `OnlyHasDefaultSectionSetting` and the merge transcripts.
6. `[main]`, `[test]`, `[regtest]`, `[devnet]`, dotted keys, and unknown-section file/line reporting: match `GetConfigOptions` / `GetUnrecognizedSections` and tests.
7. CLI section-prefixed options are rejected; config `conf=` is rejected even through an included file: exact errors match.
8. CLI includeconf rejection, including `-noincludeconf=0` reporting `true`, and `-noincludeconf` disabling includes: match.
9. Main-config multiple includes, network-section includes before default-section includes, and nonrecursive includes with warnings after a chain change: source logic matches Core `ReadConfigFiles`; the basic include cases also have passing functional ports.
10. Relative `-conf` uses the base datadir; config `datadir=` does not trigger reading a second config; the ignored-default-conf error / `allowignoredconf` warning match the ported cases and Core `InitConfig`.
11. Chain selection uses Core's special last-wins config behavior and ignores negated selectors in the compatibility path; conflicting selectors and devnet naming match the chain transcript and base parameters.
12. Missing/empty settings files, duplicate keys, top-level nonobjects, preserved number lexemes, unknown keys, `_warning_`, alternate paths and `-nosettings`: ordinary cases match; exceptions are findings below.
13. Leading `-`, missing `=`, and `#` in rpcpassword produce the exact ordinary parse errors; sensitive CLI/config logs are redacted as Core's are.
14. Help categories, hidden/debug filtering and layout match; unresolved text does not.

An additional 3,905 POSIX path-normalization probes matched effective ordinary paths. Three root-only spellings (`//`, `///`, `////`) lost Core's redundant root separators; this is a textual parity limitation, not a separate material-path finding. Windows analysis below exercised the Windows string-normalization branch in a Linux scratch harness; no native Windows build/run is claimed.

## Findings

### F1 — major — Required help still prints implementation placeholders

**Location:** `dash-args/src/args.rs:1327`; generated examples `dash-args/src/option_table.rs:84` (`-port`) and `:78` (`-onlynet`).

**Reproduction:** `ArgsManager::for_program(Program::Dashd).get_help_message()` prints `{{defaultChainParams->GetDefaultPort()}}` instead of 9999 and `{{Join(GetNetworkNames(), ", ")}}` instead of `ipv4, ipv6, onion, i2p, cjdns`. With help-debug enabled, 20 daemon help bodies differ from the pinned binary. RPC ports, logging categories, filter types and wallet defaults are similarly affected. The manager copies generated text directly and never resolves `HelpStatus::Dynamic`.

**Impact:** A required AR1/PLAN feature is unfinished; users cannot learn actual defaults or supported values for several important options. Documenting 33 placeholders does not make help generation faithful. Several allegedly dynamic expressions are constants or fixed lists at the pinned release.

**Suggested fix:** Resolve these expressions from the pinned sources or provide the corresponding runtime help context; never expose C++ expressions in public help. Add a comparison of completed help bodies against Core's generated help, rather than asserting only that categories/options are present.

### F2 — major — Windows UNC paths lose their network root

**Location:** `dash-args/src/fspath.rs:26` (`root_parts`), consumed by `lexically_normal` and `dash-args/src/args.rs:758`.

**Reproduction/reasoning:** The Windows branch recognizes a drive-letter root name only. Running that exact string branch with `\\server\share\data` yields `\server\share\data`: one leading separator is dropped. The input names a UNC share; the result names a rooted path on the current drive. `-datadir`, `-conf` and `-settings` all pass through this normalization. Extended Windows path prefixes are also treated as ordinary separators/components.

**Impact:** A valid configured storage path can be rejected or resolve to the wrong location. This violates DEC-84 portability, rather than merely lacking Windows test evidence. Core delegates path parsing to its platform filesystem path implementation.

**Suggested fix:** Preserve UNC/root-name and extended-prefix semantics using Windows-aware path components; test UNC and drive-relative paths on Windows, alongside the existing drive-root cases. The Linux string-branch reproduction proves the separator loss, but native validation remains needed.

### F3 — minor — UniValue-compatible settings decoding remains incomplete

**Location:** `dash-args/src/value.rs:527`, `:567`, `:456`; `dash-args/src/settings.rs:108`.

**Reproduction:** Put `{"unknown":"\ud800a\udc00"}` in regtest/settings.json. Pinned Core accepts and rewrites it as a string containing `a` followed by U+10000; Rust rejects the same original file as invalid JSON. Confirmed against both pinned UniValue sources compiled in scratch and live dashd configuration initialization. The UTF-8 precheck also rejects Core-accepted overlong/CESU-8 encodings: bytes `{"k":"\xc0\xaf"}` normalize to `/` in Core, while Rust rejects them. Separately, a settings file consisting solely of `-` produces Core's `Found non-object value - ...`, but Rust's `... does not contain valid JSON ...`.

**Impact:** Some files Core reads cannot round-trip through this port, and the malformed-input error classification is not byte-identical. These are disclosed gaps, but still contrary to the stated compatibility acceptance. Their unusual inputs make this minor rather than an ordinary merge/precedence defect.

**Suggested fix:** Match UniValue's decoder state machine, including byte decoding and split-surrogate behavior, and port these cases with Core-derived expectations. If deliberately narrowing compatibility, obtain an explicit acceptance change rather than claiming complete parity.

### F4 — minor — Empty directory at the settings path follows the wrong recovery path

**Location:** `dash-args/src/settings.rs:95` and `:102`.

**Reproduction:** Create an empty directory at `<datadir>/regtest/settings.json`, then initialize configuration. Core's ifstream reaches EOF, removes that empty directory, and recreates a settings file. Rust returns `<path>. Please check permissions.` and leaves the directory in place. Verified with live Core and `server_parse_args`, adding a loose final token to stop after config initialization; Core reached the unexpected-token error and had created a regular settings file, while Rust failed reading settings first.

**Impact:** Core's documented empty-path recovery behavior is not ported; the diagnostic is also wrong. This is explicitly disclosed, but the settings-read acceptance does not exclude directories.

**Suggested fix:** Match Core's stream/EOF and filesystem removal behavior, including the nonempty-directory error case, with regression coverage.

### F5 — minor — Filesystem status errors are falsely reported as nonexistent datadirs

**Location:** `dash-args/src/args.rs:905` (also the other `exists`/`is_dir` probes and I/O error conversions).

**Reproduction:** On this Linux host, pass `-datadir=<existing-scratch-dir>/<256 x characters>`. Core prints `Error: filesystem error: status: File name too long [<path>]`; Rust prints `Error: Specified data directory "<path>" does not exist.` The difference was confirmed live using identical paths. `Path::is_dir()` erases the status error, before the library could format it.

**Impact:** The explicit byte-identical error requirement fails even for a deterministic OS status error. This is more than Rust-versus-libstdc++ wording: the underlying failure is classified incorrectly. Both reject the input, so severity is minor.

**Suggested fix:** Use fallible metadata/status operations and distinguish absence from other errors. Carry operation/path context into a Core-compatible error formatter, with platform-specific expectations where Core's own wording varies.

### F6 — minor — Lossy config decoding changes values and error bytes

**Location:** `dash-args/src/args.rs:232`; related public argv API `:517`.

**Reproduction:** A dash.conf containing bytes `regtest=1\ninvalid-line-\xff\n` produces an error ending in the original byte FF in pinned Core. Rust replaces it with UTF-8 EF BF BD (U+FFFD). Verified against live dashd and `server_parse_args`. Recognized option values containing non-UTF-8 bytes are likewise altered before parsing; the `AsRef<str>` argv API cannot represent arbitrary Unix argv in the first place.

**Impact:** The port can silently change an existing config value and does not produce byte-identical parse errors. The UTF-8 limitation is disclosed, but it is not an exception in AR1's acceptance.

**Suggested fix:** Offer byte-preserving config/argv input and preserve OS strings for filesystem values; perform Unicode conversion only where the downstream interface requires it. Do not use replacement decoding in a compatibility parser.

## Known-gap judgment and code quality

Acceptable AR1 scope boundaries: full consensus chain parameters and activation-height validation belong elsewhere; AppInitParameterInteraction and network-layer functional checks need node assembly; buffering log/stderr output for caller integration is reasonable. The explicit mapping of functional behavior to tests is useful and follows the brief. A dependency-free shared crate and generated option names/categories/flags fit the architecture. The merge transcript ports provide strong regression evidence.

Unacceptable as final AR1 completion: unresolved help (F1), narrowed settings-reader compatibility (F3/F4), changed error behavior (F5), and silently lossy argv/config handling (F6). Windows being untested is honest disclosure, but the actual UNC defect (F2) needs correction. The generator's documented fixed release-build feature configuration is a reasonable reproducible option-surface choice; it does not justify unresolved user-visible help.

The test suite is strong for ordinary Core behavior but asserts the narrowed decoder's expectations and placeholder classification rather than proving parity at those boundaries. No separate stylistic nits are needed. Neither a blocker nor a consensus defect was found in this arm.

**Verdict: CHANGES-REQUIRED**

blockers=0 majors=2 minors=4 nits=0
