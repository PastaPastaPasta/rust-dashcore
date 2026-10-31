# Landing owners (DEC-973, from 2026-10-31)

Each task, slice or BL item has one owner thread. The owner is accountable for getting it **merged**, not just coded. Rules: PROCESS.md, *Operating model*. Each owner keeps its own log, `status/<OWNER>.md`.

Threads are T3 thread ids under the program project; send with `t3_thread_send`, mode `queue` unless it is a deliberate redirect (DEC-386).

| Owner | Scope (lands) | Thread id |
|---|---|---|
| **CP** (critical path; priority 1) | CH7a-4b, CH7a-5, CH7b-1, CH7b-2, CH7b-3, and EV1-5b as their dependency (directing EV1) | `thread:delegated-task:command%3Amcp%3Ab29af53d-3481-44a3-a5b7-7145044ffd96%3Adelegate-task%3Anode-sb11-impl-0905` |
| EV1 (under CP) | EV1-5b-2 farm, gate and reviews, then landing | `thread:delegated-task:command%3Amcp%3Af9b09e80-5ce8-4420-87f2-4a1480fcd3f7%3Adelegate-task%3Anode-ev1-1-impl` |
| **TRAINS** (SB1) | Train cuts and merges into node/integration end to end; layer, purity and mobile gates | `thread:delegated-task:command%3Amcp%3A5184e093-211b-456d-a40f-0d1d10c76a32%3Adelegate-task%3Anode-sb1-layer-gates` |
| **CI-S0** | Fork snapshots, RDC-CI lanes and verdicts, CI triage | `thread:delegated-task:command%3Amcp%3A31daa4a7-8ccd-4507-93f3-6eb99c3b93b4%3Adelegate-task%3Aci-s0-impl-2150` |
| **CI-S2** | node-gate workflow versions (v12, v13 rc.5, v14 macOS / BL-556), fork caches | `thread:delegated-task:command%3Amcp%3A31daa4a7-8ccd-4507-93f3-6eb99c3b93b4%3Adelegate-task%3Aci-s1-impl-2150` |
| **SW-V24** (priority 2, deadline 2026-11-09) | rc.4 → rc.5 re-pin: S2–S18, the pin-move train contents (SB1 merges it) | `thread:delegated-task:command%3Amcp%3A31daa4a7-8ccd-4507-93f3-6eb99c3b93b4%3Adelegate-task%3A91833265-fd79-4ac5-a111-6c76d30fc379` |
| **SW-UPSTREAM** | Weekly upstream sync, ADAPT-902, BS5-R re-pin, BL-553 | `thread:delegated-task:command%3Amcp%3A62932ed6-797a-49f6-a13c-b659d8b3724d%3Adelegate-task%3Anode-sw-upstream-1028` |
| **BSDK** (batch 3) | base-sdk PR code: #86, #87, #88 fix rounds and fork CI. GitHub posts and dashpay pushes stay with the manager (E1) | `thread:delegated-task:command%3Amcp%3Ab29af53d-3481-44a3-a5b7-7145044ffd96%3Adelegate-task%3Absdk-batch3-1805` |
| **CH3** | BL-546 (fix round → landing), BL-554, BL-559, BL-407, CH3 rows | `thread:delegated-task:command%3Amcp%3Acfc879d5-b12c-43d2-bcde-8bb9939fb3c5%3Adelegate-task%3Anode-ch3-design` |
| **ND1** | BL-555, BL-557, BL-558, dashd-node and rdashd rows | `thread:delegated-task:command%3Amcp%3Af9b09e80-5ce8-4420-87f2-4a1480fcd3f7%3Adelegate-task%3Anode-nd1-rdashd-impl` |
| **MG-04b** (desktop) | MG-04b 2d → desktop main (desktop merge bar) | `thread:delegated-task:command%3Amcp%3Ab29af53d-3481-44a3-a5b7-7145044ffd96%3Adelegate-task%3Adw-mg04b-impl-1825` |
| **CLERK** | Heartbeat, PR sweep and routing, STATUS.md roll-up, unit and CPU audit, disk/load guard, scratch cleanup by rule, stall nudges | `thread:delegated-task:command%3Amcp%3A31daa4a7-8ccd-4507-93f3-6eb99c3b93b4%3Adelegate-task%3Aclerk-dec973-0112` |

Any other STATUS row's existing thread is that row's landing owner when it next runs. New rows get an owner when the manager opens them.
