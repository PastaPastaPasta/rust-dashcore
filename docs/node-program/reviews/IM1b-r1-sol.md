# IM1b r1 review — `ddbac4a76`

Assumptions: the importer treats `governance.dat` and the key files as untrusted local input, and long-lived onion/I2P keys must not leave avoidable copies in freed memory. The Unix key-file mode is the supported secure-write case; the documented non-Unix private-directory requirement is a caller obligation. I reviewed the five commits against `node/integration` without changing the branch.

## Findings

### Major — quadratic decoding of a crafted governance masternode list

**`dash-coreimport/src/netstate/governance.rs:390-398`**, through **`dash-dml/src/list.rs:621-640`**. A checksum-correct `governance.dat` can embed many otherwise distinct masternodes whose ProTx hashes share the same first eight bytes. `decode_list_s3` calls `try_add` for each entry; `bucket_set` scans every existing key with that 64-bit hash, collects them, then copies the old bucket into a new vector. Adding *n* such entries takes quadratic work and allocations, even though the flat-file input stays under the 128 MiB cap. An invalid tail can still force this work before the file is discarded. **Fix:** build collision buckets incrementally or otherwise bound work per insert, preserving their order; add an adversarial same-prefix list regression with an operation or instruction-count scaling check.

### Minor — a growing key file can leave a freed, unwiped copy

**`dash-coreimport/src/keys.rs:85-89`**. `read_secret_file` reserves the file size observed by `fstat`, plus one byte. If the regular file grows between that check and `read_to_end`, the `Vec` reallocates. `Secret::drop` wipes the final allocation only; the prior allocation, holding a prefix of the onion or I2P private key, has already been freed without wiping. This contradicts the module's claim that the reader clears its own buffers. **Fix:** allocate the fixed `limit + 1` capacity before reading, or read into a fixed-size secret buffer without reallocation; test with a reader whose reported initial size is smaller than the bytes delivered.

### Minor — key writer ignores cleanup and directory-sync failures

**`dash-coreimport/src/keys.rs:159-165`**. After linking the new key into place, failure to remove the sibling temp file is ignored, leaving an additional long-lived copy of the private key while `write_secret_file` returns success. A failed directory open or `sync_all` is also ignored, so success does not establish that the final name survives a crash; a vanished key would cause a new node identity to be generated. **Fix:** surface cleanup and directory-sync failures, with an explicit partial-success outcome if the destination link already exists, and cover these paths with fault injection.

## Checks and scope

- Compared the flat-file envelope and the governance, masternode cache, net-fulfilled and spork layouts with Dash Core `v24.0.0-rc.3` source. The sampled version, magic, clear-on-format-error, duplicate-key, vote recount and signature-check behavior matched; I found no further Core-fidelity divergence in those paths. The `dash.conf` section and include ordering also matched `ArgsManager::ReadConfigFiles` in the sampled cases.
- Relied on the brief's full gate result: 4,945 tests passed, zero failed; fuzz smoke 57/57. I did not rerun the full gate on this loaded host.
- In a detached scratch worktree, replacing the onion padding guard with `if false` made `malformed_onion_keys_are_refused` fail on the expected slice panic. Restoring the source made that test pass. A weaker mutation retaining `pad > 2` still passed, because the final decoded-length check also rejects padding in an early group. The scratch source was restored.
- The documented 18 bytes per input byte memory bound excludes the embedded masternode list. I did not establish a peak-memory bound for that decoder; the Major finding concerns its demonstrable quadratic CPU and allocation work.

VERDICT: CHANGES-REQUESTED
