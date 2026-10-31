Verdict: ACCEPT-WITH-NITS

The two commits match Dash Core v24.0.0-rc.4 for the requested behavior:

- `-dbcache` remains a MiB value with Core's 300 MiB default, 4 MiB minimum, and 16,384 MiB maximum on this 64-bit build. The cache split and post-shift clamping follow `src/node/caches.cpp:15-40`.
- `-dbbatchsize` is read in bytes with Core's 16 MiB default and the same `GetIntArg`/`size_t` conversion (`src/txdb.cpp:113-118`). Zero, negative values, partial numeric strings, and saturated integer inputs therefore have Core's behavior. There is no separate min/max clamp in Core to reproduce.
- A split flush writes the durable `head_blocks` marker in B1 (`dashd-chainstate/src/flush/machine.rs:966-980`), writes coin chunks as buffered partial batches (`:653-686`), synchronizes the WAL before Bk (`:667-690`), and writes the final best-block marker last (`:700-710`). This preserves Core's ordering at `src/txdb.cpp:134-171`.
- The parsed value reaches both production paths: `dashd-node/src/node.rs:261,305-313` and `dashd-node/src/sut.rs:339,350-360`.

Findings

n-1 — Production `Node` forwarding is not covered by the new regression test. `dashd-node/src/sut.rs:1326-1360` only constructs `NodeSut`, so a change such as removing/replacing the `batch_size` argument at `dashd-node/src/node.rs:305-313` (for example, running the real node with `-dbbatchsize=1000` while silently using the default) would leave these tests green. Core's corresponding behavior is exercised by `src/txdb.cpp:117,152-154`, where the supplied value controls partial-batch writes. Failing input: start the production node with `-dbbatchsize=1000` and a flush containing more than 1,000 bytes of dirty coins; it should emit multiple partial-batch writes. Fix: add a real-node/integration case that starts `Node` with that input and asserts the split (or otherwise directly assert the value passed through `Node::open_node`).

Counts: 0 major, 0 minor, 1 nit.
