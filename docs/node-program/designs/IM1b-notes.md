# IM1b: Core data-directory import notes and D-rows

IM1b (`dash-coreimport`) reads what Dash Core wrote: the flat files (`governance.dat`,
`mncache.dat`, `netfulfilled.dat`, `sporks.dat`), the key files and `dash.conf`. This file records
the implementer's decisions where Core's sources leave a choice, per the IMPLEMENTER contract
("decide, record a D-row, continue"). The original slice's decisions live in the crate docs
(`dash-coreimport/src/netstate/`); rows here start with BL-420.

## BL-420: `governance.dat` Version-17 (v24.0.0-rc.4)

Core v24.0.0-rc.4 (#7831, MN4-04 in `SW-V24-rc4-pin-delta/mn-overlay.md`) writes `governance.dat`
as `CGovernanceManager-Version-17` and drops the historical invalid-vote cache from the file.
Citations are at tag `v24.0.0-rc.4` (commit `0e7382df80d0bc6811068d076b232a5a26d34a3c`); rc.3's
`-16` lines are in brackets.

| What | rc.4 (`-17`) | rc.3 / v23 (`-16`) |
|---|---|---|
| version string | `governance/governance.cpp:31` `"CGovernanceManager-Version-17"` | same line, `-16` |
| `Serialize` | `governance/governance.h:202-210`: version string (205), `mapErasedGovernanceObjects` (206), `cmmapOrphanVotes` (207), `mapObjects` (208), `mapLastMasternodeObject` (209), `*lastMNListForVotingKeys` (210) | `governance.h:207-213`: version string (207), erased (208), **invalid-vote cache (209, written empty)**, orphans (210), objects (211), last objects (212), MN list (213) |
| `Unserialize` | `governance.h:214-229`: `Clear()` (216), version check and return (219-222), reads (225-229) | `governance.h:224-235`: version check (224), erased (230), **invalid-vote cache read and discarded (231)**, orphans (232), objects (233), last objects (234), MN list (235) |
| object signature | `governance/common.h:85` `LIMITED_VECTOR(obj.vchSig, MAX_SIGNATURE_SIZE)`, `common.h:37` `MAX_SIGNATURE_SIZE{96}` | unbounded |
| vote (disk) | `governance/vote.h` unchanged: `vchSig` unbounded | same |
| embedded MN list | `evo/netinfo.h`: an `ExtNetInfo` must list purposes that are known and ascending | the netinfo rc.3 → rc.4 delta is in `SW-V24-rc4-pin-delta/` |

| D | Decision | Why |
|---|---|---|
| BL-420 D1 | The reader loads **both** `-17` (rc.4) and `-16` (v23, rc.3). `governance::StoreVersion { V16, V17 }`; `GovernanceDat::version: Option<StoreVersion>` says which one the file was. `-17` has no invalid-vote cache, `-16` has it and the reader discards it as Core's rc.3 did. | The slice's brief: read `-17` exactly as rc.4 does and keep `-16` for v23/rc.3 files, so an import from any node that ran a v24 release works. **This deviates from MN4-04**, which says an rc.4 node treats `-16` as a version mismatch and starts empty; that is what rc.4 *does with its own file*, while the import tool's job is to bring in what an older node learned. |
| BL-420 D2 | **Any other version string** (`-15`, `-18`, a different case, trailing space, empty) leaves the store **empty and is not an error**: `version_matched = false`, `version = None`, every container empty, the orphan-vote capacity at `MAX_CACHE_SIZE`, the MN list at its default, and the rest of the object is not read. Detection reports `Version::Flat { current: false }` for such a file. | Core's load path: `Unserialize` calls `Clear()` first (`governance.h:216`) and returns when the string differs (`:219-222`); `CFlatDB` then sees a successful read. Same as the other three flat files (`mncache.dat`, `sporks.dat`). |
| BL-420 D3 | `-17` objects enforce `vchSig` ≤ 96 bytes (`reader.limited_byte_vec(MAX_OBJECT_SIGNATURE_SIZE)`): a longer count is refused **before any byte of the signature is read**. The failure is a read error inside the object, so the whole file is **discarded** (store cleared, `Loaded::discarded = Some(Corrupt)`), as Core's `CFlatDB` treats `IncorrectFormat`. `-16` objects keep the unbounded read. Votes (`vote.h`) are unbounded in both. | rc.4 reads the object with `LIMITED_VECTOR` (`common.h:85`; the limit also applies to network reads, the hash is unchanged). v23/rc.3 had none, and a `-16` file with a long signature loads there, so the `-16` path must too. 96 itself is accepted. |
| BL-420 D4 | `version_matched` stays (it is `version.is_some()`, kept for callers and the fuzz model), and `version` is added next to it. Both are part of `PartialEq`/`Debug`. Detection's `current` is true for either version. | No caller needs to change; new callers can tell the formats apart. |
| BL-420 D5 | The flat-file `Payload` trait's `VERSION_STRING: Option<&str>` became `VERSION_STRINGS: &[&str]` (empty = no version string, as `netfulfilled.dat`; first = the string Core v24 writes). `peek` (detection and sniffing) accepts any listed string. `mncache.dat` and `sporks.dat` list their single one. | `governance.dat` now has two accepted strings; one list keeps `peek` and the readers in agreement. |
| BL-420 D6 | The embedded `lastMNListForVotingKeys` needs no change: `dash-dml` already enforces rc.4's `ExtNetInfo` purpose rules (`dash-dml/src/codec.rs:504`), and the rc.4 populated vector (six masternodes) parses with it. | Checked against the vector, not assumed. |
| BL-420 D7 | Vectors: a real rc.4 `empty/` and `populated/` set (all four flat files, plus `expected.json` from the same node) under `tests/data/flatfiles/v24.0.0-rc.4/` (provenance and sha256s in its `PROVENANCE.md`). No rc.4 mainnet/testnet oracle `governance.dat` was captured: the live oracle units and datadirs are not touched for this slice. The oracle-sys test (`dash-oracle-sys/tests/gov_regtest.rs`) keeps reading the rc.3 file. | The brief forbids touching the live oracle units; regtest rc.4 shows the `-17` layout and the populated file carries objects, votes, erased entries and a real MN list. |
| BL-420 D8 | Tests per version: both captures parse fully and match their own `expected.json` (`governance_matches_gobject_count_and_the_object_list`); `governance_versions_read_to_the_same_store` rewrites each `-16` capture as `-17` (drops the cache, resealed) and the stores agree; `a_governance_file_of_an_unknown_version_is_not_loaded`; `the_object_signature_limit_is_a_version_17_rule`. The fuzz model asserts `version_matched == version.is_some()` and has the rc.4 files as seeds. | |
