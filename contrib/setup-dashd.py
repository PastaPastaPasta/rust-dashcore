#!/usr/bin/env python3
"""Cross-platform setup script for dashd and test blockchain data.

Downloads the Dash Core binary and regtest test data for integration tests.
Outputs DASHD_PATH and DASHD_TEST_DATA lines suitable for appending to GITHUB_ENV
or evaluating in a shell.

The dashd release archive is checked against the SHA256 of its entry in the release's
SHA256SUMS.asc before it is extracted; a mismatch, a missing entry or an unreadable list stops
the script (nothing is extracted and the archive is deleted). The list comes from the same
release as the archive, so this catches corrupt and swapped downloads, not a compromised release.
Extraction is staged in a hidden directory of the cache and renamed into place. A dashcore-<v> directory
that an older version of this script half-extracted and that already holds bin/dashd is still trusted
(the cache test is unchanged): delete it once to have it fetched and checked.

Environment variables:
    DASHVERSION        - Dash Core version (default: 23.1.0)
                         (dash-spv's masternode DKG tests do not run on v24 yet; the node's own
                         dashd tests take a v24 binary through ST2_DASHD, with DASHVERSION=24.0.0-rc.4)
    TEST_DATA_VERSION  - Test data release version (default: v0.0.4)
    TEST_DATA_REPO     - GitHub repo for test data (default: dashpay/regtest-blockchain)
    CACHE_DIR          - Cache directory (default: ~/.rust-dashcore-test)
"""

import hashlib
import os
import platform
import re
import shutil
import sys
import tarfile
import tempfile
import time
import urllib.request
import zipfile

# Keep these defaults in sync with .github/workflows/build-and-test.yml
DASHVERSION = os.environ.get("DASHVERSION", "23.1.0")
TEST_DATA_VERSION = os.environ.get("TEST_DATA_VERSION", "v0.0.4")
TEST_DATA_REPO = os.environ.get("TEST_DATA_REPO", "dashpay/regtest-blockchain")


def get_cache_dir():
    if "CACHE_DIR" in os.environ:
        return os.environ["CACHE_DIR"]
    home = os.environ.get("HOME") or os.environ.get("USERPROFILE")
    if not home:
        sys.exit("Cannot determine home directory: neither HOME nor USERPROFILE is set")
    return os.path.join(home, ".rust-dashcore-test")


def get_asset_info():
    """Return the asset filename for the current platform."""
    system = platform.system()
    machine = platform.machine()

    if system == "Linux":
        linux_archs = {"aarch64": "aarch64", "arm64": "aarch64", "x86_64": "x86_64", "amd64": "x86_64"}
        arch = linux_archs.get(machine)
        if not arch:
            sys.exit(f"Unsupported Linux architecture: {machine}")
        asset = f"dashcore-{DASHVERSION}-{arch}-linux-gnu.tar.gz"
    elif system == "Darwin":
        darwin_archs = {"arm64": "arm64", "x86_64": "x86_64"}
        arch = darwin_archs.get(machine)
        if not arch:
            sys.exit(f"Unsupported macOS architecture: {machine}")
        asset = f"dashcore-{DASHVERSION}-{arch}-apple-darwin.tar.gz"
    elif system == "Windows":
        asset = f"dashcore-{DASHVERSION}-win64.zip"
    else:
        sys.exit(f"Unsupported platform: {system}")

    return asset


def log(msg):
    print(msg, file=sys.stderr)


def download(url, dest, timeout=300, retries=3):
    for attempt in range(1, retries + 1):
        try:
            log(f"Downloading {url} (attempt {attempt}/{retries})...")
            with urllib.request.urlopen(url, timeout=timeout) as response:
                with open(dest, "wb") as f:
                    while chunk := response.read(8192):
                        f.write(chunk)
            return
        except Exception as e:
            log(f"Download failed: {e}")
            if attempt == retries:
                sys.exit(f"Failed to download {url} after {retries} attempts")
            time.sleep(5 * attempt)


def parse_sha256sums(text):
    """{file name: lowercase hex digest} from a SHA256SUMS list, clearsigned (SHA256SUMS.asc) or plain.
    Lines that are not `<64 hex digits>  <name>` (PGP armour, blanks) are ignored; a name listed
    with two different digests is an error."""
    sums = {}
    for line in text.splitlines():
        line = line.removeprefix("- ")  # clearsign dash-escaping
        parts = line.split(None, 1)
        if len(parts) != 2 or len(parts[0]) != 64 or not all(c in "0123456789abcdefABCDEF" for c in parts[0]):
            continue
        digest, name = parts[0].lower(), parts[1].strip().lstrip("*")
        if sums.setdefault(name, digest) != digest:
            sys.exit(f"SHA256SUMS lists {name} with two different digests")
    return sums


def verify_sha256(archive_path, asset, sums):
    """Exit unless archive_path hashes to asset's entry in sums."""
    expected = sums.get(asset)
    if expected is None:
        sys.exit(f"SHA256SUMS has no entry for {asset}; refusing to extract {archive_path}")
    h = hashlib.sha256()
    with open(archive_path, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    if h.hexdigest() != expected:
        sys.exit(f"SHA256 mismatch for {asset}: expected {expected}, got {h.hexdigest()}; refusing to extract")
    log(f"SHA256 of {asset} matches SHA256SUMS")


def extract(archive_path, dest_dir):
    if archive_path.endswith(".zip"):
        with zipfile.ZipFile(archive_path, "r") as zf:
            zf.extractall(dest_dir)
    else:
        with tarfile.open(archive_path, "r:gz") as tf:
            tf.extractall(dest_dir, filter="data")


# A run killed by SIGKILL/SIGTERM never reaches its cleanup and leaves its hidden temporary files and
# staging tree (about 200 MB) in the cache. Entries this old are such leftovers: a live run keeps writing
# to its archive, reads the list right after fetching it, and creates its staging directory only once the
# archive is verified (extraction follows at once). Only the exact names tempfile makes for this script
# are swept (".dashcore-<version>.<8 chars>[.tar.gz|.zip]", ".SHA256SUMS-<version>.<8 chars>"), because
# CACHE_DIR may be a shared directory such as $HOME with a ".dashcore-backup" of someone's in it.
STALE_TEMP_SECONDS = 3600
TEMP_NAME = re.compile(r"^\.(?:dashcore|SHA256SUMS)-\d.*\.[a-z0-9_]{8}(?:\.tar\.gz|\.zip)?$")


def sweep_stale_temp(cache_dir, max_age=STALE_TEMP_SECONDS):
    """Remove the temporary files and staging directories of killed runs (see above) from cache_dir."""
    now = time.time()
    for name in os.listdir(cache_dir):
        path = os.path.join(cache_dir, name)
        try:
            if not TEMP_NAME.match(name) or now - os.lstat(path).st_mtime < max_age:
                continue
            if os.path.isdir(path) and not os.path.islink(path):
                shutil.rmtree(path, ignore_errors=True)
            else:
                os.remove(path)
            log(f"Removed stale {name}")
        except OSError:
            pass  # gone already (another run swept it) or not ours to remove


def setup_dashd(cache_dir):
    """Download and extract dashd binary. Returns the path to the dashd binary."""
    sweep_stale_temp(cache_dir)
    asset = get_asset_info()
    dashd_dir = os.path.join(cache_dir, f"dashcore-{DASHVERSION}")

    ext = ".exe" if platform.system() == "Windows" else ""
    dashd_bin = os.path.join(dashd_dir, "bin", f"dashd{ext}")

    if os.path.isfile(dashd_bin):
        log(f"dashd {DASHVERSION} already available")
        return dashd_bin

    log(f"Downloading dashd {DASHVERSION}...")
    base_url = f"https://github.com/dashpay/dash/releases/download/v{DASHVERSION}"
    # Unique names in the (shared) cache: a concurrent run can neither truncate the archive this run
    # verified nor delete its files, and an extraction that fails halfway never touches dashd_dir.
    suffix = ".zip" if asset.endswith(".zip") else ".tar.gz"
    archive_fd, archive_path = tempfile.mkstemp(prefix=f".{asset}.", suffix=suffix, dir=cache_dir)
    sums_fd, sums_path = tempfile.mkstemp(prefix=f".SHA256SUMS-{DASHVERSION}.", dir=cache_dir)
    os.close(archive_fd)
    os.close(sums_fd)
    staging = None
    try:
        download(f"{base_url}/SHA256SUMS.asc", sums_path)
        with open(sums_path, encoding="utf-8", errors="replace") as f:
            sums = parse_sha256sums(f.read())
        download(f"{base_url}/{asset}", archive_path)
        verify_sha256(archive_path, asset, sums)
        staging = tempfile.mkdtemp(prefix=f".dashcore-{DASHVERSION}.", dir=cache_dir)
        extract(archive_path, staging)
        staged_dir = os.path.join(staging, f"dashcore-{DASHVERSION}")
        if not os.path.isfile(os.path.join(staged_dir, "bin", f"dashd{ext}")):
            sys.exit(f"Expected binary not found after extraction: {dashd_bin}")
        if os.path.isdir(dashd_dir) and not os.path.isfile(dashd_bin):
            shutil.rmtree(dashd_dir)  # left by an earlier failed run: no dashd in it
        try:
            os.rename(staged_dir, dashd_dir)
        except OSError:
            if not os.path.isfile(dashd_bin):
                raise
            log(f"dashd {DASHVERSION} was installed by another run")
    finally:
        for leftover in (sums_path, archive_path):
            if os.path.exists(leftover):
                os.remove(leftover)
        if staging:
            shutil.rmtree(staging, ignore_errors=True)
    log(f"Downloaded dashd to {dashd_dir}")

    if not os.path.isfile(dashd_bin):
        sys.exit(f"Expected binary not found after extraction: {dashd_bin}")

    return dashd_bin


# Each entry maps a variant directory name to a marker path (relative to that
# directory) used as a cache hit / extraction-success check. Single-node
# variants ship a `regtest/blocks` subdirectory; the masternode network ships a
# top-level `network.json` plus per-node datadirs.
VARIANTS = {
    "regtest-40000": "regtest/blocks",
    "regtest-200": "regtest/blocks",
    "regtest-mn": "network.json",
}


def setup_test_data(cache_dir, variant, marker_relpath):
    """Download and extract a single test blockchain variant."""
    parent_dir = os.path.join(cache_dir, f"regtest-blockchain-{TEST_DATA_VERSION}")
    test_data_dir = os.path.join(parent_dir, variant)
    marker_path = os.path.join(test_data_dir, marker_relpath)

    if os.path.exists(marker_path):
        log(f"Test blockchain data {variant} ({TEST_DATA_VERSION}) already available")
        return test_data_dir

    log(f"Downloading test blockchain data {variant} ({TEST_DATA_VERSION})...")
    os.makedirs(parent_dir, exist_ok=True)

    archive_name = f"{variant}.tar.gz"
    archive_path = os.path.join(cache_dir, archive_name)
    url = f"https://github.com/{TEST_DATA_REPO}/releases/download/{TEST_DATA_VERSION}/{archive_name}"
    download(url, archive_path)
    extract(archive_path, parent_dir)
    os.remove(archive_path)

    if not os.path.exists(marker_path):
        sys.exit(f"Expected marker not found after extraction: {marker_path}")

    log(f"Downloaded test data to {test_data_dir}")
    return test_data_dir


def main():
    cache_dir = get_cache_dir()
    os.makedirs(cache_dir, exist_ok=True)

    dashd_path = setup_dashd(cache_dir)
    variant_dirs = {
        variant: setup_test_data(cache_dir, variant, marker)
        for variant, marker in VARIANTS.items()
    }

    datadir = os.path.join(cache_dir, f"regtest-blockchain-{TEST_DATA_VERSION}")

    # GITHUB_ENV expects bare NAME=value; shell `eval` needs `export NAME=value`.
    prefix = "" if os.environ.get("GITHUB_ACTIONS") == "true" else "export "
    print(f"{prefix}DASHD_PATH={dashd_path}")
    print(f"{prefix}DASHD_TEST_DATA={datadir}")
    print(f"{prefix}DASHD_MN_DATADIR={variant_dirs['regtest-mn']}")


if __name__ == "__main__":
    main()
