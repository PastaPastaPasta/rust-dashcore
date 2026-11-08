"""Download, verify and unpack the pinned Dash Core release."""

import hashlib
import os
import subprocess
import tarfile
import urllib.request

from . import paths

URL = "https://github.com/dashpay/dash/releases/download/v{v}/{asset}"


def fetch_dashd(version):
    arch = paths.arch()
    asset = f"dashcore-{version}-{arch}-linux-gnu.tar.gz"
    dl = paths.path("dl", f"v{version}")
    os.makedirs(dl, exist_ok=True)
    tarball, sums = os.path.join(dl, asset), os.path.join(dl, "SHA256SUMS.asc")
    for name, dst in ((asset, tarball), ("SHA256SUMS.asc", sums)):
        if not os.path.exists(dst):
            print(f"downloading {name}")
            urllib.request.urlretrieve(URL.format(v=version, asset=name), dst)
    with open(tarball, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    with open(sums) as f:
        listed = {line.split()[1]: line.split()[0] for line in f if line.strip().endswith(".tar.gz")
                  and len(line.split()) == 2}
    pinned = paths.DASHD_SHA256.get((version, arch))
    if listed.get(asset) != digest or (pinned and pinned != digest):
        raise SystemExit(f"sha256 mismatch for {asset}: got {digest}, SHA256SUMS {listed.get(asset)}, "
                         f"pinned {pinned}")
    # Signatures: report, don't require (keys must be imported by the operator).
    gpg = subprocess.run(["gpg", "--verify", sums], capture_output=True, text=True)
    good = [line for line in gpg.stderr.splitlines() if "Good signature" in line]
    print(f"sha256 ok ({digest}); gpg: {len(good)} good signature(s)" + "".join(f"\n  {g}" for g in good))
    target = os.path.join(paths.path("bin"), f"dashcore-{version}", "bin", "dashd")
    if os.path.exists(target):
        print("already unpacked (running binaries cannot be overwritten; delete the directory to re-unpack)")
    else:
        with tarfile.open(tarball) as tf:
            tf.extractall(paths.path("bin"), filter="data")
    print(f"dashd at {target}")
    return 0
