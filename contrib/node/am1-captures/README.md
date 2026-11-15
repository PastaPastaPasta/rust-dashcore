# AM1 operator-key vectors

`keyparse.py` starts a fresh Dash Core v24.0.0 regtest process for each input,
using the existing func-compat launcher's exported test tree and release binaries.
It writes `dashd-masternode/tests/fixtures/am1-key/core-v24.json`: 54 parse cases
and the public keys and constructor logs for all four `network.json` operator keys.
All keys in these fixtures are public test keys.

Recorded provenance:

- Core tag `v24.0.0`, commit `938fba502824b9d850b7c710601456dc3d71603e`.
- `dashd` SHA256: `2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`.
- Regtest network v0.0.4 `network.json` SHA256:
  `1df3ee4100b42803b887ef6c96fd34df29d7982a376aea5798a31678b5b71bfb`.

From the repository root, with the Core checkout outside the writable capture tree:

```bash
export CARGO_TARGET_DIR=/work/scratch/sw-v24-mn/a1/target
export TMPDIR=/work/scratch/sw-v24-mn/a1/tmp
mkdir -p "$TMPDIR"
eval "$(DASHVERSION=24.0.0 CACHE_DIR="$PWD/target/am1-cache" python3 contrib/setup-dashd.py)"
python3 contrib/node/am1-captures/keyparse.py \
  --core /work/scratch/sw-v24-mn/core-v24 \
  --work "$PWD/target/am1-capture" \
  --cache "$PWD/target/am1-cache" \
  --network "$PWD/target/am1-cache/regtest-blockchain-v0.0.4/regtest-mn/network.json"
```

The recorder creates mode-0700 datadirs under `--work`, keeps the raw process
output there as `capture.txt`, and normalizes the key-related error/exception or
constructor log into the fixture. It also preserves each process's exit code.
`-stopafterblockimport=1` can stop a successful key parse before startup finishes,
so a key's recorded outcome follows the constructor log rather than the exit code.
An inactive input must reach the block-import stop without a constructor or error.
Exit codes for successful parses can therefore differ between reproductions.

## Signature vectors

After `keyparse.py`, append the signature vectors with:

```bash
python3 contrib/node/am1-captures/signatures.py \
  --core /work/scratch/sw-v24-mn/core-v24 \
  --work "$PWD/target/am1-signatures"
```

The recorder exports the pinned Core commit above into `--work/core`, builds its
unmodified dashbls library, and links `signatures.cpp` with Core's unmodified
`src/bls/bls.cpp`. It calls `CBLSSecretKey::Sign(hash, true/false)`, the function
`CActiveMasternodeManager::Sign` delegates to (`src/active/masternode.cpp:290-294`),
and serializes each result in its signing scheme. The separate `sign_basic`
capture follows `CActiveMasternodeManager::SignBasic`: sign with `false`, check
validity, then `ToByteVector(false)` (`src/active/masternode.cpp:297-303`). All three
variants are available through this helper.

The fixture's `signatures` section contains four rows: network keys 0 and 1, each
signing raw message-hash bytes `000102...1e1f` and 32 bytes of `42`. These are the
bytes at `uint256::begin()`, without a display-hex reversal. Each row records the
96-byte legacy, basic and `sign_basic` outputs. The Rust test compares all three
outputs directly to those recorded bytes, independently of SDK verification.

Capture provenance:

- Core commit: `938fba502824b9d850b7c710601456dc3d71603e`.
- Compiler: `g++ (Ubuntu 15.2.0-16ubuntu1) 15.2.0`, C++20, `-O2`.
- Helper source SHA256:
  `0e10634cbb70af64e572feed143e8f261e80f7c337437e5c675029f9b47ba357`.
- Helper binary SHA256:
  `db677306ffbc3fcdeba751e56113e9b3fffb4cc4984b259ad6f0c83ab1d59c4a`.
- Built dashbls archive SHA256:
  `8b76f2fb95b7a1b6fb2bdcd9d7948f881efdbe60817d7746e7e5f993bd269ba8`.

The recorder stores these provenance fields in the fixture too. Build-artifact
hashes can vary with the compiler and build path; the signature bytes are fixed.

## A1 plan amendment: Core's recorded rule

The recording determines behavior when the AM1 plan's wording differs. The key
must contain exactly 32 bytes, be nonzero, and be at most
`73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001` (`r`).
Core's comparison is `> r` in `src/dashbls/src/privatekey.cpp:66-70`.
Exactly `r` is accepted and its public key is infinity in both schemes; the
parse-matrix test requires both recorded exactly-`r` cases and compares their
public keys. Values above `r` and the all-zero key are rejected.

The departures and clarifications relative to the plan text are:

1. The plan's rejection of scalars greater than or equal to `r` is replaced by
   Core's rejection of scalars greater than `r`.
2. "Whitespace skipped" means ASCII whitespace between complete byte pairs,
   including before and after the input. A split nibble and non-ASCII whitespace
   are rejected (`src/util/strencodings.cpp:81-97`). Odd hex and invalid suffixes
   are rejected in their entirety. The zero-key and odd-hex outcomes otherwise
   agree with the plan.
3. The interface amendment's phrase "non-string settings value = Err" follows
   Core's actual `GetArg` conversion: null is inactive, booleans and numbers are
   converted to text, and arrays/objects produce the recorded `get_str` exception.
   A 64-digit JSON number can therefore encode an accepted key. This also follows
   A1's more detailed conversion rule.
4. A1(e)'s existing `cfg(test)` lockedpool observation is also available through
   the test-only `test-support` feature so a dependent crate's unit test can
   observe the cleansed chunk. Release builds reject that feature, following the
   existing `core-layout` test-feature convention.

The implementation uses zeroizing typed SDK scalars for keys below `r` and
constructs the public infinity results directly for exactly `r`. The SDK's
Core-shaped scalar does not implement `Zeroize`; no such scalar holding an
operator secret is introduced by this module.

A1(d), the rdashd process-level log/RPC check, remains deferred to W1 as assigned.
The existing `dash-args` settings logger prints settings.json values directly
(`dash-args/src/args.rs:839-856`), as Core does. A1 tests command-line/config
masking and the key loader's own logs; the settings logger needs attention in
W1's process hygiene work.

## Active state

Core parity: `active/masternode.cpp:112-276` at v24.0.0. `state.py` launches
release Core processes over fresh mode-0700 copies of the v0.0.4 `regtest-mn`
fixture used by `dash-spv/src/test_utils/masternode_network.rs`. The controller
loads the fixture's wallet; the MNs use its public test operator keys. The
initial registration is the fixture's existing registration at height 126.

```bash
export CARGO_TARGET_DIR=/work/scratch/sw-v24-mn/a4/target
export TMPDIR=/work/scratch/sw-v24-mn/a4/tmp
eval "$(DASHVERSION=24.0.0 CACHE_DIR="$PWD/target/am1-state-cache" python3 contrib/setup-dashd.py)"
python3 contrib/node/am1-captures/state.py \
  --binary "$PWD/target/am1-state-cache/dashcore-24.0.0/bin/dashd" \
  --network "$PWD/target/am1-state-cache/regtest-blockchain-v0.0.4/regtest-mn" \
  --work "$PWD/target/am1-state-capture"
```

The output is `dashd-masternode/tests/fixtures/am1-state/core-v24.json`.
Its provenance fields record the release commit, binary SHA256 and
`network.json` SHA256. `rows` preserves `masternode status`, `protx info`, the
full registered list, and the target's `CActiveMasternodeManager::` lines at
transition blocks. `offline_blocks` preserves the list and info for every
block while the target is stopped, with `status: null` and empty target logs.
The raw datadirs and DKG observations (`dkg.json`) remain under `--work`.

Core parity: `active/masternode.cpp:140-172,196-226`. The capture covers the
registered READY state, ProUpServ address mismatch and restoration, ProUpReg
removal, restored-key POSE_BANNED, revival, ProUpRev removal, and registration
with the revoked entry's collateral. The recorder also attempts a direct
replacement of a READY entry with the same operator key; Core's
`bad-protx-dup-key` refusal is preserved in `missing` (`evo/specialtxman.cpp:413-425`).

Core parity: `active/masternode.cpp:140-142`. The remaining three fixture MNs
run DKG while the target is stopped. The recorder advances mocktime and mines
up to six 24-block cycles, stopping early on a ban. Restarting the target
observes POSE_BANNED. The Rust replay uses the recorded lists and compares
state, status, identity, collateral, service, and emitted log records against
the active rows (`active/masternode.cpp:67-110,112-228`).

## MNAUTH wire capture

`mnauth.py` records the 128-byte `mnauth` payload from an active Dash Core
v24.0.0 masternode. It runs once for each direction and each `-pushversion`
mode, with the same fixed challenge and the operator key selected from the
public regtest v0.0.4 `network.json` fixture. The outbound run asks the target
through RPC to dial the scripted listener. The JSON record names the process
scheme and the basic signing scheme, including the scheme used to serialize the
signature.

Core provenance is the read-only tree `/work/scratch/sw-v24-mn/core-v24`, tag
`v24.0.0`, commit `938fba502824b9d850b7c710601456dc3d71603e`. The wire format
and signing fields are `src/evo/mnauth.cpp:19-49` and `src/bls/bls.h:177-180`.

Example commands from the repository root:

```sh
NET="$PWD/target/am1-cache/regtest-blockchain-v0.0.4/regtest-mn"
BIN="$PWD/target/am1-cache/dashcore-24.0.0/bin/dashd"
TMP="${TMPDIR:?set TMPDIR to a writable capture directory}"
python3 contrib/node/am1-captures/mnauth.py \
  --network "$NET" --binary "$BIN" --work "$TMP/mnauth-work-in-no-pushversion" \
  --mn-index 0 --direction inbound \
  --peer-version 70242 --process-scheme basic --host 127.0.0.1 --port 19950 \
  --out target/mnauth-in-no-pushversion.json
python3 contrib/node/am1-captures/mnauth.py \
  --network "$NET" --binary "$BIN" --work "$TMP/mnauth-work-in-pushversion" \
  --mn-index 0 --direction inbound \
  --peer-version 70242 --pushversion 70100 --process-scheme basic \
  --host 127.0.0.1 --port 19950 --out target/mnauth-in-pushversion.json
python3 contrib/node/am1-captures/mnauth.py \
  --network "$NET" --binary "$BIN" --work "$TMP/mnauth-work-out-no-pushversion" \
  --mn-index 0 --direction outbound \
  --peer-version 70242 --process-scheme basic --host 127.0.0.1 --port 39992 \
  --out target/mnauth-out-no-pushversion.json
python3 contrib/node/am1-captures/mnauth.py \
  --network "$NET" --binary "$BIN" --work "$TMP/mnauth-work-out-pushversion" \
  --mn-index 0 --direction outbound \
  --peer-version 70242 --pushversion 70100 --process-scheme basic \
  --host 127.0.0.1 --port 39993 --out target/mnauth-out-pushversion.json
```

Each invocation copies the fixture's masternode datadir into `--work`, starts
the target, and stops it after recording one payload. Its `-pushversion` is
omitted for the `no-pushversion` records and is set to `70100` for the other
pair. Startup includes `network.json`'s `dashd_extra_args`, the fixture's
`-masternodeblsprivkey`, and `-mocktime=1700000063`, matching `state.py`.
The exact setup command for the release binary is:

```sh
eval "$(DASHVERSION=24.0.0 CACHE_DIR=\"$PWD/target/am1-cache\" python3 contrib/setup-dashd.py)"
```

The four records are combined in
`dashd-masternode/tests/fixtures/mnauth/core-v24.json`; each payload is 128
bytes and includes the public key, challenge, direction, advertised version,
and serialization schemes.

The pre-V19 live `masternode_network` mode is unavailable because that fixture
starts after V19. This piece therefore uses the legacy-scheme Rust sign and
verify round trip in `mnauth/sign.rs`, covering the legacy serialization in
`evo/mnauth.cpp:40-45` and `bls/bls.h:177-180`.

## MNAUTH intake capture

`mnauth.py --mode in` records MNAUTH intake on a receiving Dash Core v24.0.0
masternode (Core parity: `src/evo/mnauth.cpp:51-177`, `src/net.cpp:4514-4539`).
It writes `dashd-masternode/tests/fixtures/mnauth/core-v24-in.json`, which
`mnauth::handler_tests::core_v24_mnauth_intake_replays_through_the_handler`
replays through the real `MnAuthHandler` in the processor's sim.

Provenance: Core tag `v24.0.0`, commit `938fba502824b9d850b7c710601456dc3d71603e`
(read-only tree `/work/scratch/sw-v24-mn/core-v24`); release `dashd` SHA256
`2ad05ccb0951bc127d1048bf0bfea0956a968482644ed130f8f59fbfcd732223`; regtest
v0.0.4 `network.json` SHA256
`1df3ee4100b42803b887ef6c96fd34df29d7982a376aea5798a31678b5b71bfb`; the same
cache as the wire capture above. From the repository root:

```sh
C=/work/scratch/sw-v24-mn/a2/cache
python3 contrib/node/am1-captures/mnauth.py --mode in \
  --network "$C/regtest-blockchain-v0.0.4/regtest-mn" \
  --binary "$C/dashcore-24.0.0/bin/dashd" \
  --work "$TMPDIR/mnauth-in-work" \
  --out dashd-masternode/tests/fixtures/mnauth/core-v24-in.json
```

The receiver is fixture masternode 0 and the sender masternode 1, each started
as the wire capture starts its target (`network.json`'s `dashd_extra_args`, the
operator key, `-mocktime`, two `mnsync next`); the receiver also runs with
`-capturemessages` and `-deprecatedrpc=banscore`. The records, in order:

1. `core-inbound`: the sender dials the receiver (`addnode onetry`) and
   authenticates; the receiver grants it and, the sender being a quorum relay
   member, sends `qsendrecsigs` (`01`).
2. `core-outbound-dedup`: the receiver dials the sender, which authenticates
   again; the dedup walk drops the new outbound (`mnauth.cpp:137-144`).
3. Scripted inbound peers sending `null-signature`, `unknown-masternode`,
   `wrong-challenge` (leg 1's signature), `null-protx`, `no-bloom-service`,
   `malleable-signature` (96 bytes of `ff`) and `truncated` (3 bytes).
4. `unsynced-truncated`: a receiver started without `mnsync next` dials a
   scripted listener, which sends 3 bytes; nothing is read (`mnauth.cpp:57-60`).
   An unsynced masternode refuses inbound peers, so this leg is outbound.

Each record keeps the receiver's sent challenge unmasked (from its captured
`version`), the operator key as `protx info` prints it with the entry's state
version and scheme, the process scheme, the synced flag, the direction, the
peer's version and services, our proTxHash, `getpeerinfo` before the message
(the earlier peers in table order), the payload, and the observations:
`getpeerinfo` after (`verified_proregtx_hash`, `verified_pubkey_hash`,
`banscore`; absent when the peer was dropped), `qsendrecsigs` payloads from `msgs_sent.dat`, and the debug.log
lines starting `CMNAuth::ProcessMessage`, `Misbehaving:` and
`ProcessMessages(mnauth` in that message's window. The raw datadirs, capture
files and logs stay under `--work`.

The capture cannot supply quorum relay membership
(`IsMasternodeQuorumRelayMember`, `net.cpp:4494-4506`), so the replay derives
the handler's relay answer from the observation: a record whose observed `qsendrecsigs` is
non-empty replays with the sender as a relay member, and every other record
without.

Not observable from Core and asserted on the Rust side only: the IQR flag
(`rpc/net.cpp:270-309` has no field), the probe flag of a marked inbound, and
the score of a dropped peer except through its `Misbehaving` line. The
"marking old/new inbound" walk branches need our proTxHash to be the
deterministic outbound side, which this fixture pair does not give, and a
probe's success needs a probe connection Core opens on its own
(`mnauth.cpp:106-111`); they and the verified-inbound cap are proven by the tables and sim tests in
`mnauth/decide.rs` and `mnauth/handler_tests.rs`.
