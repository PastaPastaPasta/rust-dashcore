# SW-V24 rc.5 inventory and plan — Sol review

**Verdict: CHANGES-REQUIRED.** Findings: **4 major, 4 minor, 2 nits**.

Reviewed 2026-10-30 against the three scratch documents and `hunks-rc5.md`. Core endpoints: rc.4 `0e7382df80d0bc6811068d076b232a5a26d34a3c`, rc.5 `8163290aa0307ed4bc2fc0ba15b78c20d61dfa88`. Rust citations below use the plan's baseline `45fc7c59d`; the cited implementation files were also checked against the integration checkout. The effective base-sdk fork pin is `1eda0100802ce5f4b8d3ef1c011c74c39e76ea0a`, as recorded in that baseline's root Cargo.toml:55–62.

The hunk index is complete: an independent comparison of file names and both old/new hunk ranges matched **394/394**, including deleted files. I found no omitted rc.4→rc.5 Core runtime hunk. The blockers concern downstream consumers, the pin train, and one watch-file equivalence claim. Neutral rows remain neutral below.

## Major findings

### M-1 — MN5-07's actual metadata implementation and existing comparisons must accompany the pin

**Rows:** MN5-07; PLAN item 11, S11, §3.1 steps 2 and 4.

The mapping omits `dashd-masternode/src/meta.rs`. This is the concrete implementation used by `diff_mncache`, independently of the listed `MemPlatformBans` and P2P paths. It still implements rc.4. The plan includes only MN5-08 from S11 in the pin train and defers MN5-07 to a normal later merge.

This is already exercised by public tests, not just a possible future fuzz discovery. `mncache_seed_inputs()` supplies scripts reaching the changed MN5-07 implementation; `seeds_compare` passes every generated seed directly through the comparator. The shim records the relevant return value, and the comparator requires exact agreement on outputs and stored state. There is no predicted-difference handling in this path. Moving the pin while retaining the old metadata implementation therefore leaves an existing mandatory oracle suite red.

**Evidence:**

- Core `v24.0.0-rc.4:src/masternode/meta.h:73–84` and `src/masternode/meta.cpp:197–208`, versus `v24.0.0-rc.5:src/masternode/meta.h:73–83` and `src/masternode/meta.cpp:197–206`.
- Rust `45fc7c59d:dashd-masternode/src/meta.rs:124–133,471–484`; associated expectations at :567–607.
- Rust `45fc7c59d:node-l1/src/mnsync.rs:430–460,486–578,983–1013`; `node-l1/tests/l1_mnsync.rs:741–748,787–795,907–910`.
- Oracle `35e3bcf11ad:src/test/oracle/oracle_mnsync.cpp:314–324`.
- PROCESS.md:55,70 makes `oracle-diff`, including `node-l1 --features oracle`, a required local check.

**Required correction:** add the metadata crate and its tests/seeds to MN5-07's consumer map and S11. Include this implementation change in the pin train, or specify a concrete, separately reviewed transition that preserves the required comparisons. Merely listing MN5-07 among predicted rows does not change those tests.

### M-2 — S1's dump-schema change requires an omitted Rust consumer before the pin train can pass

**Rows:** CE5-05; INVENTORY §0 items 3/6; PLAN items 1/6/8, S1/S5/S7, §3.1 step 3.

S1 explicitly adds `minBlocksToKeep` to `ChainParamsJson`. The plan defers S5 and S7 until after the pin train, claiming that unchanged regtest parameters permit this. However, the missing consumer is `node-l1/src/chain/params.rs`, which compares the complete JSON shape, including every leaf. Its Rust renderer has `pruneAfterHeight` but no `minBlocksToKeep`; the Core side removes only `core_version` before comparison.

As planned, every regtest schedule acquires a missing-leaf mismatch even though its retention value remains 288. Existing `l1_chain_params` tests assert full leaf equality, and the same check runs before chain differential execution. This is a deterministic harness failure, beyond a reported parameter-value difference.

**Evidence:**

- Core `v24.0.0-rc.5:src/chainparams.h:116–118,170–175`; regtest value at `src/chainparams.cpp:880–881`.
- Current oracle dump implementation: `35e3bcf11ad:src/test/oracle/oracle.cpp:591` (`ChainParamsJson`); the additional key is an explicit S1 requirement, PLAN:21,46.
- Rust `45fc7c59d:node-l1/src/chain/params.rs:100–115,239–260,280–285`; `node-l1/tests/l1_chain_params.rs:21–36`.
- Rust `45fc7c59d:node-l1/src/chain/node.rs:470–480` invokes the check before running a schedule.

**Required correction:** map this renderer/check and its tests explicitly. Put the minimal SDK field/re-pin and corresponding renderer change in the pin train if S1 adds the key there. Alternatively, explicitly stage the schema addition with its consumers later. The remaining network-value and pruning work may still be separate; the current S5→S7 ordering alone does not cover this dependency.

### M-3 — MN5-01/02 are routed to an adapter, but the base-sdk implementation and corpus are absent

**Rows:** MN5-01, MN5-02; PLAN items 6/10, S5/S10.

`dash-llmq/src/dkg/bls.rs` delegates these worker operations to `dash_pkc::bls::core`. Naming that adapter and changing the DKG session does not update the actual worker. The effective fork's `pkgs/pkc/src/bls/core/worker.rs` remains based on the old Core behavior at the affected locations. S5 lists only `dash-params`; S10 names only dash-llmq and replay vectors. Neither assigns this SDK implementation, its existing BLS oracle corpus, or the additional SDK re-pin dependency.

**Evidence:**

- Core `v24.0.0-rc.5:src/bls/bls_worker.cpp:300–313,688–732`, compared with rc.4 at the same functions.
- Rust `45fc7c59d:dash-llmq/src/dkg/bls.rs:1–8,53–64,66–74`; session consumer `dash-llmq/src/dkg/session.rs:193–240`.
- SDK `1eda010080:pkgs/pkc/src/bls/core/worker.rs:51–55,86–93,149–187`; existing worker tests begin at :213.
- SDK `1eda010080:pkgs/pkc/tests/bls_oracle.rs:7–17,25–30,47–50` and `pkgs/pkc/corpus/bls_oracle/{dkg.json5,aggregate.json5,PROVENANCE.md}`.

**Required correction:** assign the `dash-pkc`/BS11b owner, the affected worker functions and corpus/provenance review, and the fork update plus rdc re-pin to S10's dependency graph. Coordinate that pin with S5 rather than treating the SDK change as exclusively a parameter-field addition. Preserve the neutral descriptions of MN5-01/02.

### M-4 — W-06 is not the same RPC behavior in v24; the 24/11/all-35 claim overstates source equivalence

**Rows:** WATCH W-06, counts table and §1 presence definition; INVENTORY §0 item 5.

W-06 is marked “in rc.4 (no net delta),” contributing to “24 already / 11 first in rc.5 / all 35 reached rc.5.” The v23 change to the provider JSON writers is not present in rc.4 or rc.5, and their pre-existing helper/check path is not behaviorally equivalent. This distinction matters under the requested RPC-reply criterion. The v23 regression test's JSON contract is not established by v24's existing implementation.

**Evidence (neutral source pointers):**

- `v23.1.9:src/evo/core_write.cpp:80–89` (`PushNetInfoFields`), :313–316 and :401–404; commit `2aa3a87275` and its `src/test/evo_netinfo_tests.cpp` regression test.
- Both `v24.0.0-rc.4:src/evo/core_write.cpp:29–44,198–208,296–305` and the identical rc.5 sections use the other implementation. The relevant files have no rc.4→rc.5 hunk.
- Supporting call-path references: `v24.0.0-rc.5:src/evo/providertx.h:162–178,269–285`, `src/evo/specialtx.h:18–26`, `src/core_write.cpp:292–297`, `src/util/check.h:25–32`.

**Required correction:** record W-06 as a distinct v23 RPC variant still absent from these v24 tags, rather than certifying the same reply behavior as present. On a strict behavioral-equivalence count, that is **23 already in rc.4, 11 first in rc.5, 1 distinct outstanding item**. If the intended count instead groups implementations by a weaker property, define that property and qualify “all ... reached rc.5”; it cannot stand as a blanket source-equivalence assertion. This is a watch correction, not a missing rc.4→rc.5 hunk or a request to change rdashd away from its single v24 pin.

## Minor findings

### m-1 — CE5-05 omits the storage layer that computes pruning bounds

**Rows:** CE5-05; INVENTORY §0 item 3; PLAN item 9, S8.

The text says dashd-chainstate has “one constant for both meanings,” but the acceptance constant and the retention constant are separate. `dashd-store` owns the latter and the actual automatic/manual selectors. Updating the listed chainstate sites alone leaves those selectors at 288. Its tests independently model Core's bounds and also need review.

**Evidence:** Core `v24.0.0-rc.5:src/node/blockstorage.cpp:175–177,190–200`; Rust `45fc7c59d:dashd-chainstate/src/chain/accept.rs:25–26`, `dashd-store/src/flatfile/blocks.rs:64–90,665–690,741–757`, `dashd-chainstate/src/flush/prune.rs:8,106–110`; storage model tests in `dashd-store/src/flatfile/tests.rs:610–640,762–790`.

**Correction:** explicitly add dashd-store and its owner to S8, pass per-network retention through both selectors and the plan/recheck paths, and retain the separate near-tip constant where appropriate. The proposed mainnet/regtest tests are useful but should cover this layer too.

### m-2 — W-11 misclassifies #7738 as a non-delta and #7518 as a P2P behavior delta

**Rows:** WATCH W-11 and class counts, especially lines 45/50. There is no new rc.4→rc.5 row for either because both already exist in rc.4.

#7738's “refactor” title does not establish verdict/reply/state preservation. It changes governance vote-record state observable through serialization and `gobject getcurrentvotes`; O17 already records this as MNO-08. Conversely, #7518's signature memo preserves the verification verdict and belongs with performance-only changes under this inventory's criteria.

**Evidence:**

- `v23.1.8:src/governance/object.cpp:83–100` versus `v23.1.9:src/governance/object.cpp:97–102,155–156`.
- V24 equivalent at `v24.0.0-rc.5:src/governance/object.cpp:414–418,472–474`; serialized state at `src/governance/object.h:302,313`; reply construction at `src/governance/governance.cpp:586–595` and `src/rpc/governance.cpp:817`.
- #7518 equivalent at `v24.0.0-rc.5:src/governance/vote.cpp:22–56,164–208`.
- Precedent `45fc7c59d:docs/node-program/designs/O17-pin-delta.md:203,208,363` explicitly makes both distinctions.

**Correction:** move #7738 into an observable governance/RPC/state class and #7518 into non-delta/performance, then recompute class subtotals. This correction does not change their tag-presence status.

### m-3 — CE5-08's promised applicability check has no plan step

**Rows:** CE5-08; WATCH W-10; PLAN impact map and S1–S17.

The inventory correctly retains a storage row and promises a check of Rust's storage/import consumers under DEC-11. That row disappears from the plan: neither the impact map nor any S-step mentions it. An inventory-level prediction of no vector change is not the promised applicability check.

**Evidence:** `v24.0.0-rc.5:src/flatfile.h:14–21`, `src/chain.h:40–45,158–164`, and their rc.4 counterparts; the inventory's consumer pointer is `45fc7c59d:dashd-chainstate/src/crash/chain.rs:87–104`. INVENTORY:117 contains the check requirement; PLAN:19–37,43–63 supplies no owner or evidence for it.

**Correction:** assign a specific check-only deliverable to a storage/import owner, recording which consumers were checked and the basis for applicability or N/A under DEC-11. No expansion of the neutral row or automatic implementation change is needed.

### m-4 — The func-compat runner's operational pin must move before S3's matrix

**Rows:** PLAN item 3, S3/S14; FT consumers of MN5-01/MN5-07/WAL5-01/CE5-05.

The runner has its own branch/tag/release tuple, still pointing at rc.4. It exports tests from that branch and links release binaries selected by that tuple. Changing `setup-dashd.py` or rebasing a new Core branch does not move it. Leaving this under S14's later generic “pin constants” rewrite risks producing an rc.4 matrix as S3's rc.5 evidence, or linking the wrong release against an updated test tree.

**Evidence:** Rust/tooling `45fc7c59d:contrib/node/func-compat/func-compat.py:76–78,294–299,312–327,346–348`; PLAN:23,48,59,67–73. Representative Core FT changes are `v24.0.0-rc.5:test/functional/feature_llmq_dkgerrors.py:50–68`, `test/functional/p2p_platform_ban.py:73–83`, `test/functional/wallet_migration.py:611–631` and `test/functional/feature_asset_locks.py:1442–1490`, compared with rc.4.

**Correction:** explicitly put the tuple update in S3 before generating its matrix, and identify the rc.5 branch/release in that evidence. Do not defer this operational pin to a citation sweep.

## Nits

### n-1 — Reconcile the inventory's summary totals with its complete hunk table

**Rows:** all; especially CE5-03 and the test-only REF-01 consumer.

The index contains **113 runtime hunks in 51 files**, not 111. Excluding the two `chainparamsseeds.h` hunks gives 111, but then it is 50 files and excludes runtime seed data. It also contains **15 test files**, not 14; §3 omits `src/wallet/test/util.cpp`. The named inventory contains **20 distinct rows: 17 behavior/consensus rows, 2 verdict-preserving rows, 1 refactor**, not “21 delta rows plus 1 refactor.”

**Evidence:** independent tag-to-tag hunk/range count matched all 394 indexed rows; Core `v24.0.0-rc.5:src/chainparamsseeds.h:9` onwards and `src/wallet/test/util.cpp:78` are the relevant excluded entries. Category totals are 113 runtime + 33 test-only + 107 build/doc + 141 Qt = 394. Correct INVENTORY §0 and PLAN §1; no coverage loss was found.

### n-2 — Fix stale step references and align formal dependencies with the stated order

**Rows:** CE5-04/06/07 and generated tables; PLAN items 2/14/15, INVENTORY §0 item 8.

PLAN items 2/14/15 refer to **S19**, which does not exist; full replay is **S17**. INVENTORY §0 item 8 calls table regeneration **S15**, whereas the actual table step is **S13**. S16's dependency cell should also include the vector/golden work it is ordered after in §3.1, so readers following the table cannot certify before the intended inputs land.

**Evidence:** PLAN:22,34–35,58–62,73; INVENTORY:88. The associated Core source anchors remain `v24.0.0-rc.5:src/validation.cpp:5205–5213,5306–5316` (CE5-06) and `src/script/interpreter.cpp:1529–1565` (CE5-07); this is a document-link/order correction, not another source delta.

## What was checked

- Read the inventory, hunk index, watch and plan, the rc.4 PIN-DELTA design/audit precedent, O17's method and governance classifications, and the pin-delta audit brief.
- Compared the real rc.4/rc.5 trees rather than inferring content from PRs or patch IDs. Checked every indexed runtime file, the test/build classifications, deleted files, and the complete hunk/range index. No build/doc/test-only hunk was found to conceal a node-runtime change.
- Confirmed no changes in `src/{dashbls,secp256k1,leveldb,crc32c,minisketch,univalue,immer}`, `depends`, `configure.ac`, consensus/kernel sources, protocol or version headers. The Makefile change at `v24.0.0-rc.5:src/Makefile.am:34–41` orders generation of `relic_conf.h`; the generator inputs did not change. No omitted vendored or code-generator runtime delta was found.
- Checked CE5-07's base/caching/deferring checker routing and REF-01's actual Span serialization call sites. Their non-delta classifications are reasonable. No asserted-log difference overturning CE5-07 or MN5-05 was established.
- Traced params/deployment consumers and SDK tables, transaction consumers, pruning selectors/tests, replay/storage consumers, DKG adapter-to-SDK calls and corpora, metadata/P2P comparators and public seeds, governance, and the unbuilt signing/wallet annotations. CE5-01/02/03, CE5-04, CE5-06, MN5-03/04/06/08–10, WAL5-01 and REF-01 have sensible top-level destinations, subject to the explicit gaps above and the planned owner checks. Generated tables and crafted fixtures remain items to verify during implementation, as the plan says.
- Checked v23.1.8→v23.1.9 runtime additions against v24 source, including moved implementations in `llmq/net_quorum.cpp`, `llmq/net_signing.cpp`, `util/fs_helpers.*`, EvoDB/credit-pool code, DKG quotas, provider RPC writers, governance and mempool code. Aside from M-4, the claimed 11 first-at-rc.5 items and the other already-present items are supported. Classification corrections in m-2 are independent of presence.
- Compared the four runtime groups W-15–W-18 with rc.5, including the governance implementation rather than relying only on new identifier searches. They are absent at rc.5. A read-only `ls-remote` confirmed `refs/heads/v24.0.x` still equals `8163290aa0307ed4bc2fc0ba15b78c20d61dfa88` during review. Treat “none of v23.1.10” as a statement about these four runtime fixes; its separate annotation/build item is not a runtime equivalence claim.
- Reviewed S1–S17 train membership, prerequisite consumers, fixture/table movement and evidence ordering. No builds or tests were run, per the brief. This review does not certify the future oracle replay, generated output, binary provenance, live datadirs, performance claims or timing estimates. No restricted directories or shared cache contents were read; no worktree, scratch input, branch, commit or remote was modified. Only this review file was written.
