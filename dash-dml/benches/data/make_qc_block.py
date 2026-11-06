"""Cut the qc_block bench fixture from an lq1-rot capture (Core-produced, see
test-vectors/lq1-rot/PROVENANCE.md): the headers, the members of every quorum mined at one height,
and their commitments, as mined.

usage: python3 -I dash-dml/benches/data/make_qc_block.py <capture.json.gz> <mined height> <out.bin>
"""
import gzip, json, struct, sys

cap = json.loads(gzip.open(sys.argv[1]).read())
mined = int(sys.argv[2])
headers = bytes.fromhex(cap["headers"])
first = cap["first_height"]
assert len(headers) % 80 == 0
n_headers = mined - first + 1
assert n_headers * 80 <= len(headers), "the capture holds the mining block's header"
dmns = [bytes.fromhex(d) for d in cap["dmns"]]
# CDeterministicMN's disk form starts with proTxHash (internal byte order).
by_pro = {d[:32][::-1].hex(): i for i, d in enumerate(dmns)}
quorums = [q for q in cap["quorums"] if q.get("commitment") and q["mined_height"] == mined]
assert quorums and all(q["verifies"] for q in quorums)
used = sorted({by_pro[m] for q in quorums for m in q["members"]})
remap = {old: new for new, old in enumerate(used)}

out = bytearray(b"lq2qcb01")
out += struct.pack("<III", first, n_headers, mined)
out += headers[: n_headers * 80]
out += struct.pack("<I", len(used))
for i in used:
    out += struct.pack("<I", len(dmns[i])) + dmns[i]
out += struct.pack("<I", len(quorums))
for q in quorums:
    qc = bytes.fromhex(q["commitment"])
    out += struct.pack("<II", q["base_height"], len(q["members"]))
    out += b"".join(struct.pack("<I", remap[by_pro[m]]) for m in q["members"])
    out += struct.pack("<I", len(qc)) + qc
open(sys.argv[3], "wb").write(out)
print(f"{len(quorums)} commitments, {len(used)} masternodes, {n_headers} headers, {len(out)} bytes")
