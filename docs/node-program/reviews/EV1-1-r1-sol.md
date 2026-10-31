# EV1-1 independent consensus review

Reviewed `node/ev1-1-netinfo` at `36fc5ac59` against `5ae846351` and Dash Core `v24.0.0-rc.3` (`0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`). I excluded the final contrib-only selftest cherry-pick from the consensus assessment. The input domain for the verdict is NetInfo decoded from transaction or DML wire data, with chain parameters supplied by the caller as the design specifies.

## Findings

No confirmed Blocker, Major, Minor, or Nit findings. I found no input in the reviewed domain for which the port changes Core's accept/reject result or first reject reason.

## Checks and evidence

- `dash-dml/src/netinfo/mod.rs:194-237` follows Core `netinfo.cpp:588-638`: version and emptiness, global address-port duplicates, then each purpose's validity, count, address duplicates, and entries before moving to the next purpose. The base-sdk decoder at the pinned dependency sorts purpose groups and retains the first repeated group, matching Core's map. `entry.rs:143-176` compares complete address strings, including maximal domains; invalid decoded entries collapse as Core's `NetInfoEntry` does. The apparent possibility of a cross-type duplicate is ruled out by Core's `NetInfoEntry::operator<` at `netinfo.cpp:154-172`, which compares type before content.
- `add.rs:60-185` preserves Core's `AddEntry` and `ProcessCandidate` check order, including purpose and existing-entry guards, the two character filters, privacy-name handling, domain lowercasing, numeric lookup, CJDNS conversion, validation, and duplicate checks. `migration.rs:19-84` follows the NetInfo reject steps of `SetStateVersion` in `specialtxman.cpp:74-105`; `network.rs:33-138` preserves `providertx.cpp:166-263` checks and its status-to-reason mapping, including the null-pointer distinction for `platformNodeID`.
- `dash-netaddr/src/{netaddr,netbase,numeric}.rs` matches the relevant Core address classes, validity and routability predicates, formatting, bad-port list, `SplitHostPort`, and numeric lookup. The omitted IPv6 scope form cannot pass either NetInfo character filter; it was already refused by the prior dash-p2p parser. I compared the mainnet, testnet, devnet and regtest default and Platform ports and `RequireRoutableExternalIP` values with Core's pinned `chainparams.cpp`; the pinned base-sdk parameters agree. The D33 setting is carried into the same Core routability predicate.
- `python3 contrib/node/ev1-census.py --core /work/home/agent/workspace/dash --check` passed: 281 rows match. An independent scan found no `Invalid(` call outside the census ranges in its seven scoped Core files. The two EV1 nonliteral migration sites can forward `bad-protx-netinfo-version`; the three block-path sites forward their transaction reason. The three unreachable classifications are consistent with the preceding Core checks.
- The new model fuzz targets check determinism and unchanged state on failed `AddEntry`; their seed replay and 2,000-mutation test are present at `node-fuzz/src/models/netinfo.rs:180-242`. `dash-p2p/src/netaddr.rs` re-exports the extracted public types under their former paths. The EV2a node L1 and fuzz model calls use `CoreNetInfoValidity`; SPV and wallet Cargo manifests have no dependency on `dash-dml` or `dash-p2p`.

## Validation limit

The owner's L0, fuzz-smoke, and trial-merge L1 differential results were supplied in the brief; I did not rerun that oracle comparison. My isolated `cargo test -p dash-netaddr --all-features` attempt was stopped while compiling dependencies under machine load, before any test ran. I removed its detached scratch worktree and target directory. The census check and the source comparisons above are my independent results.

VERDICT: ACCEPT
