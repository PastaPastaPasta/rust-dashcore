# ev2a-codec-probe

Dash Core's masternode-list codecs as a line oracle for `dash-dml`'s codec vectors (EV2a;
`designs/EV2-dml.md` §2.9, §6.2): `probe.cpp` links Core's own v24 code (`evo/snapshot.h`,
`CDeterministicMNList`, `CDeterministicMNListDiff`) at the v24.0 oracle pin, and `regen.py` writes
`dash-dml/tests/data/codec/` from its answers. It stands in for the O2a-EV codec entry points and
O8's `getcanonicalmnlist` until they exist.

```sh
# A Core build tree of the pin with unlocalized libraries and a recorded dashd link line, such as
# contrib/node/sb4-core-scenarios/build.sh makes. The tree is only read.
contrib/node/ev2a-codec-probe/build.sh <core-build>/<pin commit> /work/scratch/<task>/probe
python3 -I contrib/node/ev2a-codec-probe/regen.py --probe /work/scratch/<task>/probe/ev2a_codec_probe \
    --samples <dml-listdiff-check.py --save-samples file> --heights 1030853,1868232 \
    --out dash-dml/tests/data/codec
```

`regen.py` reads only the `height` and `prev_list` of the two samples. Without the original
samples file, the `evodb` column of the committed `lists.txt` stands in: written as
`{"height": <list height + 1>, "prev_list": <evodb>}` lines, it gives Core the 48 masternodes
it cut before (so `take` keeps them all: the run re-checks Core's decode and re-encoding of
those lists, not the cut from the full mainnet list), and the regenerated mainnet lists and
diffs must come out byte-identical (as they did for the v24.0.0 pin; an `apply` refusal may
name another missing id, which Core picks in salted-hash order).

`budget.py` writes `budget.txt` from the cases `dash-dml` prints (see its usage text).

The probe answers one request per line (see the comment at the top of `probe.cpp`). The vector
files carry the probe's MANIFEST (pin commit, source and archive hashes) in their headers.
`DASH_DML_CORE_LISTS` runs `dash-dml`'s ignored full-size test on a file of `<evodb list> <probe
s3 answer>` lines.
