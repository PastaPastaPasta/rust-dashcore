# Provenance of the `dash-p2p` test data

## `core_overlay-qfcommit-136.json5`

The 136 `qfcommit` cases T6 Q-4 (DEC-1929) removed from base-sdk's fork test corpus, copied byte for
byte from the Q-1 extract `core_overlay-qfcommit-136.json5`, which holds them unchanged
from `PastaPastaPasta/base-sdk` `de0d0c7ef200f8d6ba1df2f3464d648620298898:pkgs/p2p_core/corpus/core_overlay.json5`
(its header comment names the source and copies that file's field descriptions). Each case is Dash
Core's read of the payload from a judge linked against the v24 pin: 72 refused (`"ok": false`, the
exception class), 64 read (`"ok": true`, with `consumed`, `hash` and Core's write-back `reenc`).

| | |
|---|---|
| File sha256 | `37e6012d5ab6fd0d7747fc30f035eaa7a031864e182a95c2f423336c7dc5a13b` |
| Read by | `tests/qfcommit_core_overlay.rs` |
