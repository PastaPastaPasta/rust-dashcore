# Functional-test adaptations (O11)

These are the changes the func-compat patch set (`node/func-compat-<pin>`, `test/functional/test_framework/rust_nodes.py`) makes for rdashd nodes only. Each one has an owner. Before every run, rust-dashcore `contrib/node/func-compat/func-compat.py adaptations` fails as soon as rdashd takes one of them, and then that row is dropped. Design: rust-dashcore `docs/node-program/designs/O11-notes.md` D4.

| Adaptation | What the framework does | rdashd today | Owner |
|---|---|---|---|
| drop `port=`, `bind=` | writes them into every node's dash.conf | refuses them | P2b |
| drop `tinyblk=` | writes it into every node's dash.conf | refuses it | ST2 |
| drop `keypool=`, `fallbackfee=`, `-createwalletbackups=0` | the conf and the args of every node | refuses them | WL2a |
| startup wait: a missing `getmempoolinfo` counts as loaded | `wait_for_rpc_connection` polls `getmempoolinfo.loaded` | no method | MP1a |
| a one-node `sync_blocks` / `sync_mempools` returns at once | `getbestblockhash`, `getrawmempool`, `syncwithvalidationinterfacequeue` | no methods | RP1a, MP1a |
| lock error: `Dash Core is probably already running.` becomes `rdashd is probably already running.` (`MESSAGE_ADAPTATIONS` probe `datadir-lock`) | `feature_filelock` expects Core's lock error, which carries `PACKAGE_NAME` | names itself `rdashd` (DEC-83: honest identity, deliberate) | ND1 |

Message adaptations (DEC-359, O11 D12) don't expire when the owner lands, because the difference is deliberate. Their tripwire starts a second rdashd on a running node's datadir and fails the run once rdashd's error no longer contains the adapted text.
