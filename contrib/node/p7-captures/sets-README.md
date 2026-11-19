# Core v24.0.0 quorum connection sets

Provenance: Dash Core tag `v24.0.0`, commit
`938fba502824b9d850b7c710601456dc3d71603e`, source tree
`/work/scratch/sw-v24-mn/core-v24` (read only).
`sets.py` records `src/llmq/net_quorum.cpp:408-497,766-834` and the ordered members
from `src/rpc/quorums.cpp:193-307`. It sets spork 21 with `sporkupdate`, then
verifies `spork show` on every node before collecting each run
(`src/rpc/node.cpp:156-245`; `src/llmq/options.cpp:26-40`).

The input is a JSON array of running regtest nodes, controller first:

```json
[{"datadir": "/work/scratch/quorum-controller", "rpc_port": 19998},
 {"datadir": "/work/scratch/quorum-mn-0", "rpc_port": 19999}]
```

Use the existing datadir RPC authentication. Start every node with
`-debug=netconn -debug=llmq`, connect it to the controller, and provision the
controller's spork signing key. Do not put credentials or private keys in the
JSON or output. Every node must report version 240000. The supplied `--dashd`
binary is the one used to start those nodes; its SHA256 is stored in the output.

Command for a prepared non-rotating LLMQ_TEST quorum (`dkgInterval=24`,
`signingActiveQuorumCount=2`, `src/llmq/params.h:145-164`):

```sh
export CARGO_TARGET_DIR=/work/scratch/sw-v24-mn/p2/target
export TMPDIR=/work/scratch/sw-v24-mn/p2/tmp
python3 contrib/node/p7-captures/sets.py \
  --core /work/scratch/sw-v24-mn/core-v24 \
  --dashd /path/to/v24.0.0/dashd --cli /path/to/v24.0.0/dash-cli \
  --nodes /path/to/nodes.json --spork-value 0 \
  --dkg-interval 24 --active-quorums 2 \
  --quorum 100:QUORUM_HASH --mine-block \
  --output /work/scratch/sw-v24-mn/p2/tmp/sets-mesh.json
```

Run the same command with `--spork-value 4070908800` and another output path for
the ring case (`src/llmq/utils.cpp:757-775`). Each invocation explicitly sets
and verifies the spork. For rotation, pass `--rotation` and the rotating type's
interval and active count (`src/llmq/net_quorum.cpp:413-426`). Repeat `--quorum`
for each scanned quorum. Members keep RPC order; hashes keep RPC display order.
The output records the tip, current cycle ancestors, ordered members, mined
commitment block, spork, and normalized connection lines. Multiline connection
entries keep their two leading spaces; line order is normalized for Core's
unordered sets (`src/llmq/net_quorum.cpp:818-826`). Exit 2 means the observations
lack required lines or members. Exit 1 means capture failed. No result is a
replay proof: `replay_ready` remains false.

## Capture prerequisites

No live capture accompanies these tables. The `masternode_network` fixture
forces spork 21 to `4070908800` at startup and assumes two outbound connections
per member during DKG; `contrib/setup-dashd.py` also states that this fixture's
DKG is not supported on v24. A prepared v24 fixture must allow explicit spork
values, replace the fixed outbound count with authenticated mesh completeness,
and mine a successful commitment before recording
(`src/llmq/utils.cpp:757-775`; `src/llmq/net_quorum.cpp:791-805`).

The recorder attaches to such running nodes; it does not create the fixture or
orchestrate DKG. A subsequent replay needs initial registry state, role and
watch seed, the complete scanned quorum list, and the ring/watch topology
implementation (`src/llmq/net_quorum.cpp:441-488`;
`src/llmq/utils.cpp:778-853`). The interim topology only supplies the mesh
predicate; its ring and watch methods return empty sets. The tables establish
argument passing, set retention and registry operation order, with the mesh
predicate checked against the existing outbound vectors
(`src/llmq/utils.cpp:732-775`; `src/llmq/net_quorum.cpp:813-834`).

Recorder RPC and multiline-log checks (`src/rpc/node.cpp:207-245`;
`src/llmq/net_quorum.cpp:813-827`):

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s contrib/node/p7-captures -p test_sets.py
```
