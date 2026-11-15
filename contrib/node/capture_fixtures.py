#!/usr/bin/env python3
"""Re-capture the Core-produced `mnlistdiff` / `qrinfo` wire fixtures of rust-dashcore.

Every file is the payload of a P2P reply from a real Dash Core node (see `p2p_capture.py`), saved
byte for byte. Running this against any v23.1.x / v24 mainnet or testnet peer must reproduce the
checked-in files exactly: the replies are a pure function of the chain and the protocol version.

  contrib/node/capture_fixtures.py --mainnet-peer HOST:9999 --testnet-peer HOST:19999 [--check]

`--check` compares with the checked-in files instead of overwriting them. Paths are relative to the
repository root. See `dash/tests/data/test_DML_diffs/PROVENANCE.md` for which peers produced the
checked-in bytes and how they were verified.
"""

import argparse
import hashlib
import os
import subprocess
import sys
import time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CAPTURE = os.path.join(os.path.dirname(__file__), "p2p_capture.py")
WIRE = "dash/tests/data/test_DML_diffs/core_wire"
MAINNET_GENESIS = "00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6"
TESTNET_GENESIS = "00000bafbc94add76cb75e2ec92894837288a481e5c005f6563d91623bf8bc2c"

# Whole-list and tip diffs that live at their historical paths.
NAMED_DIFFS = [
    ("mainnet", "dash/tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin", MAINNET_GENESIS,
     "000000000000000899fdcd85241296146c365b238a655517da8dcd08a8a79b98"),
    ("mainnet", "dash/artifacts/mn_list_diff_0_2227096.bin", MAINNET_GENESIS,
     "000000000000000899fdcd85241296146c365b238a655517da8dcd08a8a79b98"),
    ("mainnet", "dash/tests/data/test_DML_diffs/mn_list_diff_2227096_2241332.bin",
     "000000000000000899fdcd85241296146c365b238a655517da8dcd08a8a79b98",
     "00000000000000155f43e85cc4df6b0eab1940b5c50e4b04a42206ff8c9e20b4"),
    ("testnet", "dash/artifacts/mn_list_diff_testnet_0_1296600.bin", TESTNET_GENESIS,
     "0000005d8b4322610f47557f4ff4d8fa66603474bf8429a1d52bf242803aaaf7"),
]

# (base height, height, base hash, block hash): the pairs `mnlistdiffs_2240504.dat` is keyed by.
# The first one (0 -> 2227096) is the NAMED_DIFFS file above.
MAINNET_DIFFS = [
    (0, 2227096, "00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6", "000000000000000899fdcd85241296146c365b238a655517da8dcd08a8a79b98"),
    (2227096, 2239192, "000000000000000899fdcd85241296146c365b238a655517da8dcd08a8a79b98", "0000000000000024be78ce2fbe6599a5e535ed68bdd8aa808ceb2a9fb18f1892"),
    (2227096, 2239480, "000000000000000899fdcd85241296146c365b238a655517da8dcd08a8a79b98", "0000000000000036df07313d8859a3ad56f8dcca34ef4e10d0b631321fcce029"),
    (2227096, 2239768, "000000000000000899fdcd85241296146c365b238a655517da8dcd08a8a79b98", "000000000000000aa7261cb101624d192378f5c00a84234728548b14c9fee383"),
    (2227096, 2240056, "000000000000000899fdcd85241296146c365b238a655517da8dcd08a8a79b98", "000000000000002c29db981bb07d3f34ec3fd0413b585f2826b513df3f09eb9c"),
    (2227096, 2240344, "000000000000000899fdcd85241296146c365b238a655517da8dcd08a8a79b98", "00000000000000271435c71750b45817d373f5bc6a3abe05edecc6d32271c97f"),
    (2227096, 2240504, "000000000000000899fdcd85241296146c365b238a655517da8dcd08a8a79b98", "00000000000000218d17031cc693da5c2d422b2644ec56c3fb6f43a617426ae6"),
    (2239480, 2239768, "0000000000000036df07313d8859a3ad56f8dcca34ef4e10d0b631321fcce029", "000000000000000aa7261cb101624d192378f5c00a84234728548b14c9fee383"),
    (2239768, 2239912, "000000000000000aa7261cb101624d192378f5c00a84234728548b14c9fee383", "0000000000000013c66d34e294ae3d6ad6c4c34a956c1b517514c78671fa4dfa"),
    (2239912, 2239936, "0000000000000013c66d34e294ae3d6ad6c4c34a956c1b517514c78671fa4dfa", "0000000000000007e15ebd09f70db70f6231e657d3d1e204b0010518f1e5d610"),
    (2239936, 2239960, "0000000000000007e15ebd09f70db70f6231e657d3d1e204b0010518f1e5d610", "0000000000000007fe7686f8100c810a4c2949b95a28ab6a77cbc3e849c68d63"),
    (2239960, 2239984, "0000000000000007fe7686f8100c810a4c2949b95a28ab6a77cbc3e849c68d63", "000000000000001e47099fac7a87c4a12e86a9030ce676c4ef951fb5c92ac82c"),
    (2239984, 2240008, "000000000000001e47099fac7a87c4a12e86a9030ce676c4ef951fb5c92ac82c", "000000000000001ec7c3d8e68faad0a26a3dcb3359dab1db116b50714dee300a"),
    (2240008, 2240032, "000000000000001ec7c3d8e68faad0a26a3dcb3359dab1db116b50714dee300a", "000000000000000ca4bdfaa404967603ca9233519b9e94b4da11dae261244752"),
    (2240032, 2240056, "000000000000000ca4bdfaa404967603ca9233519b9e94b4da11dae261244752", "000000000000002c29db981bb07d3f34ec3fd0413b585f2826b513df3f09eb9c"),
    (2240056, 2240080, "000000000000002c29db981bb07d3f34ec3fd0413b585f2826b513df3f09eb9c", "000000000000002e17dab935a29bf4ebb9509664ab3f4e274cd4fe91079a02d9"),
    (2240080, 2240104, "000000000000002e17dab935a29bf4ebb9509664ab3f4e274cd4fe91079a02d9", "000000000000001a4a67f2eb1fd7bad85ca8f47b33e34ccccde9ea58e6241bf8"),
    (2240104, 2240128, "000000000000001a4a67f2eb1fd7bad85ca8f47b33e34ccccde9ea58e6241bf8", "00000000000000145d3adcd6ff44b8124f58f82a4d04b1a72305315227a9a41d"),
    (2240128, 2240152, "00000000000000145d3adcd6ff44b8124f58f82a4d04b1a72305315227a9a41d", "000000000000000e55e050af40b8962e838373bd59fc7bc8786de6de88a44743"),
    (2240152, 2240176, "000000000000000e55e050af40b8962e838373bd59fc7bc8786de6de88a44743", "000000000000000922d5e44e02a76b0f8083a13ee5490775c2a327c8327ed870"),
    (2240176, 2240200, "000000000000000922d5e44e02a76b0f8083a13ee5490775c2a327c8327ed870", "000000000000000c52076ab122e62396589ed3b6ce1ae79ca44243f6aa97c0c1"),
    (2240200, 2240224, "000000000000000c52076ab122e62396589ed3b6ce1ae79ca44243f6aa97c0c1", "0000000000000027754e5bd1e1f4033cebfa547772384f71208d6e6d5fc43c67"),
    (2240224, 2240248, "0000000000000027754e5bd1e1f4033cebfa547772384f71208d6e6d5fc43c67", "00000000000000106b293663d2734d563f0b9cd431f882702f6d55a7cdeef763"),
    (2240248, 2240272, "00000000000000106b293663d2734d563f0b9cd431f882702f6d55a7cdeef763", "00000000000000198f6dc7cc4e4f2857479038ff7f0b6610cd5904482814f79d"),
    (2240272, 2240296, "00000000000000198f6dc7cc4e4f2857479038ff7f0b6610cd5904482814f79d", "0000000000000020e061c8b15cee278eb31b4c32f1361094fa3be2b30479bb1c"),
    (2240296, 2240320, "0000000000000020e061c8b15cee278eb31b4c32f1361094fa3be2b30479bb1c", "000000000000001a6d144086d582b00d459d8dcfdb911f7a02a36142fd291f73"),
    (2240320, 2240344, "000000000000001a6d144086d582b00d459d8dcfdb911f7a02a36142fd291f73", "00000000000000271435c71750b45817d373f5bc6a3abe05edecc6d32271c97f"),
    (2240344, 2240368, "00000000000000271435c71750b45817d373f5bc6a3abe05edecc6d32271c97f", "0000000000000002c6ec0904eae608671c291a01fe9413199e61eb2565ae3b6e"),
    (2240368, 2240392, "0000000000000002c6ec0904eae608671c291a01fe9413199e61eb2565ae3b6e", "00000000000000122ff65a9a0ccf4d70eb179b469bc313a31bbeb5788175fd45"),
    (2240392, 2240416, "00000000000000122ff65a9a0ccf4d70eb179b469bc313a31bbeb5788175fd45", "00000000000000224c4c3f572fdec71989fa7785240b17add78f8c3c0aec4041"),
    (2240416, 2240440, "00000000000000224c4c3f572fdec71989fa7785240b17add78f8c3c0aec4041", "000000000000001e3fd12254dc97dcf25c3682b88cd2fc40e4a7350eaa66ef9b"),
    (2240440, 2240464, "000000000000001e3fd12254dc97dcf25c3682b88cd2fc40e4a7350eaa66ef9b", "0000000000000028cf5d09002476eafbb2b0cd3fdf210bc6ac6fd138b2241f28"),
]

# `mnListDiffTip` of a qrinfo reply is built against the responding node's *current* chain tip, so a
# qrinfo capture is only reproducible apart from that diff. The fixtures built from these qrinfos
# (`qrinfo_*.dat`) need the tip diff of the block they were originally taken at; Core serves exactly
# that as a plain `getmnlistd` from the base its tip diff used: the highest base of the request
# for the legacy construction (2227096), the work block of H for the efficient one.
# (file name, base hash, tip block hash). The 2240504 tip diff is the 2227096 -> 2240504 entry of
# MAINNET_DIFFS.
TIP_DIFFS = [
    ("mnlistdiff_tip_2518986.bin",
     "000000000000001970a8305e163c57cbeb0bec14046af8f55b594b86d10995b4",
     "00000000000000144d5c18277d2e4e2cefcb7c6dea6deaf0b78691a92d71b8eb"),
]

# (name, request block, base block hashes, announced protocol version); extraShare is always 1.
# The two fixtures were taken the way their original consumers asked: `qrinfo_2240504` with the
# single known list at 2227096 and a protocol below 70236, which selects the legacy construction
# (every diff is based on the highest request base below its work block, here always 2227096);
# `qrinfo_2518986` with no base list at the default protocol, which selects the efficient one
# (cycle diffs chain upward from genesis).
QRINFOS = [
    ("2240504", "00000000000000218d17031cc693da5c2d422b2644ec56c3fb6f43a617426ae6",
     ["000000000000000899fdcd85241296146c365b238a655517da8dcd08a8a79b98"], 70235),
    ("2518986", "00000000000000144d5c18277d2e4e2cefcb7c6dea6deaf0b78691a92d71b8eb", [], None),
]


def capture(network, peer, request, out, protocol=None):
    """Runs p2p_capture.py for one request, writing to `<out>.new`; returns (path, tmp)."""
    path = os.path.join(ROOT, out)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".new"
    kind, *params = request
    if kind == "mnlistd":
        args = ["mnlistd", params[0], params[1], tmp]
    else:  # qrinfo: block, extra share, output, then the base hashes
        args = ["qrinfo", params[0], "1", tmp, *params[1]]
    proto = ["--protocol", str(protocol)] if protocol else []
    r = subprocess.run(
        [sys.executable, CAPTURE, "--network", network, "--peer", peer, *proto, *args],
        capture_output=True,
        text=True,
    )
    sys.stderr.write(r.stderr)
    if r.returncode != 0:
        sys.exit("capture failed for " + out)
    return path, tmp


def finish(path, tmp, check):
    new = open(tmp, "rb").read()
    os.remove(tmp)
    if check:
        old = open(path, "rb").read() if os.path.exists(path) else None
        status = "same" if old == new else "DIFFERENT"
        print("%s: %s sha256=%s" % (os.path.relpath(path, ROOT), status, hashlib.sha256(new).hexdigest()))
        return old == new
    with open(path, "wb") as f:
        f.write(new)
    print("%s: wrote %d bytes" % (os.path.relpath(path, ROOT), len(new)))
    return True


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--mainnet-peer", required=True)
    ap.add_argument("--testnet-peer", help="required unless --skip-testnet")
    ap.add_argument("--skip-testnet", action="store_true")
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    if a.testnet_peer is None and not a.skip_testnet:
        ap.error("--testnet-peer is required (or pass --skip-testnet)")
    ok = True
    for network, out, base, block in NAMED_DIFFS:
        peer = a.mainnet_peer if network == "mainnet" else a.testnet_peer
        if peer is None:  # --skip-testnet
            continue
        path, tmp = capture(network, peer, ("mnlistd", base, block), out)
        ok &= finish(path, tmp, a.check)
        time.sleep(0.3)
    for base_h, h, base, block in MAINNET_DIFFS:
        if base_h == 0:
            continue
        path, tmp = capture("mainnet", a.mainnet_peer, ("mnlistd", base, block),
                            "%s/mnlistdiff_%d_%d.bin" % (WIRE, base_h, h))
        ok &= finish(path, tmp, a.check)
        time.sleep(0.3)
    for file_name, base, block in TIP_DIFFS:
        path, tmp = capture("mainnet", a.mainnet_peer, ("mnlistd", base, block), "%s/%s" % (WIRE, file_name))
        ok &= finish(path, tmp, a.check)
        time.sleep(0.3)
    for name, block, bases, protocol in QRINFOS:
        path, tmp = capture("mainnet", a.mainnet_peer, ("qrinfo", block, bases),
                            "%s/qrinfo_%s.bin" % (WIRE, name), protocol)
        if a.check:
            # Only the tip diff moves; report the rest of the file as compared by the Rust test.
            os.remove(tmp)
            print("%s: tip-dependent, not compared byte for byte" % os.path.relpath(path, ROOT))
        else:
            ok &= finish(path, tmp, a.check)
        time.sleep(1)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
